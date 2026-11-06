//! Deep branch and mutation-killing unit tests for `system_actor.rs`,
//! `system_actor/cycles.rs`, `system_actor/cycles_tackle.rs`,
//! `system_actor/disruptors.rs`, `system_actor/celestials.rs`, and `system_actor/belts.rs`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, TargetLock};
use eve_sde::{DogmaEffectType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ship_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 2_000_000.0,
        radius: 60.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn destiny_event_routing_rah_resonance_clamping_and_belt_downtime_respawn() {
    // 1. apply_destiny_event_in_actor: OnSovereigntyChanged succeeds even for unknown ball_id
    let mut bp = Ballpark::new(30_000_142);
    assert!(!cycles_tackle::apply_destiny_event_in_actor(
        &mut bp,
        999_999,
        DestinyEvent::SetBallFree { ball_id: 999_999, is_free: true },
    ));
    assert!(cycles_tackle::apply_destiny_event_in_actor(
        &mut bp,
        888_888,
        DestinyEvent::OnSovereigntyChanged { solar_system_id: 30_000_142, alliance_id: 99_001_234 },
    ));

    let ship_id: BallId = 4101;
    let drone_id: BallId = 4102;
    bp.add_ball(make_ship_ball(ship_id, 587, DVec3::ZERO));
    bp.add_ball(make_ship_ball(drone_id, 2454, DVec3::new(1000.0, 0.0, 0.0)));
    bp.ball_slim_meta.insert(
        ship_id,
        eve_destiny::SlimItemMeta {
            owner_id: 90_001_111,
            char_id: None,
            ..Default::default()
        },
    );

    // Mismatched inner ball_id falls through to `other` rather than mutating ball_id
    for ev in [
        DestinyEvent::SetBallFree { ball_id: drone_id, is_free: true },
        DestinyEvent::SetBallHarmonic { ball_id: drone_id, harmonic: 3 },
        DestinyEvent::SetBallInteractive { ball_id: drone_id, is_interactive: false },
        DestinyEvent::SetBallRadius { ball_id: drone_id, radius: 999.0 },
    ] {
        assert!(cycles_tackle::apply_destiny_event_in_actor(&mut bp, ship_id, ev));
    }
    assert!((bp.balls[&ship_id].radius - 60.0).abs() < 1e-9);

    // Matching variants update ballpark state
    for (target, ev) in [
        (ship_id, DestinyEvent::SetBallFree { ball_id: ship_id, is_free: true }),
        (ship_id, DestinyEvent::SetBallHarmonic { ball_id: ship_id, harmonic: 2 }),
        (ship_id, DestinyEvent::SetBallInteractive { ball_id: ship_id, is_interactive: true }),
        (ship_id, DestinyEvent::SetBallRadius { ball_id: ship_id, radius: 75.0 }),
        (ship_id, DestinyEvent::OnClientControllerEvent {
            ball_id: ship_id, target_id: None, event_name: "ping".into(), value: 1, delay: Some(0.0),
        }),
        (ship_id, DestinyEvent::OnShipStateUpdate { module_states: Vec::new() }),
        (ship_id, DestinyEvent::OnDbuffUpdated { ball_id: ship_id, dbuffs: Vec::new() }),
        (ship_id, DestinyEvent::OnDotVictimUpdated {
            ball_id: ship_id, flat_dps: 15.0, pct_dps: 0.02, remaining_ticks: 10,
        }),
        (drone_id, DestinyEvent::OnDroneStateChange {
            drone_id, owner_id: 90_001_111, controller_id: ship_id,
            activity_state: 1, type_id: 2454, controller_owner_id: 90_001_111, target_id: Some(ship_id),
        }),
        (ship_id, DestinyEvent::OnSlimItemChange { ball_id: ship_id }),
    ] {
        assert!(cycles_tackle::apply_destiny_event_in_actor(&mut bp, target, ev));
    }
    assert!((bp.balls[&ship_id].radius - 75.0).abs() < 1e-9);
    assert_eq!(bp.ball_slim_meta[&ship_id].char_id, Some(90_001_111));

    // 2. step_rah_resonances: saturated donors early return, asymmetric shares & duration clamps
    let sat = cycles::step_rah_resonances([1.0, 1.0, 1.0, 1.0], [100.0, 0.0, 0.0, 0.0], 6.0);
    assert_eq!(sat, [1.0, 1.0, 1.0, 1.0]);
    let all_four = cycles::step_rah_resonances([0.85, 0.85, 0.85, 0.85], [50.0, 50.0, 50.0, 50.0], 6.0);
    assert_eq!(all_four, [0.85, 0.85, 0.85, 0.85]);

    // 75% EM + 25% Therm incoming over 6.0s (shift_pct = 0.06): taker0(EM) -0.045, taker3(Therm) -0.015, donors 1,2 +0.03 each
    let asym = cycles::step_rah_resonances([0.80, 0.80, 0.80, 0.80], [300.0, 100.0, 0.0, 0.0], 6.0);
    assert!((asym[0] - 0.755).abs() < 1e-9);
    assert!((asym[1] - 0.830).abs() < 1e-9);
    assert!((asym[2] - 0.830).abs() < 1e-9);
    assert!((asym[3] - 0.785).abs() < 1e-9);

    // 3. Belt mining exhaustion -> due_unix respawn and downtime ice/full-belt respawn
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = SdeRegistry::with_canonical_seed();
    let sample = sde.types.values().next().unwrap().clone();
    let mut ice_type = sample.clone();
    ice_type.type_id = 16262;
    ice_type.group_id = 465; // Ice
    ice_type.volume = 1000.0;
    sde.types.insert(16262, ice_type);
    let mut ore_type = sample;
    ore_type.type_id = 1230;
    ore_type.group_id = 450; // Veldspar
    ore_type.volume = 0.1;
    sde.types.insert(1230, ore_type);
    let sde_opt = Some(Arc::new(sde));

    let belt_ore: BallId = 40_000_100;
    let belt_ice: BallId = 40_000_200;
    let ast_ore: BallId = 70_000_101;
    let ast_ice_partial: BallId = 70_000_201;
    let ast_ice_full: BallId = 70_000_202;

    let mut asteroids = HashMap::new();
    let mut templates = HashMap::new();
    for (aid, bid, tid, units) in [
        (ast_ore, belt_ore, 1230_u32, 500_u64),
        (ast_ice_partial, belt_ice, 16262_u32, 100_u64),
        (ast_ice_full, belt_ice, 16262_u32, 100_u64),
    ] {
        let st = AsteroidState {
            asteroid_id: aid,
            belt_id: bid,
            type_id: tid,
            initial_units: units,
            remaining_units: units,
            unit_volume: 1.0,
            depleted: false,
            respawn_at_tick: None,
        };
        let ball = Ball {
            id: aid,
            type_id: tid,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        bp.add_ball(ball.clone());
        asteroids.insert(aid, st.clone());
        templates.insert(aid, (st, ball));
    }

    // Partially mine ice rock so ice belt is NOT fully depleted, only 1 rock touched
    let rng = crate::rng::MechanicRng::seeded(42);
    let interval_a = std::sync::atomic::AtomicU64::new(3600);
    let dt_a = std::sync::atomic::AtomicU64::new(39600);
    let mine_res = belts::handle_mine_asteroid(
        30_000_142, ast_ice_partial, 20, 0.0, &mut bp, &mut asteroids, &rng, &Some(Arc::clone(&db)), &interval_a, &dt_a,
    ).unwrap();
    assert_eq!(mine_res.mined_units, 20);
    assert_eq!(mine_res.remaining_units, 80);

    // Deplete sole rock in belt_ore with waste_probability = 1.0
    let mine_full = belts::handle_mine_asteroid(
        30_000_142, ast_ore, 300, 1.0, &mut bp, &mut asteroids, &rng, &Some(Arc::clone(&db)), &interval_a, &dt_a,
    ).unwrap();
    assert_eq!(mine_full.mined_units, 300);
    assert_eq!(mine_full.wasted_units, 200);
    assert!(mine_full.depleted);
    assert!(!bp.balls.contains_key(&ast_ore));
    assert!(asteroids[&ast_ore].respawn_at_tick.is_some());

    // Downtime respawn restores BOTH depleted ore belt rock AND partially mined ice block (group 465)!
    let restored_count = belts::respawn_depleted_belts_on_downtime_inner(
        &mut bp, &mut asteroids, &templates, &sde_opt, &Some(Arc::clone(&db)),
    );
    assert_eq!(restored_count, 2);
    assert_eq!(asteroids[&ast_ore].remaining_units, 500);
    assert_eq!(asteroids[&ast_ice_partial].remaining_units, 100);
    assert!(bp.balls.contains_key(&ast_ore));

    // Set due respawn on depleted belt_ore and verify respawn_due_asteroids restores it
    asteroids.get_mut(&ast_ore).unwrap().remaining_units = 0;
    asteroids.get_mut(&ast_ore).unwrap().respawn_at_tick = Some(1);
    let due_respawned = belts::respawn_due_asteroids(&mut bp, &mut asteroids, &templates, &sde_opt, &Some(Arc::clone(&db)));
    assert_eq!(due_respawned, vec![ast_ore]);
    assert_eq!(asteroids[&ast_ore].remaining_units, 500);
}

#[test]
fn overload_cloak_web_shuttle_nullification_hic_and_lance_spool_cycles() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let sample = sde.types.values().next().unwrap().clone();
    sde.effects.insert(64_991, DogmaEffectType {
        effect_id: 64_991, name: "overloadSelfDurationBonus".into(), effect_category: 5,
        ..Default::default()
    });
    let mut ovl_mod = sample;
    ovl_mod.type_id = 945_001;
    ovl_mod.effect_ids = vec![64_991];
    for (aid, val) in [(1223_u16, 50.0), (1206, -20.0), (1230, 25.0), (1231, 30.0), (1210, 15.0), (1222, 40.0)] {
        ovl_mod.default_attributes.insert(aid, val);
    }
    sde.types.insert(945_001, ovl_mod);
    let sde_opt = Some(Arc::new(sde));

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let db_opt = Some(Arc::clone(&db));

    let ship_a: BallId = 5101;
    let ship_b: BallId = 5102;
    let shuttle_id: BallId = 5103;
    let sleeper_id: BallId = 5104;

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_ship_ball(ship_a, 587, DVec3::ZERO));
    bp.add_ball(make_ship_ball(ship_b, 587, DVec3::new(10_000.0, 0.0, 0.0)));
    let mut shuttle = make_ship_ball(shuttle_id, 672, DVec3::new(2_000.0, 0.0, 0.0));
    shuttle.mode = MovementMode::new_warp(DVec3::new(2_000.0, 0.0, 0.0), DVec3::new(100_000.0, 0.0, 0.0), 5.0);
    bp.add_ball(shuttle);
    bp.ball_slim_meta.insert(
        shuttle_id,
        eve_destiny::SlimItemMeta { group_id: 31, ..Default::default() },
    );
    let sleeper = make_ship_ball(sleeper_id, 30_195, DVec3::new(5_000.0, 0.0, 0.0));
    bp.add_ball(sleeper);

    for id in [ship_a, ship_b, shuttle_id, sleeper_id] {
        bp.combat_states.insert(
            id,
            CombatState {
                capacitor: 500.0,
                max_capacitor: 1000.0,
                shield_hp: 400.0,
                max_shield_hp: 1000.0,
                armor_hp: 500.0,
                max_armor_hp: 1000.0,
                structure_hp: 600.0,
                max_structure_hp: 1000.0,
                ..CombatState::default()
            },
        );
    }

    let mut cycles: Vec<ActiveModuleCycle> = Vec::new();
    let scramble_map = HashMap::new();
    let core_map = HashMap::new();

    // 1. Cloak + MWD interaction: cloak first, then add MWD -> MWD uses cloak_original_max_velocity and stays cloaked!
    let mut cloak = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 9101,
        module_type_id: 11578,
        effect_name: "cloaking".into(),
        duration_s: 10.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    };
    cloak.speed_boost_factor = -0.75; // 400 * 0.25 = 100 m/s cloaked
    cycles_tackle::handle_add_module_cycle(
        &mut bp, &mut cycles, &scramble_map, &core_map, &sde_opt, &db_opt, None, cloak,
    ).unwrap();
    assert_eq!(
        bp.combat_states.get(&ship_a).and_then(|cs| cs.cloak_original_max_velocity),
        Some(400.0)
    );
    assert!((bp.balls[&ship_a].max_velocity - 100.0).abs() < 1e-6);

    let mwd = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 9102,
        module_type_id: 434,
        effect_name: "moduleBonusMicrowarpdrive".into(),
        duration_s: 10.0,
        repeat: 1,
        is_mwd_or_mjd: true,
        speed_boost_factor: 5.0,
        ..ActiveModuleCycle::default()
    };
    cycles_tackle::handle_add_module_cycle(
        &mut bp, &mut cycles, &scramble_map, &core_map, &sde_opt, &db_opt, None, mwd.clone(),
    ).unwrap();
    // Cloaked speed remains 100 m/s while MWD stored original_max_velocity = 400.0!
    assert!((bp.balls[&ship_a].max_velocity - 100.0).abs() < 1e-6);
    assert_eq!(cycles.last().unwrap().original_max_velocity, Some(400.0));

    // Remove cloak -> restores 400.0 base velocity
    let mut state = SystemActorState {
        active_module_cycles: cycles,
        sde_opt: sde_opt.clone(),
        db_opt: db_opt.clone(),
        ..SystemActorState::default()
    };
    let cloak_removed = state.active_module_cycles.remove(0);
    cycles::cleanup_removed_module_cycle(&mut bp, &cloak_removed, &db_opt);
    assert!((bp.balls[&ship_a].max_velocity - 400.0).abs() < 1e-6);

    // Now scram ship_a from ship_b -> reconcile_tackle_and_bubbles immediately kills ship_a's active MWD!
    let scram = ActiveModuleCycle {
        ship_id: ship_b,
        module_id: 9103,
        module_type_id: 447,
        target_id: Some(ship_a),
        effect_name: "warpScrambleForEntity".into(),
        duration_s: 10.0,
        repeat: 1,
        warp_scramble_points: 2,
        disables_target_mwd_mjd: true,
        ..ActiveModuleCycle::default()
    };
    cycles_tackle::handle_add_module_cycle(
        &mut bp, &mut state.active_module_cycles, &scramble_map, &core_map, &sde_opt, &db_opt, None, scram,
    ).unwrap();
    assert!(bp.combat_states[&ship_a].is_mwd_mjd_disabled);
    assert!(!state.active_module_cycles.iter().any(|c| c.module_id == 9102));

    // Attempting to re-add MWD while scrambled returns WarpScrambled error
    assert!(cycles_tackle::handle_add_module_cycle(
        &mut bp, &mut state.active_module_cycles, &scramble_map, &core_map, &sde_opt, &db_opt, None, mwd,
    ).is_err());

    // Add bubble over shuttle (type_id 672 -> group 31 shuttle) -> shuttle stays in Warp::Aligning because group 31 is interdiction nullified!
    bp.add_warp_bubble(77_001, DVec3::new(2_000.0, 0.0, 0.0), 20_000.0);
    // Non-shuttle aligning inside bubble without emergency warp IS stopped
    if let Some(sb) = bp.balls.get_mut(&ship_b) {
        sb.position = DVec3::new(2_000.0, 0.0, 0.0);
        sb.mode = MovementMode::new_warp(DVec3::new(2_000.0, 0.0, 0.0), DVec3::new(100_000.0, 0.0, 0.0), 3.0);
    }
    cycles_tackle::reconcile_tackle_and_bubbles(
        &mut bp, &mut state.active_module_cycles, &scramble_map, &core_map, &db_opt, None,
    );
    assert!(matches!(bp.balls[&shuttle_id].mode, MovementMode::Warp { .. }));
    assert!(matches!(bp.balls[&ship_b].mode, MovementMode::Stop));

    // Overload toggle on & off symmetry on a grappler + repairer cycle
    let mut grp = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 9104,
        module_type_id: 945_001,
        target_id: Some(ship_b),
        effect_name: "moduleBonusStasisGrappler".into(),
        duration_s: 5.0,
        repeat: 2,
        speed_boost_factor: 1.0,
        original_max_velocity: Some(400.0),
        remote_armor_amount: 100.0,
        remote_shield_amount: 100.0,
        armor_repair_amount: 100.0,
        shield_boost_amount: 100.0,
        hull_repair_amount: 100.0,
        remote_optimal: 10_000.0,
        remote_falloff: 5_000.0,
        grappler_optimal_m: 1_000.0,
        grappler_falloff_m: 10_000.0,
        grappler_speed_factor_pct: -60.0,
        damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0),
        max_effective_range_m: Some(25_000.0),
        ..ActiveModuleCycle::default()
    };
    cycles_tackle::apply_overload_transition_to_cycle(&mut grp, true, &mut bp, &sde_opt);
    assert!(grp.is_overloaded);
    assert!((grp.duration_s - 4.0).abs() < 1e-9);
    assert!((grp.remote_armor_amount - 125.0).abs() < 1e-9);
    assert!((grp.remote_shield_amount - 130.0).abs() < 1e-9);
    assert!((grp.remote_optimal - 14_000.0).abs() < 1e-9);
    assert!(grp.target_velocity_multiplier.is_some());
    cycles_tackle::apply_overload_transition_to_cycle(&mut grp, false, &mut bp, &sde_opt);
    assert!(!grp.is_overloaded);
    assert!((grp.duration_s - 5.0).abs() < 1e-9);
    assert!((grp.remote_armor_amount - 100.0).abs() < 1e-9);

    // Multi-mechanic step_cycles_and_capacitor: Nosferatu drains Sleeper (infinite cap) while Energy Neut is ignored on Sleeper, plus disallow_assistance blocking remote rep!
    if let Some(cs) = bp.combat_states.get_mut(&ship_a) {
        cs.capacitor = 400.0;
        cs.disallow_assistance = true;
    }
    let nos = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 9105,
        module_type_id: 530,
        target_id: Some(sleeper_id),
        effect_name: "energyNosferatuFalloff".into(),
        duration_s: 1.0,
        repeat: 0,
        cap_drain_target: 120.0,
        cap_transfer_target: 90.0,
        remote_optimal: 50_000.0,
        remote_falloff: 10_000.0,
        ..ActiveModuleCycle::default()
    };
    let neut = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 9106,
        module_type_id: 531,
        target_id: Some(sleeper_id),
        effect_name: "energyNeutralizerFalloff".into(),
        duration_s: 1.0,
        repeat: 0,
        cap_drain_target: 200.0,
        remote_optimal: 50_000.0,
        remote_falloff: 10_000.0,
        ..ActiveModuleCycle::default()
    };
    let blocked_rr = ActiveModuleCycle {
        ship_id: ship_b,
        module_id: 9107,
        module_type_id: 3530,
        target_id: Some(ship_a),
        effect_name: "remoteArmorRepairFalloff".into(),
        duration_s: 1.0,
        repeat: 0,
        remote_armor_amount: 150.0,
        ..ActiveModuleCycle::default()
    };
    state.active_module_cycles = vec![nos, neut, blocked_rr];
    cycles::step_cycles_and_capacitor(
        &mut bp,
        &mut state.active_module_cycles,
        &mut state.pending_anchoring_disruptors,
        &mut state.active_nanite_repairs,
        &mut state.ship_rack_heat,
        &mut state.rah_incoming_damage,
        &scramble_map,
        &core_map,
        &sde_opt,
        &db_opt,
        &None,
        &state.tactical_state,
        &mut state.pending_cycle_actions,
        1.0,
        None,
    );
    // Nosferatu transferred +90 GJ to ship_a from infinite-cap Sleeper, Sleeper cap stayed 500, blocked RR left ship_a armor at 500!
    assert!((bp.combat_states[&ship_a].capacitor - 490.0).abs() < 1e-6);
    assert!((bp.combat_states[&sleeper_id].capacitor - 500.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship_a].armor_hp - 500.0).abs() < 1e-6);
}

#[tokio::test(flavor = "current_thread")]
async fn system_registry_actor_commands_tidi_ecm_burst_warp_reconcile_and_hooks() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let sys_id: u32 = 30_000_144;
    let owner_note_id = -(7_500_000_000_000_i64 + i64::from(sys_id));
    db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            owner_note_id,
            eve_db::OwnerNoteRecord {
                note_id: owner_note_id,
                character_id: 0,
                item_id: owner_note_id,
                label: "tidi".into(),
                note: "0.25".into(),
                timestamp_unix: 100,
            },
        );
    });

    let hook_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h1 = Arc::clone(&hook_hits);
    let registry = SystemRegistry::with_sde_and_db(Arc::new(SdeRegistry::with_canonical_seed()), Arc::clone(&db));
    registry.set_target_lock_completed_hook(Arc::new(move |_, _| {
        h1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    let handle = registry.get_or_spawn(sys_id);
    assert!((handle.tidi_factor() - 0.25).abs() < 1e-6);

    // Clamping & NaN/Inf guards on set_tidi_factor_raw
    handle.set_tidi_factor_raw(0.01);
    assert!((handle.tidi_factor() - 0.10).abs() < 1e-6);
    handle.set_tidi_factor_raw(2.5);
    assert!((handle.tidi_factor() - 1.0).abs() < 1e-6);
    handle.set_tidi_factor_raw(f64::NAN);
    assert!((handle.tidi_factor() - 1.0).abs() < 1e-6);

    let ship_1: BallId = 6101;
    let ship_2: BallId = 6102;
    let ship_3: BallId = 6103;
    handle.spawn_ball_with_meta(
        make_ship_ball(ship_1, 587, DVec3::ZERO),
        CombatState { capacitor: 800.0, max_capacitor: 1000.0, ..CombatState::default() },
        eve_destiny::SlimItemMeta { char_id: Some(90_001_111), owner_id: 90_001_111, ..Default::default() },
    ).await.unwrap();
    handle.spawn_ball_with_combat(
        make_ship_ball(ship_2, 587, DVec3::new(3_000.0, 0.0, 0.0)),
        CombatState::default(),
    ).await.unwrap();
    handle.spawn_ball_with_combat(
        make_ship_ball(ship_3, 587, DVec3::new(50_000.0, 0.0, 0.0)),
        CombatState::default(),
    ).await.unwrap();

    // Target lock completion in step_manual fires target_lock_completed_hook
    handle.add_target_lock(
        ship_1,
        TargetLock {
            target_id: ship_2,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 1.0,
            locked: false,
            max_range_m: Some(100_000.0),
        },
    ).await.unwrap();
    let _bcast = handle.step_manual(1.0).await.unwrap();
    assert_eq!(hook_hits.load(std::sync::atomic::Ordering::Relaxed), 1);

    // ApplyEcmBurst jams ship_2 (within range with guaranteed strength 500 >= 10) and leaves out-of-range ship_3 unjammed!
    let jammed = handle.apply_ecm_burst(ship_1, 15_000.0, [500.0; 4], 10.0, 0.0).await.unwrap();
    assert_eq!(jammed, vec![ship_2]);

    // Nanite repair start & stop refunds unconsumed paste and leaves remaining module damage!
    let mod_id: i64 = 95_001;
    db.with_mut_snapshot(|snap| {
        let mut item = eve_db::InventoryItemRecord {
            item_id: mod_id,
            type_id: 587,
            owner_id: 90_001_111,
            location_id: ship_1,
            flag: eve_db::InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 40.0);
        snap.items.insert(mod_id, item);
    });
    let _ = handle.start_nanite_repair(ActiveNaniteRepair {
        char_id: 90_001_111,
        ship_id: ship_1,
        module_id: mod_id,
        initial_damage_hp: 40.0,
        paste_total_needed: 10,
        paste_consumed_so_far: 10,
        duration_s: 10.0,
        elapsed_s: 4.0,
    }).await;
    let stop_res = handle.stop_nanite_repair(ship_1, mod_id).await.unwrap();
    assert_eq!(stop_res, Some((24.0, 6)));

    // Disconnected warp sync: already within <= 5000m threshold vs distant target (> 5000m initiates warp & stays in warp)
    let arrived_dest = DVec3::new(1_000.0, 0.0, 0.0);
    handle.prepare_disconnected_warp_sync(
        ship_1, 587, 60.0, 2_000_000.0, 2.0, 400.0, 3.0, DVec3::ZERO, arrived_dest,
    );
    let (pos1, arrived1) = handle.reconcile_disconnected_warp_sync(ship_1, arrived_dest);
    assert!(arrived1 && (pos1 - arrived_dest).length() < 1e-6);
    assert!(matches!(handle.get_ball_snapshot(ship_1).await.unwrap().unwrap().1.mode, MovementMode::Stop));

    let far_dest = DVec3::new(500_000_000.0, 0.0, 0.0);
    let pre_warp_pos2 = handle.get_ball_snapshot(ship_2).await.unwrap().unwrap().1.position;
    handle.prepare_disconnected_warp_sync(
        ship_2, 587, 60.0, 2_000_000.0, 2.0, 400.0, 3.0, pre_warp_pos2, far_dest,
    );
    let (pos2, arrived2) = handle.reconcile_disconnected_warp_sync(ship_2, far_dest);
    assert!(!arrived2 && (pos2 - pre_warp_pos2).length() < 1e-6);

    // AcceptDocking + RemoveBall preserve queued OnDockingAccepted observer events for departing ships
    handle.accept_docking(ship_1, 60_003_760, DVec3::ZERO).await.unwrap();
    assert!(handle.remove_ball(ship_2).await.is_some());
    let bcast_after = handle.step_manual(0.1).await.unwrap();
    assert!(bcast_after.observer_events.contains_key(&ship_1));
}
