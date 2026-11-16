use super::*;
use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, MovementMode};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[cfg(test)]
#[path = "cycles_thermodynamics_tests.rs"]
mod cycles_thermodynamics_tests;

fn make_ship(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_sde_for_cycles() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        587,
        InvType {
            type_id: 587,
            group_id: 25,
            category_id: 6,
            name: "Rifter".into(),
            mass: 1_067_000.0,
            volume: 27_000.0,
            capacity: 140.0,
            portion_size: 1,
            radius: 39.0,
            base_price: 250_000.0,
            published: true,
            market_group_id: None,
            race_id: Some(2),
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
            default_attributes: HashMap::from([(54u16, 6_000.0), (158u16, 12_000.0), (20u16, -80.0)]),
        },
    );
    Arc::new(sde)
}

#[test]
fn step_cycles_handles_range_warp_mjd_nosferatu_grappler_and_lethality() {
    let sde_opt = Some(make_sde_for_cycles());
    let mut bp = Ballpark::new(30_000_142);

    // 1001: attacker at origin
    bp.add_ball(make_ship(1001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        1001,
        CombatState {
            capacitor: 200.0,
            max_capacitor: 1_000.0,
            shield_hp: 100.0,
            max_shield_hp: 500.0,
            shield_recharge_time_ms: 200_000.0,
            armor_hp: 100.0,
            max_armor_hp: 500.0,
            structure_hp: 100.0,
            max_structure_hp: 500.0,
            ..CombatState::default()
        },
    );

    // 1002: within 10km, high capacitor (800 GJ -> Nosferatu transfers to 1001)
    bp.add_ball(make_ship(1002, 587, DVec3::new(10_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        1002,
        CombatState {
            capacitor: 800.0,
            max_capacitor: 1_000.0,
            shield_hp: 400.0,
            max_shield_hp: 500.0,
            ..CombatState::default()
        },
    );

    // 1003: far away (60km) -> out-of-range targeted module auto-deactivates
    bp.add_ball(make_ship(1003, 587, DVec3::new(60_000.0, 0.0, 0.0)));
    bp.combat_states.insert(1003, CombatState::default());

    // 1004: warping -> targeted module auto-deactivates
    let mut warp_ball = make_ship(1004, 587, DVec3::new(15_000.0, 0.0, 0.0));
    warp_ball.mode = MovementMode::new_warp(
        DVec3::new(15_000.0, 0.0, 0.0),
        DVec3::new(150_000.0, 0.0, 0.0),
        3.0,
    );
    bp.add_ball(warp_ball);
    bp.combat_states.insert(1004, CombatState::default());

    // 1005: low HP target killed by weapon cycle on 1001
    bp.add_ball(make_ship(1005, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        1005,
        CombatState {
            shield_hp: 1.0,
            max_shield_hp: 10.0,
            armor_hp: 1.0,
            max_armor_hp: 10.0,
            structure_hp: 1.0,
            max_structure_hp: 10.0,
            ..CombatState::default()
        },
    );

    let deactivated = Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
    let deact_clone = Arc::clone(&deactivated);
    let hook: ModuleDeactivationHook = Arc::new(move |_cid, cycle| {
        deact_clone.lock().unwrap().push(cycle.module_id);
    });

    let mut cycles = vec![
        // Missing source ship 9999 -> removed immediately
        ActiveModuleCycle {
            ship_id: 9999,
            owner_char_id: Some(90_000_001),
            module_id: 4001,
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
        // Out-of-range module on 1003 (max_effective_range_m = 20km, dist = 60km) -> removed
        ActiveModuleCycle {
            ship_id: 1001,
            owner_char_id: Some(90_000_001),
            module_id: 4002,
            target_id: Some(1003),
            max_effective_range_m: Some(20_000.0),
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
        // Warping target 1004 -> removed
        ActiveModuleCycle {
            ship_id: 1001,
            owner_char_id: Some(90_000_001),
            module_id: 4003,
            target_id: Some(1004),
            duration_s: 5.0,
            ..ActiveModuleCycle::default()
        },
        // Grappler + Nosferatu + Local Armor + Hull Repair on 1001 -> 1002
        ActiveModuleCycle {
            ship_id: 1001,
            owner_char_id: Some(90_000_001),
            module_id: 4004,
            module_type_id: 587,
            target_id: Some(1002),
            target_velocity_multiplier: Some(0.2),
            grappler_optimal_m: 5_000.0,
            grappler_falloff_m: 10_000.0,
            grappler_speed_factor_pct: -80.0,
            cap_transfer_target: 150.0,
            armor_repair_amount: 50.0,
            hull_repair_amount: 40.0,
            shield_boost_amount: 60.0,
            duration_s: 5.0,
            repeat: -1,
            pending_overloaded: Some(true),
            ..ActiveModuleCycle::default()
        },
        // Weapon killing 1005
        ActiveModuleCycle {
            ship_id: 1001,
            owner_char_id: Some(90_000_001),
            module_id: 4005,
            target_id: Some(1005),
            damage_vector: DamageVector::new(100.0, 100.0, 100.0, 100.0),
            duration_s: 5.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        // MJD completing spool on 1001 (jumps 100km along heading)
        ActiveModuleCycle {
            ship_id: 1001,
            owner_char_id: Some(90_000_001),
            module_id: 4006,
            mjd_jump_distance_m: Some(100_000.0),
            duration_s: 5.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
    ];

    let mut pending_actions = Vec::new();
    let destroyed = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut HashMap::new(),
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &None,
        &None,
        &Arc::new(RwLock::new(Default::default())),
        &mut pending_actions,
        5.0,
        Some(&hook),
    );

    assert_eq!(destroyed, vec![(1005, Some(90_000_001))]);
    // Local armor (100 + 50 = 150), hull (100 + 40 = 140), start-of-next-cycle overloaded shield boost (100 + 60 * 1.2 = 172)
    let cs1 = &bp.combat_states[&1001];
    assert!((cs1.armor_hp - 150.0).abs() < 1e-6);
    assert!((cs1.structure_hp - 140.0).abs() < 1e-6);
    assert!((cs1.shield_hp - 172.0).abs() < 1e-6);
    // Nosferatu transferred 150 GJ from 1002 to 1001 (200 -> 350)
    assert!((cs1.capacitor - 350.0).abs() < 1e-6);
    assert!((bp.combat_states[&1002].capacitor - 650.0).abs() < 1e-6);
    // MJD jumped ~100km from origin
    assert!(bp.balls[&1001].position.length() > 90_000.0);
    // SyncOverloadState emitted when pending_overloaded was consumed
    assert!(pending_actions
        .iter()
        .any(|a| matches!(a, PendingCycleAction::SyncOverloadState { module_id: 4004, is_overloaded: true, .. })));
    // Deactivated modules include 4001, 4002, 4003, 4005, 4006
    let deact = deactivated.lock().unwrap();
    for mid in [4001, 4002, 4003, 4005, 4006] {
        assert!(deact.contains(&mid), "expected module {mid} deactivated");
    }
}

#[tokio::test]
async fn cleanup_and_ship_cycle_removal_restore_ship_attributes_and_handle_warp() {
    let reg = crate::system_actor::SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);

    // Prepare and reconcile disconnected warp
    handle.prepare_disconnected_warp_sync(
        2001,
        587,
        40.0,
        1_200_000.0,
        2.5,
        350.0,
        4.0,
        DVec3::ZERO,
        DVec3::new(100_000.0, 0.0, 0.0),
    );
    let (_, arrived_early) =
        handle.reconcile_disconnected_warp_sync(2001, DVec3::new(100_000.0, 0.0, 0.0));
    assert!(!arrived_early);
    handle.try_set_ball_position(2001, DVec3::new(100_000.0, 0.0, 0.0));
    let (final_pos, arrived_now) =
        handle.reconcile_disconnected_warp_sync(2001, DVec3::new(100_000.0, 0.0, 0.0));
    assert!(arrived_now);
    assert!((final_pos.x - 100_000.0).abs() < 1e-3);

    // Add both a nullified cycle and a normal cycle, then verify remove_non_warp_ship_module_cycles keeps nullifier
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.combat_states.insert(
            2001,
            CombatState {
                base_max_velocity: Some(350.0),
                base_mass: Some(1_200_000.0),
                base_signature_radius: Some(39.0),
                base_shield_resonance: Some([0.9; 4]),
                base_armor_resonance: Some([0.8; 4]),
                base_structure_resonance: Some([0.7; 4]),
                ..CombatState::default()
            },
        );
        if let Some(b) = bp.balls.get_mut(&2001) {
            b.max_velocity = 1_200.0;
            b.mass = 50_000_000.0;
        }
        let mut st = handle.actor_state.write().unwrap();
        let st_mut = &mut *st;
        st_mut.active_module_cycles.push(ActiveModuleCycle {
            ship_id: 2001,
            owner_char_id: Some(90_000_001),
            module_id: 7001,
            is_mwd_or_mjd: true,
            speed_boost_factor: 2.5,
            self_mass_multiplier: Some(1.5),
            is_adc: true,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        });
        st_mut.active_module_cycles.push(ActiveModuleCycle {
            ship_id: 2001,
            owner_char_id: Some(90_000_001),
            module_id: 7002,
            grants_interdiction_nullification: true,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        });

        remove_non_warp_ship_module_cycles(
            &mut bp,
            &mut st_mut.active_module_cycles,
            &st_mut.base_scramble_strengths,
            &st_mut.base_core_strengths,
            &None,
            2001,
            None,
        );
        assert_eq!(st_mut.active_module_cycles.len(), 1);
        assert_eq!(st_mut.active_module_cycles[0].module_id, 7002);
        assert!((bp.balls[&2001].max_velocity - 350.0).abs() < 1e-6);
        assert!((bp.balls[&2001].mass - 1_200_000.0).abs() < 1e-3);

        // Now remove all remaining cycles on 2001
        let removed_all = remove_all_ship_module_cycles(
            &mut bp,
            &mut st_mut.active_module_cycles,
            &st_mut.base_scramble_strengths,
            &st_mut.base_core_strengths,
            &None,
            2001,
            None,
        );
        assert_eq!(removed_all.len(), 1);
        assert!(st_mut.active_module_cycles.is_empty());
    }

    // Step full tick + auto-stepped timestamp helpers
    handle.set_last_auto_stepped_unix(1_700_000_000);
    assert!(handle.take_auto_stepped_at(1_700_000_000));
    assert!(!handle.take_auto_stepped_at(1_700_000_000));
    assert!(!handle.has_active_modules_or_cloak_sync(2001));
    assert!(handle.try_step_full_tick(1.0) >= 1);
}

#[test]
fn repeating_weapon_cycles_apply_turret_and_missile_mitigation() {
    let mut bp = Ballpark::new(30_000_142);
    let sde_opt = Some(make_sde_for_cycles());
    let rng = Arc::new(crate::rng::MechanicRng::seeded(42));

    // Attacker at origin
    bp.add_ball(make_ship(5001, 587, DVec3::ZERO));
    bp.combat_states.insert(
        5001,
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            capacitor: 500.0,
            max_capacitor: 500.0,
            signature_radius: 40.0,
            ..CombatState::default()
        },
    );
    // Target A: stationary inside turret optimal (2 km) -> 100% hit chance
    bp.add_ball(make_ship(5002, 587, DVec3::new(2_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        5002,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            signature_radius: 100.0,
            ..CombatState::default()
        },
    );
    // Target B: extreme transversal (10,000 m/s at 1 km -> omega = 10 rad/s >> tracking 0.05) -> 0% hit chance
    let mut fast_orbiter = make_ship(5003, 587, DVec3::new(1_000.0, 0.0, 0.0));
    fast_orbiter.velocity = DVec3::new(0.0, 10_000.0, 0.0);
    bp.add_ball(fast_orbiter);
    bp.combat_states.insert(
        5003,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            signature_radius: 25.0,
            ..CombatState::default()
        },
    );
    // Target C: small fast frigate (sig 25m, speed 2,500 m/s) hit by heavy cruise missile (exp_rad 300m, exp_vel 100 m/s) -> DRF-mitigated
    let mut fast_frig = make_ship(5004, 587, DVec3::new(10_000.0, 0.0, 0.0));
    fast_frig.velocity = DVec3::new(0.0, 2_500.0, 0.0);
    bp.add_ball(fast_frig);
    bp.combat_states.insert(
        5004,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            signature_radius: 25.0,
            ..CombatState::default()
        },
    );
    // Target D: beyond missile max flight range (50 km > 20 km) -> 0 damage
    bp.add_ball(make_ship(5005, 587, DVec3::new(50_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        5005,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            signature_radius: 400.0,
            ..CombatState::default()
        },
    );

    let mut cycles = vec![
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8001,
            target_id: Some(5002),
            damage_vector: DamageVector::new(200.0, 0.0, 0.0, 0.0),
            weapon_is_turret: true,
            weapon_optimal_m: 10_000.0,
            weapon_falloff_m: 5_000.0,
            weapon_tracking_speed: 0.5,
            weapon_sig_resolution_m: 40_000.0,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8002,
            target_id: Some(5003),
            damage_vector: DamageVector::new(200.0, 0.0, 0.0, 0.0),
            weapon_is_turret: true,
            weapon_optimal_m: 5_000.0,
            weapon_falloff_m: 2_000.0,
            weapon_tracking_speed: 0.05,
            weapon_sig_resolution_m: 40_000.0,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8003,
            target_id: Some(5004),
            damage_vector: DamageVector::new(0.0, 0.0, 500.0, 0.0),
            weapon_is_missile: true,
            weapon_optimal_m: 30_000.0,
            missile_explosion_radius_m: 300.0,
            missile_explosion_velocity_ms: 100.0,
            missile_drf: 0.88,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: 5001,
            module_id: 8004,
            target_id: Some(5005),
            damage_vector: DamageVector::new(0.0, 0.0, 500.0, 0.0),
            weapon_is_missile: true,
            weapon_optimal_m: 20_000.0,
            missile_explosion_radius_m: 40.0,
            missile_explosion_velocity_ms: 200.0,
            missile_drf: 0.60,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
    ];

    let _ = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut HashMap::new(),
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &None,
        &Some(rng),
        &Arc::new(RwLock::new(Default::default())),
        &mut Vec::new(),
        1.0,
        None,
    );

    // Target A (stationary inside optimal): took >= 50% of 200 EM damage
    assert!(
        bp.combat_states[&5002].shield_hp <= 900.0,
        "Stationary target inside turret optimal must take mitigated hit damage"
    );
    // Target B (extreme angular velocity): missed cleanly (0 damage)
    assert!(
        (bp.combat_states[&5003].shield_hp - 1_000.0).abs() < 1e-9,
        "Extreme-transversal target must evade turret cycle tick"
    );
    // Target C (small fast frigate vs cruise missile): took heavily DRF-mitigated (< 10% of 500) > 0 damage
    let c_dmg_taken = 1_000.0 - bp.combat_states[&5004].shield_hp;
    assert!(
        c_dmg_taken > 0.1 && c_dmg_taken < 50.0,
        "Fast small frigate must receive DRF-mitigated missile damage, got {c_dmg_taken}"
    );
    // Target D (beyond missile max flight range): 0 damage
    assert!(
        (bp.combat_states[&5005].shield_hp - 1_000.0).abs() < 1e-9,
        "Target beyond missile flight range must take 0 damage"
    );
}

#[test]
fn w3_mutant_killers_crystal_volatility_infinite_crystal_and_cargo_autoreload() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = (*make_sde_for_cycles()).clone();
    let mut launcher = sde.types[&587].clone();
    launcher.type_id = 995_001;
    launcher.capacity = 0.05; // 20 rounds of 0.0025 m3 ammo
    launcher.default_attributes.insert(1795, 5_000.0); // 5s reload
    sde.types.insert(995_001, launcher);

    let mut ammo = sde.types[&587].clone();
    ammo.type_id = 995_002;
    ammo.volume = 0.0025;
    sde.types.insert(995_002, ammo);
    let sde_opt = Some(Arc::new(sde));
    let db_opt = Some(Arc::clone(&db));

    let mut bp = Ballpark::new(30_000_142);
    let ship_id = 5101_i64;
    let tgt_id = 5102_i64;
    bp.add_ball(make_ship(ship_id, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            capacitor: 1_000.0,
            max_capacitor: 1_000.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_ship(tgt_id, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        tgt_id,
        CombatState {
            shield_hp: 10_000.0,
            max_shield_hp: 10_000.0,
            ..CombatState::default()
        },
    );

    let mod_inf_crystal = 6101_i64;
    let chg_inf_crystal = 6102_i64;
    let mod_vol_crystal = 6103_i64;
    let chg_vol_crystal = 6104_i64;
    let mod_proj = 6105_i64;
    let chg_proj = 6106_i64;
    let cargo_ammo = 6107_i64;

    db.with_mut_snapshot(|snap| {
        for (mid, flag) in [
            (mod_inf_crystal, eve_db::InventoryFlag::HiSlot0),
            (mod_vol_crystal, eve_db::InventoryFlag::HiSlot1),
            (mod_proj, eve_db::InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: 995_001,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // 1. T1 frequency crystal (crystal_volatility_damage < 0.0): never consumes quantity
        snap.items.insert(
            chg_inf_crystal,
            eve_db::InventoryItemRecord {
                item_id: chg_inf_crystal,
                type_id: 995_002,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // 2. Volatile crystal stack of 2 with 0.60 damage already + 0.50 volatility damage -> shatters 1 unit, resets DAMAGE to 0.0, keeps qty 1
        snap.items.insert(
            chg_vol_crystal,
            eve_db::InventoryItemRecord {
                item_id: chg_vol_crystal,
                type_id: 995_002,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot1,
                quantity: 2,
                singleton: false,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::DAMAGE, 0.60)]),
                version: 1,
                ..Default::default()
            },
        );
        // 3. Projectile ammo with 1 round left + 50 rounds in Cargo -> consumes last round and auto-reloads 20 rounds from Cargo
        snap.items.insert(
            chg_proj,
            eve_db::InventoryItemRecord {
                item_id: chg_proj,
                type_id: 995_002,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_ammo,
            eve_db::InventoryItemRecord {
                item_id: cargo_ammo,
                type_id: 995_002,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut cycles = vec![
        ActiveModuleCycle {
            ship_id,
            module_id: mod_inf_crystal,
            module_type_id: 995_001,
            linked_turrets: vec![mod_inf_crystal],
            target_id: Some(tgt_id),
            ammo_type_id: Some(995_002),
            crystal_volatility_damage: -1.0,
            damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
            duration_s: 1.0,
            repeat: 10,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id,
            module_id: mod_vol_crystal,
            module_type_id: 995_001,
            linked_turrets: vec![mod_vol_crystal],
            target_id: Some(tgt_id),
            ammo_type_id: Some(995_002),
            crystal_volatility_chance: 1.0,
            crystal_volatility_damage: 0.50,
            damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
            duration_s: 1.0,
            repeat: 10,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id,
            module_id: mod_proj,
            module_type_id: 995_001,
            linked_turrets: vec![mod_proj],
            target_id: Some(tgt_id),
            ammo_type_id: Some(995_002),
            crystal_volatility_damage: 0.0,
            damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
            duration_s: 1.0,
            repeat: 10,
            ..ActiveModuleCycle::default()
        },
    ];

    step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut HashMap::new(),
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        &None,
        &Arc::new(RwLock::new(Default::default())),
        &mut Vec::new(),
        1.0,
        None,
    );

    db.with_snapshot(|snap| {
        assert_eq!(snap.items[&chg_inf_crystal].quantity, 1);
        assert_eq!(snap.items[&chg_vol_crystal].quantity, 1);
        assert_eq!(
            snap.items[&chg_vol_crystal]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied(),
            Some(0.0)
        );
        // Cargo stack had 20 rounds moved into HiSlot2 (50 -> 30)
        assert_eq!(snap.items[&cargo_ammo].quantity, 30);
    });
}

#[test]
fn resolve_ball_pilot_char_id_covers_slim_meta_active_ship_item_owner_and_non_positive_filter() {
    let mut bp = Ballpark::new(30_000_142);
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    let active_ship = db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    // 1. Direct ball_slim_meta char_id > 0
    bp.ball_slim_meta.insert(
        500_001,
        eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            ..Default::default()
        },
    );
    assert_eq!(
        super::resolve_ball_pilot_char_id(&bp, Some(&db), 500_001),
        Some(char_id)
    );

    // 2. Non-positive char_id (0 or -1) is rejected by filter(|&c| c > 0)
    bp.ball_slim_meta.insert(
        500_002,
        eve_destiny::SlimItemMeta {
            char_id: Some(0),
            ..Default::default()
        },
    );
    assert_eq!(
        super::resolve_ball_pilot_char_id(&bp, Some(&db), 500_002),
        None
    );

    // 3. Fallback to character with active_ship_id == ball_id
    assert_eq!(
        super::resolve_ball_pilot_char_id(&bp, Some(&db), active_ship),
        Some(char_id)
    );

    // 4. Fallback to item.owner_id when owner_id is in snap.characters vs non-character owner
    let char_owned_ball = 500_003_i64;
    let corp_owned_ball = 500_004_i64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            char_owned_ball,
            eve_db::InventoryItemRecord {
                item_id: char_owned_ball,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_owned_ball,
            eve_db::InventoryItemRecord {
                item_id: corp_owned_ball,
                type_id: 587,
                owner_id: 98_000_001,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        super::resolve_ball_pilot_char_id(&bp, Some(&db), char_owned_ball),
        Some(char_id)
    );
    assert_eq!(
        super::resolve_ball_pilot_char_id(&bp, Some(&db), corp_owned_ball),
        None
    );
}

#[test]
fn tick_capacitor_deltas_emit_attr_18_and_repair_unhydrated_cap() {
    let mut bp = Ballpark::new(30_000_142);
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    let ship_id = db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    bp.add_ball(make_ship(ship_id, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            capacitor: 250.0,
            max_capacitor: 250.0,
            capacitor_recharge_time_ms: 100_000.0,
            ..Default::default()
        },
    );

    // 1. Unhydrated (250.0, 250.0) CombatState repairs cleanly to effective_max_capacitor (400.0).
    let mut active_cycles = Vec::new();
    let cycle = ActiveModuleCycle {
        ship_id,
        owner_char_id: Some(char_id),
        module_id: 901_001,
        module_type_id: 484,
        target_id: None,
        effect_name: "shieldBoosting".to_string(),
        duration_s: 4.0,
        elapsed_s: 0.0,
        repeat: 1,
        cap_need_per_cycle: 100.0,
        effective_max_capacitor: Some(400.0),
        ..Default::default()
    };
    let zero_i32_map: HashMap<BallId, i32> = HashMap::new();
    crate::system_actor::cycles_tackle::handle_add_module_cycle(
        &mut bp,
        &mut active_cycles,
        &zero_i32_map,
        &zero_i32_map,
        &None,
        &Some(db.clone()),
        None,
        cycle,
    )
    .expect("repaired cap 400 >= 100 GJ");
    assert_eq!(bp.combat_states[&ship_id].max_capacitor, 400.0);
    assert_eq!(bp.combat_states[&ship_id].capacitor, 300.0);

    // 2. Passive recharge across bp.step(1.0) emits PendingCycleAction::ShipCapacitorChanged ->
    //    flush_pending_cycle_damage_notifications enqueues both OnCapacitorChange & OnModuleAttributeChanges (attr 18).
    let pre = super::snapshot_player_ship_capacitors(&bp, Some(&db));
    let _ = bp.step(1.0);
    let mut pending_actions = Vec::new();
    super::record_player_ship_capacitor_deltas(&bp, &pre, &mut pending_actions);
    assert!(pending_actions.iter().any(|a| matches!(
        a,
        PendingCycleAction::ShipCapacitorChanged {
            char_id: c,
            ship_id: s,
            new_cap,
            old_cap,
            notified: false,
            ..
        } if *c == char_id && *s == ship_id && *new_cap > *old_cap
    )));

    let q_p: RwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>> =
        RwLock::new(HashMap::new());
    flush_pending_cycle_damage_notifications(Some(&db), Some(&q_p), None, &mut pending_actions);
    assert!(matches!(
        pending_actions.first(),
        Some(PendingCycleAction::ShipCapacitorChanged { notified: true, .. })
    ));
    let notifs = &q_p.read().unwrap()[&char_id];
    assert!(notifs.iter().any(|n| n.event == "OnCapacitorChange"));
    assert!(notifs.iter().any(|n| {
        n.event == "OnModuleAttributeChanges"
            && matches!(n.args.first(), Some(eve_proto::PyRep::List(entries)) if entries.iter().any(|e| {
                matches!(e, eve_proto::PyRep::Tuple(t) if t.len() >= 6 && t[2].as_i64() == Some(ship_id) && t[3].as_i64() == Some(18))
            }))
    }));
}

#[test]
fn cap_telemetry_coalesces_and_evicts_before_state_notifications() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    let ship_id = db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let q_p: RwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>> =
        RwLock::new(HashMap::new());
    let now = 133_500_000_000_000_000_i64;

    enqueue_cycle_char_notification(
        Some(&db),
        &q_p,
        None,
        now,
        char_id,
        "OnSovereigntyChanged",
        vec![eve_proto::PyRep::Integer(30_000_142), eve_proto::PyRep::None],
    );
    enqueue_cycle_char_notification(
        Some(&db),
        &q_p,
        None,
        now,
        char_id,
        "OnSolarSystemSovStructuresUpdated",
        vec![
            eve_proto::PyRep::Integer(30_000_142),
            eve_proto::PyRep::List(vec![]),
            eve_proto::PyRep::Dict(vec![]),
        ],
    );
    // Unrelated non-capacitor OnModuleAttributeChanges (attr 9) must not be coalesced with attr 18.
    enqueue_cycle_char_notification(
        Some(&db),
        &q_p,
        None,
        now,
        char_id,
        "OnModuleAttributeChanges",
        vec![eve_proto::PyRep::List(vec![eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::String("OnModuleAttributeChange".into()),
            eve_proto::PyRep::Integer(char_id),
            eve_proto::PyRep::Integer(ship_id),
            eve_proto::PyRep::Integer(9),
            eve_proto::PyRep::Integer(now),
            eve_proto::PyRep::Real(100.0),
            eve_proto::PyRep::Real(90.0),
            eve_proto::PyRep::None,
        ])])],
    );

    // 1. 150 passive recharge ticks for one ship coalesce to the latest cap pair without evicting sov notices.
    for tick in 1..=150_u32 {
        let mut actions = vec![PendingCycleAction::ShipCapacitorChanged {
            char_id,
            ship_id,
            new_cap: 250.0 + f64::from(tick),
            max_cap: 1_500.0,
            old_cap: 249.0 + f64::from(tick),
            notified: false,
        }];
        flush_pending_cycle_damage_notifications(Some(&db), Some(&q_p), None, &mut actions);
    }
    {
        let q = &q_p.read().unwrap()[&char_id];
        assert_eq!(q.len(), 5, "expected 3 non-telemetry notices + latest cap pair");
        assert!(q.iter().any(|n| n.event == "OnSovereigntyChanged"));
        assert!(q.iter().any(|n| n.event == "OnSolarSystemSovStructuresUpdated"));
        let latest_cap = q
            .iter()
            .find(|n| n.event == "OnCapacitorChange")
            .and_then(|n| n.args.get(1))
            .and_then(eve_proto::PyRep::as_f64);
        assert_eq!(latest_cap, Some(400.0));
    }

    // 2. Overflowing MAX_PENDING_NOTIFICATIONS_PER_CHAR across many distinct ships evicts old cap
    //    telemetry first rather than dropping earlier state notifications.
    for extra_ship in 1..=crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR as i64 {
        let mut actions = vec![PendingCycleAction::ShipCapacitorChanged {
            char_id,
            ship_id: ship_id + extra_ship,
            new_cap: 300.0,
            max_cap: 1_000.0,
            old_cap: 250.0,
            notified: false,
        }];
        flush_pending_cycle_damage_notifications(Some(&db), Some(&q_p), None, &mut actions);
    }
    let q = &q_p.read().unwrap()[&char_id];
    assert_eq!(q.len(), crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(q.iter().any(|n| n.event == "OnSovereigntyChanged"));
    assert!(q.iter().any(|n| n.event == "OnSolarSystemSovStructuresUpdated"));
}

#[tokio::test]
async fn ecm_jam_and_burst_clear_target_bound_cycles_and_fit_projection_syncs_turret_on_cycle_boundary() {
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_143);
    let attacker: BallId = 800_001;
    let target: BallId = 800_002;
    let mod_id: i64 = 900_001;

    handle
        .spawn_ball_with_combat(
            make_ship(attacker, 587, DVec3::ZERO),
            CombatState {
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                capacitor_recharge_time_ms: 0.0,
                shield_recharge_time_ms: 0.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    handle
        .spawn_ball_with_combat(
            make_ship(target, 587, DVec3::new(1_000.0, 0.0, 0.0)),
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                signature_radius: 100.0,
                shield_recharge_time_ms: 0.0,
                capacitor_recharge_time_ms: 0.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mk_turret_cycle = |elapsed_s: f64| ActiveModuleCycle {
        ship_id: attacker,
        module_id: mod_id,
        module_type_id: 565,
        effect_name: "targetAttack".into(),
        target_id: Some(target),
        repeat: 1,
        duration_s: 2.0,
        elapsed_s,
        cap_need_per_cycle: 0.0,
        damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0),
        weapon_optimal_m: 5_000.0,
        weapon_falloff_m: 2_000.0,
        weapon_tracking_speed: 0.25,
        weapon_sig_resolution_m: 40.0,
        ..Default::default()
    };

    // 1. ApplyEcmJam clears active target-bound cycles (`#859`).
    handle.add_module_cycle(mk_turret_cycle(0.0)).await.unwrap();
    assert!(handle.has_module_cycle(attacker, mod_id).await);
    assert!(handle.apply_ecm_jam(attacker, 5.0).await.unwrap());
    assert!(
        !handle.has_module_cycle(attacker, mod_id).await,
        "ApplyEcmJam must clear active target-bound cycles on jammed attacker"
    );

    // Clear jam by spawning fresh combat state, then verify ApplyEcmBurst also clears target-bound cycles (`#859`).
    handle
        .set_combat_state(
            attacker,
            CombatState {
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                capacitor_recharge_time_ms: 0.0,
                shield_recharge_time_ms: 0.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    handle.add_module_cycle(mk_turret_cycle(0.0)).await.unwrap();
    let jammed = handle
        .apply_ecm_burst(target, 20_000.0, [200.0; 4], 5.0, 0.0)
        .await
        .unwrap();
    assert!(jammed.contains(&attacker), "ApplyEcmBurst must jam nearby attacker");
    assert!(
        !handle.has_module_cycle(attacker, mod_id).await,
        "ApplyEcmBurst must clear active target-bound cycles on jammed ships"
    );

    // 2. Mid-fight FitProjection push updates active turret params on next cycle boundary (`#859`).
    let mut cycle = mk_turret_cycle(1.5);
    let mut modules = HashMap::new();
    modules.insert(
        mod_id,
        eve_dogma::CycleParams {
            module_id: mod_id,
            module_type_id: 565,
            flag_id: 27,
            state: eve_dogma::ModuleState::Active,
            is_overloaded: false,
            charge_type_id: None,
            duration_ms: 1_500.0,
            capacitor_need: 0.0,
            is_turret: true,
            is_missile: false,
            damage: [250.0, 0.0, 0.0, 0.0],
            damage_multiplier: 1.0,
            optimal_range_m: 12_000.0,
            falloff_m: 4_000.0,
            tracking_speed: 0.45,
            sig_resolution_m: 40.0,
            missile_explosion_radius_m: 0.0,
            missile_explosion_velocity_ms: 0.0,
            missile_drf: 0.0,
            missile_flight_time_ms: 0.0,
            missile_max_velocity_ms: 0.0,
            shield_boost_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            speed_factor: 0.0,
            speed_boost_factor: 0.0,
            mass_addition: 0.0,
            sig_radius_bonus_pct: 0.0,
            prop_speed_boost: 0.0,
            is_mwd: false,
            is_afterburner: false,
            is_cloak: false,
            cloak_velocity_bonus_pct: 0.0,
            is_hic_wdfg: false,
        },
    );
    let proj = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(2),
        ship_id: attacker,
        ship_type_id: 587,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline: eve_dogma::CombatBaseline {
            max_velocity: 350.0,
            mass: 1_500_000.0,
            agility: 2.5,
            signature_radius: 40.0,
            max_shield_hp: 500.0,
            max_armor_hp: 500.0,
            max_structure_hp: 400.0,
            max_capacitor: 1_000.0,
            shield_recharge_time_ms: 100_000.0,
            capacitor_recharge_time_ms: 100_000.0,
            shield_resonance: [1.0; 4],
            armor_resonance: [1.0; 4],
            structure_resonance: [1.0; 4],
            warp_speed_au_s: 5.0,
            max_locked_targets: 4,
            max_target_range: 25_000.0,
            scan_resolution: 500.0,
        },
        propulsion: None,
        modules,
    };
    let mut projections = HashMap::new();
    projections.insert(attacker, proj.clone());
    sync_cycle_from_fit_projection(&mut cycle, &projections);
    assert!((cycle.duration_s - 1.5).abs() < 1e-6);
    assert!((cycle.weapon_optimal_m - 12_000.0).abs() < 1e-6);
    assert!((cycle.weapon_tracking_speed - 0.45).abs() < 1e-6);
    assert!((cycle.damage_vector.em - 250.0).abs() < 1e-6);

    // End-to-end actor step crosses cycle 1 boundary (100 EM), syncs projected 250 EM / 1.5s, and completes cycle 2.
    handle.add_module_cycle(mk_turret_cycle(1.5)).await.unwrap();
    assert!(handle.apply_fit_projection(attacker, proj).await);
    let _ = handle.step_manual(1.0).await.unwrap();
    let _ = handle.step_manual(1.6).await.unwrap();
    let target_cs = handle.get_combat_state(target).await.unwrap().unwrap();
    assert!(
        target_cs.shield_hp < 700.0,
        "Crossing cycle boundary with active FitProjection must apply updated 250 EM volley on cycle 2 (got shield={})",
        target_cs.shield_hp
    );
}

#[tokio::test]
async fn fit_projection_sync_updates_mwd_rep_cloak_and_preserves_hic_wdfg_fields() {
    let ship_id: BallId = 800_101;
    let mwd_id: i64 = 900_101;
    let sb_id: i64 = 900_102;
    let rsb_id: i64 = 900_103;
    let ar_id: i64 = 900_104;
    let aar_id: i64 = 900_105;
    let hr_id: i64 = 900_106;
    let cloak_id: i64 = 900_107;
    let wdfg_id: i64 = 900_108;

    let mk_cp = |module_id: i64, module_type_id: u32| eve_dogma::CycleParams {
        module_id,
        module_type_id,
        flag_id: 19,
        state: eve_dogma::ModuleState::Active,
        is_overloaded: false,
        charge_type_id: None,
        duration_ms: 5_000.0,
        capacitor_need: 20.0,
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
        speed_factor: 0.0,
        speed_boost_factor: 0.0,
        mass_addition: 0.0,
        sig_radius_bonus_pct: 0.0,
        prop_speed_boost: 0.0,
        is_mwd: false,
        is_afterburner: false,
        is_cloak: false,
        cloak_velocity_bonus_pct: 0.0,
        is_hic_wdfg: false,
    };

    let mut modules = HashMap::new();

    let mut cp_mwd = mk_cp(mwd_id, 434);
    cp_mwd.duration_ms = 2_000.0;
    cp_mwd.capacitor_need = 12.0;
    cp_mwd.is_mwd = true;
    cp_mwd.sig_radius_bonus_pct = 375.0;
    cp_mwd.mass_addition = 420_000.0;
    cp_mwd.prop_speed_boost = 6.2;
    modules.insert(mwd_id, cp_mwd);

    let mut cp_sb = mk_cp(sb_id, 399);
    cp_sb.shield_boost_amount = 145.0;
    modules.insert(sb_id, cp_sb);

    let mut cp_rsb = mk_cp(rsb_id, 3608);
    cp_rsb.shield_boost_amount = 999.0;
    modules.insert(rsb_id, cp_rsb);

    let mut cp_ar = mk_cp(ar_id, 3530);
    cp_ar.armor_repair_amount = 115.0;
    modules.insert(ar_id, cp_ar);

    let mut cp_aar = mk_cp(aar_id, 33076);
    cp_aar.armor_repair_amount = 80.0;
    modules.insert(aar_id, cp_aar);

    let mut cp_hr = mk_cp(hr_id, 3532);
    cp_hr.hull_repair_amount = 75.0;
    modules.insert(hr_id, cp_hr);

    let mut cp_cloak = mk_cp(cloak_id, 11_370);
    cp_cloak.is_cloak = true;
    cp_cloak.cloak_velocity_bonus_pct = -0.60;
    modules.insert(cloak_id, cp_cloak);

    let mut cp_wdfg = mk_cp(wdfg_id, 27_951);
    cp_wdfg.is_hic_wdfg = true;
    modules.insert(wdfg_id, cp_wdfg);

    let proj = eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(4),
        ship_id,
        ship_type_id: 587,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline: eve_dogma::CombatBaseline {
            max_velocity: 350.0,
            mass: 1_500_000.0,
            agility: 2.5,
            signature_radius: 40.0,
            max_shield_hp: 500.0,
            max_armor_hp: 500.0,
            max_structure_hp: 400.0,
            max_capacitor: 1_000.0,
            shield_recharge_time_ms: 100_000.0,
            capacitor_recharge_time_ms: 100_000.0,
            shield_resonance: [1.0; 4],
            armor_resonance: [1.0; 4],
            structure_resonance: [1.0; 4],
            warp_speed_au_s: 5.0,
            max_locked_targets: 4,
            max_target_range: 25_000.0,
            scan_resolution: 500.0,
        },
        propulsion: None,
        modules,
    };
    let projections = HashMap::from([(ship_id, proj.clone())]);

    // 1. Local shield booster vs remote shield booster (remote must keep shield_boost_amount == 0.0).
    let mut sb_cycle = ActiveModuleCycle {
        ship_id,
        module_id: sb_id,
        module_type_id: 399,
        duration_s: 4.0,
        shield_boost_amount: 100.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut sb_cycle, &projections);
    assert!((sb_cycle.shield_boost_amount - 145.0).abs() < 1e-6);

    let mut rsb_cycle = ActiveModuleCycle {
        ship_id,
        module_id: rsb_id,
        module_type_id: 3608,
        duration_s: 4.0,
        shield_boost_amount: 0.0,
        remote_shield_amount: 120.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut rsb_cycle, &projections);
    assert_eq!(rsb_cycle.shield_boost_amount, 0.0);

    // 2. Normal armor repairer updates vs AAR paste-boosted amount preserved.
    let mut ar_cycle = ActiveModuleCycle {
        ship_id,
        module_id: ar_id,
        module_type_id: 3530,
        duration_s: 4.0,
        armor_repair_amount: 80.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut ar_cycle, &projections);
    assert!((ar_cycle.armor_repair_amount - 115.0).abs() < 1e-6);

    let mut aar_cycle = ActiveModuleCycle {
        ship_id,
        module_id: aar_id,
        module_type_id: 33076,
        duration_s: 4.0,
        is_aar: true,
        armor_repair_amount: 240.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut aar_cycle, &projections);
    assert!((aar_cycle.armor_repair_amount - 240.0).abs() < 1e-6);

    // 3. Hull repairer and non-covert cloak velocity modifier.
    let mut hr_cycle = ActiveModuleCycle {
        ship_id,
        module_id: hr_id,
        module_type_id: 3532,
        duration_s: 4.0,
        hull_repair_amount: 50.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut hr_cycle, &projections);
    assert!((hr_cycle.hull_repair_amount - 75.0).abs() < 1e-6);

    let mut cloak_cycle = ActiveModuleCycle {
        ship_id,
        module_id: cloak_id,
        module_type_id: 11_370,
        effect_name: "cloakingPrototype".into(),
        duration_s: 5.0,
        speed_boost_factor: -0.75,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut cloak_cycle, &projections);
    assert!((cloak_cycle.speed_boost_factor - (-0.60)).abs() < 1e-6);

    // 4. Negative isolation: HIC WDFG hic_sig_multiplier / hic_mass_addition must NOT be overwritten,
    //    and overload mismatch must skip sync.
    let mut wdfg_cycle = ActiveModuleCycle {
        ship_id,
        module_id: wdfg_id,
        module_type_id: 27_951,
        duration_s: 5.0,
        hic_sig_multiplier: Some(2.0),
        hic_mass_addition: Some(1_000_000.0),
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut wdfg_cycle, &projections);
    assert_eq!(wdfg_cycle.hic_sig_multiplier, Some(2.0));
    assert_eq!(wdfg_cycle.hic_mass_addition, Some(1_000_000.0));

    let mut ovl_mwd_cycle = ActiveModuleCycle {
        ship_id,
        module_id: mwd_id,
        module_type_id: 434,
        is_overloaded: true,
        duration_s: 1.5,
        hic_sig_multiplier: Some(6.0),
        hic_mass_addition: Some(500_000.0),
        speed_boost_factor: 5.0,
        ..Default::default()
    };
    sync_cycle_from_fit_projection(&mut ovl_mwd_cycle, &projections);
    assert_eq!(ovl_mwd_cycle.hic_sig_multiplier, Some(6.0));
    assert_eq!(ovl_mwd_cycle.hic_mass_addition, Some(500_000.0));
    assert!((ovl_mwd_cycle.speed_boost_factor - 5.0).abs() < 1e-6);

    // 5. End-to-end actor step: MWD crosses cycle boundary with FitProjection, updates
    //    hic_sig_multiplier / hic_mass_addition / speed_boost_factor, and reconciles Ball + CombatState.
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_144);
    handle
        .spawn_ball_with_combat(
            make_ship(ship_id, 587, DVec3::ZERO),
            CombatState {
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                signature_radius: 40.0,
                base_signature_radius: Some(40.0),
                base_max_velocity: Some(350.0),
                base_mass: Some(1_500_000.0),
                base_inertia: Some(2.5),
                shield_recharge_time_ms: 100_000.0,
                capacitor_recharge_time_ms: 100_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: mwd_id,
            module_type_id: 434,
            effect_name: "moduleBonusMicrowarpdrive".into(),
            repeat: 1,
            duration_s: 1.5,
            cap_need_per_cycle: 10.0,
            speed_boost_factor: 5.0,
            hic_sig_multiplier: Some(6.0),
            hic_mass_addition: Some(500_000.0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(handle.apply_fit_projection(ship_id, proj).await);

    let _ = handle.step_manual(0.1).await.unwrap();
    let cs_mid = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_mid.module_sig_multiplier - 6.0).abs() < 1e-6);

    let _ = handle.step_manual(1.6).await.unwrap();
    let cs_after = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_after) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        (cs_after.module_sig_multiplier - 4.75).abs() < 1e-6,
        "MWD cycle boundary must sync hic_sig_multiplier to 4.75 (got {})",
        cs_after.module_sig_multiplier
    );
    assert!(
        (cs_after.module_mass_addition - 420_000.0).abs() < 1e-3,
        "MWD cycle boundary must sync hic_mass_addition to 420_000 (got {})",
        cs_after.module_mass_addition
    );
    assert!(
        (cs_after.signature_radius - 190.0).abs() < 1e-6,
        "reconciled signature radius must reflect 4.75x bloom (got {})",
        cs_after.signature_radius
    );
    assert!(
        (ball_after.mass - 1_920_000.0).abs() < 1e-3,
        "reconciled ball mass must reflect 420_000 added mass (got {})",
        ball_after.mass
    );
    assert!(
        (ball_after.max_velocity - 2_520.0).abs() < 1e-3,
        "reconciled ball max_velocity must reflect 7.2x boost (got {})",
        ball_after.max_velocity
    );
}
