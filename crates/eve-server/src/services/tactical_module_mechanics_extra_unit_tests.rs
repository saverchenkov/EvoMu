//! Deep branch-coverage and mutation-killing unit tests for `services::tactical_module_mechanics`.

#![forbid(unsafe_code)]

use super::tactical_module_mechanics::*;
use crate::router::ServerContext;
use crate::services::tactical_modules;
use crate::system_actor::ActiveModuleCycle;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ModuleState};
use eve_proto::PyRep;
use eve_sde::{attr_ids, InvType, SdeRegistry, SolarSystem};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    capacity: f64,
    volume: f64,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.capacity = capacity;
    t.volume = volume;
    t.name = format!("Type_{type_id}");
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn make_ctx<F: FnOnce(&mut SdeRegistry)>(
    base_unix: u64,
    mutate_sde: F,
) -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[test]
fn validate_module_fitting_constraints_all_hull_groups_and_max_group_codes() {
    let (ctx, _) = make_ctx(1_775_000_000, |sde| {
        sde.types.insert(
            999_901,
            custom_type(sde, 999_901, 1672, 7, 0.0, 5.0, &[]), // Heavy Stasis Grappler
        );
        sde.types.insert(
            999_902,
            custom_type(sde, 999_902, 589, 7, 15.0, 5.0, &[]), // Interdiction Sphere Launcher (implicit max 1)
        );
        sde.types.insert(
            999_903,
            custom_type(sde, 999_903, 777, 7, 0.0, 5.0, &[(1544, 2.0)]), // Explicit maxGroupFitted = 2
        );
    });

    let grappler = ctx.sde.types.get(&999_901).unwrap().clone();
    let dictor_launcher = ctx.sde.types.get(&999_902).unwrap().clone();
    let max2_mod = ctx.sde.types.get(&999_903).unwrap().clone();

    let ship_id = 1_100_001_i64;
    // Every allowed hull group for Heavy Stasis Grappler (group 1672)
    for allowed_group in [
        27, 30, 485, 513, 547, 659, 883, 898, 900, 902, 941, 1538, 4594, 5120,
    ] {
        let hull = custom_type(&ctx.sde, 600, allowed_group, 6, 500.0, 10_000.0, &[]);
        ctx.db.with_snapshot(|snap| {
            assert!(
                validate_module_fitting_constraints(&ctx, snap, ship_id, 10, &grappler, &hull)
                    .is_ok(),
                "hull group {allowed_group} should allow Heavy Stasis Grappler"
            );
        });
    }

    let bs_hull = custom_type(&ctx.sde, 641, 27, 6, 500.0, 10_000.0, &[]);
    ctx.db.with_mut_snapshot(|snap| {
        // Fitted launcher in HiSlot0, plus another launcher in Cargo (Cargo does NOT count as fitted!)
        snap.items.insert(
            1_100_010,
            InventoryItemRecord {
                item_id: 1_100_010,
                type_id: 999_902,
                owner_id: 1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_100_011,
            InventoryItemRecord {
                item_id: 1_100_011,
                type_id: 999_903,
                owner_id: 1,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_100_012,
            InventoryItemRecord {
                item_id: 1_100_012,
                type_id: 999_903,
                owner_id: 1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo, // Not a fitted slot -> ignored!
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        // Re-validating the SAME item_id (1_100_010) does not count against itself -> Ok
        assert!(validate_module_fitting_constraints(
            &ctx,
            snap,
            ship_id,
            1_100_010,
            &dictor_launcher,
            &bs_hull
        )
        .is_ok());

        // Fitting a second launcher (new item_id 1_100_099) -> FittingValidationFailed (since group != 1672)
        let err_launcher = validate_module_fitting_constraints(
            &ctx,
            snap,
            ship_id,
            1_100_099,
            &dictor_launcher,
            &bs_hull,
        )
        .unwrap_err();
        assert_eq!(err_launcher.code, "FittingValidationFailed");

        // Second module of group 777 (maxGroupFitted = 2) -> Ok because 1_100_012 is in Cargo
        assert!(
            validate_module_fitting_constraints(&ctx, snap, ship_id, 1_100_098, &max2_mod, &bs_hull)
                .is_ok()
        );
    });
}

#[tokio::test]
async fn activate_interdiction_sphere_launcher_guards_excess_return_auto_reload_and_wubble() {
    let dictor_ship_tid = 999_910_u32;
    let launcher_tid = 999_911_u32;
    let disrupt_probe_tid = 22778_u32;
    let wubble_probe_tid = 56748_u32;
    let null_sys = 30_004_759_u32;
    let high_sys = 30_000_142_u32;
    let low_sys = 30_002_537_u32;
    let hazard_sys = 30_100_000_u32;

    let (ctx, clock) = make_ctx(1_775_000_000, |sde| {
        sde.types.insert(
            dictor_ship_tid,
            custom_type(sde, dictor_ship_tid, 541, 6, 15.0, 10_000.0, &[(38, 15.0)]), // Interdictor group 541, 15 m3 cargo
        );
        sde.types.insert(
            launcher_tid,
            custom_type(
                sde,
                launcher_tid,
                589,
                7,
                15.0,
                5.0,
                &[(51, 5_000.0), (1795, 60_000.0), (5561, 1.0)],
            ),
        );
        sde.types.insert(
            disrupt_probe_tid,
            custom_type(
                sde,
                disrupt_probe_tid,
                548,
                8,
                0.0,
                5.0,
                &[(103, 20_000.0), (281, 120_000.0), (9, 1_000.0)],
            ),
        );
        sde.types.insert(
            wubble_probe_tid,
            custom_type(
                sde,
                wubble_probe_tid,
                4088,
                8,
                0.0,
                3.0,
                &[(2279, 15_000.0), (2280, 30_000.0), (9, 800.0)],
            ),
        );
        sde.solar_systems.insert(
            null_sys,
            SolarSystem {
                solar_system_id: null_sys,
                region_id: 10_000_060,
                constellation_id: 20_000_700,
                name: "Delve Null".into(),
                security_status: -0.5,
                radius: 1e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: None,
            },
        );
        sde.solar_systems.insert(
            low_sys,
            SolarSystem {
                solar_system_id: low_sys,
                region_id: 10_000_030,
                constellation_id: 20_000_400,
                name: "Amamake Low".into(),
                security_status: 0.3,
                radius: 1e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: None,
            },
        );
    });

    let char_id = 90_000_001_i64;
    let ship_id = 1_200_001_i64;
    let mod_id = 1_200_002_i64;
    let charge_id = 1_200_003_i64;
    let mod_type = ctx.sde.types.get(&launcher_tid).unwrap().clone();
    let fitted_mod = FittedModule {
        item_id: mod_id,
        type_id: launcher_tid,
        flag_id: InventoryFlag::HiSlot0 as u16,
        state: ModuleState::Online,
        charge_type_id: Some(disrupt_probe_tid),
        charge_quantity: 1,
        damage_hp: 0.0,
        max_hp: 40.0,
        dynamic_attribute_overrides: HashMap::new(),
    };

    let sys_null = ctx.systems.get_or_spawn(null_sys);

    // 1. Hazard system, Empire (high/low-sec), non-interdictor hull, and ship not in space
    assert!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, hazard_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().message.contains("ModuleDisallowedInHazardSystem"));
    for disallow_sys in [high_sys, low_sys] {
        assert_eq!(activate_interdiction_sphere_launcher(
            &ctx, &sys_null, disallow_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
        ).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
    }
    assert_eq!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, 587, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().code, "InvalidShipGroup");
    assert_eq!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().code, "ShipNotInSpace");

    // Spawn ship ball in null_sys and verify NoProbesLoaded when launcher is empty
    sys_null.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: dictor_ship_tid, position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_500_000.0, radius: 60.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        CombatState::default(),
        SlimItemMeta::default(),
    ).await.unwrap();
    assert_eq!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().code, "NoProbesLoaded");

    // 6. Load 6 probes (3 excess = 15 m3) while cargo has only 6 m3 free -> NotEnoughCargoSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: dictor_ship_tid, owner_id: char_id, location_id: null_sys as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(charge_id, InventoryItemRecord {
            item_id: charge_id, type_id: disrupt_probe_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 6, singleton: false, ..Default::default()
        });
        snap.items.insert(1_200_004, InventoryItemRecord {
            item_id: 1_200_004, type_id: wubble_probe_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 3, singleton: false, ..Default::default()
        });
    });
    assert_eq!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().code, "NotEnoughCargoSpace");

    // 7. Set charge qty = 1 in launcher while 3 wubble probes are in Cargo -> fires & auto-reloads wubble probes
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&charge_id).unwrap().quantity = 1; });
    let launch1 = activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap();
    assert_eq!(launch1.dict_get("isWubble"), Some(&PyRep::Bool(false)));
    assert_eq!(launch1.dict_get("reloading"), Some(&PyRep::Bool(true)));
    assert_eq!(launch1.dict_get("remainingInLauncher"), Some(&PyRep::Integer(3)));

    // 8. Immediate second fire (< 5s cadence) -> ModuleReactivationDelay; after 6s fires auto-reloaded wubble probe
    assert_eq!(activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap_err().code, "ModuleReactivationDelay");
    clock.advance_secs(6);
    let launch2 = activate_interdiction_sphere_launcher(
        &ctx, &sys_null, null_sys, char_id, ship_id, dictor_ship_tid, mod_id, &fitted_mod, &mod_type, "useMissiles", None,
    ).await.unwrap();
    assert_eq!(launch2.dict_get("isWubble"), Some(&PyRep::Bool(true)));
    assert_eq!(launch2.dict_get("lifetimeSeconds"), Some(&PyRep::Real(30.0)));
    let wubble_pid = launch2.dict_get("probeID").unwrap().as_i64().unwrap();

    assert!(!is_untargetable_interdiction_probe(&ctx, &sys_null, ship_id).await);
    assert!(is_untargetable_interdiction_probe(&ctx, &sys_null, wubble_pid).await);
    assert!(!is_untargetable_interdiction_probe(&ctx, &sys_null, 999_999_999).await);

    let mut bp = Ballpark::new(null_sys);
    bp.add_warp_bubble(wubble_pid, DVec3::ZERO, 15_000.0);
    let db_opt = Some(Arc::clone(&ctx.db));
    on_probe_ball_removed_in_actor(&mut bp, &db_opt, &ctx.tactical_state, wubble_pid);
    let db_key = tactical_modules::db_ptr_key(&ctx.db);
    assert!(!ctx.tactical_state.read().unwrap().active_interdiction_probes.contains_key(&(db_key, wubble_pid)));
}

#[test]
fn multi_stack_paste_deduction_legacy_rack_heat_migration_and_mid_low_rack_propagation() {
    let (ctx, _) = make_ctx(1_775_000_000, |sde| {
        sde.types.insert(
            999_920,
            custom_type(
                sde,
                999_920,
                46,
                7,
                0.0,
                5.0,
                &[(1180, 0.20), (1211, 16.0), (40, 40.0)],
            ),
        );
    });

    let char_id = 90_000_001_i64;
    let ship_id = 1_300_001_i64;
    let mid0_id = 1_300_010_i64;
    let mid1_id = 1_300_011_i64;
    let low0_id = 1_300_020_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Deducting <= 0 or refunding <= 0 is a no-op; missing ship free cargo is 0.0
        assert_eq!(deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, 0), 0);
        assert_eq!(deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, -5), 0);
        assert_eq!(
            refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 0),
            0
        );
        assert_eq!(ship_free_cargo_m3_in_snap(snap, &ctx.sde, ship_id), 0.0);

        let mut ship_rec = InventoryItemRecord {
            item_id: ship_id,
            type_id: 587,
            owner_id: char_id,
            location_id: 30_000_142,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        // Seed legacy dynamic_attributes rack heat [1175, 1176, 1177] to test migration to transient_item_attributes
        // and set total slots = 3 + heatGenerationMultiplier (1224) = 1.0 so slot_ratio = 1.0 and heating > 0
        ship_rec.dynamic_attributes.insert(14, 1.0);
        ship_rec.dynamic_attributes.insert(13, 1.0);
        ship_rec.dynamic_attributes.insert(12, 1.0);
        ship_rec.dynamic_attributes.insert(1137, 0.0);
        ship_rec.dynamic_attributes.insert(1224, 1.0);
        ship_rec.dynamic_attributes.insert(1261, 0.5);
        ship_rec.dynamic_attributes.insert(1196, 0.0); // zero med-rack dissipation
        ship_rec.dynamic_attributes.insert(1175, 0.0); // <= 0.0 -> removed
        ship_rec.dynamic_attributes.insert(1176, 0.85); // med rack heat -> migrated
        ship_rec.dynamic_attributes.insert(1177, 1.50); // low rack heat -> clamped to 1.0
        snap.items.insert(ship_id, ship_rec);

        // Two separate Nanite Repair Paste stacks (30 + 40 = 70 units)
        for (pid, qty) in [(1_300_002, 30), (1_300_003, 40)] {
            snap.items.insert(
                pid,
                InventoryItemRecord {
                    item_id: pid,
                    type_id: 28668,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        // Deduct 50 -> empties stack 1_300_002 (30) and leaves 20 in stack 1_300_003
        assert_eq!(deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, 50), 50);
        assert!(!snap.items.contains_key(&1_300_002));
        assert_eq!(snap.items[&1_300_003].quantity, 20);
        // Drain remaining 20, then refund 15 into a brand-new stack
        assert_eq!(deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, 25), 20);
        assert_eq!(
            refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 15),
            15
        );

        // Train Thermodynamics (28164) to level 4 (-20% heat damage -> 16.0 * 0.8 = 12.8 damage per hit)
        snap.skills.entry(char_id).or_default().insert(
            28164,
            TrainedSkillRecord {
                type_id: 28164,
                level: 4,
                skill_points: 50_000.0,
            },
        );
        for (mid, flag) in [
            (mid0_id, InventoryFlag::MedSlot0),
            (mid1_id, InventoryFlag::MedSlot1),
            (low0_id, InventoryFlag::LoSlot0),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 999_920,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // Default evaluation when SDE or DB is None
    let def_eval = evaluate_ship_thermodynamics(ship_id, &None, &None);
    assert_eq!(def_eval.heat_gen_mult, 1.0);
    assert_eq!(def_eval.slot_ratio, 1.0);

    let sde_opt = Some(Arc::clone(&ctx.sde));
    let db_opt = Some(Arc::clone(&ctx.db));
    let mut ship_rack_heat = HashMap::new();

    // Non-positive or non-finite dt is a no-op
    step_rack_heat_per_tick(&mut ship_rack_heat, &[], &sde_opt, &db_opt, 0.0);
    step_rack_heat_per_tick(&mut ship_rack_heat, &[], &sde_opt, &db_opt, f64::NAN);

    // Step mid-rack overloaded cycle: migrates legacy heat (0.85 med, 1.0 low) to transient_item_attributes
    let mid_cyc = ActiveModuleCycle {
        module_id: mid0_id,
        ship_id,
        owner_char_id: Some(char_id),
        module_type_id: 999_920,
        is_overloaded: true,
        rack_idx: 1, // Med rack
        heat_damage_per_cycle: 16.0,
        duration_s: 5.0,
        ..Default::default()
    };
    step_rack_heat_per_tick(&mut ship_rack_heat, &[mid_cyc.clone()], &sde_opt, &db_opt, 1.0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items[&ship_id].dynamic_attributes.contains_key(&1176));
        assert!(snap.transient_item_attributes[&ship_id][&1176] > 0.85);
    });

    // Set med rack heat to 1.0 so slot_dist = 0 has damage_chance = 1.0 * 1.0 * 1.0 = 1.0 (100% hit)
    ship_rack_heat.insert(ship_id, [0.0, 1.0, 1.0]);
    let rng = Some(Arc::new(crate::rng::MechanicRng::seeded(1)));
    let mut burned = false;
    let mut burned_others = Vec::new();
    apply_overheat_rack_and_damage_on_cycle(
        &mid_cyc,
        &mut ship_rack_heat,
        &sde_opt,
        &db_opt,
        &rng,
        &mut burned,
        &mut burned_others,
    );
    ctx.db.with_snapshot(|snap| {
        let mid0_dmg = snap.items[&mid0_id]
            .dynamic_attributes
            .get(&attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0);
        // Thermodynamics IV reduces 16.0 heat damage by 20% to 12.8
        assert!((mid0_dmg - 12.8).abs() < 1e-6);
        // Low slot module is not damaged by a Med rack cycle
        assert!(!snap.items[&low0_id]
            .dynamic_attributes
            .contains_key(&attr_ids::DAMAGE));
    });
}
