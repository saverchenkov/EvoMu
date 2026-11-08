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
