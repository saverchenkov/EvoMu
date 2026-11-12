//! Unit and mutation-killing tests for `system_actor::celestials`.

use super::*;
use crate::router::ServerContext;
use crate::system_actor::{ActiveModuleCycle, ActiveNaniteRepair, AsteroidState};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta, TargetLock};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: hull,
        max_structure_hp: hull,
        capacitor: cap,
        max_capacitor: cap,
        ..CombatState::default()
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::new(10.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tidi_factor_load_asteroids_and_sync_ball_helpers_cover_boundaries() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);

    // TiDi factor clamping and non-finite fallback
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);
    let prev = sys.set_tidi_factor_raw(0.25);
    assert!((prev - 1.0).abs() < 1e-9);
    assert!((sys.tidi_factor() - 0.25).abs() < 1e-9);

    let prev2 = sys.set_tidi_factor_raw(0.01); // clamps to 0.10
    assert!((prev2 - 0.25).abs() < 1e-9);
    assert!((sys.tidi_factor() - 0.10).abs() < 1e-9);

    let _ = sys.set_tidi_factor_raw(f64::NAN); // non-finite -> 1.0
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);

    // Spawn ball with meta & combat, stop ball, set position, update combat state
    let ship_id = 701_001_i64;
    let pos1 = DVec3::new(100_000.0, 200_000.0, 300_000.0);
    sys.try_spawn_ball_with_meta(
        make_ball(ship_id, 606, pos1),
        make_combat_state(500.0, 500.0, 500.0, 250.0),
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: 90_000_001,
            char_id: Some(90_000_001),
            ..Default::default()
        },
    );
    assert_eq!(sys.get_ball_position_sync(ship_id), Some(pos1));
    assert!(!sys.is_ball_scrambled_sync(ship_id));

    let pos2 = DVec3::new(150_000.0, 200_000.0, 300_000.0);
    sys.try_set_ball_position(ship_id, pos2);
    assert_eq!(sys.get_ball_position_sync(ship_id), Some(pos2));

    sys.try_stop_ball(ship_id);
    {
        let bp = sys.ballpark.read().unwrap();
        let b = bp.balls.get(&ship_id).unwrap();
        assert_eq!(b.speed_fraction, 0.0);
        assert_eq!(b.velocity, DVec3::ZERO);
        assert!(matches!(b.mode, MovementMode::Stop));
    }

    assert!(sys.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 2));
    assert!(!sys.try_update_combat_state(999_999_999, |_cs| {}));

    // Mobile warp disruptor bubble scrambles ship in range
    let bubble_id = 701_050_i64;
    sys.try_add_mobile_warp_disruptor(bubble_id, pos2, 20_000.0);
    assert!(sys.is_ball_scrambled_sync(ship_id));
    sys.try_remove_warp_bubble(bubble_id);

    // Asteroid spawning and belt-depletion respawn_at_tick scheduling
    let belt_id = 40_009_077_i64;
    let ast1 = 702_001_i64;
    let ast2 = 702_002_i64;
    for aid in [ast1, ast2] {
        sys.try_spawn_asteroid(
            AsteroidState {
                asteroid_id: aid,
                belt_id,
                type_id: 1230,
                initial_units: 1_000,
                remaining_units: 1_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(aid, 1230, pos2 + DVec3::new(1_000.0, 0.0, 0.0)),
        );
    }
    sys.sync_asteroid_quantity(ast1, 0, 55_555);
    {
        let st = sys.actor_state.read().unwrap();
        assert!(st.asteroids[&ast1].depleted);
        assert_eq!(st.asteroids[&ast1].respawn_at_tick, None); // ast2 still has ore!
    }
    sys.sync_asteroid_quantity(ast2, 0, 55_555);
    {
        let st = sys.actor_state.read().unwrap();
        assert_eq!(st.asteroids[&ast1].respawn_at_tick, Some(55_555));
        assert_eq!(st.asteroids[&ast2].respawn_at_tick, Some(55_555));
    }
    assert!(sys.respawn_depleted_belts_on_downtime() >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_cycle_queries_nanite_repairs_and_try_remove_ball_cleanup_work() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let char_id = 90_000_001_i64;
    let ship_id = 703_001_i64;
    let mod_adc = 703_101_i64;
    let mod_rah = 703_102_i64;
    let mod_mjd = 703_103_i64;
    let mod_ind = 703_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, char_id, 30_000_142, InventoryFlag::None, 1),
        );
        for mid in [mod_adc, mod_rah, mod_mjd, mod_ind] {
            let mut it = make_item(mid, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1);
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 40.0);
            snap.items.insert(mid, it);
            snap.module_active_states.insert(mid);
            snap.module_active_charges.insert(mid, 240);
        }
        snap.transient_item_attributes
            .entry(ship_id)
            .or_default()
            .insert(267, 0.5);
        snap.transient_item_attributes
            .entry(mod_rah)
            .or_default()
            .insert(84, 1.5);
        snap.transient_item_attributes
            .entry(mod_rah)
            .or_default()
            .insert(268, 0.3);
    });

    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 606, DVec3::new(200_000.0, 0.0, 0.0)),
        make_combat_state(500.0, 500.0, 500.0, 250.0),
    );

    {
        let mut st = sys.actor_state.write().unwrap();
        let c_adc = ActiveModuleCycle {
            ship_id,
            module_id: mod_adc,
            module_type_id: 2048,
            duration_s: 10.0,
            repeat: 0,
            is_adc: true,
            is_overloaded: true,
            ..Default::default()
        };
        st.active_module_cycles.push(c_adc);

        let c_rah = ActiveModuleCycle {
            ship_id,
            module_id: mod_rah,
            module_type_id: 2048,
            duration_s: 10.0,
            repeat: 1,
            is_rah: true,
            mutadaptive_spool_step: 0.05,
            ..Default::default()
        };
        st.active_module_cycles.push(c_rah);

        let c_mjd = ActiveModuleCycle {
            ship_id,
            module_id: mod_mjd,
            module_type_id: 4383,
            duration_s: 10.0,
            repeat: 0,
            mjd_jump_distance_m: Some(100_000.0),
            ..Default::default()
        };
        st.active_module_cycles.push(c_mjd);

        let c_ind = ActiveModuleCycle {
            ship_id,
            module_id: mod_ind,
            module_type_id: 28583,
            duration_s: 60.0,
            repeat: 1,
            ..Default::default()
        };
        st.active_module_cycles.push(c_ind);

        st.active_nanite_repairs.push(ActiveNaniteRepair {
            ship_id,
            char_id,
            module_id: mod_adc,
            initial_damage_hp: 40.0,
            duration_s: 20.0,
            elapsed_s: 10.0, // 50% done -> 20 dmg remaining, 2 paste used, 2 refunded
            paste_total_needed: 4,
            paste_consumed_so_far: 4,
        });
    }

    assert!(sys.has_active_module_cycle_sync(ship_id, mod_adc));
    assert!(sys.has_active_mjd_or_mjfg_spool_sync(ship_id));
    assert!(sys.is_module_mjd_or_mjfg_spooling_sync(ship_id, mod_mjd));
    assert!(sys.has_active_overloaded_cycle_sync(ship_id));
    assert!(sys.has_active_industrial_core_sync(ship_id));
    assert!(sys.has_active_nanite_repair_sync(ship_id, mod_adc));
    assert!(sys.has_any_active_nanite_repair_sync(ship_id));
    assert!(sys.is_module_being_nanite_repaired_sync(ship_id, mod_adc));
    assert!(sys.active_entity_and_cycle_load() >= 10);

    // Cancel nanite repairs via try_remove_ball and verify DB state cleanup
    sys.try_remove_ball(ship_id);
    assert!(!sys.has_any_active_nanite_repair_sync(ship_id));
    assert!(!sys.has_active_module_cycle_sync(ship_id, mod_adc));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_adc));
        assert!(!snap.module_active_charges.contains_key(&mod_adc));
        let dmg = snap.items[&mod_adc]
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0);
        assert!((dmg - 20.0).abs() < 1e-6);
        assert!(!snap.transient_item_attributes[&ship_id].contains_key(&267));
        assert!(!snap.transient_item_attributes[&mod_rah].contains_key(&84));
        assert!(!snap.transient_item_attributes[&mod_rah].contains_key(&268));
    });

    // Also test cancel_nanite_repairs_for_ship_sync and persist_landed_warps_in_db + try_remove_ball_for_db
    let ship2 = 703_002_i64;
    let landing_pos = DVec3::new(999_000.0, 888_000.0, 777_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship2,
            make_item(ship2, 606, char_id, 30_000_142, InventoryFlag::None, 1),
        );
    });
    sys.try_spawn_ball_with_combat(
        make_ball(ship2, 606, landing_pos),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    {
        let bp = sys.ballpark.read().unwrap();
        persist_landed_warps_in_db(
            &bp,
            &[DestinyEvent::OnDeactivatingWarp { ball_id: ship2 }],
            &Some(Arc::clone(&ctx.db)),
        );
    }
    let saved_pos = ctx.db.with_snapshot(|s| s.item_positions.get(&ship2).copied());
    assert_eq!(saved_pos, Some(landing_pos.to_array()));

    try_remove_ball_for_db(&ctx.db, 30_000_142, ship2);
    assert_eq!(sys.get_ball_position_sync(ship2), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_system_handle_commands_execute_roundtrip() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let b1 = 704_001_i64;
    let b2 = 704_002_i64;
    let p1 = DVec3::new(50_000.0, 0.0, 0.0);
    let p2 = DVec3::new(55_000.0, 0.0, 0.0);

    let _ = sys
        .spawn_ball_with_meta(
            make_ball(b1, 606, p1),
            make_combat_state(500.0, 500.0, 500.0, 200.0),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = sys
        .spawn_ball_with_combat(
            make_ball(b2, 606, p2),
            make_combat_state(500.0, 500.0, 500.0, 200.0),
        )
        .await
        .unwrap();

    assert_eq!(sys.get_ball_group_id(b1).await.unwrap(), Some(25));
    assert!(sys.get_ball_slim_item(b1).await.unwrap().is_some());
    assert!(sys.set_ball_wormhole_stages(b1, 1, 0.5).await.unwrap());
    assert!(
        sys.change_slim_item(
            b1,
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                security_status: 2.5,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    );

    let _ = sys.set_speed_fraction(b1, 0.5).await.unwrap();
    let _ = sys.goto_point(b1, p2).await.unwrap();
    assert!(sys.set_ball_position(b1, p1 + DVec3::new(100.0, 0.0, 0.0)).await);
    assert!(
        sys.emit_destiny_event(
            b1,
            DestinyEvent::OnSpecialFX {
                ball_id: b1,
                module_id: None,
                guid: "effects.WarpScramble".into(),
                is_active: true,
            }
        )
        .await
    );
    assert!(sys.emit_packaged_actions(b1, vec![]).await);
    assert!(
        !sys.emit_observer_destiny_event(999_999, DestinyEvent::OnSlimItemChange { ball_id: b1 })
            .await
    );

    // Locks, capacitor, combat state, scramble, interdiction nullification, MJD
    sys.add_target_lock(
        b1,
        TargetLock {
            target_id: b2,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(sys.get_target_locks(b1).await.unwrap().len(), 1);
    assert!(sys.remove_target_lock(b1, b2).await.unwrap());
    assert_eq!(sys.clear_target_locks(b1).await.unwrap(), 0);

    sys.set_capacitor(b1, 123.0).await.unwrap();
    assert!((sys.get_combat_state(b1).await.unwrap().unwrap().capacitor - 123.0).abs() < 1e-6);

    let _ = sys
        .apply_damage(
            b1,
            DamageVector {
                em: 50.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
        )
        .await
        .unwrap();
    let healed = sys.heal_combat_state(b1).await.unwrap();
    assert!((healed.shield_hp - 500.0).abs() < 1e-6);

    let _ = sys.set_warp_scramble_strength(b1, 1).await.unwrap();
    assert!(sys.is_ball_scrambled(b1).await.unwrap());
    let _ = sys.set_warp_core_strength(b1, 2).await.unwrap();
    assert!(!sys.is_ball_scrambled(b1).await.unwrap());
    assert!(sys.set_interdiction_nullified(b1, true).await.unwrap());

    let jumped_pos = sys.micro_jump(b1, 100_000.0).await.unwrap();
    assert!(jumped_pos.distance(p1) > 50_000.0);

    let _ = sys.push_set_state(b1).await.unwrap();
    let (_tick, _state_rep, _updates_rep) = sys.get_set_state(b1).await.unwrap();
    let _ = sys.drain_observer_updates(b1).await.unwrap();
    assert!(sys.get_bubble_id_for_ball(b1).await.unwrap().is_some());
    assert!(!sys.list_balls_in_bubble(b1).await.unwrap().is_empty());
    assert!(!sys.list_dynamic_balls_in_bubble(b1).await.unwrap().is_empty());

    sys.try_step_manual(0.5);
    assert!(sys.sim_elapsed_secs() >= 0.5);

    assert!(sys.remove_ball(b2).await.is_some());
    assert!(sys.accept_docking(b1, 60_003_760, DVec3::ZERO).await.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn celestials_gate_cloak_wormhole_group_velocity_and_fast_forward_boundaries() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);

    // 1. Unknown ball returns false/None guards cleanly
    assert!(!sys.break_gate_cloak(999_888_111).await.unwrap());
    assert_eq!(sys.get_ball_group_id(999_888_111).await.unwrap(), None);

    let ship_id = 706_001_i64;
    let wh_id = 706_002_i64;
    let mut cloak_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    cloak_cs.is_cloaked = true;
    cloak_cs.gate_cloak_remaining_ticks = 20;

    let mut ship_meta = SlimItemMeta::default();
    ship_meta.group_id = 25;
    ship_meta.is_cloaked = true;

    let _ = sys.spawn_ball_with_combat(make_ball(ship_id, 587, DVec3::ZERO), cloak_cs).await.unwrap();
    let _ = sys.change_slim_item(ship_id, ship_meta).await.unwrap();

    assert_eq!(sys.get_ball_group_id(ship_id).await.unwrap(), Some(25));
    assert!(sys.break_gate_cloak(ship_id).await.unwrap());
    // Calling a second time once uncloaked returns false
    assert!(!sys.break_gate_cloak(ship_id).await.unwrap());
    {
        let bp = sys.ballpark.read().unwrap();
        assert!(!bp.combat_states[&ship_id].is_cloaked);
        assert_eq!(bp.combat_states[&ship_id].gate_cloak_remaining_ticks, 0);
        assert!(!bp.ball_slim_meta[&ship_id].is_cloaked);
    }

    // 2. Speed fraction clamping [0.0, 1.0] & max_velocity update
    let _ = sys.set_ball_max_velocity(ship_id, 420.0).await.unwrap();
    let _ = sys.set_speed_fraction(ship_id, 1.5).await.unwrap();
    assert!((sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity - 420.0).abs() < 1e-6);
    assert!((sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.speed_fraction - 1.0).abs() < 1e-6);
    let _ = sys.set_speed_fraction(ship_id, -0.5).await.unwrap();
    assert!((sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.speed_fraction - 0.0).abs() < 1e-6);

    // 3. Wormhole mass/life stages update cleanly
    let mut wh_meta = SlimItemMeta::default();
    wh_meta.group_id = 988;
    wh_meta.category_id = 2;
    let _ = sys
        .spawn_ball_with_meta(
            make_ball(wh_id, 30_679, DVec3::new(10_000.0, 0.0, 0.0)),
            CombatState::default(),
            wh_meta,
        )
        .await
        .unwrap();
    let _ = sys.set_ball_wormhole_stages(wh_id, 2, 1.0).await.unwrap();

    // 4. fast_forward_clock_in_actor: missing ball, stationary non-invuln early return, stationary invuln 65s cap
    let mut bp = Ballpark::new(30_000_142);
    let pos_cache = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new()));
    let scram_cache = Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let mut sim_elapsed = 0.0_f64;

    // Missing ball -> no change
    super::celestials_helpers::fast_forward_clock_in_actor(
        30_000_142,
        &mut bp,
        999_777,
        30,
        &mut sim_elapsed,
        &None,
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert_eq!(sim_elapsed, 0.0);

    // Stationary + non-invulnerable -> early return (no steps)
    let mut b100 = make_ball(100, 587, DVec3::ZERO);
    b100.mode = MovementMode::Stop;
    bp.add_ball(b100);
    bp.combat_states.insert(100, make_combat_state(100.0, 100.0, 100.0, 100.0));
    super::celestials_helpers::fast_forward_clock_in_actor(
        30_000_142,
        &mut bp,
        100,
        50,
        &mut sim_elapsed,
        &None,
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert_eq!(sim_elapsed, 0.0);

    // Stationary + invulnerable for 200 ticks with secs = 120 -> capped at 65s (NOT 120/300!)
    bp.combat_states.get_mut(&100).unwrap().undock_invulnerable_remaining_ticks = 200;
    super::celestials_helpers::fast_forward_clock_in_actor(
        30_000_142,
        &mut bp,
        100,
        120,
        &mut sim_elapsed,
        &None,
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!((sim_elapsed - 65.0).abs() < 1e-9);

    // Moving ship with secs = 120 runs full 120 steps (<= 300 cap)
    let mut mover = make_ball(101, 587, DVec3::ZERO);
    mover.mode = MovementMode::GotoDirection { direction: DVec3::X };
    bp.add_ball(mover);
    bp.combat_states.insert(101, make_combat_state(100.0, 100.0, 100.0, 100.0));
    let before = sim_elapsed;
    super::celestials_helpers::fast_forward_clock_in_actor(
        30_000_142,
        &mut bp,
        101,
        120,
        &mut sim_elapsed,
        &None,
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!((sim_elapsed - before - 120.0).abs() < 1e-9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_handle_sync_helpers_hooks_and_closed_channel_error_paths() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let mut rx_ev = sys.subscribe();

    let char_id = 90_000_001_i64;
    let ship1 = 705_001_i64;
    let ship2 = 705_002_i64;
    let mod_scram = 705_101_i64;
    let mod_nanite = 705_102_i64;

    sys.spawn_ball_with_combat(
        make_ball(ship1, 606, DVec3::new(10_000.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    )
    .await
    .unwrap();
    sys.spawn_ball_with_combat(
        make_ball(ship2, 606, DVec3::new(12_000.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    )
    .await
    .unwrap();

    assert!(sys.set_ball_max_velocity(ship1, 450.0).await.unwrap());
    let snap = sys.get_ball_snapshot(ship1).await.unwrap().unwrap().1;
    assert!((snap.max_velocity - 450.0).abs() < 1e-6);

    // active_offensive_aggressors_on_sync + cancel_nanite_repairs_for_ship_sync
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: ship2,
            module_id: mod_scram,
            module_type_id: 3242, // Warp Scrambler
            target_id: Some(ship1),
            duration_s: 10.0,
            ..Default::default()
        });
        st.active_nanite_repairs.push(ActiveNaniteRepair {
            ship_id: ship1,
            char_id,
            module_id: mod_nanite,
            initial_damage_hp: 10.0,
            duration_s: 10.0,
            elapsed_s: 10.0, // 100% done -> rem_dmg <= 1e-6 -> removes DAMAGE attr
            paste_total_needed: 2,
            paste_consumed_so_far: 2,
        });
        st.active_nanite_repairs.push(ActiveNaniteRepair {
            ship_id: ship2,
            char_id,
            module_id: mod_nanite + 1,
            initial_damage_hp: 10.0,
            duration_s: 10.0,
            elapsed_s: 1.0,
            paste_total_needed: 2,
            paste_consumed_so_far: 1,
        });
    }
    let agg = sys.active_offensive_aggressors_on_sync(ship1, Some(&ctx.sde));
    assert_eq!(agg, vec![ship2]);
    let cancelled = sys.cancel_nanite_repairs_for_ship_sync(ship2);
    assert_eq!(cancelled.len(), 1);
    assert!(!sys.has_any_active_nanite_repair_sync(ship2));
    assert!(sys.has_any_active_nanite_repair_sync(ship1));

    // cancel_ship_nanite_repairs_sync when rem_dmg <= 1e-6 removes DAMAGE attribute
    ctx.db.with_mut_snapshot(|snap| {
        let mut it = make_item(mod_nanite, 2048, char_id, ship1, InventoryFlag::LoSlot0, 1);
        it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(mod_nanite, it);
    });
    sys.cancel_ship_nanite_repairs_sync(ship1, true);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items[&mod_nanite]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
    });

    // fast_forward_clock + step_manual + take_pending_cycle_actions + try_step_manual
    sys.fast_forward_clock(ship1, 2).await.unwrap();
    let _ = sys.step_manual(1.0).await.unwrap();
    let _ = rx_ev.try_recv();
    let _ = sys.take_pending_cycle_actions();
    sys.try_step_manual(0.5);

    // Closed-channel SystemHandle covers every "Solar system actor stopped" error branch
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<SystemCommand>(1);
    drop(cmd_rx);
    let (event_tx, _) = tokio::sync::broadcast::channel::<TickBroadcast>(8);
    let dead = SystemHandle {
        solar_system_id: 30_000_142,
        cmd_tx,
        event_tx,
        ball_positions: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        scrambled_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        committed_warp_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        active_module_ships: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        ballpark: Arc::new(eve_db::lock_rank::RankedRwLock::new(Ballpark::new(30_000_142))),
        actor_state: Arc::new(eve_db::lock_rank::RankedRwLock::new(super::super::SystemActorState::default())),
        last_auto_stepped_unix: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        tidi_factor_bits: Arc::new(std::sync::atomic::AtomicU64::new(1.0_f64.to_bits())),
        command_panics: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        panic_next_command_for_test: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        npc_death_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        module_deactivation_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        target_lock_completed_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        system_step_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
    };

    assert!(dead.spawn_ball(make_ball(1, 606, DVec3::ZERO)).await.is_err());
    assert!(dead.set_ball_max_velocity(1, 100.0).await.is_err());
    assert!(dead.spawn_ball_with_combat(make_ball(1, 606, DVec3::ZERO), CombatState::default()).await.is_err());
    assert!(dead.spawn_ball_with_meta(make_ball(1, 606, DVec3::ZERO), CombatState::default(), SlimItemMeta::default()).await.is_err());
    assert!(dead.change_slim_item(1, SlimItemMeta::default()).await.is_err());
    assert!(dead.set_ball_wormhole_stages(1, 1, 0.5).await.is_err());
    assert!(dead.get_ball_slim_item(1).await.is_err());
    assert!(dead.spawn_asteroid(AsteroidState {
        asteroid_id: 1, belt_id: 2, type_id: 1230, initial_units: 10, remaining_units: 10, unit_volume: 0.1, depleted: false, respawn_at_tick: None
    }, make_ball(1, 1230, DVec3::ZERO)).await.is_err());
    assert!(dead.mine_asteroid(1, 10, 0.0).await.is_err());
    assert!(dead.get_asteroid_state(1).await.is_err());
    assert!(dead.break_gate_cloak(1).await.is_err());
    assert!(dead.populate_asteroid_belt_entities().await.is_err());
    assert!(dead.remove_ball(1).await.is_none());
    assert!(dead.accept_docking(1, 2, DVec3::ZERO).await.is_none());
    assert!(!dead.emit_packaged_actions(1, vec![]).await);
    assert!(dead.set_movement_mode(1, MovementMode::Stop).await.is_err());
    assert!(dead.set_warp_scramble_strength(1, 1).await.is_err());
    assert!(dead.set_warp_core_strength(1, 1).await.is_err());
    assert!(dead.add_warp_bubble(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(dead.remove_warp_bubble(1).await.is_err());
    assert!(dead.set_speed_fraction(1, 1.0).await.is_err());
    assert!(!dead.set_ball_position(1, DVec3::ZERO).await);
    assert!(!dead.emit_destiny_event(1, DestinyEvent::RemoveBall { ball_id: 1 }).await);
    assert!(
        !dead
            .emit_observer_destiny_event(1, DestinyEvent::RemoveBall { ball_id: 1 })
            .await
    );
    assert!(dead.goto_point(1, DVec3::ZERO).await.is_err());
    assert!(dead.push_set_state(1).await.is_err());
    assert!(dead.get_ball_snapshot(1).await.is_err());
    assert!(dead.get_ball_group_id(1).await.is_err());
    assert!(dead.list_balls().await.is_err());
    assert!(dead.list_balls_in_bubble(1).await.is_err());
    assert!(dead.list_dynamic_balls_in_bubble(1).await.is_err());
    assert!(dead.get_set_state(1).await.is_err());
    assert!(dead.get_bubble_id_for_ball(1).await.is_err());
    assert!(dead.drain_observer_updates(1).await.is_err());
    assert!(dead.apply_damage(1, DamageVector::default()).await.is_err());
    assert!(dead.step_manual(1.0).await.is_err());
    assert!(dead.fast_forward_clock(1, 5).await.is_err());
    assert!(dead.get_target_locks(1).await.is_err());
    assert!(dead.add_target_lock(1, TargetLock { target_id: 2, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.is_err());
    assert!(dead.remove_target_lock(1, 2).await.is_err());
    assert!(dead.clear_target_locks(1).await.is_err());
    assert!(dead.set_capacitor(1, 10.0).await.is_err());
    assert!(dead.heal_combat_state(1).await.is_err());
    assert!(dead.set_combat_state(1, CombatState::default()).await.is_err());
    assert!(dead.get_combat_state(1).await.is_err());
    assert!(dead.add_module_cycle(ActiveModuleCycle::default()).await.is_err());
    assert!(dead.remove_module_cycle(1, 2).await.is_err());
    assert!(!dead.has_module_cycle(1, 2).await);
    assert!(dead.stop_non_warp_module_cycles(1).await.is_err());
    assert!(dead.set_interdiction_nullified(1, true).await.is_err());
    assert!(dead.micro_jump(1, 100.0).await.is_err());
    assert!(dead.micro_jump_field(1, 100.0, 1.0, 1).await.is_err());
    assert!(dead.tick().await.is_err());
    assert!(dead.is_ball_scrambled(1).await.is_err());
    assert!(dead.add_interdiction_sphere(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(dead.add_mobile_warp_disruptor(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(dead.apply_ecm_jam(1, 5.0).await.is_err());
    assert!(dead.apply_ecm_burst(1, 100.0, [1.0; 4], 5.0, 0.5).await.is_err());
    assert!(dead.apply_ecm_burst_ships(1, 100.0, [1.0; 4], 5.0, 0.5, None).await.is_err());
    assert!(dead.schedule_mobile_warp_disruptor(1, 12198, DVec3::ZERO, 100.0, 5.0).await.is_err());
    assert!(dead.update_module_overload(1, 2, true).await.is_err());
    assert!(dead.start_nanite_repair(ActiveNaniteRepair {
        ship_id: 1, char_id: 1, module_id: 2, initial_damage_hp: 1.0, duration_s: 1.0, elapsed_s: 0.0, paste_total_needed: 1, paste_consumed_so_far: 0
    }).await.is_err());
    assert!(dead.stop_nanite_repair(1, 2).await.is_err());
    assert!(dead.record_incoming_armor_damage(1, [1.0; 4]).await.is_err());

    // NaN TiDi clamps safely to 1.0 in both setter and getter
    sys.tidi_factor_bits.store(f64::NAN.to_bits(), std::sync::atomic::Ordering::Relaxed);
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);
    let prev_nan = sys.set_tidi_factor_raw(f64::NAN);
    assert!((prev_nan - 1.0).abs() < 1e-9);
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);

    // try_spawn_ball_with_meta and try_spawn_ball_with_combat replace existing balls cleanly
    let dup_id = 9_190_101;
    sys.try_spawn_ball_with_meta(make_ball(dup_id, 606, DVec3::ZERO), CombatState::default(), SlimItemMeta::default());
    sys.try_spawn_ball_with_meta(make_ball(dup_id, 606, DVec3::new(10.0, 0.0, 0.0)), CombatState::default(), SlimItemMeta::default());
    sys.try_spawn_ball_with_combat(make_ball(dup_id, 606, DVec3::new(20.0, 0.0, 0.0)), CombatState::default());

    // sync_asteroid_quantity depletes whole belt and sets respawn_at_tick
    let b_ast1 = 9_190_201;
    let b_ast2 = 9_190_202;
    sys.try_spawn_asteroid(
        AsteroidState { asteroid_id: b_ast1, belt_id: 777, type_id: 1230, initial_units: 100, remaining_units: 100, unit_volume: 0.1, depleted: false, respawn_at_tick: None },
        make_ball(b_ast1, 1230, DVec3::ZERO),
    );
    sys.try_spawn_asteroid(
        AsteroidState { asteroid_id: b_ast2, belt_id: 777, type_id: 1230, initial_units: 100, remaining_units: 0, unit_volume: 0.1, depleted: true, respawn_at_tick: None },
        make_ball(b_ast2, 1230, DVec3::new(50.0, 0.0, 0.0)),
    );
    sys.sync_asteroid_quantity(b_ast1, 0, 55_555);
    assert_eq!(sys.get_asteroid_state(b_ast1).await.unwrap().unwrap().respawn_at_tick, Some(55_555));

    // try_remove_ball with ADC + RAH transient attrs + deactivation hook + DB fallback
    let rem_ship = 9_190_301;
    let rem_mod_adc = 9_190_302;
    let rem_mod_rah = 9_190_303;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = rem_ship;
        }
        let tr_ship = snap.transient_item_attributes.entry(rem_ship).or_default();
        tr_ship.insert(267, 0.7);
        let tr_mod = snap.transient_item_attributes.entry(rem_mod_rah).or_default();
        tr_mod.insert(267, 0.7);
    });
    sys.try_spawn_ball_with_combat(make_ball(rem_ship, 606, DVec3::ZERO), CombatState::default());
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: rem_ship,
            module_id: rem_mod_adc,
            owner_char_id: None,
            is_adc: true,
            ..Default::default()
        });
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: rem_ship,
            module_id: rem_mod_rah,
            owner_char_id: None,
            is_rah: true,
            ..Default::default()
        });
        st.active_nanite_repairs.push(ActiveNaniteRepair {
            ship_id: rem_ship,
            char_id,
            module_id: rem_mod_adc,
            initial_damage_hp: 20.0,
            duration_s: 10.0,
            elapsed_s: 2.0,
            paste_total_needed: 10,
            paste_consumed_so_far: 6,
        });
    }
    sys.cancel_ship_nanite_repairs_sync(9_999_999, true); // keeps rem_ship's job (covers retain else-arm)
    let deact_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deact_calls_c = Arc::clone(&deact_calls);
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |_cid, _cyc| {
        deact_calls_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    sys.try_remove_ball(rem_ship);
    assert_eq!(deact_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    *sys.module_deactivation_hook.write().unwrap() = None;

    // Reply-dropped mock actor covers every "Solar system reply dropped" error branch
    let (drop_tx, mut drop_rx) = tokio::sync::mpsc::channel::<SystemCommand>(128);
    let dropper = tokio::spawn(async move {
        while let Some(cmd) = drop_rx.recv().await {
            drop(cmd);
        }
    });
    let (drop_evt_tx, _) = tokio::sync::broadcast::channel::<TickBroadcast>(8);
    let drop_h = SystemHandle {
        solar_system_id: 30_000_142,
        cmd_tx: drop_tx,
        event_tx: drop_evt_tx,
        ball_positions: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        scrambled_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        committed_warp_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        active_module_ships: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        ballpark: Arc::new(eve_db::lock_rank::RankedRwLock::new(Ballpark::new(30_000_142))),
        actor_state: Arc::new(eve_db::lock_rank::RankedRwLock::new(super::super::SystemActorState::default())),
        last_auto_stepped_unix: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        tidi_factor_bits: Arc::new(std::sync::atomic::AtomicU64::new(1.0_f64.to_bits())),
        command_panics: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        panic_next_command_for_test: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        npc_death_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        module_deactivation_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        target_lock_completed_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        system_step_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
    };
    assert!(drop_h.spawn_ball(make_ball(1, 606, DVec3::ZERO)).await.is_err());
    assert!(drop_h.set_ball_max_velocity(1, 100.0).await.is_err());
    assert!(drop_h.spawn_ball_with_combat(make_ball(1, 606, DVec3::ZERO), CombatState::default()).await.is_err());
    assert!(drop_h.spawn_ball_with_meta(make_ball(1, 606, DVec3::ZERO), CombatState::default(), SlimItemMeta::default()).await.is_err());
    assert!(drop_h.change_slim_item(1, SlimItemMeta::default()).await.is_err());
    assert!(drop_h.set_ball_wormhole_stages(1, 1, 0.5).await.is_err());
    assert!(drop_h.get_ball_slim_item(1).await.is_err());
    assert!(drop_h.spawn_asteroid(AsteroidState {
        asteroid_id: 1, belt_id: 2, type_id: 1230, initial_units: 10, remaining_units: 10, unit_volume: 0.1, depleted: false, respawn_at_tick: None
    }, make_ball(1, 1230, DVec3::ZERO)).await.is_err());
    assert!(drop_h.mine_asteroid(1, 10, 0.0).await.is_err());
    assert!(drop_h.get_asteroid_state(1).await.is_err());
    assert!(drop_h.break_gate_cloak(1).await.is_err());
    assert!(drop_h.populate_asteroid_belt_entities().await.is_err());
    assert!(drop_h.remove_ball(1).await.is_none());
    assert!(drop_h.accept_docking(1, 2, DVec3::ZERO).await.is_none());
    assert!(!drop_h.emit_packaged_actions(1, vec![]).await);
    assert!(drop_h.set_movement_mode(1, MovementMode::Stop).await.is_err());
    assert!(drop_h.set_warp_scramble_strength(1, 1).await.is_err());
    assert!(drop_h.set_warp_core_strength(1, 1).await.is_err());
    assert!(drop_h.add_warp_bubble(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(drop_h.remove_warp_bubble(1).await.is_err());
    assert!(drop_h.set_speed_fraction(1, 1.0).await.is_err());
    assert!(!drop_h.set_ball_position(1, DVec3::ZERO).await);
    assert!(!drop_h.emit_destiny_event(1, DestinyEvent::RemoveBall { ball_id: 1 }).await);
    assert!(drop_h.goto_point(1, DVec3::ZERO).await.is_err());
    assert!(drop_h.push_set_state(1).await.is_err());
    assert!(drop_h.get_ball_snapshot(1).await.is_err());
    assert!(drop_h.get_ball_group_id(1).await.is_err());
    assert!(drop_h.list_balls().await.is_err());
    assert!(drop_h.list_balls_in_bubble(1).await.is_err());
    assert!(drop_h.list_dynamic_balls_in_bubble(1).await.is_err());
    assert!(drop_h.get_set_state(1).await.is_err());
    assert!(drop_h.get_bubble_id_for_ball(1).await.is_err());
    assert!(drop_h.drain_observer_updates(1).await.is_err());
    assert!(drop_h.apply_damage(1, DamageVector::default()).await.is_err());
    assert!(drop_h.step_manual(1.0).await.is_err());
    assert!(drop_h.fast_forward_clock(1, 5).await.is_err());
    assert!(drop_h.get_target_locks(1).await.is_err());
    assert!(drop_h.add_target_lock(1, TargetLock { target_id: 2, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.is_err());
    assert!(drop_h.remove_target_lock(1, 2).await.is_err());
    assert!(drop_h.clear_target_locks(1).await.is_err());
    assert!(drop_h.set_capacitor(1, 10.0).await.is_err());
    assert!(drop_h.heal_combat_state(1).await.is_err());
    assert!(drop_h.set_combat_state(1, CombatState::default()).await.is_err());
    assert!(drop_h.get_combat_state(1).await.is_err());
    assert!(drop_h.add_module_cycle(ActiveModuleCycle::default()).await.is_err());
    assert!(drop_h.remove_module_cycle(1, 2).await.is_err());
    assert!(!drop_h.has_module_cycle(1, 2).await);
    assert!(drop_h.stop_non_warp_module_cycles(1).await.is_err());
    assert!(drop_h.set_interdiction_nullified(1, true).await.is_err());
    assert!(drop_h.micro_jump(1, 100.0).await.is_err());
    assert!(drop_h.micro_jump_field(1, 100.0, 1.0, 1).await.is_err());
    assert!(drop_h.is_ball_scrambled(1).await.is_err());
    assert!(drop_h.add_interdiction_sphere(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(drop_h.add_mobile_warp_disruptor(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(drop_h.apply_ecm_jam(1, 5.0).await.is_err());
    assert!(drop_h.apply_ecm_burst_ships(1, 100.0, [1.0; 4], 5.0, 0.5, None).await.is_err());
    assert!(drop_h.schedule_mobile_warp_disruptor(1, 12198, DVec3::ZERO, 100.0, 5.0).await.is_err());
    assert!(drop_h.update_module_overload(1, 2, true).await.is_err());
    assert!(drop_h.start_nanite_repair(ActiveNaniteRepair {
        ship_id: 1, char_id: 1, module_id: 2, initial_damage_hp: 1.0, duration_s: 1.0, elapsed_s: 0.0, paste_total_needed: 1, paste_consumed_so_far: 0
    }).await.is_err());
    assert!(drop_h.stop_nanite_repair(1, 2).await.is_err());
    assert!(drop_h.record_incoming_armor_damage(1, [1.0; 4]).await.is_err());
    drop(drop_h);
    let _ = dropper.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_handle_async_commands_asteroids_ecm_nanite_bubbles_and_hooks() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let mut rx = sys.subscribe();

    // 1. spawn_ball, set_ball_max_velocity, get_ball_snapshot, list_balls, break_gate_cloak, set_movement_mode
    let b1 = 710_001_i64;
    let b2 = 710_002_i64;
    sys.spawn_ball(make_ball(b1, 587, DVec3::new(10_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    assert!(sys.set_ball_max_velocity(b1, 555.0).await.unwrap());
    assert!(!sys.set_ball_max_velocity(999_999_999, 555.0).await.unwrap());
    let (_, snap_b1) = sys.get_ball_snapshot(b1).await.unwrap().unwrap();
    assert!((snap_b1.max_velocity - 555.0).abs() < 1e-6);
    assert!(sys.get_ball_snapshot(999_999_999).await.unwrap().is_none());
    assert!(sys.list_balls().await.unwrap().iter().any(|b| b.id == b1));

    sys.spawn_ball_with_combat(
        make_ball(b2, 587, DVec3::new(12_000.0, 0.0, 0.0)),
        CombatState {
            gate_cloak_remaining_ticks: 10,
            max_shield_hp: 500.0,
            shield_hp: 500.0,
            max_armor_hp: 500.0,
            armor_hp: 500.0,
            max_structure_hp: 500.0,
            structure_hp: 500.0,
            max_capacitor: 1_000.0,
            capacitor: 1_000.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    assert!(sys.break_gate_cloak(b2).await.unwrap());
    assert!(!sys.break_gate_cloak(b2).await.unwrap());
    assert!(!sys.break_gate_cloak(999_999_999).await.unwrap());

    let _ = sys
        .set_movement_mode(b2, MovementMode::Orbit { target_id: b1, range: 5_000.0 })
        .await
        .unwrap();
    assert!(sys
        .set_movement_mode(999_999_999, MovementMode::Stop)
        .await
        .is_err());

    // 2. try_step_manual with non-positive / NaN dt does not corrupt sim_elapsed_secs, while valid dt emits TickBroadcast
    let elapsed_before = sys.sim_elapsed_secs();
    sys.try_step_manual(0.0);
    sys.try_step_manual(f64::NAN);
    assert!((sys.sim_elapsed_secs() - elapsed_before).abs() < 1e-9);
    let _ = sys.set_tidi_factor_raw(0.5);
    sys.try_step_manual(2.0);
    assert!((sys.sim_elapsed_secs() - (elapsed_before + 1.0)).abs() < 1e-6);
    let bcast = rx.try_recv().expect("TickBroadcast emitted");
    assert_eq!(bcast.solar_system_id, 30_000_142);
    let _ = sys.set_tidi_factor_raw(1.0);

    // 3. Asteroids: spawn_asteroid, get_asteroid_state, mine_asteroid, populate_asteroid_belt_entities
    let ast_id = 711_001_i64;
    sys.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40_009_099,
            type_id: 1228,
            initial_units: 100,
            remaining_units: 100,
            unit_volume: 0.15,
            depleted: false,
            respawn_at_tick: None,
        },
        make_ball(ast_id, 1228, DVec3::new(15_000.0, 0.0, 0.0)),
    )
    .await
    .unwrap();
    let st_ast = sys.get_asteroid_state(ast_id).await.unwrap().unwrap();
    assert_eq!(st_ast.remaining_units, 100);
    assert!(sys.get_asteroid_state(999_999_999).await.unwrap().is_none());

    let ext1 = sys.mine_asteroid(ast_id, 40, 0.0).await.unwrap();
    assert_eq!(ext1.mined_units, 40);
    assert_eq!(ext1.remaining_units, 60);
    assert!(!ext1.depleted);
    let ext2 = sys.mine_asteroid(ast_id, 100, 0.0).await.unwrap();
    assert_eq!(ext2.mined_units, 60);
    assert!(ext2.depleted);
    assert!(sys.mine_asteroid(ast_id, 10, 0.0).await.is_err());

    let _belts = sys.populate_asteroid_belt_entities().await.unwrap();

    // 4. Bubbles & Disruptors (async): add_warp_bubble, remove_warp_bubble, add_interdiction_sphere, add_mobile_warp_disruptor, schedule_mobile_warp_disruptor
    let pos_b2 = sys.get_ball_position_sync(b2).unwrap();
    let _ = sys.add_warp_bubble(712_001, pos_b2, 5_000.0).await.unwrap();
    assert!(sys.is_ball_scrambled(b2).await.unwrap());
    assert!(sys.remove_warp_bubble(712_001).await.unwrap());
    assert!(!sys.remove_warp_bubble(712_001).await.unwrap());

    assert!(sys.add_interdiction_sphere(712_002, pos_b2, 5_000.0).await.unwrap());
    assert!(sys.is_ball_scrambled(b2).await.unwrap());
    assert!(sys.remove_warp_bubble(712_002).await.unwrap());

    assert!(sys.add_mobile_warp_disruptor(712_003, pos_b2, 5_000.0).await.unwrap());
    assert!(sys.is_ball_scrambled(b2).await.unwrap());
    assert!(sys.remove_warp_bubble(712_003).await.unwrap());

    assert!(
        sys.schedule_mobile_warp_disruptor(712_004, 12_198, pos_b2, 5_000.0, 2.0)
            .await
            .unwrap()
    );
    assert!(!sys.is_ball_scrambled(b2).await.unwrap());
    let _ = sys.step_manual(3.0).await.unwrap();
    assert!(sys.is_ball_scrambled(b2).await.unwrap());
    assert!(sys.remove_warp_bubble(712_004).await.unwrap());

    // 5. ECM jam & ECM burst
    assert!(sys.apply_ecm_jam(b2, 15.0).await.unwrap());
    assert!(!sys.apply_ecm_jam(999_999_999, 15.0).await.unwrap());
    let jammed = sys
        .apply_ecm_burst_ships(
            b1,
            20_000.0,
            [20.0; 4],
            15.0,
            0.0,
            Some(HashMap::from([(b2, (1usize, 5.0_f64, 0.0_f64))])),
        )
        .await
        .unwrap();
    assert!(jammed.contains(&b2));
    let _ = sys.apply_ecm_burst(b1, 20_000.0, [20.0; 4], 15.0, 0.99999).await.unwrap();

    // 6. Module cycles async, overload, fast_forward_clock, record_incoming_armor_damage, take_pending_cycle_actions, stop_non_warp_module_cycles
    sys.set_combat_state(
        b2,
        CombatState {
            max_capacitor: 1_000.0,
            capacitor: 1_000.0,
            max_shield_hp: 500.0,
            shield_hp: 500.0,
            max_armor_hp: 500.0,
            armor_hp: 500.0,
            max_structure_hp: 500.0,
            structure_hp: 500.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: b2,
        module_id: 713_001,
        module_type_id: 526,
        target_id: Some(b1),
        duration_s: 10.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    assert!(sys.has_module_cycle(b2, 713_001).await);
    assert!(!sys.has_module_cycle(b2, 999_999).await);
    assert!(sys.update_module_overload(b2, 713_001, true).await.unwrap());
    sys.fast_forward_clock(b2, 3).await.unwrap();
    assert!(sys.remove_module_cycle(b2, 713_001).await.unwrap().is_some());
    assert!(sys.remove_module_cycle(b2, 713_001).await.unwrap().is_none());

    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: b2,
        module_id: 713_002,
        module_type_id: 526,
        target_id: Some(b1),
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    sys.stop_non_warp_module_cycles(b2).await.unwrap();
    assert!(!sys.has_module_cycle(b2, 713_002).await);

    sys.record_incoming_armor_damage(b2, [10.0, 20.0, 30.0, 40.0])
        .await
        .unwrap();
    assert_eq!(
        sys.actor_state.read().unwrap().rah_incoming_damage.get(&b2).copied(),
        Some([10.0, 20.0, 30.0, 40.0])
    );

    {
        sys.actor_state
            .write()
            .unwrap()
            .pending_cycle_actions
            .push(super::super::PendingCycleAction::InvalidateShipFitting {
                char_id: None,
                ship_id: b2,
            });
    }
    assert_eq!(sys.take_pending_cycle_actions().len(), 1);
    assert!(sys.take_pending_cycle_actions().is_empty());

    // 7. Nanite repair async & sync helpers
    sys.start_nanite_repair(ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: b2,
        module_id: 714_001,
        initial_damage_hp: 20.0,
        paste_total_needed: 4,
        paste_consumed_so_far: 4,
        duration_s: 10.0,
        elapsed_s: 0.0,
    })
    .await
    .unwrap();
    assert!(sys.has_active_nanite_repair_sync(b2, 714_001));
    let stopped = sys.stop_nanite_repair(b2, 714_001).await.unwrap();
    assert!(stopped.is_some());
    assert!(sys.stop_nanite_repair(b2, 714_001).await.unwrap().is_none());

    // cancel_nanite_repairs_for_ship_sync preserves other ships
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_nanite_repairs.push(ActiveNaniteRepair {
            char_id: 90_000_001,
            ship_id: b2,
            module_id: 714_002,
            initial_damage_hp: 10.0,
            paste_total_needed: 2,
            paste_consumed_so_far: 2,
            duration_s: 10.0,
            elapsed_s: 10.0,
        });
        st.active_nanite_repairs.push(ActiveNaniteRepair {
            char_id: 90_000_002,
            ship_id: b1,
            module_id: 714_003,
            initial_damage_hp: 10.0,
            paste_total_needed: 2,
            paste_consumed_so_far: 2,
            duration_s: 10.0,
            elapsed_s: 0.0,
        });
    }
    let cancelled_b2 = sys.cancel_nanite_repairs_for_ship_sync(b2);
    assert_eq!(cancelled_b2.len(), 1);
    assert_eq!(cancelled_b2[0].module_id, 714_002);
    assert!(sys.has_active_nanite_repair_sync(b1, 714_003));

    // cancel_ship_nanite_repairs_sync when elapsed >= duration removes DAMAGE attr
    ctx.db.with_mut_snapshot(|snap| {
        let mut it = make_item(714_003, 2048, 90_000_002, b1, InventoryFlag::LoSlot0, 1);
        it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(714_003, it);
    });
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_nanite_repairs[0].elapsed_s = 10.0;
    }
    sys.cancel_ship_nanite_repairs_sync(b1, false);
    assert!(!ctx.db.with_snapshot(|s| s.items[&714_003]
        .dynamic_attributes
        .contains_key(&eve_sde::attr_ids::DAMAGE)));

    // 8. micro_jump_field, active_offensive_aggressors_on_sync, industrial core variants
    let jumped = sys.micro_jump_field(b2, 100_000.0, 5_000.0, 25).await.unwrap();
    assert!(jumped.contains(&b2));

    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: b1,
            module_id: 715_001,
            module_type_id: 42_890, // Industrial Core variant
            target_id: Some(b2),
            damage_vector: DamageVector {
                em: 50.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
            ..ActiveModuleCycle::default()
        });
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: 710_003,
            module_id: 715_002,
            module_type_id: 41,
            target_id: Some(b2),
            remote_shield_amount: 100.0, // Assistance, NOT offensive
            ..ActiveModuleCycle::default()
        });
    }
    assert!(sys.has_active_industrial_core_sync(b1));
    assert_eq!(sys.active_offensive_aggressors_on_sync(b2, Some(&ctx.sde)), vec![b1]);

    // 9. try_spawn_ball_with_meta / try_spawn_ball_with_combat replace existing ball, and hooks fire on step_manual / lethal damage / try_remove_ball
    let hook_hits = Arc::new(std::sync::Mutex::new(Vec::<(i64, Option<i64>)>::new()));
    let hh = Arc::clone(&hook_hits);
    *sys.npc_death_hook.write().unwrap() = Some(Arc::new(move |_sid, bid, att| {
        let hh2 = Arc::clone(&hh);
        Box::pin(async move {
            hh2.lock().unwrap().push((bid, att));
        })
    }));
    let step_hits = Arc::new(std::sync::Mutex::new(0usize));
    let sh = Arc::clone(&step_hits);
    *sys.system_step_hook.write().unwrap() = Some(Arc::new(move |_sid, _dt, _destroyed| {
        let sh2 = Arc::clone(&sh);
        Box::pin(async move {
            *sh2.lock().unwrap() += 1;
        })
    }));

    // Replace existing b1 cleanly
    sys.try_spawn_ball_with_meta(
        make_ball(b1, 587, DVec3::ZERO),
        make_combat_state(10.0, 10.0, 10.0, 10.0),
        SlimItemMeta::default(),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(b1, 587, DVec3::ZERO),
        make_combat_state(10.0, 10.0, 10.0, 10.0),
    );
    let _ = sys
        .apply_damage_with_attacker(
            b1,
            DamageVector {
                em: 10_000.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap();
    assert!(!hook_hits.lock().unwrap().is_empty());
    let _ = sys.tick().await.unwrap();
    assert!(*step_hits.lock().unwrap() >= 1);

    // Clear hooks and test try_remove_ball deactivation hook with DB owner lookup
    *sys.npc_death_hook.write().unwrap() = None;
    *sys.system_step_hook.write().unwrap() = None;
    let deact_calls = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let dc = Arc::clone(&deact_calls);
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |cid, cyc| {
        dc.lock().unwrap().push((cid, cyc.module_id));
    }));
    let ship_owner_test = 716_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_owner_test,
            make_item(ship_owner_test, 587, 90_000_001, 30_000_142, InventoryFlag::None, 1),
        );
    });
    sys.try_spawn_ball_with_combat(
        make_ball(ship_owner_test, 587, DVec3::ZERO),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    sys.actor_state.write().unwrap().active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_owner_test,
        module_id: 716_010,
        owner_char_id: None,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });
    sys.try_remove_ball(ship_owner_test);
    assert_eq!(*deact_calls.lock().unwrap(), vec![(90_000_001, 716_010)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_handle_stopped_actor_returns_error_or_false_on_all_commands() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(1);
    drop(cmd_rx);
    let (event_tx, _) = tokio::sync::broadcast::channel(4);
    let handle = SystemHandle {
        solar_system_id: 30_000_142,
        cmd_tx,
        event_tx,
        ball_positions: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        scrambled_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        committed_warp_balls: Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new())),
        active_module_ships: Arc::new(eve_db::lock_rank::RankedRwLock::new(std::collections::HashSet::new())),
        ballpark: Arc::new(eve_db::lock_rank::RankedRwLock::new(eve_destiny::Ballpark::new(30_000_142))),
        actor_state: Arc::new(eve_db::lock_rank::RankedRwLock::new(super::super::SystemActorState::default())),
        last_auto_stepped_unix: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        tidi_factor_bits: Arc::new(std::sync::atomic::AtomicU64::new(1.0_f64.to_bits())),
        command_panics: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        panic_next_command_for_test: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        npc_death_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        module_deactivation_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        target_lock_completed_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
        system_step_hook: Arc::new(eve_db::lock_rank::RankedRwLock::new(None)),
    };

    assert!(handle.spawn_ball(make_ball(1, 587, DVec3::ZERO)).await.is_err());
    assert!(handle.set_ball_max_velocity(1, 100.0).await.is_err());
    assert!(handle.spawn_ball_with_combat(make_ball(1, 587, DVec3::ZERO), CombatState::default()).await.is_err());
    assert!(handle.spawn_ball_with_meta(make_ball(1, 587, DVec3::ZERO), CombatState::default(), SlimItemMeta::default()).await.is_err());
    assert!(handle.change_slim_item(1, SlimItemMeta::default()).await.is_err());
    assert!(handle.set_ball_wormhole_stages(1, 1, 1.0).await.is_err());
    assert!(handle.get_ball_slim_item(1).await.is_err());
    assert!(handle.mine_asteroid(1, 10, 0.0).await.is_err());
    assert!(handle.get_asteroid_state(1).await.is_err());
    assert!(handle.break_gate_cloak(1).await.is_err());
    assert!(handle.populate_asteroid_belt_entities().await.is_err());
    assert!(handle.remove_ball(1).await.is_none());
    assert!(handle.accept_docking(1, 2, DVec3::ZERO).await.is_none());
    assert!(!handle.emit_packaged_actions(1, vec![]).await);
    assert!(handle.set_movement_mode(1, MovementMode::Stop).await.is_err());
    assert!(handle.set_warp_scramble_strength(1, 1).await.is_err());
    assert!(handle.set_warp_core_strength(1, 1).await.is_err());
    assert!(handle.add_warp_bubble(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(handle.remove_warp_bubble(1).await.is_err());
    assert!(handle.set_speed_fraction(1, 0.5).await.is_err());
    assert!(!handle.set_ball_position(1, DVec3::ZERO).await);
    assert!(!handle.emit_destiny_event(1, DestinyEvent::OnSlimItemChange { ball_id: 1 }).await);
    assert!(handle.goto_point(1, DVec3::ZERO).await.is_err());
    assert!(handle.push_set_state(1).await.is_err());
    assert!(handle.get_ball_snapshot(1).await.is_err());
    assert!(handle.get_ball_group_id(1).await.is_err());
    assert!(handle.list_balls().await.is_err());
    assert!(handle.list_balls_in_bubble(1).await.is_err());
    assert!(handle.list_dynamic_balls_in_bubble(1).await.is_err());
    assert!(handle.get_set_state(1).await.is_err());
    assert!(handle.get_bubble_id_for_ball(1).await.is_err());
    assert!(handle.drain_observer_updates(1).await.is_err());
    assert!(handle.apply_damage(1, DamageVector::default()).await.is_err());
    assert!(handle.step_manual(1.0).await.is_err());
    assert!(handle.fast_forward_clock(1, 5).await.is_err());
    assert!(handle.get_target_locks(1).await.is_err());
    assert!(handle.add_target_lock(1, TargetLock { target_id: 2, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.is_err());
    assert!(handle.remove_target_lock(1, 2).await.is_err());
    assert!(handle.clear_target_locks(1).await.is_err());
    assert!(handle.set_capacitor(1, 10.0).await.is_err());
    assert!(handle.heal_combat_state(1).await.is_err());
    assert!(handle.set_combat_state(1, CombatState::default()).await.is_err());
    assert!(handle.get_combat_state(1).await.is_err());
    assert!(handle.add_module_cycle(ActiveModuleCycle::default()).await.is_err());
    assert!(handle.remove_module_cycle(1, 2).await.is_err());
    assert!(!handle.has_module_cycle(1, 2).await);
    assert!(handle.stop_non_warp_module_cycles(1).await.is_err());
    assert!(handle.set_interdiction_nullified(1, true).await.is_err());
    assert!(handle.micro_jump(1, 100.0).await.is_err());
    assert!(handle.micro_jump_field(1, 100.0, 50.0, 5).await.is_err());
    assert!(handle.is_ball_scrambled(1).await.is_err());
    assert!(handle.add_interdiction_sphere(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(handle.add_mobile_warp_disruptor(1, DVec3::ZERO, 100.0).await.is_err());
    assert!(handle.apply_ecm_jam(1, 5.0).await.is_err());
    assert!(handle.apply_ecm_burst(1, 100.0, [1.0; 4], 5.0, 0.5).await.is_err());
    assert!(handle.schedule_mobile_warp_disruptor(1, 12198, DVec3::ZERO, 100.0, 5.0).await.is_err());
    assert!(handle.update_module_overload(1, 2, true).await.is_err());
    assert!(handle.start_nanite_repair(ActiveNaniteRepair { char_id: 1, ship_id: 1, module_id: 2, initial_damage_hp: 1.0, paste_total_needed: 1, paste_consumed_so_far: 1, duration_s: 1.0, elapsed_s: 0.0 }).await.is_err());
    assert!(handle.stop_nanite_repair(1, 2).await.is_err());
    assert!(handle.record_incoming_armor_damage(1, [0.0; 4]).await.is_err());

    // `try_sync_ship_ballpark_stats` returns false for unknown ball and updates baseline + SetMaxSpeed for live ship
    let ctx_sync = ServerContext::new();
    let sys_sync = ctx_sync.systems.get_or_spawn(30_000_142);
    assert!(!sys_sync.try_sync_ship_ballpark_stats(999_999, &CombatState::default()));
    let sync_ship = 9_190_501;
    let mut init_cs = make_combat_state(100.0, 200.0, 300.0, 150.0);
    init_cs.base_max_velocity = Some(250.0);
    sys_sync.try_spawn_ball_with_combat(
        make_ball(sync_ship, 587, DVec3::ZERO),
        init_cs.clone(),
    );
    let mut updated_cs = init_cs;
    updated_cs.base_max_velocity = Some(375.0);
    updated_cs.base_max_shield_hp = Some(500.0);
    assert!(sys_sync.try_sync_ship_ballpark_stats(sync_ship, &updated_cs));
    let bp = sys_sync.ballpark.read().unwrap();
    assert!((bp.balls[&sync_ship].max_velocity - 375.0).abs() < 1e-6);
    assert_eq!(bp.combat_states[&sync_ship].base_max_shield_hp, Some(500.0));
}

#[test]
fn ranked_cmd_sender_panics_when_lock_held_and_sync_registry_wrappers_respect_rank() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let ship_id = 9_190_601;
    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 587, DVec3::new(1_200.0, 3_400.0, -500.0)),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().max_velocity = 420.0;
        super::super::sync_ball_positions(&bp, &sys.ball_positions, &sys.scrambled_balls);
    }
    assert_eq!(
        ctx.systems.get_ball_position_sync(30_000_142, ship_id),
        Some(DVec3::new(1_200.0, 3_400.0, -500.0))
    );
    assert!(!ctx.systems.is_ball_scrambled_sync(30_000_142, ship_id));
    assert_eq!(
        ctx.systems.find_ball_position_sync(ship_id),
        Some(DVec3::new(1_200.0, 3_400.0, -500.0))
    );
    assert_eq!(ctx.systems.find_ball_system_sync(ship_id), Some(30_000_142));

    if cfg!(any(debug_assertions, feature = "lock-rank")) {
        let _bp = sys.ballpark.read().unwrap();
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sys.cmd_sender();
        }))
        .expect_err("cmd_sender() while holding RankedRwLock must panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&'static str>().map(|s| (*s).to_string()))
            .unwrap_or_default();
        assert!(msg.contains("lock-rank"), "unexpected panic msg: {msg}");
    }
    let _ = sys.cmd_sender();
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());

    if cfg!(any(debug_assertions, feature = "lock-rank")) {
        {
            let _bp = sys.ballpark.read().unwrap();
            let _actor = sys.actor_state.read().unwrap();
            let _pos = sys.ball_positions.read().unwrap();
            assert_eq!(
                eve_db::lock_rank::held_lock_ranks(),
                vec![
                    eve_db::lock_rank::RANK_BALLPARK,
                    eve_db::lock_rank::RANK_ACTOR_STATE,
                    eve_db::lock_rank::RANK_LEAF,
                ]
            );
        }
        let _leaf = sys.ball_positions.read().unwrap();
        let inv = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sys.ballpark.read();
        }));
        assert!(inv.is_err(), "acquiring RANK_BALLPARK under RANK_LEAF must panic");
    }
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());
}
