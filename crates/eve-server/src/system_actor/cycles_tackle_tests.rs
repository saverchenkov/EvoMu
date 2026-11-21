use super::*;
use crate::system_actor::ActiveModuleCycle;
use eve_destiny::{Ball, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_tackle_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_tackle_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Stasis Webifier (group 65, overload speed factor bonus attr 1222 / speed factor)
    let mut web_attrs = HashMap::new();
    web_attrs.insert(1222u16, 30.0);
    sde.types.insert(
        526,
        InvType {
            type_id: 526,
            group_id: 65,
            category_id: 7,
            name: "Stasis Webifier I".into(),
            mass: 5.0,
            volume: 5.0,
            capacity: 0.0,
            portion_size: 1,
            radius: 1.0,
            base_price: 50_000.0,
            published: true,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: Some(1),
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: web_attrs,
        },
    );
    // Heavy Stasis Grappler with overload range bonus (attr 1222 = +300%)
    let mut grappler_attrs = HashMap::new();
    grappler_attrs.insert(1222u16, 300.0);
    sde.types.insert(
        40_340,
        InvType {
            type_id: 40_340,
            group_id: 1672,
            category_id: 7,
            name: "Heavy Stasis Grappler II".into(),
            mass: 50.0,
            volume: 25.0,
            capacity: 0.0,
            portion_size: 1,
            radius: 1.0,
            base_price: 1_500_000.0,
            published: true,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: Some(5),
            tech_level: Some(2),
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: grappler_attrs,
        },
    );
    Arc::new(sde)
}

#[test]
fn reconcile_tackle_handles_scram_mwd_shutdown_hic_bubble_and_bastion_immunity() {
    let mut bp = Ballpark::new(30_000_142);

    // 1001: attacker running scram (2 pts + MWD disable) on 1002 + unscripted HIC bubble
    bp.add_ball(make_tackle_ball(1001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        1001,
        CombatState {
            ecm_jammed_remaining_s: 10.0,
            ..CombatState::default()
        },
    );

    // 1002: target with active MWD (must be shut down) and warp core stab (+1)
    bp.add_ball(make_tackle_ball(1002, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(1002, CombatState::default());

    // 1003: Shuttle hull (group 31) -> automatically interdiction nullified
    bp.add_ball(make_tackle_ball(1003, 672, DVec3::new(8_000.0, 0.0, 0.0)));
    bp.combat_states.insert(1003, CombatState::default());
    bp.ball_slim_meta.insert(
        1003,
        SlimItemMeta {
            group_id: 31,
            category_id: 6,
            ..Default::default()
        },
    );

    let base_scram = HashMap::new();
    let base_core = HashMap::from([(1002i64, 1i32)]);
    let deactivated_ids = Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
    let deact_clone = Arc::clone(&deactivated_ids);
    let hook: ModuleDeactivationHook = Arc::new(move |_char_id, cyc| {
        deact_clone.lock().unwrap().push(cyc.module_id);
    });

    let mut cycles = vec![
        // Scrambler from 1001 on 1002
        ActiveModuleCycle {
            ship_id: 1001,
            module_id: 8001,
            target_id: Some(1002),
            warp_scramble_points: 2,
            disables_target_mwd_mjd: true,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
        // MWD on 1002 (should be aborted by scrambler)
        ActiveModuleCycle {
            ship_id: 1002,
            owner_char_id: Some(90_000_002),
            module_id: 8002,
            is_mwd_or_mjd: true,
            speed_boost_factor: 5.0,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
        // Bastion + HIC bubble + Warp Core Stab on 1001
        ActiveModuleCycle {
            ship_id: 1001,
            module_id: 8003,
            self_ewar_immune: true,
            self_disallow_assistance: true,
            hic_bubble_radius_m: Some(20_000.0),
            grants_interdiction_nullification: true,
            duration_s: 60.0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 1002,
            module_id: 8004,
            warp_core_stab_bonus: 1,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
    ];

    reconcile_tackle_and_bubbles(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &None,
        Some(&hook),
    );

    // MWD cycle 8002 must have been aborted and deactivation hook called
    assert!(!cycles.iter().any(|c| c.module_id == 8002));
    assert_eq!(*deactivated_ids.lock().unwrap(), vec![8002]);

    // 1002: scram=2, core=2 (base 1 + stab 1), is_mwd_mjd_disabled=true
    let cs1002 = bp.combat_states.get(&1002).unwrap();
    assert_eq!(cs1002.warp_scramble_strength, 2);
    assert_eq!(cs1002.warp_core_strength, 2);
    assert!(cs1002.is_mwd_mjd_disabled);

    // 1001: Bastion cleared ECM jam, set disallow_assistance & nullification, scram >= 100
    let cs1001 = bp.combat_states.get(&1001).unwrap();
    assert!((cs1001.ecm_jammed_remaining_s - 0.0).abs() < 1e-9);
    assert!(cs1001.disallow_assistance);
    assert!(cs1001.is_interdiction_nullified);
    assert!(cs1001.warp_scramble_strength >= 100);

    // 1003: Shuttle hull automatically interdiction nullified
    let cs1003 = bp.combat_states.get(&1003).unwrap();
    assert!(cs1003.is_interdiction_nullified);
}

#[test]
fn add_module_cycle_and_overload_scale_grappler_and_queue_repeat_action() {
    let sde = Some(make_tackle_sde());
    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(2001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        2001,
        CombatState {
            max_capacitor: 1_000.0,
            capacitor: 1_000.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(2002, 587, DVec3::new(15_000.0, 0.0, 0.0)));
    bp.combat_states.insert(2002, CombatState::default());

    let mut cycles = Vec::new();

    // Add Heavy Stasis Grappler cycle with overload requested at activation
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde,
        &None,
        None,
        ActiveModuleCycle {
            ship_id: 2001,
            owner_char_id: Some(90_000_001),
            module_id: 9101,
            module_type_id: 40_340,
            target_id: Some(2002),
            duration_s: 5.0,
            is_overloaded: true,
            grappler_optimal_m: 6_000.0,
            grappler_falloff_m: 12_000.0,
            grappler_speed_factor_pct: -80.0,
            effective_max_capacitor: Some(2_500.0),
            ..ActiveModuleCycle::default()
        },
    )
    .expect("grappler cycle added");

    assert_eq!(cycles.len(), 1);
    assert!((bp.combat_states[&2001].max_capacitor - 2_500.0).abs() < 1e-6);

    // Toggle overload OFF via handle_update_module_overload (queues pending_overloaded)
    cycles[0].elapsed_s = 2.0;
    let queued = handle_update_module_overload(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde,
        &None,
        None,
        2001,
        9101,
        false,
    );
    assert!(queued);
    assert_eq!(cycles[0].pending_overloaded, Some(false));

    // At cycle boundary, apply_overload_transition_to_cycle toggles overload OFF
    cycles[0].elapsed_s = 0.0;
    apply_overload_transition_to_cycle(&mut cycles[0], false, &mut bp, &sde);
    assert!(!cycles[0].is_overloaded);

    // Unlocking target 2002 removes targeted module cycle on 2001
    on_target_unlocked_in_actor(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &None,
        2001,
        Some(2002),
        None,
    );
    assert!(cycles.is_empty());
}

#[test]
fn apply_destiny_event_in_actor_handles_all_event_variants_and_unknown_ball() {
    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(3001, 587, DVec3::new(100.0, 200.0, 300.0)));
    bp.combat_states.insert(3001, CombatState::default());
    bp.ball_slim_meta.insert(
        3001,
        SlimItemMeta {
            owner_id: 90_000_001,
            ..Default::default()
        },
    );

    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::SetBallFree {
            ball_id: 3001,
            is_free: true,
        }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::SetBallHarmonic {
            ball_id: 3001,
            harmonic: 2,
        }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::SetBallInteractive {
            ball_id: 3001,
            is_interactive: true,
        }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::SetBallRadius {
            ball_id: 3001,
            radius: 120.0,
        }
    ));
    assert!((bp.balls[&3001].radius - 120.0).abs() < 1e-6);

    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::OnDotVictimUpdated {
            ball_id: 3001,
            flat_dps: 50.0,
            pct_dps: 0.01,
            remaining_ticks: 10,
            expiry_filetime: 0,
        }
    ));
    assert!(bp.combat_states[&3001].active_breacher_dot.is_some());

    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::OnSlimItemChange { ball_id: 3001 }
    ));
    assert_eq!(bp.ball_slim_meta[&3001].char_id, Some(90_000_001));

    assert!(apply_destiny_event_in_actor(
        &mut bp,
        3001,
        DestinyEvent::OnDroneStateChange {
            drone_id: 3001,
            owner_id: 90_000_001,
            controller_id: 3000,
            activity_state: 6,
            type_id: 2488,
            controller_owner_id: 90_000_001,
            target_id: Some(3002),
        }
    ));

    // OnSovereigntyChanged succeeds even for non-existent ball_id
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        999_999,
        DestinyEvent::OnSovereigntyChanged {
            solar_system_id: 30_000_142,
            alliance_id: 99_000_007,
        }
    ));

    // Unknown ball with normal event returns false
    assert!(!apply_destiny_event_in_actor(
        &mut bp,
        999_999,
        DestinyEvent::SetBallFree {
            ball_id: 999_999,
            is_free: false,
        }
    ));
}

#[test]
fn arch_584_overlapping_and_out_of_order_modifier_expiration_preserves_baselines() {
    let sde = make_tackle_sde();
    let sde_opt = Some(sde);
    let mut bp = Ballpark::new(30_000_142);
    let attacker_id = 4001_i64;
    let ship_id = 4002_i64;

    let base_sr = [0.5, 0.4, 0.3, 0.2];
    let base_ar = [0.5, 0.5, 0.6, 0.7];
    let base_hr = [0.67, 0.67, 0.67, 0.67];
    bp.add_ball(make_tackle_ball(attacker_id, 587, DVec3::ZERO));
    bp.combat_states.insert(attacker_id, CombatState::default());
    bp.add_ball(make_tackle_ball(ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            signature_radius: 40.0,
            shield_resonance: base_sr,
            armor_resonance: base_ar,
            structure_resonance: base_hr,
            ..CombatState::default()
        },
    );
    if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
        let b = &bp.balls[&ship_id];
        cs.capture_missing_baselines(b);
    }

    let mut cycles: Vec<ActiveModuleCycle> = Vec::new();
    let scramble_map = HashMap::new();
    let core_map = HashMap::new();

    // 1. Activate MWD (5x velocity, 1.5x mass, 5x sig), Stasis Web (0.4x), Cloak (0.25x), ADC, RAH
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 90_001,
            effect_name: "moduleBonusMicrowarpdrive".into(),
            duration_s: 10.0,
            is_mwd_or_mjd: true,
            speed_boost_factor: 4.0,
            self_mass_multiplier: Some(1.5),
            hic_sig_multiplier: Some(5.0),
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id: attacker_id,
            target_id: Some(ship_id),
            module_id: 90_002,
            effect_name: "decreaseTargetSpeed".into(),
            duration_s: 5.0,
            target_velocity_multiplier: Some(0.4),
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 90_003,
            effect_name: "cloaking".into(),
            duration_s: 60.0,
            speed_boost_factor: -0.75,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 90_004,
            duration_s: 12.0,
            is_adc: true,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 90_005,
            duration_s: 10.0,
            is_rah: true,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();

    // Overlap HIC WDFG (0.25x), Abyssal cloud, MJFG spool, Target Painter, and CRAB interference
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &scramble_map,
        &core_map,
        &sde_opt,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 90_006,
            effect_name: "warpDisruptSphere".into(),
            duration_s: 30.0,
            hic_velocity_multiplier: Some(0.25),
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
        cs.cloud_velocity_multiplier = Some(2.0);
        cs.cloud_sig_multiplier = Some(1.25);
        cs.mjfg_sig_multiplier = Some(2.5);
        cs.tp_sig_multiplier = Some(1.3);
        cs.crab_resonance_multiplier = Some(0.90);
    }
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scramble_map, &core_map, &None, None);

    // Cloak overrides MWD self_v (0.25); cloud (2.0); HIC WDFG (0.25); Web (0.4)
    let v_cloaked_stacked = 400.0 * 2.0 * (0.25 * 0.25) * 0.40;
    assert!((bp.balls[&ship_id].max_velocity - v_cloaked_stacked).abs() < 1e-6);
    assert!((bp.balls[&ship_id].mass - 2_250_000.0).abs() < 1e-3);
    assert!((bp.combat_states[&ship_id].signature_radius - 40.0 * 5.0 * 1.25 * 2.5 * 1.3).abs() < 1e-6);
    assert_eq!(bp.combat_states[&ship_id].shield_resonance, [0.25; 4]);
    assert_eq!(bp.combat_states[&ship_id].armor_resonance, [0.25; 4]);

    // Out-of-order expiration 1: Cloak expires FIRST while MWD is still running -> MWD 5x activates without lost web/WDFG/cloud!
    let cloak_idx = cycles.iter().position(|c| c.module_id == 90_003).unwrap();
    let cloak_rem = cycles.remove(cloak_idx);
    crate::system_actor::cycles::cleanup_removed_module_cycle(&mut bp, &cloak_rem, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scramble_map, &core_map, &None, None);
    let v_uncloaked_mwd = 400.0 * 2.0 * (5.0 * 0.25) * 0.40;
    assert!((bp.balls[&ship_id].max_velocity - v_uncloaked_mwd).abs() < 1e-6);

    // Out-of-order expiration 2: Web expires while HIC WDFG, MWD, and cloud remain active
    let web_idx = cycles.iter().position(|c| c.module_id == 90_002).unwrap();
    let web_rem = cycles.remove(web_idx);
    crate::system_actor::cycles::cleanup_removed_module_cycle(&mut bp, &web_rem, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scramble_map, &core_map, &None, None);
    assert!((bp.balls[&ship_id].max_velocity - 400.0 * 2.0 * 5.0 * 0.25).abs() < 1e-6);

    // Out-of-order expiration 3: ADC expires BEFORE CRAB & RAH -> CRAB 0.90x now applies to baseline shield/hull & RAH armor!
    let adc_idx = cycles.iter().position(|c| c.module_id == 90_004).unwrap();
    let adc_rem = cycles.remove(adc_idx);
    crate::system_actor::cycles::cleanup_removed_module_cycle(&mut bp, &adc_rem, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scramble_map, &core_map, &None, None);
    assert_eq!(bp.combat_states[&ship_id].shield_resonance, base_sr.map(|r| r * 0.90));
    assert_eq!(bp.combat_states[&ship_id].structure_resonance, base_hr.map(|r| r * 0.90));
    assert_eq!(bp.combat_states[&ship_id].armor_resonance, [0.85 * 0.90; 4]);

    // Out-of-order expiration 4: CRAB expires while RAH remains active -> baseline shield/hull, RAH 0.85 armor
    if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
        cs.crab_resonance_multiplier = None;
    }
    bp.recompute_ball_attributes(ship_id);
    assert_eq!(bp.combat_states[&ship_id].shield_resonance, base_sr);
    assert_eq!(bp.combat_states[&ship_id].structure_resonance, base_hr);
    assert_eq!(bp.combat_states[&ship_id].armor_resonance, [0.85; 4]);

    // Expire remaining MWD, RAH, WDFG, cloud, MJFG, TP in mixed order -> exact baselines restored
    while let Some(rem) = cycles.pop() {
        crate::system_actor::cycles::cleanup_removed_module_cycle(&mut bp, &rem, &None);
    }
    if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
        cs.cloud_velocity_multiplier = None;
        cs.cloud_sig_multiplier = None;
        cs.mjfg_sig_multiplier = None;
        cs.tp_sig_multiplier = None;
    }
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scramble_map, &core_map, &None, None);

    assert!((bp.balls[&ship_id].max_velocity - 400.0).abs() < 1e-9);
    assert!((bp.balls[&ship_id].mass - 1_500_000.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship_id].signature_radius - 40.0).abs() < 1e-9);
    assert_eq!(bp.combat_states[&ship_id].shield_resonance, base_sr);
    assert_eq!(bp.combat_states[&ship_id].armor_resonance, base_ar);
    assert_eq!(bp.combat_states[&ship_id].structure_resonance, base_hr);
}

#[test]
fn reconcile_velocity_and_immobilize_emit_set_max_speed_and_cross_ball_interactive() {
    let sde = make_tackle_sde();
    let sde_opt = Some(sde);
    let mut bp = Ballpark::new(30_000_142);
    let ship_id = 5001_i64;
    let observer_id = 5002_i64;
    let mut ship_b = make_tackle_ball(ship_id, 587, DVec3::ZERO);
    ship_b.max_velocity = 400.0;
    bp.add_ball(ship_b);
    bp.combat_states.insert(
        ship_id,
        CombatState {
            base_max_velocity: Some(400.0),
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(observer_id, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.combat_states.insert(observer_id, CombatState::default());
    let _ = bp.drain_all_observer_events();

    let mut cycles = Vec::new();
    let mut scram_map = HashMap::new();
    let mut core_map = HashMap::new();

    // 1. Immobilizing cycle (e.g. Bastion/Siege/Cyno) must emit SetMaxSpeed(..., 0.0).
    let siege_cycle = ActiveModuleCycle {
        ship_id,
        module_id: 95_001,
        effect_name: "siege".into(),
        duration_s: 300.0,
        immobilize_self: true,
        ..Default::default()
    };
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &mut scram_map,
        &mut core_map,
        &sde_opt,
        &None,
        None,
        siege_cycle,
    )
    .expect("add immobilize cycle");
    assert_eq!(bp.balls[&ship_id].max_velocity, 0.0);
    let evs1 = bp.drain_observer_events(observer_id);
    assert!(evs1.iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id, max_speed } if *ball_id == ship_id && *max_speed == 0.0
    )));

    // 2. Removing immobilizing cycle restores velocity and emits SetMaxSpeed(..., 400.0).
    let rem = cycles.pop().unwrap();
    crate::system_actor::cycles::cleanup_removed_module_cycle(&mut bp, &rem, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram_map, &core_map, &None, None);
    assert!((bp.balls[&ship_id].max_velocity - 400.0).abs() < 1e-6);
    let evs2 = bp.drain_observer_events(observer_id);
    assert!(evs2.iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id, max_speed } if *ball_id == ship_id && (*max_speed - 400.0).abs() < 1e-6
    )));

    // 3. Cross-ball SetBallInteractive (observer scope != target ball_id) must update
    //    ball_interactive_overrides and emit SetBallInteractive for target ball_id.
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        observer_id,
        DestinyEvent::SetBallInteractive {
            ball_id: ship_id,
            is_interactive: false,
        }
    ));
    assert_eq!(bp.ball_interactive_overrides.get(&ship_id), Some(&false));
    let evs3 = bp.drain_observer_events(observer_id);
    assert!(evs3.iter().any(|e| matches!(
        e,
        DestinyEvent::SetBallInteractive { ball_id, is_interactive: false } if *ball_id == ship_id
    )));
}

#[test]
fn w3_mutant_killers_apply_destiny_event_in_actor_all_variants_and_scram_range_drop() {
    let mut bp = Ballpark::new(30_000_142);
    let ship_a = 6001_i64;
    let ship_b = 6002_i64;
    bp.add_ball(make_tackle_ball(ship_a, 587, DVec3::ZERO));
    bp.combat_states.insert(ship_a, CombatState::default());
    bp.add_ball(make_tackle_ball(ship_b, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(ship_b, CombatState::default());

    assert!(!apply_destiny_event_in_actor(
        &mut bp,
        999_999,
        DestinyEvent::Stop { ball_id: 999_999 }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        ship_a,
        DestinyEvent::GotoDirection {
            ball_id: ship_a,
            direction: DVec3::X.into(),
        }
    ));
    assert!(matches!(
        bp.balls[&ship_a].mode,
        MovementMode::GotoDirection { .. }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        ship_a,
        DestinyEvent::SetSpeedFraction {
            ball_id: ship_a,
            fraction: 0.75,
        }
    ));
    assert!((bp.balls[&ship_a].speed_fraction - 0.75).abs() < 1e-6);
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        ship_a,
        DestinyEvent::Stop { ball_id: ship_a }
    ));
    assert_eq!(bp.balls[&ship_a].mode, MovementMode::Stop);

    // Scram cycle drops when target moves beyond max_effective_range_m
    let mut cycles = vec![ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 7001,
        target_id: Some(ship_b),
        warp_scramble_points: 2,
        disables_target_mwd_mjd: true,
        max_effective_range_m: Some(10_000.0),
        duration_s: 10.0,
        ..Default::default()
    }];
    let scram_map = HashMap::new();
    let core_map = HashMap::new();
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram_map, &core_map, &None, None);
    assert_eq!(bp.combat_states[&ship_b].warp_scramble_strength, 2);
    assert!(bp.combat_states[&ship_b].is_mwd_mjd_disabled);

    // Move ship_b to 12 km (> 10 km range) -> cycle is removed and scram cleared
    bp.balls.get_mut(&ship_b).unwrap().position = DVec3::new(12_000.0, 0.0, 0.0);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram_map, &core_map, &None, None);
    assert!(cycles.is_empty());
    assert_eq!(bp.combat_states[&ship_b].warp_scramble_strength, 0);
    assert!(!bp.combat_states[&ship_b].is_mwd_mjd_disabled);
}

#[test]
fn spawn_set_combat_state_siren_pruning_and_sync_warp_expiry_helpers() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let db_opt = Some(Arc::clone(&db));
    let sde = Some(make_tackle_sde());
    let mut bp = Ballpark::new(30_000_142);
    let mut cycles = Vec::new();
    let mut base_scram = HashMap::new();
    let mut base_core = HashMap::new();
    let mut last_totals = HashMap::new();

    // 4001: ship with zero sig radius -> SDE attr 552 or radius fallback, plus re-spawn replacement
    let b4001 = make_tackle_ball(4001, 587, DVec3::ZERO);
    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &sde,
        &db_opt,
        None,
        b4001.clone(),
        CombatState {
            signature_radius: 0.0,
            warp_scramble_strength: 0,
            warp_core_strength: 1,
            ..CombatState::default()
        },
        None,
    );
    assert!(bp.combat_states[&4001].base_signature_radius.is_some());

    // Add a Siren tackle cycle with missing target -> pruned by reconcile_tackle_and_bubbles
    cycles.push(ActiveModuleCycle {
        ship_id: 4099,
        module_id: 9901,
        target_id: Some(4098),
        effect_name: "siren_tackle:4001:555".into(),
        warp_scramble_points: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });

    // Re-spawning 4001 replaces existing ball and invokes reconcile_tackle_and_bubbles (pruning missing Siren)
    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &sde,
        &db_opt,
        None,
        b4001,
        CombatState {
            warp_scramble_strength: 1,
            warp_core_strength: 2,
            ..CombatState::default()
        },
        None,
    );
    assert!(cycles.is_empty(), "orphaned siren_tackle must be pruned");

    // Test set_combat_state_in_actor stale-total tracking & fresh updates + DB HP persistence
    cycles.push(ActiveModuleCycle {
        ship_id: 4002,
        module_id: 9902,
        target_id: Some(4001),
        warp_scramble_points: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });
    cycles.push(ActiveModuleCycle {
        ship_id: 4001,
        module_id: 9903,
        warp_core_stab_bonus: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &db_opt, None);
    snapshot_active_tackle_totals(&bp, &cycles, &mut last_totals);
    assert!(last_totals.contains_key(&4001));

    // Update 4001 combat state with different scram/core totals (updates base maps & clears last_totals)
    set_combat_state_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &mut last_totals,
        &db_opt,
        None,
        4001,
        CombatState {
            warp_scramble_strength: 5,
            warp_core_strength: 6,
            shield_hp: 111.0,
            ..CombatState::default()
        },
    );
    assert_eq!(base_scram[&4001], 3); // 5 - 2 scram_delta
    assert_eq!(base_core[&4001], 4);  // 6 - 2 core_delta

    // Also test set_combat_state_in_actor on a brand-new ball (None branch)
    set_combat_state_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &mut last_totals,
        &db_opt,
        None,
        4003,
        CombatState {
            warp_scramble_strength: 1,
            warp_core_strength: 2,
            ..CombatState::default()
        },
    );
    assert_eq!(base_scram[&4003], 1);

    // Siren tackle with live carrier, fighter, and target, then on_target_unlocked_in_actor(None) removes all
    bp.add_ball(make_tackle_ball(4002, 587, DVec3::new(500.0, 0.0, 0.0)));
    bp.combat_states.insert(4002, CombatState::default());
    cycles.push(ActiveModuleCycle {
        ship_id: 4002,
        module_id: 9904,
        target_id: Some(4001),
        effect_name: "siren_tackle:4001:777".into(),
        warp_scramble_points: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });
    on_target_unlocked_in_actor(&mut bp, &mut cycles, &base_scram, &base_core, &db_opt, 4001, None, None);
    assert!(!cycles.iter().any(|c| c.module_id == 9904));
}

#[test]
fn add_module_cycle_covers_cap_drain_remote_repair_adc_ehe_rah_mass_and_immobilize() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let db_opt = Some(Arc::clone(&db));
    let sde = Some(make_tackle_sde());
    let mut bp = Ballpark::new(30_000_142);

    bp.add_ball(make_tackle_ball(5001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        5001,
        CombatState {
            capacitor: 500.0,
            max_capacitor: 1_000.0,
            shield_hp: 100.0,
            max_shield_hp: 1_000.0,
            shield_boost_multiplier: 1.5,
            signature_radius: 40.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(5002, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        5002,
        CombatState {
            capacitor: 200.0,
            max_capacitor: 500.0,
            shield_hp: 50.0,
            max_shield_hp: 500.0,
            ..CombatState::default()
        },
    );
    // Sleeper NPC (type_id 30185 -> capacitor immune)
    bp.add_ball(make_tackle_ball(5003, 30185, DVec3::new(2_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        5003,
        CombatState {
            capacitor: 300.0,
            max_capacitor: 300.0,
            ..CombatState::default()
        },
    );

    let mut cycles = Vec::new();
    let base_scram = HashMap::new();
    let base_core = HashMap::new();

    // Insufficient capacitor error
    let err = handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8001,
            cap_need_per_cycle: 999.0,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap_err();
    assert!(err.contains("Insufficient capacitor"));

    // Cap booster + shield boost + remote shield/cap + cap drain on normal target
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8002,
            module_type_id: 3538, // Capital Remote Shield Booster
            target_id: Some(5002),
            cap_need_per_cycle: 50.0,
            cap_booster_inject_amount: 100.0,
            shield_boost_amount: 40.0,
            cap_drain_target: 30.0,
            remote_shield_amount: 80.0,
            remote_cap_amount: 25.0,
            remote_optimal: 5_000.0,
            remote_falloff: 5_000.0,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert!((bp.combat_states[&5001].shield_hp - 160.0).abs() < 1e-6);
    assert!((bp.combat_states[&5002].shield_hp - 130.0).abs() < 1e-6);

    // Sleeper target (30185) is immune to cap_drain_target
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8003,
            target_id: Some(5003),
            cap_drain_target: 100.0,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert!((bp.combat_states[&5003].capacitor - 300.0).abs() < 1e-6);

    // Mutadaptive spool preservation on re-add
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8004,
            target_id: Some(5002),
            mutadaptive_spool_step: 0.05,
            current_spool_cycles: 4,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8004,
            target_id: Some(5002),
            mutadaptive_spool_step: 0.05,
            current_spool_cycles: 0,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert_eq!(cycles.iter().find(|c| c.module_id == 8004).unwrap().current_spool_cycles, 4);

    // ADC + EHE + RAH + HIC mass/sig + self_mass_multiplier + immobilize_self + interdiction nullification
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8005,
            is_adc: true,
            self_mass_multiplier: Some(2.0),
            hic_sig_multiplier: Some(1.5),
            grants_interdiction_nullification: true,
            duration_s: 12.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert!(bp.combat_states[&5001].is_interdiction_nullified);
    assert_eq!(bp.combat_states[&5001].shield_resonance, [0.25; 4]);
    cycles.retain(|c| c.module_id != 8005);

    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8006,
            ehe_hull_resists: Some([0.05; 4]),
            hic_mass_addition: Some(500_000.0),
            is_rah: true,
            immobilize_self: true,
            duration_s: 15.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert_eq!(bp.combat_states[&5001].structure_resonance, [0.05; 4]);
    assert_eq!(bp.combat_states[&5001].armor_resonance, [0.85; 4]);
    assert_eq!(bp.balls[&5001].max_velocity, 0.0);

    // Non-targeted cycle with max_effective_range_m + overload range bonus (1222)
    let mut sde_mut = (*make_tackle_sde()).clone();
    sde_mut.types.insert(
        40_999,
        InvType {
            type_id: 40_999,
            group_id: 65,
            category_id: 7,
            name: "Test Web".into(),
            mass: 5.0,
            volume: 5.0,
            capacity: 0.0,
            portion_size: 1,
            radius: 1.0,
            base_price: 100.0,
            published: true,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: Some(1),
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::from([(1222u16, 30.0)]),
        },
    );
    let sde2 = Some(Arc::new(sde_mut));
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &sde2,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 5002,
            module_id: 8007,
            module_type_id: 40_999,
            is_overloaded: false,
            max_effective_range_m: Some(10_000.0),
            remote_optimal: 10_000.0,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    let c8007_mut = cycles.iter_mut().find(|c| c.module_id == 8007).unwrap();
    apply_overload_transition_to_cycle(c8007_mut, true, &mut bp, &sde2);
    assert!((c8007_mut.remote_optimal - 13_000.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_handle_and_registry_warp_and_module_expiry_sync_helpers() {
    let reg = crate::system_actor::SystemRegistry::new();
    let sys_id = 30_000_142;
    let handle = reg.get_or_spawn(sys_id);
    let ship_id = 6001_i64;

    assert!(!handle.is_ball_warping_sync(ship_id));
    assert!(!handle.is_ball_aligning_warp_sync(ship_id));
    assert_eq!(handle.committed_warp_destination_sync(ship_id), None);
    assert!(!reg.is_ball_in_committed_warp_sync(sys_id, ship_id));

    let dest = DVec3::new(1_000_000.0, 0.0, 0.0);
    handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: dest,
                warp_speed_au_s: 3.0,
                total_distance: dest.length(),
                phase: eve_destiny::WarpPhase::Aligning,
                elapsed_warp_time: 0.0,
            },
        })
        .await
        .unwrap();

    assert!(handle.is_ball_warping_sync(ship_id));
    assert!(handle.is_ball_aligning_warp_sync(ship_id));
    assert_eq!(handle.committed_warp_destination_sync(ship_id), None);

    handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: dest,
                warp_speed_au_s: 3.0,
                total_distance: dest.length(),
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        )
        .await
        .unwrap();
    assert!(!handle.is_ball_aligning_warp_sync(ship_id));
    assert_eq!(handle.committed_warp_destination_sync(ship_id), Some(dest));
    assert!(reg.is_ball_in_committed_warp_sync(sys_id, ship_id));

    {
        let mut st = handle.actor_state.write().unwrap();
        // Cyno cycle skipped when cyno_expiry_unix > 0
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 9501,
            effect_name: "cynosuralGeneration".into(),
            duration_s: 300.0,
            started_at_unix: 1_000,
            ..Default::default()
        });
        // Lance warmup adds repeat seconds
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 9502,
            effect_name: "superWeaponLanceWarmup".into(),
            duration_s: 15.0,
            elapsed_s: 2.0,
            started_at_unix: 1_000,
            repeat: 5,
            ..Default::default()
        });
        // Normal module with started_at_unix == 0
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 9503,
            effect_name: "warpScramble".into(),
            duration_s: 10.0,
            elapsed_s: 1.0,
            started_at_unix: 0,
            ..Default::default()
        });
    }
    // now = 1005 -> lance rem = 10 + 5 = 15 -> exp = 1020
    let exp = reg.max_active_module_cycle_expiry_unix_sync(sys_id, ship_id, 1_010, 1_005);
    assert_eq!(exp, 1_020);
    // Unknown system falls back to cyno_expiry_unix
    assert_eq!(reg.max_active_module_cycle_expiry_unix_sync(30_999_999, ship_id, 1_010, 1_005), 1_010);
}


fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
    effect_ids: Vec<u16>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 10.0,
        volume: 10.0,
        capacity: 0.0,
        portion_size: 1,
        radius: 1.0,
        base_price: 10_000.0,
        published: true,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids,
        default_attributes,
    }
}

#[test]
fn sleeper_immunity_siren_tackle_ids_and_active_tackle_deltas_cover_all_branches() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        77_001,
        make_inv_type(77_001, 959, 11, "Sleeper Frigate", HashMap::new(), vec![]),
    );
    sde.types.insert(
        587,
        make_inv_type(587, 25, 6, "Rifter", HashMap::new(), vec![]),
    );
    let sde_opt = Some(Arc::new(sde));

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(100, 30_185, DVec3::ZERO)); // Sleeper type range
    bp.add_ball(make_tackle_ball(101, 37_472, DVec3::ZERO)); // Drifter type
    bp.add_ball(make_tackle_ball(102, 77_001, DVec3::ZERO)); // Sleeper SDE group
    bp.add_ball(make_tackle_ball(103, 587, DVec3::ZERO));    // Regular ship

    assert!(!is_sleeper_or_drifter_capacitor_immune(&bp, 999_999, &sde_opt));
    assert!(is_sleeper_or_drifter_capacitor_immune(&bp, 100, &None));
    assert!(is_sleeper_or_drifter_capacitor_immune(&bp, 101, &None));
    assert!(!is_sleeper_or_drifter_capacitor_immune(&bp, 102, &None));
    assert!(is_sleeper_or_drifter_capacitor_immune(&bp, 102, &sde_opt));
    assert!(!is_sleeper_or_drifter_capacitor_immune(&bp, 103, &sde_opt));

    // parse_siren_tackle_ids
    assert_eq!(parse_siren_tackle_ids("siren_tackle:1001:2002"), Some((1001, 2002)));
    assert_eq!(parse_siren_tackle_ids("other:1001:2002"), None);
    assert_eq!(parse_siren_tackle_ids("siren_tackle:1001"), None);
    assert_eq!(parse_siren_tackle_ids("siren_tackle:abc:2002"), None);
    assert_eq!(parse_siren_tackle_ids("siren_tackle:1001:xyz"), None);
    assert_eq!(parse_siren_tackle_ids("siren_tackle:1001:2002:3"), None);

    // active_tackle_and_core_deltas: immobilize_self, self_disallow_assistance, lance protection flags
    let cycles = vec![
        ActiveModuleCycle {
            ship_id: 200,
            target_id: Some(103),
            warp_scramble_points: 3,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 103,
            immobilize_self: true,
            warp_core_stab_bonus: 2,
            ..ActiveModuleCycle::default()
        },
    ];
    let unprotected = CombatState {
        lance_disrupted_remaining_s: 5.0,
        ..CombatState::default()
    };
    assert_eq!(active_tackle_and_core_deltas(103, &unprotected, &cycles), (203, 2));

    for mut prot in [
        CombatState {
            lance_disrupted_remaining_s: 5.0,
            undock_invulnerable_remaining_ticks: 1,
            ..CombatState::default()
        },
        CombatState {
            lance_disrupted_remaining_s: 5.0,
            gate_cloak_remaining_ticks: 1,
            ..CombatState::default()
        },
        CombatState {
            lance_disrupted_remaining_s: 5.0,
            is_cloaked: true,
            ..CombatState::default()
        },
        CombatState {
            lance_disrupted_remaining_s: 5.0,
            is_tethered: true,
            ..CombatState::default()
        },
        CombatState {
            lance_disrupted_remaining_s: 5.0,
            warp_exit_invulnerable_remaining_ticks: 5,
            ..CombatState::default()
        },
    ] {
        assert!(prot.is_invulnerable() || prot.is_cloaked);
        assert_eq!(active_tackle_and_core_deltas(103, &prot, &cycles), (103, 2));
        prot.lance_disrupted_remaining_s = 0.0;
        assert_eq!(active_tackle_and_core_deltas(103, &prot, &cycles), (103, 2));
    }
}

#[test]
fn spawn_and_set_combat_state_reconcile_base_tackle_and_siren_pruning() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        40_520,
        make_inv_type(
            40_520,
            893,
            6,
            "Nullified Hull",
            HashMap::from([(552u16, 42.0), (1538u16, 1.0)]),
            vec![],
        ),
    );
    let sde_opt = Some(Arc::new(sde));

    let mut bp = Ballpark::new(30_000_142);
    let mut cycles = Vec::new();
    let mut base_scram = HashMap::new();
    let mut base_core = HashMap::new();
    let mut last_active = HashMap::new();

    // 1. Spawn ball with signature_radius == 0 -> uses SDE attr 552 (42.0), attr 1538 -> nullified
    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &sde_opt,
        &None,
        None,
        make_tackle_ball(4001, 40_520, DVec3::ZERO),
        CombatState {
            signature_radius: 0.0,
            warp_core_strength: 2,
            ..CombatState::default()
        },
        None,
    );
    assert_eq!(bp.combat_states[&4001].base_signature_radius, Some(42.0));
    assert!(bp.combat_states[&4001].is_interdiction_nullified);
    assert_eq!(base_core[&4001], 2);

    // 2. Spawn parent (4002) and siren fighter (4003) + target (4004)
    for id in [4002_i64, 4003, 4004] {
        spawn_ball_with_combat_in_actor(
            &mut bp,
            &mut cycles,
            &mut base_scram,
            &mut base_core,
            &sde_opt,
            &None,
            None,
            make_tackle_ball(id, 587, DVec3::new(1_000.0, 0.0, 0.0)),
            CombatState {
                warp_core_strength: 1,
                ..CombatState::default()
            },
            Some(SlimItemMeta {
                owner_id: 90_000_001,
                ..Default::default()
            }),
        );
    }

    // Add valid siren_tackle cycle + orphan siren_tackle cycle (missing target 9999)
    cycles.push(ActiveModuleCycle {
        ship_id: 4003,
        module_id: 4003,
        effect_name: "siren_tackle:4002:4003".into(),
        target_id: Some(4004),
        warp_scramble_points: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });
    cycles.push(ActiveModuleCycle {
        ship_id: 4003,
        module_id: 4099,
        effect_name: "siren_tackle:4002:4099".into(),
        target_id: Some(9999),
        warp_scramble_points: 2,
        duration_s: 10.0,
        ..ActiveModuleCycle::default()
    });

    // Re-spawning 4004 preserves SlimItemMeta when slim_meta_opt == None, prunes orphan siren cycle, keeps valid one
    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &sde_opt,
        &None,
        None,
        make_tackle_ball(4004, 587, DVec3::new(1_500.0, 0.0, 0.0)),
        CombatState {
            warp_scramble_strength: 2,
            warp_core_strength: 1,
            ..CombatState::default()
        },
        None,
    );
    assert_eq!(cycles.len(), 1);
    assert_eq!(bp.ball_slim_meta[&4004].owner_id, 90_000_001);
    assert_eq!(bp.combat_states[&4004].warp_scramble_strength, 2);
    assert_eq!(base_scram[&4004], 0);

    // 3. set_combat_state_in_actor snapshots active totals and handles stale scram/core on external reset
    set_combat_state_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &mut last_active,
        &None,
        None,
        4004,
        CombatState {
            warp_scramble_strength: 2,
            warp_core_strength: 1,
            ..CombatState::default()
        },
    );
    assert_eq!(last_active[&4004].0, Some(2));
    assert_eq!(base_core[&4004], 1);

    // Put 4004 in Aligning warp while scrambled -> reconcile_tackle_and_bubbles stops warp
    bp.balls.get_mut(&4004).unwrap().mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(100_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 100_000.0,
        phase: eve_destiny::WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!(matches!(bp.balls[&4004].mode, MovementMode::Stop));

    // Unlock all targets on parent 4002 clears siren_tackle:4002:4003
    on_target_unlocked_in_actor(&mut bp, &mut cycles, &base_scram, &base_core, &None, 4002, None, None);
    assert!(cycles.is_empty());

    // Now scram_delta == 0; calling set_combat_state_in_actor with stale scram == 2 ignores stale delta
    bp.combat_states.get_mut(&4004).unwrap().warp_scramble_strength = 0;
    set_combat_state_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &mut last_active,
        &None,
        None,
        4004,
        CombatState {
            warp_scramble_strength: 2,
            warp_core_strength: 3,
            ..CombatState::default()
        },
    );
    assert_eq!(base_scram[&4004], 0);
    assert_eq!(bp.combat_states[&4004].warp_scramble_strength, 0);
}

#[test]
fn deterministic_velocity_stacking_cloak_immobilize_and_ewar_immunity() {
    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(5001, 587, DVec3::ZERO));
    bp.balls.get_mut(&5001).unwrap().max_velocity = 400.0;
    bp.combat_states.insert(
        5001,
        CombatState {
            base_max_velocity: Some(400.0),
            ..CombatState::default()
        },
    );

    let wubbles = HashMap::from([(5001_i64, 0.70_f64)]);

    // Two webs + wubble + MWD (speed_boost_factor = 4.0 -> 5x) + HIC (0.5x)
    let mut cycles = vec![
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 1,
            speed_boost_factor: 4.0,
            hic_velocity_multiplier: Some(0.5),
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 5002,
            module_id: 2,
            target_id: Some(5001),
            target_velocity_multiplier: Some(0.40), // -60%
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 5003,
            module_id: 3,
            target_id: Some(5001),
            target_velocity_multiplier: Some(0.50), // -50%
            ..ActiveModuleCycle::default()
        },
    ];
    reconcile_velocity_for_ship(&mut bp, 5001, &cycles, &wubbles);
    let v = bp.balls[&5001].max_velocity;
    // self_mult = 5.0 * 0.5 = 2.5; sorted penalties = [0.60, 0.50, 0.30]
    let expected_web = (1.0 - 0.60)
        * (1.0 - 0.50 * (-(1.0_f64 / 2.67).powi(2)).exp())
        * (1.0 - 0.30 * (-(2.0_f64 / 2.67).powi(2)).exp());
    assert!((v - 400.0 * 2.5 * expected_web).abs() < 1e-6);

    // Cloak multiplier overrides prop_mult
    cycles.push(ActiveModuleCycle {
        ship_id: 5001,
        module_id: 4,
        effect_name: "cloakingWarpSafe".into(),
        speed_boost_factor: -0.75, // 0.25x
        ..ActiveModuleCycle::default()
    });
    reconcile_velocity_for_ship(&mut bp, 5001, &cycles, &wubbles);
    let v_cloak = bp.balls[&5001].max_velocity;
    assert!((v_cloak - 400.0 * 0.25 * 0.5 * expected_web).abs() < 1e-6);

    // self_ewar_immune ignores target webs & wubbles; immobilize_self forces 0.0
    cycles.push(ActiveModuleCycle {
        ship_id: 5001,
        module_id: 5,
        self_ewar_immune: true,
        ..ActiveModuleCycle::default()
    });
    reconcile_velocity_for_ship(&mut bp, 5001, &cycles, &wubbles);
    let v_imm = bp.balls[&5001].max_velocity;
    assert!((v_imm - 400.0 * 0.25 * 0.5).abs() < 1e-6);

    cycles.push(ActiveModuleCycle {
        ship_id: 5001,
        module_id: 6,
        immobilize_self: true,
        ..ActiveModuleCycle::default()
    });
    reconcile_velocity_for_ship(&mut bp, 5001, &cycles, &wubbles);
    let v_zero = bp.balls[&5001].max_velocity;
    assert_eq!(v_zero, 0.0);

    // reconcile_velocity_for_ship restores baseline when no modifiers remain
    bp.combat_states.get_mut(&5001).unwrap().cloak_velocity_multiplier = None;
    reconcile_velocity_for_ship(&mut bp, 5001, &[], &HashMap::new());
    assert!((bp.balls[&5001].max_velocity - 400.0).abs() < 1e-6);

    // Missing ball is a no-op
    reconcile_velocity_for_ship(&mut bp, 999_999, &[], &HashMap::new());
}

#[test]
fn handle_add_module_cycle_and_overload_cover_cap_sleeper_adc_ehe_rah_and_cat5() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.effects.insert(
        9901,
        eve_sde::DogmaEffectType {
            effect_id: 9901,
            name: "overloadSelfArmorBonus".into(),
            effect_category: 5,
            ..Default::default()
        },
    );
    sde.types.insert(
        47_271, // Mutadaptive Remote / Turret with cat5 + damage/duration/range overload attrs
        make_inv_type(
            47_271,
            1986,
            7,
            "Veles Entropic Disintegrator",
            HashMap::from([
                (1205u16, -15.0),
                (1210u16, 15.0),
                (1222u16, 20.0),
                (1223u16, 50.0),
                (1230u16, 10.0),
                (1231u16, 10.0),
            ]),
            vec![9901],
        ),
    );
    sde.types.insert(
        35_792, // Capital Remote Shield Booster (power >= 10_000)
        make_inv_type(
            35_792,
            41,
            7,
            "CONCORD Capital Remote Shield Booster",
            HashMap::from([(30u16, 25_000.0)]),
            vec![],
        ),
    );
    let sde_opt = Some(Arc::new(sde));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let db_opt = Some(Arc::clone(&db));

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(6001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        6001,
        CombatState {
            max_capacitor: 500.0,
            capacitor: 50.0,
            max_shield_hp: 1_000.0,
            shield_hp: 200.0,
            undock_invulnerable_remaining_ticks: 5,
            is_mwd_mjd_disabled: true,
            signature_radius: 40.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(6002, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        6002,
        CombatState {
            max_capacitor: 500.0,
            capacitor: 300.0,
            max_shield_hp: 1_000.0,
            shield_hp: 100.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(6003, 30_185, DVec3::new(1_000.0, 0.0, 0.0))); // Sleeper
    bp.combat_states.insert(
        6003,
        CombatState {
            max_capacitor: 500.0,
            capacitor: 500.0,
            ..CombatState::default()
        },
    );

    let mut cycles = Vec::new();

    // 1. MWD rejected while is_mwd_mjd_disabled == true (preserves undock invuln)
    let mwd_err = handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7001,
            is_mwd_or_mjd: true,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap_err();
    assert!(mwd_err.contains("WarpScrambled"));
    assert_eq!(bp.combat_states[&6001].undock_invulnerable_remaining_ticks, 5);

    // 2. Insufficient capacitor rejected (preserves undock invuln)
    let cap_err = handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7002,
            cap_need_per_cycle: 100.0,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap_err();
    assert!(cap_err.contains("Insufficient capacitor"));
    assert_eq!(bp.combat_states[&6001].undock_invulnerable_remaining_ticks, 5);

    // 3. Cap booster + shield booster + ADC + self_mass_multiplier + hic_sig_multiplier (clears undock invuln)
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7003,
            cap_need_per_cycle: 10.0,
            cap_booster_inject_amount: 200.0,
            shield_boost_amount: 150.0,
            self_mass_multiplier: Some(2.0),
            hic_sig_multiplier: Some(1.5),
            is_adc: true,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert_eq!(bp.combat_states[&6001].undock_invulnerable_remaining_ticks, 0);
    assert!((bp.combat_states[&6001].capacitor - 240.0).abs() < 1e-6);
    assert!((bp.combat_states[&6001].shield_hp - 350.0).abs() < 1e-6);
    assert!((bp.balls[&6001].mass - 3_000_000.0).abs() < 1e-6);
    assert!((bp.combat_states[&6001].signature_radius - 60.0).abs() < 1e-6);
    assert_eq!(bp.combat_states[&6001].shield_resonance, [0.25; 4]);
    db.with_snapshot(|s| {
        assert_eq!(s.transient_item_attributes[&6001][&267], 0.25);
    });

    // 4. EHE + RAH + Cloaking
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6002,
            module_id: 7004,
            ehe_hull_resists: Some([0.1, 0.2, 0.3, 0.4]),
            is_rah: true,
            effect_name: "cloakingDevice".into(),
            speed_boost_factor: -0.5,
            hic_mass_addition: Some(500_000.0),
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert_eq!(bp.combat_states[&6002].structure_resonance, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(bp.combat_states[&6002].armor_resonance, [0.85; 4]);
    assert_eq!(bp.combat_states[&6002].cloak_velocity_multiplier, Some(0.5));
    assert!((bp.balls[&6002].mass - 2_000_000.0).abs() < 1e-6);

    // 5. Neut on Sleeper 6003 leaves capacitor untouched; neut + capital remote shield + cap transfer on 6002 applies
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7005,
            target_id: Some(6003),
            cap_drain_target: 200.0,
            duration_s: 6.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert!((bp.combat_states[&6003].capacitor - 500.0).abs() < 1e-6);

    // First capital remote shield on 6002 (cap_rank = 0 -> 1.0x), then second (cap_rank = 1 -> 0.8x)
    for (mid, amt) in [(7006_i64, 100.0), (7007_i64, 100.0)] {
        handle_add_module_cycle(
            &mut bp,
            &mut cycles,
            &HashMap::new(),
            &HashMap::new(),
            &sde_opt,
            &db_opt,
            None,
            ActiveModuleCycle {
                ship_id: 6001,
                module_id: mid,
                module_type_id: 35_792,
                target_id: Some(6002),
                remote_shield_amount: amt,
                remote_cap_amount: 25.0,
                cap_drain_target: 10.0,
                remote_optimal: 10_000.0,
                remote_falloff: 5_000.0,
                duration_s: 5.0,
                ..ActiveModuleCycle::default()
            },
        )
        .unwrap();
    }
    // 100 + 100 + 80 = 280 shield, 300 - 10 + 25 - 10 + 25 = 330 cap
    assert!((bp.combat_states[&6002].shield_hp - 280.0).abs() < 1e-6);
    assert!((bp.combat_states[&6002].capacitor - 330.0).abs() < 1e-6);

    // 6. Mutadaptive spool preservation on re-add with same target
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7008,
            module_type_id: 47_271,
            target_id: Some(6002),
            mutadaptive_spool_step: 0.05,
            current_spool_cycles: 4,
            duration_s: 4.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        ActiveModuleCycle {
            ship_id: 6001,
            module_id: 7008,
            module_type_id: 47_271,
            target_id: Some(6002),
            mutadaptive_spool_step: 0.05,
            current_spool_cycles: 0,
            duration_s: 4.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    let mut_cyc = cycles.iter().find(|c| c.module_id == 7008).unwrap();
    assert_eq!(mut_cyc.current_spool_cycles, 4);

    // 7. handle_update_module_overload: unknown module -> false; same state -> clears pending_overloaded
    assert!(!handle_update_module_overload(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        6001,
        999_999,
        true,
    ));
    assert!(handle_update_module_overload(
        &mut bp,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        None,
        6001,
        7008,
        false,
    ));
    assert_eq!(cycles.iter().find(|c| c.module_id == 7008).unwrap().pending_overloaded, None);

    // 8. apply_overload_transition_to_cycle: ON and OFF roundtrip with attr 1205/1210/1222/1223/1230/1231
    let mut cyc = ActiveModuleCycle {
        ship_id: 6001,
        module_id: 7009,
        module_type_id: 47_271,
        duration_s: 10.0,
        speed_boost_factor: 1.0,
        armor_repair_amount: 100.0,
        shield_boost_amount: 100.0,
        hull_repair_amount: 100.0,
        damage_vector: eve_destiny::DamageVector {
            em: 0.0,
            thermal: 100.0,
            kinetic: 0.0,
            explosive: 100.0,
        },
        max_effective_range_m: Some(20_000.0),
        remote_optimal: 10_000.0,
        ..ActiveModuleCycle::default()
    };
    apply_overload_transition_to_cycle(&mut cyc, true, &mut bp, &sde_opt);
    assert!(cyc.is_overloaded);
    assert!((cyc.duration_s - 8.5).abs() < 1e-6);
    assert!((cyc.speed_boost_factor - 1.5).abs() < 1e-6);
    assert!((cyc.armor_repair_amount - 110.0).abs() < 1e-6);
    assert!((cyc.shield_boost_amount - 110.0).abs() < 1e-6);
    assert!((cyc.damage_vector.thermal - 115.0).abs() < 1e-6);
    assert!((cyc.max_effective_range_m.unwrap() - 24_000.0).abs() < 1e-6);
    assert!((cyc.remote_optimal - 12_000.0).abs() < 1e-6);

    apply_overload_transition_to_cycle(&mut cyc, false, &mut bp, &sde_opt);
    assert!(!cyc.is_overloaded);
    assert!((cyc.duration_s - 10.0).abs() < 1e-6);
    assert!((cyc.speed_boost_factor - 1.0).abs() < 1e-6);
    assert!((cyc.armor_repair_amount - 100.0).abs() < 1e-6);
    assert!((cyc.damage_vector.thermal - 100.0).abs() < 1e-6);
    assert!((cyc.max_effective_range_m.unwrap() - 20_000.0).abs() < 1e-6);

    // Grappler out-of-range during overload sets target_velocity_multiplier = Some(1.0)
    bp.add_ball(make_tackle_ball(6004, 587, DVec3::new(100_000.0, 0.0, 0.0)));
    let mut grap_far = ActiveModuleCycle {
        ship_id: 6001,
        module_id: 7010,
        module_type_id: 40_340,
        target_id: Some(6004),
        grappler_optimal_m: 1_000.0,
        grappler_falloff_m: 2_000.0,
        grappler_speed_factor_pct: -80.0,
        ..ActiveModuleCycle::default()
    };
    let sde_tackle = Some(make_tackle_sde());
    apply_overload_transition_to_cycle(&mut grap_far, true, &mut bp, &sde_tackle);
    assert_eq!(grap_far.target_velocity_multiplier, Some(1.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destiny_events_warp_queries_and_cycle_expiry_helpers_cover_remaining_paths() {
    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_tackle_ball(8001, 587, DVec3::ZERO));
    bp.combat_states.insert(8001, CombatState::default());

    assert!(apply_destiny_event_in_actor(
        &mut bp,
        8001,
        DestinyEvent::OnClientControllerEvent {
            ball_id: 8001,
            target_id: Some(8002),
            event_name: "lock".into(),
            value: 1,
            delay: Some(0.0),
        }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        8001,
        DestinyEvent::OnShipStateUpdate {
            module_states: vec![],
        }
    ));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        8001,
        DestinyEvent::OnDbuffUpdated {
            ball_id: 8001,
            dbuffs: vec![],
        }
    ));

    let ctx = crate::router::ServerContext::new();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let ship_id = 810_001_i64;
    sys.try_spawn_ball_with_combat(
        make_tackle_ball(ship_id, 587, DVec3::ZERO),
        CombatState::default(),
    );
    assert!(!sys.is_ball_warping_sync(ship_id));
    assert_eq!(sys.committed_warp_destination_sync(ship_id), None);
    assert!(!sys.is_ball_aligning_warp_sync(ship_id));

    {
        let mut bp_w = sys.ballpark.write().unwrap();
        bp_w.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000.0,
            phase: eve_destiny::WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
        };
    }
    assert!(sys.is_ball_warping_sync(ship_id));
    assert!(sys.is_ball_aligning_warp_sync(ship_id));
    assert_eq!(sys.committed_warp_destination_sync(ship_id), None);
    assert!(!ctx.systems.is_ball_in_committed_warp_sync(30_000_142, ship_id));

    {
        let mut bp_w = sys.ballpark.write().unwrap();
        bp_w.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000.0,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        };
    }
    assert!(!sys.is_ball_aligning_warp_sync(ship_id));
    assert_eq!(
        sys.committed_warp_destination_sync(ship_id),
        Some(DVec3::new(100_000.0, 0.0, 0.0))
    );
    assert!(ctx.systems.is_ball_in_committed_warp_sync(30_000_142, ship_id));

    // max_active_module_cycle_expiry_unix_sync: cyno skipped when cyno_expiry_unix > 0, lance repeat added
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 91,
            effect_name: "cynosuralGeneration".into(),
            duration_s: 300.0,
            started_at_unix: 1000,
            ..ActiveModuleCycle::default()
        });
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 92,
            effect_name: "superWeaponLanceWarmup".into(),
            duration_s: 10.0,
            elapsed_s: 2.0,
            repeat: 5,
            started_at_unix: 0,
            ..ActiveModuleCycle::default()
        });
    }
    let exp = ctx
        .systems
        .max_active_module_cycle_expiry_unix_sync(30_000_142, ship_id, 1005, 1000);
    // lance rem = ceil(10 - 2) = 8 + repeat 5 -> 1000 + 13 = 1013 > cyno 1005
    assert_eq!(exp, 1013);
    // Unknown system returns cyno_expiry_unix
    assert_eq!(
        ctx.systems
            .max_active_module_cycle_expiry_unix_sync(39_999_999, ship_id, 777, 1000),
        777
    );
}

#[test]
fn w4_mutant_killers_baselines_scaled_recompute_dirty_filters_immobilize_and_overload() {
    // 1. spawn_ball_with_combat_in_actor baseline realignment (env_* = None) + Rigid static + max_velocity restore
    let mut bp = Ballpark::new(30_000_142);
    let mut cycles = Vec::new();
    let mut base_scram = HashMap::new();
    let mut base_core = HashMap::new();
    let mut last_active = HashMap::new();
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let db_opt = Some(Arc::clone(&db));

    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &None,
        &db_opt,
        None,
        make_tackle_ball(9101, 587, DVec3::ZERO),
        CombatState {
            max_shield_hp: 250.0,
            shield_hp: 250.0,
            base_max_shield_hp: Some(100.0),
            max_armor_hp: 300.0,
            armor_hp: 300.0,
            base_max_armor_hp: Some(100.0),
            max_structure_hp: 400.0,
            structure_hp: 400.0,
            base_max_structure_hp: Some(100.0),
            max_capacitor: 500.0,
            capacitor: 500.0,
            base_max_capacitor: Some(100.0),
            capacitor_recharge_time_ms: 120_000.0,
            base_capacitor_recharge_time_ms: Some(10_000.0),
            signature_radius: 55.0,
            base_signature_radius: Some(40.0),
            ..CombatState::default()
        },
        None,
    );
    let s1 = &bp.combat_states[&9101];
    assert_eq!(s1.base_max_shield_hp, Some(250.0));
    assert_eq!(s1.base_max_armor_hp, Some(300.0));
    assert_eq!(s1.base_max_structure_hp, Some(400.0));
    assert_eq!(s1.base_max_capacitor, Some(500.0));
    assert_eq!(s1.base_capacitor_recharge_time_ms, Some(120_000.0));
    assert_eq!(s1.base_signature_radius, Some(55.0));

    // Re-spawn Rigid static ball with nonzero max_velocity -> Rigid branch; re-spawn mobile -> max_velocity restored
    let mut rigid_ball = make_tackle_ball(9102, 587, DVec3::new(5_000.0, 0.0, 0.0));
    rigid_ball.mode = MovementMode::Rigid;
    rigid_ball.velocity = DVec3::new(10.0, 0.0, 0.0);
    spawn_ball_with_combat_in_actor(
        &mut bp, &mut cycles, &mut base_scram, &mut base_core, &None, &None, None, rigid_ball, CombatState::default(), None,
    );
    assert_eq!(bp.balls[&9102].velocity, DVec3::ZERO);

    let mut b_rest = make_tackle_ball(9101, 587, DVec3::ZERO);
    b_rest.max_velocity = 650.0;
    let prev_9101 = bp.combat_states[&9101].clone();
    spawn_ball_with_combat_in_actor(
        &mut bp, &mut cycles, &mut base_scram, &mut base_core, &None, &db_opt, None, b_rest,
        CombatState { base_max_velocity: Some(650.0), ..prev_9101 },
        None,
    );
    assert!((bp.balls[&9101].max_velocity - 650.0).abs() < 1e-6);

    // 2. set_combat_state_in_actor recomputes scaled baselines when env_* != None & persists damaged HP to DB
    set_combat_state_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &mut last_active,
        &db_opt,
        None,
        9101,
        CombatState {
            max_shield_hp: 300.0,
            shield_hp: 150.0, // damaged -> persist_ship_combat_hp_in_db
            base_max_shield_hp: Some(250.0),
            env_max_shield_multiplier: Some(1.5),
            max_armor_hp: 600.0,
            armor_hp: 600.0,
            base_max_armor_hp: Some(300.0),
            env_max_armor_multiplier: Some(2.0),
            max_structure_hp: 500.0,
            structure_hp: 500.0,
            base_max_structure_hp: Some(400.0),
            max_capacitor: 625.0,
            capacitor: 625.0,
            base_max_capacitor: Some(500.0),
            env_max_cap_multiplier: Some(1.25),
            capacitor_recharge_time_ms: 80_000.0,
            base_capacitor_recharge_time_ms: Some(120_000.0),
            cloud_cap_recharge_multiplier: Some(0.8),
            signature_radius: 150.0,
            base_signature_radius: Some(55.0),
            tp_sig_multiplier: Some(1.5),
            env_sig_multiplier: Some(2.0),
            ..CombatState::default()
        },
    );
    let s1_after = &bp.combat_states[&9101];
    assert!((s1_after.base_max_shield_hp.unwrap() - 200.0).abs() < 1e-6);
    assert!((s1_after.base_max_armor_hp.unwrap() - 300.0).abs() < 1e-6);
    assert!((s1_after.base_max_structure_hp.unwrap() - 500.0).abs() < 1e-6);
    assert!((s1_after.base_max_capacitor.unwrap() - 500.0).abs() < 1e-6);
    assert!((s1_after.base_capacitor_recharge_time_ms.unwrap() - 100_000.0).abs() < 1e-6);
    assert!((s1_after.base_signature_radius.unwrap() - 50.0).abs() < 1e-6);
    db.with_snapshot(|snap| {
        assert!(snap.sp6_lifecycle.ship_combat_hp.contains_key(&9101));
    });

    // 3. reconcile_tackle_and_bubbles resets expired module_mass/sig/adc/ehe with zero cycles + missing ball safety
    bp.combat_states.get_mut(&9101).unwrap().module_mass_multiplier = 2.0;
    bp.combat_states.get_mut(&9101).unwrap().module_mass_addition = 50_000.0;
    bp.combat_states.get_mut(&9101).unwrap().module_sig_multiplier = 1.5;
    bp.combat_states.get_mut(&9101).unwrap().adc_active = true;
    bp.combat_states.get_mut(&9101).unwrap().ehe_hull_resists = Some([0.1; 4]);
    bp.combat_states.get_mut(&9101).unwrap().shield_resonance = [0.25; 4];
    bp.combat_states.get_mut(&9101).unwrap().base_shield_resonance = Some([0.5; 4]);
    cycles.push(ActiveModuleCycle { ship_id: 999_999, immobilize_self: true, ..ActiveModuleCycle::default() });
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    cycles.clear();
    let r_cs = &bp.combat_states[&9101];
    assert_eq!(r_cs.module_mass_multiplier, 1.0);
    assert_eq!(r_cs.module_mass_addition, 0.0);
    assert_eq!(r_cs.module_sig_multiplier, 1.0);
    assert!(!r_cs.adc_active);
    assert!(r_cs.ehe_hull_resists.is_none());

    // 4. handle_add_module_cycle immobilize_self zeroes velocity & max_velocity
    bp.balls.get_mut(&9101).unwrap().velocity = DVec3::new(100.0, 0.0, 0.0);
    bp.balls.get_mut(&9101).unwrap().max_velocity = 400.0;
    handle_add_module_cycle(
        &mut bp, &mut cycles, &base_scram, &base_core, &None, &None, None,
        ActiveModuleCycle {
            ship_id: 9101,
            module_id: 9201,
            immobilize_self: true,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
    )
    .unwrap();
    assert_eq!(bp.balls[&9101].velocity, DVec3::ZERO);
    assert_eq!(bp.balls[&9101].max_velocity, 0.0);

    // 5. apply_overload_transition_to_cycle branches: no cat5 -> 1.20 fallback vs has_cat5 -> 1.0,
    //    1206 duration, 1210 on armor repairer group 62, 1208 on shield booster group 40,
    //    weapon_optimal_m with 1222, cloaking skips prop_velocity_multiplier update while prop updates it
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.effects.insert(9901, eve_sde::DogmaEffectType { effect_id: 9901, name: "overloadSelf".into(), effect_category: 5, ..Default::default() });
    sde.types.insert(99_001, make_inv_type(99_001, 62, 7, "Armor Rep", HashMap::from([(1206u16, -20.0), (1210u16, 25.0)]), vec![9901]));
    sde.types.insert(99_002, make_inv_type(99_002, 40, 7, "Shield Boost", HashMap::from([(1181u16, -10.0), (1208u16, 30.0)]), vec![9901]));
    sde.types.insert(99_003, make_inv_type(99_003, 53, 7, "Weapon NoCat5", HashMap::from([(1222u16, 50.0)]), vec![]));
    let sde_opt = Some(Arc::new(sde));

    let mut cyc_ar = ActiveModuleCycle { ship_id: 9101, module_type_id: 99_001, duration_s: 10.0, armor_repair_amount: 100.0, shield_boost_amount: 100.0, ..ActiveModuleCycle::default() };
    apply_overload_transition_to_cycle(&mut cyc_ar, true, &mut bp, &sde_opt);
    assert!((cyc_ar.duration_s - 8.0).abs() < 1e-6);
    assert!((cyc_ar.armor_repair_amount - 125.0).abs() < 1e-6);
    assert!((cyc_ar.shield_boost_amount - 100.0).abs() < 1e-6, "has_cat5 without 1231/1208 -> 1.0");

    let mut cyc_sb = ActiveModuleCycle { ship_id: 9101, module_type_id: 99_002, duration_s: 10.0, shield_boost_amount: 100.0, hull_repair_amount: 100.0, ..ActiveModuleCycle::default() };
    apply_overload_transition_to_cycle(&mut cyc_sb, true, &mut bp, &sde_opt);
    assert!((cyc_sb.duration_s - 9.0).abs() < 1e-6);
    assert!((cyc_sb.shield_boost_amount - 130.0).abs() < 1e-6);
    assert!((cyc_sb.hull_repair_amount - 100.0).abs() < 1e-6);

    bp.combat_states.get_mut(&9101).unwrap().prop_velocity_multiplier = 2.0;
    let mut cyc_wep_cloak = ActiveModuleCycle {
        ship_id: 9101,
        module_type_id: 99_003,
        effect_name: "cloakingDevice".into(),
        speed_boost_factor: 1.0,
        weapon_optimal_m: 10_000.0,
        remote_falloff: 500.0,
        max_effective_range_m: Some(20_000.0),
        hull_repair_amount: 100.0,
        ..ActiveModuleCycle::default()
    };
    apply_overload_transition_to_cycle(&mut cyc_wep_cloak, true, &mut bp, &sde_opt);
    assert!((cyc_wep_cloak.weapon_optimal_m - 15_000.0).abs() < 1e-6);
    assert_eq!(cyc_wep_cloak.max_effective_range_m, Some(20_000.0), "remote_falloff != 0 leaves max_effective_range_m");
    assert!((cyc_wep_cloak.hull_repair_amount - 120.0).abs() < 1e-6, "no cat5 -> 1.20 fallback");
    assert!((bp.combat_states[&9101].prop_velocity_multiplier - 2.0).abs() < 1e-6, "cloaking skips prop_velocity_multiplier update");

    let mut cyc_prop = ActiveModuleCycle { ship_id: 9101, module_type_id: 99_003, speed_boost_factor: 1.0, ..ActiveModuleCycle::default() };
    apply_overload_transition_to_cycle(&mut cyc_prop, true, &mut bp, &sde_opt);
    // old_factor=2.0, ovl_speed=1.5 -> new speed_boost_factor=1.5 -> new_factor=2.5 -> 2.0/2.0*2.5 = 2.5
    assert!((bp.combat_states[&9101].prop_velocity_multiplier - 2.5).abs() < 1e-6);

    // 6. SetBallInteractive emits only on state change
    bp.add_ball(make_tackle_ball(9103, 587, DVec3::new(100.0, 0.0, 0.0)));
    bp.combat_states.insert(9103, CombatState::default());
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        9101,
        DestinyEvent::SetBallInteractive {
            ball_id: 9101,
            is_interactive: true,
        },
    ));
    let _ = bp.drain_all_observer_events();
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        9101,
        DestinyEvent::SetBallInteractive {
            ball_id: 9101,
            is_interactive: false,
        },
    ));
    let evs1 = bp.drain_observer_events(9103);
    assert!(evs1.iter().any(|e| matches!(
        e,
        DestinyEvent::SetBallInteractive {
            ball_id: 9101,
            is_interactive: false,
        }
    )));
    assert!(apply_destiny_event_in_actor(
        &mut bp,
        9101,
        DestinyEvent::SetBallInteractive {
            ball_id: 9101,
            is_interactive: false,
        },
    ));
    let evs2 = bp.drain_observer_events(9103);
    assert!(!evs2
        .iter()
        .any(|e| matches!(e, DestinyEvent::SetBallInteractive { .. })));

    // 7. #871: spawn_ball_with_combat_in_actor with is_cloaked=true inserts CombatState before add_ball
    let _ = bp.drain_all_observer_events();
    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    spawn_ball_with_combat_in_actor(
        &mut bp,
        &mut cycles,
        &mut base_scram,
        &mut base_core,
        &None,
        &db_opt,
        None,
        make_tackle_ball(9104, 587, DVec3::new(150.0, 0.0, 0.0)),
        cloaked_cs,
        None,
    );
    let obs_evs = bp.drain_observer_events(9103);
    assert!(
        !obs_evs.iter().any(|e| matches!(e, DestinyEvent::AddBalls2 { .. })),
        "cloaked ball spawn via spawn_ball_with_combat_in_actor must not emit AddBalls2 (#871)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gh890_rejects_reactivating_active_cycle_and_defers_paid_end_of_cycle_deactivation() {
    let mut bp = Ballpark::new(30_000_142);
    let ship_id = 9890_001_i64;
    let target_a = 9890_002_i64;
    let target_b = 9890_003_i64;
    bp.add_ball(make_tackle_ball(ship_id, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            capacitor: 500.0,
            max_capacitor: 500.0,
            armor_hp: 200.0,
            max_armor_hp: 1_000.0,
            undock_invulnerable_remaining_ticks: 10,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_tackle_ball(target_a, 587, DVec3::new(2_000.0, 0.0, 0.0)));
    bp.combat_states.insert(target_a, CombatState::default());
    bp.add_ball(make_tackle_ball(target_b, 587, DVec3::new(3_000.0, 0.0, 0.0)));
    bp.combat_states.insert(target_b, CombatState::default());

    let mut cycles = Vec::new();
    let base_scram = HashMap::new();
    let base_core = HashMap::new();

    // 1. Activate Stasis Webifier on target_a
    handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &None,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 890_101,
            target_id: Some(target_a),
            duration_s: 5.0,
            elapsed_s: 2.0,
            cap_need_per_cycle: 25.0,
            target_velocity_multiplier: Some(0.4),
            ..ActiveModuleCycle::default()
        },
    )
    .expect("first web activation succeeds");
    assert!((bp.combat_states[&ship_id].capacitor - 475.0).abs() < 1e-6);
    assert!((bp.balls[&target_a].max_velocity - 160.0).abs() < 1e-6);
    assert!((bp.balls[&target_b].max_velocity - 400.0).abs() < 1e-6);

    // Re-adding the same active module on target_b mid-cycle MUST be rejected with ModuleAlreadyActive
    // without deducting capacitor or swapping the web to target_b (#890).
    let dup_web_err = handle_add_module_cycle(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &None,
        &None,
        None,
        ActiveModuleCycle {
            ship_id,
            module_id: 890_101,
            target_id: Some(target_b),
            duration_s: 5.0,
            cap_need_per_cycle: 25.0,
            target_velocity_multiplier: Some(0.4),
            ..ActiveModuleCycle::default()
        },
    )
    .expect_err("re-activating already-active module must fail");
    assert!(dup_web_err.contains("ModuleAlreadyActive"), "got {dup_web_err}");
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].target_id, Some(target_a));
    assert!((cycles[0].elapsed_s - 2.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship_id].capacitor - 475.0).abs() < 1e-6);
    assert!((bp.balls[&target_a].max_velocity - 160.0).abs() < 1e-6);
    assert!((bp.balls[&target_b].max_velocity - 400.0).abs() < 1e-6);

    // 2. has_paid_end_of_cycle_effect & request_cycle_deactivation (#890)
    assert!(ActiveModuleCycle {
        armor_repair_amount: 80.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(ActiveModuleCycle {
        hull_repair_amount: 40.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(ActiveModuleCycle {
        remote_armor_amount: 120.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(!ActiveModuleCycle {
        remote_armor_amount: 120.0,
        mutadaptive_spool_step: 0.1,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(ActiveModuleCycle {
        remote_hull_amount: 60.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(ActiveModuleCycle {
        is_aar: true,
        aar_base_armor_repair: 50.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(ActiveModuleCycle {
        cap_transfer_target: 40.0,
        remote_cap_amount: 0.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());
    assert!(!ActiveModuleCycle {
        cap_transfer_target: 40.0,
        remote_cap_amount: 40.0,
        ..ActiveModuleCycle::default()
    }
    .has_paid_end_of_cycle_effect());

    let ctx = crate::router::ServerContext::new();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.spawn_ball_with_combat(
        make_tackle_ball(ship_id, 587, DVec3::ZERO),
        CombatState {
            armor_hp: 200.0,
            max_armor_hp: 1_000.0,
            structure_hp: 150.0,
            max_structure_hp: 500.0,
            capacitor: 500.0,
            max_capacitor: 500.0,
            capacitor_recharge_time_ms: 0.0,
            shield_recharge_time_ms: 0.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 890_201,
        duration_s: 4.0,
        elapsed_s: 1.5,
        repeat: 1,
        cap_need_per_cycle: 50.0,
        hull_repair_amount: 90.0,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    // Re-adding active hull repairer via SystemHandle::add_module_cycle is rejected
    let actor_dup = sys
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 890_201,
            duration_s: 4.0,
            repeat: 1,
            cap_need_per_cycle: 50.0,
            hull_repair_amount: 90.0,
            ..ActiveModuleCycle::default()
        })
        .await
        .expect_err("actor must reject duplicate active module cycle");
    assert!(actor_dup.contains("ModuleAlreadyActive"));
    // Deactivating mid-cycle marks repeat=0 and still delivers the paid 90 HP hull repair at cycle end
    assert_eq!(sys.request_cycle_deactivation(ship_id, 890_201).await, Ok(true));
    assert!(sys.has_module_cycle(ship_id, 890_201).await);
    let _ = sys.step_manual(2.6).await.unwrap();
    assert!(!sys.has_module_cycle(ship_id, 890_201).await);
    let cs_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_after.structure_hp - 240.0).abs() < 1e-3);
    assert!((cs_after.capacitor - 450.0).abs() < 1e-6);
}

#[test]
fn propulsion_overload_transition_respects_t3d_propulsion_mode_stacking_penalty_1364() {
    // Regression test for #1364: Confessor Propulsion Mode (34323) applies a stacking-penalized
    // PostDiv 0.6 (+66.67%) bonus to attribute 20 (`speedFactor`) on 1MN Afterburner I (439).
    // Toggling Overload (+50% attr 1223) ON/OFF at cycle boundary must apply Dogma's rank-1
    // stacking penalty (`1 + 0.50 * exp(-1/7.1289)`) rather than raw 1.50x.
    let ship_id: BallId = 1_364_001;
    let ab_id: i64 = 1_364_010;
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.effects.insert(
        3175,
        eve_sde::DogmaEffectType {
            effect_id: 3175,
            name: "overloadSelfSpeedBonus".into(),
            effect_category: 5,
            ..Default::default()
        },
    );
    sde.effects.insert(
        6801,
        eve_sde::DogmaEffectType {
            effect_id: 6801,
            name: "modeMWDandABBoostPostDiv".into(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "LocationRequiredSkillModifier".into(),
                    modified_attribute_id: 20,
                    modifying_attribute_id: 2003,
                    operator: 5,
                    group_id: None,
                    skill_type_id: Some(3454),
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "LocationRequiredSkillModifier".into(),
                    modified_attribute_id: 20,
                    modifying_attribute_id: 2003,
                    operator: 5,
                    group_id: None,
                    skill_type_id: Some(3450),
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    sde.types.insert(
        439,
        make_inv_type(
            439,
            46,
            7,
            "1MN Afterburner I",
            HashMap::from([(20u16, 115.0), (182u16, 3450.0), (1223u16, 50.0)]),
            vec![3175],
        ),
    );
    sde.types.insert(
        34_323,
        make_inv_type(
            34_323,
            1306,
            7,
            "Confessor Propulsion Mode",
            HashMap::from([(2003u16, 0.6)]),
            vec![6801],
        ),
    );
    let sde_opt = Some(Arc::new(sde));

    let base_vel = 312.5;
    let base_boost = 1.50;
    let stacked_ovl_mult = 1.0 + 0.50 * eve_dogma::stacking_penalty_factor(1);
    let overloaded_boost = base_boost * stacked_ovl_mult;

    let mut bp = Ballpark::new(30_002_423);
    let mut ball = make_tackle_ball(ship_id, 34_317, DVec3::ZERO);
    ball.max_velocity = base_vel;
    bp.add_ball(ball);
    bp.combat_states.insert(
        ship_id,
        CombatState {
            base_max_velocity: Some(base_vel),
            prop_velocity_multiplier: 1.0 + overloaded_boost,
            ..CombatState::default()
        },
    );
    bp.recompute_ball_attributes(ship_id);

    let proj_prop_mode = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(1),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: Some(34_323),
        last_stance_switch_ms: None,
        baseline: eve_dogma::CombatBaseline {
            max_velocity: base_vel,
            mass: 1_800_000.0,
            agility: 2.0,
            signature_radius: 50.0,
            max_shield_hp: 500.0,
            max_armor_hp: 1_000.0,
            max_structure_hp: 800.0,
            max_capacitor: 600.0,
            shield_recharge_time_ms: 100_000.0,
            capacitor_recharge_time_ms: 100_000.0,
            shield_resonance: [1.0; 4],
            armor_resonance: [1.0; 4],
            structure_resonance: [1.0; 4],
            warp_speed_au_s: 4.5,
            max_locked_targets: 6,
            max_target_range: 45_000.0,
            scan_resolution: 400.0,
        },
        propulsion: None,
        modules: HashMap::new(),
    };

    let mut cycle = ActiveModuleCycle {
        ship_id,
        module_id: ab_id,
        module_type_id: 439,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 7.5,
        is_overloaded: true,
        speed_boost_factor: overloaded_boost,
        ..ActiveModuleCycle::default()
    };

    // StopOverload (true -> false) under Confessor Propulsion Mode must restore exact 1.50 boost
    // and 781.25 m/s (NOT 760.799997 m/s from dividing by unstacked 1.50).
    apply_overload_transition_to_cycle_with_projection(
        &mut cycle,
        false,
        &mut bp,
        &sde_opt,
        Some(&proj_prop_mode),
    );
    assert!(!cycle.is_overloaded);
    assert!(
        (cycle.speed_boost_factor - base_boost).abs() < 1e-12,
        "StopOverload under Propulsion Mode must restore exact boost {base_boost}, got {}",
        cycle.speed_boost_factor
    );
    assert!((bp.combat_states[&ship_id].prop_velocity_multiplier - 2.50).abs() < 1e-12);
    assert!(
        (bp.balls[&ship_id].max_velocity - 781.25).abs() < 1e-9,
        "StopOverload under Propulsion Mode must restore exact 781.25 m/s, got {}",
        bp.balls[&ship_id].max_velocity
    );

    // Overload (false -> true) under Confessor Propulsion Mode must apply stacked multiplier.
    apply_overload_transition_to_cycle_with_projection(
        &mut cycle,
        true,
        &mut bp,
        &sde_opt,
        Some(&proj_prop_mode),
    );
    assert!(cycle.is_overloaded);
    assert!(
        (cycle.speed_boost_factor - overloaded_boost).abs() < 1e-12,
        "Overload under Propulsion Mode must apply stacked boost {overloaded_boost}, got {}",
        cycle.speed_boost_factor
    );
    assert!(
        (bp.balls[&ship_id].max_velocity - base_vel * (1.0 + overloaded_boost)).abs() < 1e-9
    );

    // Without a speedFactor-modifying stance, full unstacked 1.50x overload multiplier applies.
    let mut proj_no_stance = proj_prop_mode;
    proj_no_stance.active_stance_type_id = None;
    let mut cycle_plain = ActiveModuleCycle {
        ship_id,
        module_id: ab_id,
        module_type_id: 439,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 7.5,
        is_overloaded: false,
        speed_boost_factor: base_boost,
        ..ActiveModuleCycle::default()
    };
    bp.combat_states.get_mut(&ship_id).unwrap().prop_velocity_multiplier = 1.0 + base_boost;
    apply_overload_transition_to_cycle_with_projection(
        &mut cycle_plain,
        true,
        &mut bp,
        &sde_opt,
        Some(&proj_no_stance),
    );
    assert!((cycle_plain.speed_boost_factor - (base_boost * 1.50)).abs() < 1e-12);
}

#[test]
fn apply_fit_projection_with_cycles_resyncs_propulsion_and_reconciles_max_velocity_1364() {
    // Regression test for #1364: when SyncOverloadState (or an active propulsion fit/stance change)
    // pushes a fresh FitProjection, apply_fit_projection_with_cycles_in_actor must re-sync active
    // propulsion cycles, reconcile CombatState::prop_velocity_multiplier & Ball::max_velocity, and
    // emit DestinyEvent::SetMaxSpeed while leaving unrelated mid-cycle weapons untouched.
    let ship_id: BallId = 1_364_101;
    let ab_id: i64 = 1_364_110;
    let turret_id: i64 = 1_364_111;
    let base_vel = 312.5;
    let stacked_ovl_boost = 1.50 * (1.0 + 0.50 * eve_dogma::stacking_penalty_factor(1));
    let drifted_boost = stacked_ovl_boost / 1.50;

    let mut bp = Ballpark::new(30_002_423);
    let mut ball = make_tackle_ball(ship_id, 34_317, DVec3::ZERO);
    ball.max_velocity = base_vel;
    bp.add_ball(ball);
    bp.combat_states.insert(
        ship_id,
        CombatState {
            base_max_velocity: Some(base_vel),
            base_mass: Some(1_800_000.0),
            base_inertia: Some(2.0),
            base_signature_radius: Some(50.0),
            prop_velocity_multiplier: 1.0 + drifted_boost,
            module_mass_addition: 500_000.0,
            ..CombatState::default()
        },
    );
    bp.recompute_ball_attributes(ship_id);
    bp.pending_events.clear();
    assert!((bp.balls[&ship_id].max_velocity - base_vel * (1.0 + drifted_boost)).abs() < 1e-6);

    let mk_cp = |module_id: i64, module_type_id: u32, is_overloaded: bool, prop_speed_boost: f64, optimal_range_m: f64, is_turret: bool, is_afterburner: bool| {
        eve_dogma::CycleParams {
            module_id,
            module_type_id,
            flag_id: 19,
            state: if is_overloaded {
                eve_dogma::ModuleState::Overloaded
            } else {
                eve_dogma::ModuleState::Active
            },
            is_overloaded,
            charge_type_id: None,
            duration_ms: 7_500.0,
            capacitor_need: 10.0,
            is_turret,
            is_missile: false,
            damage: [0.0; 4],
            damage_multiplier: 1.0,
            optimal_range_m,
            falloff_m: 1_000.0,
            tracking_speed: 0.4,
            sig_resolution_m: 40.0,
            missile_explosion_radius_m: 0.0,
            missile_explosion_velocity_ms: 0.0,
            missile_drf: 0.0,
            missile_flight_time_ms: 0.0,
            missile_max_velocity_ms: 0.0,
            shield_boost_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            speed_factor: 115.0 / 0.6,
            speed_boost_factor: 1_500_000.0,
            mass_addition: if is_afterburner { 500_000.0 } else { 0.0 },
            sig_radius_bonus_pct: 0.0,
            prop_speed_boost,
            is_mwd: false,
            is_afterburner,
            is_cloak: false,
            cloak_velocity_bonus_pct: 0.0,
            is_hic_wdfg: false,
        }
    };
    let baseline = eve_dogma::CombatBaseline {
        max_velocity: base_vel,
        mass: 1_800_000.0,
        agility: 2.0,
        signature_radius: 50.0,
        max_shield_hp: 500.0,
        max_armor_hp: 1_000.0,
        max_structure_hp: 800.0,
        max_capacitor: 600.0,
        shield_recharge_time_ms: 100_000.0,
        capacitor_recharge_time_ms: 100_000.0,
        shield_resonance: [1.0; 4],
        armor_resonance: [1.0; 4],
        structure_resonance: [1.0; 4],
        warp_speed_au_s: 4.5,
        max_locked_targets: 6,
        max_target_range: 45_000.0,
        scan_resolution: 400.0,
    };

    // Prior FitProjection (rev 1): AB still Overloaded before SyncOverloadState ran.
    let prev_proj = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(1),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: Some(34_323),
        last_stance_switch_ms: None,
        baseline,
        propulsion: Some(eve_dogma::PropulsionParams {
            module_id: ab_id,
            module_type_id: 439,
            is_mwd: false,
            is_overloaded: true,
            speed_factor: (115.0 / 0.6) * (1.0 + 0.50 * eve_dogma::stacking_penalty_factor(1)),
            speed_boost_factor: 1_500_000.0,
            mass_addition: 500_000.0,
            signature_radius_bonus_pct: 0.0,
            speed_boost: stacked_ovl_boost,
        }),
        modules: HashMap::from([
            (ab_id, mk_cp(ab_id, 439, true, stacked_ovl_boost, 0.0, false, true)),
            (turret_id, mk_cp(turret_id, 3_041, false, 0.0, 4_000.0, true, false)),
        ]),
    };
    let mut projections = HashMap::from([(ship_id, prev_proj)]);

    // Active cycles after StopOverload cycle boundary with drifted AB speed_boost_factor.
    let mut cycles = vec![
        ActiveModuleCycle {
            ship_id,
            module_id: ab_id,
            module_type_id: 439,
            effect_name: "moduleBonusAfterburner".into(),
            duration_s: 7.5,
            is_overloaded: false,
            speed_boost_factor: drifted_boost,
            hic_mass_addition: Some(500_000.0),
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id,
            module_id: turret_id,
            module_type_id: 3_041,
            effect_name: "targetHostile".into(),
            duration_s: 3.0,
            is_overloaded: false,
            weapon_is_turret: true,
            weapon_optimal_m: 4_000.0,
            ..ActiveModuleCycle::default()
        },
    ];

    // SyncOverloadState delivers rev 2 (AB un-overloaded with authoritative prop_speed_boost=1.50;
    // turret optimal reduced by mid-cycle TD which must NOT take effect mid-cycle).
    let next_proj = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(2),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: Some(34_323),
        last_stance_switch_ms: None,
        baseline,
        propulsion: Some(eve_dogma::PropulsionParams {
            module_id: ab_id,
            module_type_id: 439,
            is_mwd: false,
            is_overloaded: false,
            speed_factor: 115.0 / 0.6,
            speed_boost_factor: 1_500_000.0,
            mass_addition: 500_000.0,
            signature_radius_bonus_pct: 0.0,
            speed_boost: 1.50,
        }),
        modules: HashMap::from([
            (ab_id, mk_cp(ab_id, 439, false, 1.50, 0.0, false, true)),
            (turret_id, mk_cp(turret_id, 3_041, false, 0.0, 1_200.0, true, false)),
        ]),
    };

    let applied = crate::system_actor::apply_fit_projection_with_cycles_in_actor(
        &mut bp,
        &mut projections,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &None,
        None,
        ship_id,
        next_proj,
    );
    assert!(applied);
    assert!((cycles[0].speed_boost_factor - 1.50).abs() < 1e-12);
    assert!((bp.combat_states[&ship_id].prop_velocity_multiplier - 2.50).abs() < 1e-12);
    assert!(
        (bp.balls[&ship_id].max_velocity - 781.25).abs() < 1e-9,
        "ApplyFitProjection after SyncOverloadState must reconcile Ball::max_velocity to 781.25, got {}",
        bp.balls[&ship_id].max_velocity
    );
    assert!(
        bp.pending_events.iter().any(|ev| matches!(
            ev,
            DestinyEvent::SetMaxSpeed { ball_id, max_speed }
                if *ball_id == ship_id && (*max_speed - 781.25).abs() < 1e-9
        )),
        "ApplyFitProjection reconciling propulsion velocity must emit SetMaxSpeed(781.25)"
    );
    assert!(
        (cycles[1].weapon_optimal_m - 4_000.0).abs() < 1e-6,
        "unrelated mid-cycle turret must defer FitProjection sync until its next cycle boundary"
    );

    // Active propulsion stance change (rev 3, Defense Mode -> prop_speed_boost = 0.90) reconciles immediately.
    bp.pending_events.clear();
    let stance_proj = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(3),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: Some(34_319),
        last_stance_switch_ms: None,
        baseline,
        propulsion: Some(eve_dogma::PropulsionParams {
            module_id: ab_id,
            module_type_id: 439,
            is_mwd: false,
            is_overloaded: false,
            speed_factor: 115.0,
            speed_boost_factor: 1_500_000.0,
            mass_addition: 500_000.0,
            signature_radius_bonus_pct: 0.0,
            speed_boost: 0.90,
        }),
        modules: HashMap::from([
            (ab_id, mk_cp(ab_id, 439, false, 0.90, 0.0, false, true)),
            (turret_id, mk_cp(turret_id, 3_041, false, 0.0, 1_200.0, true, false)),
        ]),
    };
    assert!(crate::system_actor::apply_fit_projection_with_cycles_in_actor(
        &mut bp,
        &mut projections,
        &mut cycles,
        &HashMap::new(),
        &HashMap::new(),
        &None,
        None,
        ship_id,
        stance_proj,
    ));
    assert!((cycles[0].speed_boost_factor - 0.90).abs() < 1e-12);
    assert!((bp.combat_states[&ship_id].prop_velocity_multiplier - 1.90).abs() < 1e-12);
    assert!((bp.balls[&ship_id].max_velocity - (base_vel * 1.90)).abs() < 1e-9);
}

#[test]
fn sync_cycle_prefers_mass_recomputed_propulsion_speed_boost_over_cycle_params_1444() {
    // Regression test for #1444 F-01: when build_fit_projection recomputes
    // FitProjection::propulsion.speed_boost against dbuff/system-effect-adjusted mass,
    // sync_cycle_from_fit_projection_with_shield_boost_mult must prefer matching
    // proj.propulsion.speed_boost over stale pre-dbuff CycleParams::prop_speed_boost,
    // while keeping both fallbacks (modules-omitted and propulsion-absent/mismatched).
    let ship_id: BallId = 1_444_001;
    let ab_id: i64 = 1_444_010;
    let pre_dbuff_boost = 1.50;
    let post_dbuff_boost = 1.85;

    let baseline = eve_dogma::CombatBaseline {
        max_velocity: 300.0,
        mass: 1_400_000.0,
        agility: 2.0,
        signature_radius: 50.0,
        max_shield_hp: 500.0,
        max_armor_hp: 1_000.0,
        max_structure_hp: 800.0,
        max_capacitor: 600.0,
        shield_recharge_time_ms: 100_000.0,
        capacitor_recharge_time_ms: 100_000.0,
        shield_resonance: [1.0; 4],
        armor_resonance: [1.0; 4],
        structure_resonance: [1.0; 4],
        warp_speed_au_s: 4.5,
        max_locked_targets: 6,
        max_target_range: 45_000.0,
        scan_resolution: 400.0,
    };
    let cp = eve_dogma::CycleParams {
        module_id: ab_id,
        module_type_id: 439,
        flag_id: 19,
        state: eve_dogma::ModuleState::Active,
        is_overloaded: false,
        charge_type_id: None,
        duration_ms: 7_500.0,
        capacitor_need: 10.0,
        is_turret: false,
        is_missile: false,
        damage: [0.0; 4],
        damage_multiplier: 1.0,
        optimal_range_m: 0.0,
        falloff_m: 0.0,
        tracking_speed: 0.0,
        sig_resolution_m: 0.0,
        missile_explosion_radius_m: 0.0,
        missile_explosion_velocity_ms: 0.0,
        missile_drf: 0.0,
        missile_flight_time_ms: 0.0,
        missile_max_velocity_ms: 0.0,
        shield_boost_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        speed_factor: 115.0,
        speed_boost_factor: 1_500_000.0,
        mass_addition: 500_000.0,
        sig_radius_bonus_pct: 0.0,
        prop_speed_boost: pre_dbuff_boost,
        is_mwd: false,
        is_afterburner: true,
        is_cloak: false,
        cloak_velocity_bonus_pct: 0.0,
        is_hic_wdfg: false,
    };
    let prop = eve_dogma::PropulsionParams {
        module_id: ab_id,
        module_type_id: 439,
        is_mwd: false,
        is_overloaded: false,
        speed_factor: 115.0,
        speed_boost_factor: 1_500_000.0,
        mass_addition: 500_000.0,
        signature_radius_bonus_pct: 0.0,
        speed_boost: post_dbuff_boost,
    };

    // Case 1: Both proj.modules[&ab_id] (pre-dbuff 1.50) and matching proj.propulsion (post-dbuff 1.85)
    // are present -> must prefer proj.propulsion.speed_boost (1.85).
    let proj_both = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(1),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline,
        propulsion: Some(prop.clone()),
        modules: HashMap::from([(ab_id, cp.clone())]),
    };
    let projections = HashMap::from([(ship_id, proj_both)]);
    let mut cycle = ActiveModuleCycle {
        ship_id,
        module_id: ab_id,
        module_type_id: 439,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 7.5,
        is_overloaded: false,
        speed_boost_factor: 1.0,
        ..ActiveModuleCycle::default()
    };
    assert!(
        crate::system_actor::cycles::sync_cycle_from_fit_projection_with_shield_boost_mult(
            &mut cycle,
            &projections,
            1.0,
        )
    );
    assert!(
        (cycle.speed_boost_factor - post_dbuff_boost).abs() < 1e-12,
        "expected mass-recomputed FitProjection::propulsion.speed_boost {post_dbuff_boost}, got {}",
        cycle.speed_boost_factor
    );

    // Case 2: proj.modules omits ab_id while matching proj.propulsion is present -> fallback still works.
    let proj_prop_only = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(2),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline,
        propulsion: Some(prop),
        modules: HashMap::new(),
    };
    let projections_prop_only = HashMap::from([(ship_id, proj_prop_only)]);
    let mut cycle_prop_only = ActiveModuleCycle {
        ship_id,
        module_id: ab_id,
        module_type_id: 439,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 7.5,
        is_overloaded: false,
        speed_boost_factor: 1.0,
        ..ActiveModuleCycle::default()
    };
    assert!(
        crate::system_actor::cycles::sync_cycle_from_fit_projection_with_shield_boost_mult(
            &mut cycle_prop_only,
            &projections_prop_only,
            1.0,
        )
    );
    assert!((cycle_prop_only.speed_boost_factor - post_dbuff_boost).abs() < 1e-12);
    assert_eq!(cycle_prop_only.hic_mass_addition, Some(500_000.0));

    // Case 3: proj.propulsion is absent -> falls back to CycleParams::prop_speed_boost (1.50).
    let proj_mod_only = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(3),
        ship_id,
        ship_type_id: 34_317,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline,
        propulsion: None,
        modules: HashMap::from([(ab_id, cp)]),
    };
    let projections_mod_only = HashMap::from([(ship_id, proj_mod_only)]);
    let mut cycle_mod_only = ActiveModuleCycle {
        ship_id,
        module_id: ab_id,
        module_type_id: 439,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 7.5,
        is_overloaded: false,
        speed_boost_factor: 1.0,
        ..ActiveModuleCycle::default()
    };
    assert!(
        crate::system_actor::cycles::sync_cycle_from_fit_projection_with_shield_boost_mult(
            &mut cycle_mod_only,
            &projections_mod_only,
            1.0,
        )
    );
    assert!((cycle_mod_only.speed_boost_factor - pre_dbuff_boost).abs() < 1e-12);
}

#[test]
fn propulsion_overload_skips_pre_group_and_stacking_exempt_sources_1444() {
    // Regression test for #1444 F-02:
    // - PreMul (op 0) / PreDiv (op 1) modifiers run in Dogma's pre-group pass and must NOT
    //   stack-penalize against overloadSelfSpeedBonus (PostPercent, op 6).
    // - Stacking-exempt source categories (e.g., category_id = 6 Ship) must NOT stack-penalize.
    // - When attribute 20 (speedFactor) is marked stackable=true, no stacking penalty applies.
    // - Real non-exempt Confessor Propulsion Mode (34323, category_id = 7, PostDiv op 5) DOES
    //   apply exact Dogma rank-1 stacking penalty.
    let ship_id: BallId = 1_444_101;
    let ab_id: i64 = 1_444_110;
    let base_boost = 1.50;
    let unstacked_ovl_mult = 1.50;
    let stacked_ovl_mult = 1.0 + 0.50 * eve_dogma::stacking_penalty_factor(1);

    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        439,
        make_inv_type(
            439,
            46,
            7,
            "1MN Afterburner I",
            HashMap::from([(20u16, 115.0), (182u16, 3450.0), (1223u16, 50.0)]),
            vec![3175],
        ),
    );
    // Synthetic PreMul (op 0) and PreDiv (op 1) effects on attr 20.
    sde.effects.insert(
        60_001,
        eve_sde::DogmaEffectType {
            effect_id: 60_001,
            name: "syntheticPreMulSpeedFactor".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationRequiredSkillModifier".into(),
                modified_attribute_id: 20,
                modifying_attribute_id: 2003,
                operator: 0,
                group_id: None,
                skill_type_id: Some(3450),
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        60_002,
        eve_sde::DogmaEffectType {
            effect_id: 60_002,
            name: "syntheticPreDivSpeedFactor".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationRequiredSkillModifier".into(),
                modified_attribute_id: 20,
                modifying_attribute_id: 2003,
                operator: 1,
                group_id: None,
                skill_type_id: Some(3450),
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        6801,
        eve_sde::DogmaEffectType {
            effect_id: 6801,
            name: "modeMWDandABBoostPostDiv".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationRequiredSkillModifier".into(),
                modified_attribute_id: 20,
                modifying_attribute_id: 2003,
                operator: 5,
                group_id: None,
                skill_type_id: Some(3450),
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    // Non-exempt stance with PreMul (op 0, 1.6x).
    sde.types.insert(
        990_101,
        make_inv_type(
            990_101,
            1306,
            7,
            "Synthetic PreMul Mode",
            HashMap::from([(2003u16, 1.6)]),
            vec![60_001],
        ),
    );
    // Non-exempt stance with PreDiv (op 1, /0.6).
    sde.types.insert(
        990_102,
        make_inv_type(
            990_102,
            1306,
            7,
            "Synthetic PreDiv Mode",
            HashMap::from([(2003u16, 0.6)]),
            vec![60_002],
        ),
    );
    // Stacking-exempt source (category_id = 6 Ship) with PostDiv (op 5, /0.6).
    sde.types.insert(
        990_103,
        make_inv_type(
            990_103,
            1306,
            6,
            "Synthetic Exempt Ship Source",
            HashMap::from([(2003u16, 0.6)]),
            vec![6801],
        ),
    );
    // Real non-exempt Confessor Propulsion Mode (category_id = 7 Module) with PostDiv (op 5, /0.6).
    sde.types.insert(
        34_323,
        make_inv_type(
            34_323,
            1306,
            7,
            "Confessor Propulsion Mode",
            HashMap::from([(2003u16, 0.6)]),
            vec![6801],
        ),
    );

    let baseline = eve_dogma::CombatBaseline {
        max_velocity: 312.5,
        mass: 1_800_000.0,
        agility: 2.0,
        signature_radius: 50.0,
        max_shield_hp: 500.0,
        max_armor_hp: 1_000.0,
        max_structure_hp: 800.0,
        max_capacitor: 600.0,
        shield_recharge_time_ms: 100_000.0,
        capacitor_recharge_time_ms: 100_000.0,
        shield_resonance: [1.0; 4],
        armor_resonance: [1.0; 4],
        structure_resonance: [1.0; 4],
        warp_speed_au_s: 4.5,
        max_locked_targets: 6,
        max_target_range: 45_000.0,
        scan_resolution: 400.0,
    };

    let run_overload = |sde_arc: &Option<Arc<SdeRegistry>>, stance_tid: u32| -> f64 {
        let mut bp = Ballpark::new(30_002_423);
        let mut ball = make_tackle_ball(ship_id, 34_317, DVec3::ZERO);
        ball.max_velocity = 312.5;
        bp.add_ball(ball);
        bp.combat_states.insert(
            ship_id,
            CombatState {
                base_max_velocity: Some(312.5),
                prop_velocity_multiplier: 1.0 + base_boost,
                ..CombatState::default()
            },
        );
        let proj = eve_dogma::FitProjection {
            rev: eve_dogma::FitRev(1),
            ship_id,
            ship_type_id: 34_317,
            active_stance_type_id: Some(stance_tid),
            last_stance_switch_ms: None,
            baseline,
            propulsion: None,
            modules: HashMap::new(),
        };
        let mut cycle = ActiveModuleCycle {
            ship_id,
            module_id: ab_id,
            module_type_id: 439,
            effect_name: "moduleBonusAfterburner".into(),
            duration_s: 7.5,
            is_overloaded: false,
            speed_boost_factor: base_boost,
            ..ActiveModuleCycle::default()
        };
        apply_overload_transition_to_cycle_with_projection(
            &mut cycle,
            true,
            &mut bp,
            sde_arc,
            Some(&proj),
        );
        cycle.speed_boost_factor / base_boost
    };

    let sde_non_stackable = Some(Arc::new(sde.clone()));
    assert!(
        (run_overload(&sde_non_stackable, 990_101) - unstacked_ovl_mult).abs() < 1e-12,
        "PreMul (op 0) must not stack-penalize against PostPercent overloadSelfSpeedBonus"
    );
    assert!(
        (run_overload(&sde_non_stackable, 990_102) - unstacked_ovl_mult).abs() < 1e-12,
        "PreDiv (op 1) must not stack-penalize against PostPercent overloadSelfSpeedBonus"
    );
    assert!(
        (run_overload(&sde_non_stackable, 990_103) - unstacked_ovl_mult).abs() < 1e-12,
        "stacking-exempt category_id=6 source must not stack-penalize against overloadSelfSpeedBonus"
    );
    assert!(
        (run_overload(&sde_non_stackable, 34_323) - stacked_ovl_mult).abs() < 1e-12,
        "non-exempt Confessor Propulsion Mode (category_id=7, PostDiv op 5) must apply exact rank-1 stacking penalty"
    );

    // If attribute 20 is marked stackable=true, even a non-exempt PostDiv stance must not stack-penalize.
    let mut sde_stackable_attr20 = sde;
    if let Some(attr20) = sde_stackable_attr20.attributes.get_mut(&20) {
        attr20.stackable = true;
    }
    let sde_stackable_opt = Some(Arc::new(sde_stackable_attr20));
    assert!(
        (run_overload(&sde_stackable_opt, 34_323) - unstacked_ovl_mult).abs() < 1e-12,
        "stackable attribute 20 must bypass stacking penalty"
    );
}

#[test]
fn drone_web_cycle_parsing_range_orphan_pruning_and_parent_unlock() {
    assert_eq!(parse_drone_web_ids("drone_web:5001:6001"), Some((5001, 6001)));
    assert_eq!(parse_drone_web_ids("siren_tackle:5001:6001"), None);
    assert_eq!(parse_drone_web_ids("drone_web:5001"), None);
    assert_eq!(parse_drone_web_ids("drone_web:abc:6001"), None);
    assert_eq!(parse_drone_web_ids("drone_web:5001:xyz"), None);
    assert_eq!(parse_drone_web_ids("drone_web:5001:6001:9"), None);

    let mut bp = Ballpark::new(30_000_142);
    let mut cycles = Vec::new();
    let mut base_scram = HashMap::new();
    let mut base_core = HashMap::new();
    let parent_id = 5001_i64;
    let drone_id = 5002_i64;
    let target_id = 5003_i64;

    let spawn_triad = |bp: &mut Ballpark,
                       cycles: &mut Vec<ActiveModuleCycle>,
                       base_scram: &mut HashMap<BallId, i32>,
                       base_core: &mut HashMap<BallId, i32>| {
        for (id, pos) in [
            (parent_id, DVec3::ZERO),
            (drone_id, DVec3::new(500.0, 0.0, 0.0)),
            (target_id, DVec3::new(1_500.0, 0.0, 0.0)),
        ] {
            let mut cs = CombatState::default();
            cs.base_max_velocity = Some(300.0);
            spawn_ball_with_combat_in_actor(
                bp,
                cycles,
                base_scram,
                base_core,
                &None,
                &None,
                None,
                make_tackle_ball(id, 587, pos),
                cs,
                None,
            );
        }
    };
    let make_web_cycle = || ActiveModuleCycle {
        ship_id: drone_id,
        module_id: -drone_id,
        effect_name: format!("drone_web:{parent_id}:9001"),
        target_id: Some(target_id),
        repeat: 1_000,
        duration_s: 5.0,
        target_velocity_multiplier: Some(0.5),
        max_effective_range_m: Some(10_000.0),
        ..ActiveModuleCycle::default()
    };

    // 1. In-range drone_web slows target; drifting beyond max_effective_range_m clears cycle & restores speed
    spawn_triad(&mut bp, &mut cycles, &mut base_scram, &mut base_core);
    cycles.push(make_web_cycle());
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert_eq!(cycles.len(), 1);
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 0.5).abs() < 1e-6);
    assert!((bp.balls[&target_id].max_velocity - 150.0).abs() < 1e-3);

    bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(25_000.0, 0.0, 0.0);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!(cycles.is_empty(), "out-of-range drone_web must be cleared");
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 1.0).abs() < 1e-6);
    assert!((bp.balls[&target_id].max_velocity - 300.0).abs() < 1e-3);

    // 2. Removing parent ship (or drone / target) prunes drone_web and restores target speed
    bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(1_500.0, 0.0, 0.0);
    cycles.push(make_web_cycle());
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 0.5).abs() < 1e-6);
    let _ = bp.remove_ball(parent_id);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!(cycles.is_empty(), "drone_web must be pruned when parent ship leaves space");
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 1.0).abs() < 1e-6);

    // 3. Removing drone ball prunes drone_web and restores target speed
    spawn_triad(&mut bp, &mut cycles, &mut base_scram, &mut base_core);
    cycles.push(make_web_cycle());
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 0.5).abs() < 1e-6);
    let _ = bp.remove_ball(drone_id);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!(cycles.is_empty(), "drone_web must be pruned when drone ball leaves space");
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 1.0).abs() < 1e-6);

    // 4. Parent unlocking target clears drone_web and restores target speed
    spawn_triad(&mut bp, &mut cycles, &mut base_scram, &mut base_core);
    cycles.push(make_web_cycle());
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &base_scram, &base_core, &None, None);
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 0.5).abs() < 1e-6);
    on_target_unlocked_in_actor(
        &mut bp,
        &mut cycles,
        &base_scram,
        &base_core,
        &None,
        parent_id,
        Some(target_id),
        None,
    );
    assert!(cycles.is_empty(), "parent unlock must clear drone_web");
    assert!((bp.combat_states[&target_id].web_velocity_multiplier - 1.0).abs() < 1e-6);
    assert!((bp.balls[&target_id].max_velocity - 300.0).abs() < 1e-3);
}
