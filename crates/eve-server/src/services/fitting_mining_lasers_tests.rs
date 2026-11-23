#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository,
};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t.effect_ids = vec![13];
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn compute_nanite_repair_params_scales_with_nanite_operation_and_interfacing_skills() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            994_001,
            custom_inv_type(sde, 994_001, 46, 7, "Test MWD", &[(1267, 10.0), (1277, 1.0)]),
        );
    });
    let mod_t = ctx.sde.types.get(&994_001);

    let (p0, r0, _) = compute_nanite_repair_params(mod_t, 0.0, 40.0, 0, 0, 0);
    assert_eq!(p0, 0);
    assert!((r0 - 10.0).abs() < 1e-9);

    let (paste_unskilled, rate_unskilled, dur_unskilled) =
        compute_nanite_repair_params(mod_t, 20.0, 40.0, 0, 0, 0);
    assert_eq!(paste_unskilled, 20);
    assert!((rate_unskilled - 10.0).abs() < 1e-9);
    assert!((dur_unskilled - 2.0).abs() < 1e-9);

    // Level 5 Nanite Operation (-25% paste cost -> 15) and Level 5 Nanite Interfacing (+100% rate -> 20 HP/s, 1s duration)
    let (paste_v, rate_v, dur_v) = compute_nanite_repair_params(mod_t, 20.0, 40.0, 5, 5, 5);
    assert_eq!(paste_v, 15);
    assert!((rate_v - 20.0).abs() < 1e-9);
    assert!((dur_v - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn deactivate_stop_overload_overload_rack_nanite_repair_and_mining_scan() {
    let ctx = test_ctx_with_custom(|sde| {
        let mut eff = sde.effects.values().next().expect("seed effect").clone();
        eff.effect_id = 65_001;
        eff.name = "overloadTestEffect".into();
        eff.effect_category = 5;
        sde.effects.insert(65_001, eff);
        let mut t = custom_inv_type(sde, 994_010, 49, 7, "Survey Scanner I", &[(40, 40.0), (197, 20_000.0), (669, 5_000.0), (1212, 1.0)]);
        t.effect_ids.push(65_001);
        sde.types.insert(994_010, t);
    });

    let char_id = 95_400_001;
    let sys_id = 30_002_505u32;
    let ship_id = 1_000_954_001;
    let mod_id = 1_000_954_002;
    let paste_id = 1_000_954_003;
    let ast_id = 1_000_954_010;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut mod_rec = InventoryItemRecord {
            item_id: mod_id,
            type_id: 994_010,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::MedSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        mod_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        mod_rec.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(mod_id, mod_rec);
        snap.skills.entry(char_id).or_default().insert(
            28164,
            eve_db::TrainedSkillRecord {
                type_id: 28164,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 200.0,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        mod_id,
        FittedModule {
            item_id: mod_id,
            type_id: 994_010,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 10.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.recalculate_stats(&ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Deactivate active module -> clears module_active_states, records 5s reactivation delay
    let deact = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Deactivate");
    assert_eq!(deact.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // 2. OverloadRack (rack 1 = med slots) & StopOverload
    let _ = ctx.drain_notifications_for_char(char_id);
    let over_rack = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "OverloadRack",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OverloadRack");
    assert_eq!(over_rack, PyRep::List(vec![PyRep::Integer(mod_id)]));
    let ov_notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        ov_notes.iter().any(|n| {
            n.event == "OnHeatAdded"
                && n.args == vec![PyRep::Integer(1176), PyRep::Integer(mod_id)]
        }),
        "expected OnHeatAdded([1176, mod_id]), got: {ov_notes:?}"
    );

    let stop_over = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "StopOverload",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StopOverload");
    assert_eq!(stop_over.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));
    let stop_notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        stop_notes.iter().any(|n| {
            n.event == "OnHeatRemoved"
                && n.args == vec![PyRep::Integer(1176), PyRep::Integer(mod_id)]
        }),
        "expected OnHeatRemoved([1176, mod_id]), got: {stop_notes:?}"
    );

    // 3. RepairModule (in space, deducts 1 initial paste) & StopRepairModule
    let rep_start = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "RepairModule",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("RepairModule");
    assert_eq!(rep_start.dict_get("pasteConsumed").and_then(|v| v.as_i64()), Some(1));

    let rep_stop = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "StopRepairModule",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StopRepairModule");
    assert_eq!(rep_stop.dict_get("stopped").and_then(|v| v.as_bool()), Some(true));

    // 4. miningScanMgr.perform_scan reveals nearby asteroid
    let scan_res = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "miningScanMgr",
        "perform_scan",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("perform_scan");
    assert_eq!(scan_res.dict_get("asteroidCount").and_then(|v| v.as_i64()), Some(1));
}

#[tokio::test]
async fn deactivate_active_armor_repairer_defers_removal_and_finishes_paid_cycle_once_890() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            994_062,
            custom_inv_type(
                sde,
                994_062,
                62,
                7,
                "Small Armor Repairer I",
                &[(6, 40.0), (73, 4_000.0), (84, 80.0)],
            ),
        );
    });
    let char_id = 97_406_201;
    let sys_id = 30_000_142;
    let ship_id = 1_004_062_001;
    let rep_id = 1_004_062_002;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            rep_id,
            InventoryItemRecord {
                item_id: rep_id,
                type_id: 994_062,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(rep_id, true);
        snap.module_active_states.insert(rep_id);
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState {
            armor_hp: 200.0,
            max_armor_hp: 1_000.0,
            capacitor: 500.0,
            max_capacitor: 500.0,
            capacitor_recharge_time_ms: 0.0,
            shield_recharge_time_ms: 0.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: rep_id,
        module_type_id: 994_062,
        effect_name: "armorRepair".into(),
        target_id: None,
        repeat: 1,
        duration_s: 4.0,
        elapsed_s: 1.0,
        cap_need_per_cycle: 40.0,
        armor_repair_amount: 80.0,
        ..Default::default()
    })
    .await
    .unwrap();

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(rep_id), PyRep::String("armorRepair".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Deactivate Armor Repairer mid-cycle");

    // Cycle remains active (repeat=0) until end of paid cycle; armor not repaired early.
    assert!(sys.has_module_cycle(ship_id, rep_id).await);
    let mid_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((mid_cs.armor_hp - 200.0).abs() < 1e-6);

    // Stepping past end of 4s cycle applies the 80 HP repair once, removes cycle, and charges 0 extra cap for cycle 2 (500 - 40 = 460).
    let _ = sys.step_manual(3.2).await.unwrap();
    assert!(!sys.has_module_cycle(ship_id, rep_id).await);
    let after_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((after_cs.armor_hp - 280.0).abs() < 1e-3, "got armor_hp={}", after_cs.armor_hp);
    assert!((after_cs.capacitor - 460.0).abs() < 1e-6, "must not charge cap for cycle 2 (got {})", after_cs.capacitor);

    // Also verify synthetic / custom-group paid end-of-cycle Hull Repairer (hull_repair_amount > 0)
    // defers removal on Deactivate via has_paid_end_of_cycle_effect() (#890).
    let hull_rep_id = 1_000_994_063;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            hull_rep_id,
            InventoryItemRecord {
                item_id: hull_rep_id,
                type_id: 3_528,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(hull_rep_id, true);
        snap.module_active_states.insert(hull_rep_id);
    });
    if let Ok(Some(mut cs)) = sys.get_combat_state(ship_id).await {
        cs.structure_hp = 100.0;
        cs.max_structure_hp = 500.0;
        let _ = sys.set_combat_state(ship_id, cs).await;
    }
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: hull_rep_id,
        module_type_id: 0, // synthetic type_id outside SDE group table -> exercises has_paid_end_of_cycle_effect()
        effect_name: "structureRepair".into(),
        target_id: None,
        repeat: 1,
        duration_s: 4.0,
        elapsed_s: 1.0,
        cap_need_per_cycle: 20.0,
        hull_repair_amount: 60.0,
        ..Default::default()
    })
    .await
    .unwrap();
    handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(hull_rep_id), PyRep::String("structureRepair".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Deactivate Hull Repairer mid-cycle");
    assert!(sys.has_module_cycle(ship_id, hull_rep_id).await);
    let _ = sys.step_manual(3.2).await.unwrap();
    assert!(!sys.has_module_cycle(ship_id, hull_rep_id).await);
    let hull_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((hull_after.structure_hp - 160.0).abs() < 1e-3, "got structure_hp={}", hull_after.structure_hp);
}

#[tokio::test]
async fn deactivate_enforces_active_space_cloak_and_ownership_guards_without_mutation() {
    use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
    use glam::DVec3;

    let miner_tid = 994_201_u32;
    let cloak_tid = 994_330_u32;
    let turret_tid = 994_053_u32;
    let mwd_tid = 994_046_u32;
    let probe_tid = 994_481_u32;
    let nullifier_tid = 994_417_u32;
    let ctx = test_ctx_with_custom(|sde| {
        let mut miner = custom_inv_type(
            sde,
            miner_tid,
            54,
            7,
            "Test Miner I",
            &[(50, 1.0), (30, 1.0), (51, 60_000.0), (77, 100.0), (54, 10_000.0), (6, 5.0)],
        );
        miner.effect_ids = vec![12, 16, 67];
        sde.types.insert(miner_tid, miner);

        let mut cloak = custom_inv_type(
            sde,
            cloak_tid,
            330,
            7,
            "Test Cloaking Device I",
            &[(50, 1.0), (30, 1.0), (669, 5_000.0)],
        );
        cloak.effect_ids = vec![12, 16];
        sde.types.insert(cloak_tid, cloak);

        let mut turret = custom_inv_type(
            sde,
            turret_tid,
            53,
            7,
            "Test Light Autocannon I",
            &[(50, 1.0), (30, 1.0), (51, 2_000.0), (54, 10_000.0), (6, 5.0)],
        );
        turret.effect_ids = vec![10, 12, 16];
        sde.types.insert(turret_tid, turret);

        let mut mwd = custom_inv_type(
            sde,
            mwd_tid,
            46,
            7,
            "5MN Test Microwarpdrive I",
            &[(50, 1.0), (30, 1.0), (6, 10.0), (73, 10_000.0), (20, 100.0), (554, 500.0)],
        );
        mwd.effect_ids = vec![13, 16, 6730];
        sde.types.insert(mwd_tid, mwd);

        let mut probe = custom_inv_type(
            sde,
            probe_tid,
            481,
            7,
            "Core Probe Launcher I",
            &[(50, 1.0), (30, 1.0), (51, 5_000.0)],
        );
        probe.effect_ids = vec![12, 16];
        sde.types.insert(probe_tid, probe);

        let mut nullifier = custom_inv_type(
            sde,
            nullifier_tid,
            4117,
            7,
            "Interdiction Nullifier I",
            &[(50, 1.0), (30, 1.0), (73, 10_000.0)],
        );
        nullifier.effect_ids = vec![12, 16];
        sde.types.insert(nullifier_tid, nullifier);

        let mut hull = sde.types.get(&587).expect("seed type").clone();
        hull.type_id = 994_587;
        hull.default_attributes.insert(14, 5.0);
        sde.types.insert(994_587, hull);
    });

    let char_id = 95_420_001_i64;
    let sys_id = 30_002_505_u32;
    let ship_id = 1_095_420_001_i64;
    let miner_id = 1_095_420_002_i64;
    let cloak_id = 1_095_420_003_i64;
    let cargo_mod_id = 1_095_420_004_i64;
    let idle_turret_id = 1_095_420_005_i64;
    let mwd_id = 1_095_420_006_i64;
    let probe_id = 1_095_420_007_i64;
    let nullifier_id = 1_095_420_008_i64;
    let ast_id = 1_095_420_010_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 994_587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (iid, tid, flag) in [
            (miner_id, miner_tid, InventoryFlag::HiSlot0),
            (cloak_id, cloak_tid, InventoryFlag::HiSlot1),
            (idle_turret_id, turret_tid, InventoryFlag::HiSlot2),
            (probe_id, probe_tid, InventoryFlag::HiSlot3),
            (nullifier_id, nullifier_tid, InventoryFlag::HiSlot4),
            (mwd_id, mwd_tid, InventoryFlag::MedSlot0),
            (cargo_mod_id, miner_tid, InventoryFlag::Cargo),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 200.0,
            },
        );
        snap.module_online_states.insert(miner_id, true);
        snap.module_online_states.insert(cloak_id, true);
        snap.module_online_states.insert(idle_turret_id, true);
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(probe_id, true);
        snap.module_online_states.insert(nullifier_id, true);
        snap.module_active_states.insert(miner_id);
        snap.module_active_states.insert(cloak_id);
        snap.module_active_states.insert(mwd_id);
        snap.module_active_states.insert(probe_id);
        snap.module_active_states.insert(nullifier_id);
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let mut fit = ShipFittingEngine::from_sde(ship_id, 994_587, &ctx.sde);
    for (iid, tid, flag, state) in [
        (miner_id, miner_tid, InventoryFlag::HiSlot0, ModuleState::Active),
        (cloak_id, cloak_tid, InventoryFlag::HiSlot1, ModuleState::Active),
        (idle_turret_id, turret_tid, InventoryFlag::HiSlot2, ModuleState::Online),
        (probe_id, probe_tid, InventoryFlag::HiSlot3, ModuleState::Active),
        (nullifier_id, nullifier_tid, InventoryFlag::HiSlot4, ModuleState::Active),
        (mwd_id, mwd_tid, InventoryFlag::MedSlot0, ModuleState::Active),
    ] {
        fit.modules.insert(
            iid,
            FittedModule {
                item_id: iid,
                type_id: tid,
                flag_id: flag as u16,
                state,
                charge_type_id: None,
                charge_quantity: 0,
                damage_hp: 0.0,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }
    fit.recalculate_stats(&ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);

    ctx.mining_cycles.write().unwrap().insert(
        miner_id,
        crate::services::mining_cycle::ActiveMiningCycle {
            char_id,
            ship_id,
            module_id: miner_id,
            target_id: ast_id,
            solar_system_id: sys_id,
            kind: crate::services::mining_cycle::MiningKind::Asteroid,
            effect_name: "miningLaser".into(),
            cycle_start_ms: 1_800_000_000_000,
            duration_ms: 60_000,
        },
    );

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.register_external_active_module_sync(ship_id, miner_id);
    let mut cs = CombatState::default();
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    cs.is_cloaked = true;
    cs.gate_cloak_remaining_ticks = 0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 994_587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 1_000_009,
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: mwd_id,
        module_type_id: mwd_tid,
        owner_char_id: Some(char_id),
        effect_name: "moduleBonusMicrowarpdrive".into(),
        duration_s: 0.1,
        repeat: 1,
        is_mwd_or_mjd: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1a. While actively cloaked, inactive non-exempt Deactivate and any Deactivate(..., "online")
    //     must reject with CannotActivateCloaked and 0 mutations.
    for args in [
        vec![PyRep::Integer(idle_turret_id)],
        vec![PyRep::Integer(miner_id), PyRep::String("online".into())],
        vec![PyRep::Integer(cloak_id), PyRep::String("online".into())],
        vec![PyRep::Integer(mwd_id), PyRep::String("online".into())],
        vec![PyRep::Integer(probe_id), PyRep::String("online".into())],
    ] {
        let err = handle_mining_and_deactivate(&ctx, &mut sess, None, "dogmaIM", "Deactivate", &args, &kw)
            .await
            .unwrap()
            .expect_err("actively cloaked inactive Deactivate / any offline must reject");
        assert_eq!(err.code, "CannotActivateCloaked");
        assert!(
            crate::services::mining_cycle::active_cycles_for_ship(&ctx, ship_id).contains_key(&miner_id),
            "rejected Deactivate must leave active mining cycle untouched"
        );
        assert!(!ctx.is_module_in_reactivation_delay(ship_id, miner_id));
        ctx.db.with_snapshot(|snap| {
            assert!(snap.module_active_states.contains(&miner_id));
            assert!(snap.module_active_states.contains(&cloak_id));
            assert!(snap.module_active_states.contains(&mwd_id));
            assert!(!snap.module_active_states.contains(&idle_turret_id));
            assert_eq!(snap.module_online_states.get(&miner_id), Some(&true));
            assert_eq!(snap.module_online_states.get(&cloak_id), Some(&true));
        });
    }

    // 1b. Non-"online" Deactivate while actively cloaked MUST succeed for modules already active
    //     before cloak (e.g. MWD+cloak trick) plus groups 481 (probe launcher) & 4117 (nullifier);
    //     re-deactivating an online inactive 481 stays idempotent while an offline 481 rejects
    //     cleanly with DogmaDeactivateFailed.
    for mod_id in [mwd_id, probe_id, nullifier_id] {
        let ok = handle_mining_and_deactivate(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Deactivate",
            &[PyRep::Integer(mod_id)],
            &kw,
        )
        .await
        .unwrap()
        .expect("active pre-cloak module / 481 / 4117 Deactivate must succeed while cloaked");
        assert_eq!(ok.dict_get("overloaded"), Some(&PyRep::Bool(false)));
        assert!(
            sys.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked,
            "stopping MWD/481/4117 must not decloak ship"
        );
    }
    let _ = sys.step_manual(0.2).await.unwrap();
    let idle_probe_ok = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(probe_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("online inactive 481 Deactivate while cloaked stays idempotent");
    assert_eq!(idle_probe_ok.dict_get("overloaded"), Some(&PyRep::Bool(false)));
    assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(probe_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let offline_probe_err = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(probe_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("offline 481 Deactivate while cloaked rejects with DogmaDeactivateFailed");
    assert_eq!(offline_probe_err.code, "DogmaDeactivateFailed");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mwd_id));
        assert!(!snap.module_active_states.contains(&probe_id));
        assert!(!snap.module_active_states.contains(&nullifier_id));
        assert!(snap.module_active_states.contains(&miner_id));
        assert_eq!(snap.module_online_states.get(&probe_id), Some(&false));
    });

    // 2. Non-"online" Deactivate on the cloak module (group 330) MUST be allowed to decloak.
    let decloak_ok = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(cloak_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("decloak via Deactivate(cloak_id) must succeed");
    assert_eq!(decloak_ok.dict_get("overloaded"), Some(&PyRep::Bool(false)));
    assert!(
        !sys.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked,
        "Deactivate(cloak_id) must clear combat_state.is_cloaked"
    );

    // 3. Unfitted cargo module fails ownership validation with 0 mining-cycle mutation.
    let cargo_err = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(cargo_mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("cargo module must fail ownership before mining_cycle::deactivate");
    assert_eq!(cargo_err.code, "ModuleNotOwnedByShip");
    assert!(crate::services::mining_cycle::active_cycles_for_ship(&ctx, ship_id).contains_key(&miner_id));

    // 4. Uncloaked Deactivate(miner) stops the mining cycle; Deactivate(miner, "online") offlines cleanly.
    let stop_ok = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(miner_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked Deactivate(miner) must succeed");
    assert_eq!(stop_ok.dict_get("active"), Some(&PyRep::Bool(false)));
    assert!(!crate::services::mining_cycle::active_cycles_for_ship(&ctx, ship_id).contains_key(&miner_id));

    let off_ok = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(miner_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked Deactivate(miner, \"online\") must succeed");
    assert_eq!(off_ok.dict_get("online"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| assert_eq!(snap.module_online_states.get(&miner_id), Some(&false)));

    // 5. Gate cloak (gate_cloak_remaining_ticks > 0) does NOT trigger active-module cloak block.
    let mut gate_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    gate_cs.is_cloaked = true;
    gate_cs.gate_cloak_remaining_ticks = 20;
    sys.set_combat_state(ship_id, gate_cs).await.unwrap();
    let gate_off_ok = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(cloak_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("gate-cloaked Deactivate(cloak_id, \"online\") must remain allowed");
    assert_eq!(gate_off_ok.dict_get("online"), Some(&PyRep::Bool(false)));
}

#[tokio::test]
async fn consolidated_active_module_lookup_reads_actor_cycle_registry() {
    let ctx = test_ctx_with_custom(|_| {});
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 88_101_i64;
    let mod_id = 88_201_i64;

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta::default(),
    )
    .await
    .unwrap();

    assert!(!is_module_currently_active_sync(
        &ctx,
        Some(sys_id),
        char_id,
        ship_id,
        mod_id
    ));

    // Stale DB `module_active_states` or cached `ShipFittingEngine` active state without an actor,
    // mining, or entosis cycle MUST return `false` (`PVE-133` single source of truth).
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 438,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        snap.module_active_states.insert(mod_id);
    });
    let mut drift_fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    drift_fit.modules.insert(mod_id, FittedModule {
        item_id: mod_id, type_id: 438, flag_id: InventoryFlag::MedSlot0 as u16, state: ModuleState::Active,
        charge_type_id: None, charge_quantity: 0, damage_hp: 0.0, max_hp: 40.0, dynamic_attribute_overrides: HashMap::new(),
    });
    ctx.store_ship_fitting(ship_id, drift_fit);
    assert!(!is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mod_id));

    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id, module_id: mod_id, owner_char_id: Some(char_id), duration_s: 10.0, repeat: 1, ..Default::default()
    }).await.unwrap();

    assert!(is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mod_id));
    assert!(!is_module_currently_active_sync(&ctx, None, char_id, ship_id, mod_id));
    assert!(!is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mod_id + 1));

    let _ = sys.remove_module_cycle(ship_id, mod_id).await.unwrap();
    assert!(!is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mod_id));

    let mining_mod_id = 88_202_i64;
    ctx.mining_cycles.write().unwrap().insert(mining_mod_id, crate::services::mining_cycle::ActiveMiningCycle {
        char_id, ship_id, module_id: mining_mod_id, target_id: 88_900, solar_system_id: sys_id,
        kind: crate::services::mining_cycle::MiningKind::Asteroid, effect_name: "miningLaser".into(),
        cycle_start_ms: 1_800_000_000_000, duration_ms: 60_000,
    });
    assert!(
        !is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mining_mod_id),
        "stale ctx.mining_cycles alone must not report active when actor external_active_modules is empty"
    );
    ctx.systems
        .register_external_active_module(sys_id, ship_id, mining_mod_id);
    assert!(is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mining_mod_id));
    assert!(ctx.systems.has_active_modules_or_cloak_sync(sys_id, ship_id));
    ctx.systems
        .unregister_external_active_module(sys_id, ship_id, mining_mod_id);
    ctx.mining_cycles.write().unwrap().remove(&mining_mod_id);
    assert!(!is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, mining_mod_id));
    assert!(!ctx.systems.has_active_modules_or_cloak_sync(sys_id, ship_id));

    let entosis_mod_id = 88_203_i64;
    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    ctx.tactical_state.write().unwrap().entosis_active_links.insert((db_key, ship_id), crate::services::tactical_modules::ActiveEntosisLinkState {
        ship_id, char_id, side_id: 500_001, module_id: entosis_mod_id, target_id: 88_901,
        cycle_index: 1, cycle_duration_s: 300.0, started_unix: 1_700_000_000, expires_unix: 1_700_000_300, is_capital: false,
    });
    assert!(
        !is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, entosis_mod_id),
        "stale entosis_active_links alone must not report active when actor external_active_modules is empty"
    );
    ctx.systems
        .register_external_active_module(sys_id, ship_id, entosis_mod_id);
    assert!(is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, entosis_mod_id));
    ctx.systems
        .unregister_external_active_module_in_loaded_systems(ship_id, entosis_mod_id);
    ctx.tactical_state.write().unwrap().entosis_active_links.remove(&(db_key, ship_id));
    assert!(!is_module_currently_active_sync(&ctx, Some(sys_id), char_id, ship_id, entosis_mod_id));
}

#[tokio::test]
async fn deactivate_mining_module_with_zero_solar_system_emits_godma_only_without_panicking() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            482,
            custom_inv_type(sde, 482, 54, 7, "Miner I", &[(77, 40.0), (50, 5.0), (73, 60_000.0)]),
        );
    });
    let char_id = 88_301_i64;
    let ship_id = 88_302_i64;
    let miner_id = 88_303_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut acc = snap.accounts.values().next().cloned().expect("seed account");
        acc.user_id = char_id as u64;
        acc.username = format!("miner_{char_id}");
        acc.banned_until_unix = None;
        snap.accounts.insert(acc.username.clone(), acc);
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = 0;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 0,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: 482,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(miner_id, true);
        snap.module_active_states.insert(miner_id);
    });
    ctx.db
        .acquire_online_lock(
            char_id as u64,
            char_id as u64,
            Some(char_id),
            true,
            ctx.now_unix_secs(),
        )
        .unwrap();
    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        miner_id,
        FittedModule {
            item_id: miner_id,
            type_id: 482,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit);

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(0),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();
    let res = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(miner_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("mining Deactivate with solar_system_id=Some(0) must succeed without panicking");
    assert_eq!(res.dict_get("active"), Some(&PyRep::Bool(false)));
    let deact_notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        deact_notes.iter().any(|n| n.event == "OnGodmaShipEffect"),
        "expected OnGodmaShipEffect on mining Deactivate with solar_system_id=0, got: {deact_notes:?}"
    );
    assert!(
        !deact_notes.iter().any(|n| n.event == "OnSpecialFX"),
        "expected zero OnSpecialFX when solar_system_id=0, got: {deact_notes:?}"
    );

    crate::services::mining_cycle::emit_mining_godma_and_special_fx(
        &ctx,
        char_id,
        ship_id,
        0,
        miner_id,
        Some(482),
        None,
        None,
        67,
        "effects.Laser",
        false,
        false,
        60_000,
    );
    let direct_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(direct_notes.len(), 1, "expected only OnGodmaShipEffect for sys_id=0");
    assert_eq!(direct_notes[0].event, "OnGodmaShipEffect");
}
