//! Deep branch-coverage and mutation-killing unit tests for `fitting_mining_lasers`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_mining_lasers::{
    compute_nanite_repair_params, handle_mining_and_deactivate,
};
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType, SdeRegistry};
use glam::DVec3;
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
    effects: &[u16],
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
    t.effect_ids = effects.to_vec();
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

async fn call_dogma(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle_mining_and_deactivate(ctx, sess, None, "dogmaIM", method, args, &kw)
        .await
        .expect("dogmaIM method handled")
}

async fn call_scan(
    ctx: &ServerContext,
    sess: &mut SessionState,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_mining_and_deactivate(ctx, sess, None, "miningScanMgr", "perform_scan", args, kw).await
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    dmg: f64,
) -> InventoryItemRecord {
    let mut rec = InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: true,
        ..Default::default()
    };
    if dmg > 0.0 {
        rec.dynamic_attributes.insert(attr_ids::DAMAGE, dmg);
    }
    rec.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
    rec
}

#[test]
fn compute_nanite_repair_params_boundary_and_fallback_branches() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_001,
            custom_inv_type(sde, 995_001, 46, 7, "ZeroAttrMod", &[(1267, 0.0), (1277, -2.0)], &[]),
        );
        sde.types.insert(
            995_002,
            custom_inv_type(sde, 995_002, 46, 7, "CustomRateMod", &[(1267, 25.0), (1277, 1.5)], &[]),
        );
    });

    let (p_neg, r_neg, d_neg) = compute_nanite_repair_params(None, -5.0, 40.0, 0, 0, 0);
    assert_eq!(p_neg, 0);
    assert!((r_neg - 10.0).abs() < 1e-9);
    assert!((d_neg - 1.0).abs() < 1e-9);

    let zero_mod = ctx.sde.types.get(&995_001);
    let (p_fb, r_fb, d_fb) = compute_nanite_repair_params(zero_mod, 30.0, 40.0, 0, 0, 0);
    assert_eq!(p_fb, 15);
    assert!((r_fb - 10.0).abs() < 1e-9);
    assert!((d_fb - 3.0).abs() < 1e-9);

    let (p_tiny, _, d_tiny) = compute_nanite_repair_params(None, 0.01, 40.0, 0, 5, 5);
    assert_eq!(p_tiny, 1);
    assert!((d_tiny - 1.0).abs() < 1e-9);

    let custom_mod = ctx.sde.types.get(&995_002);
    let (p_clamp, r_clamp, d_clamp) =
        compute_nanite_repair_params(custom_mod, 100.0, 120.0, 5, 10, 10);
    assert_eq!(p_clamp, 113);
    assert!((r_clamp - 50.0).abs() < 1e-9);
    assert!((d_clamp - 2.0).abs() < 1e-9);
}

#[tokio::test]
async fn deactivate_error_guards_entosis_mining_cycle_gated_and_weather_generator() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(995_010, custom_inv_type(sde, 995_010, 54, 7, "Miner I", &[(40, 40.0)], &[]));
        sde.types.insert(
            995_011,
            custom_inv_type(sde, 995_011, 353, 7, "Cloak", &[(40, 40.0), (669, 4_000.0)], &[]),
        );
        sde.types.insert(
            995_012,
            custom_inv_type(sde, 995_012, 1533, 7, "Capital Micro Jump Field Generator", &[(40, 40.0)], &[]),
        );
        sde.types.insert(
            995_013,
            custom_inv_type(sde, 995_013, 1815, 7, "Weather Generator", &[(40, 40.0)], &[]),
        );
    });

    let char_id = 95_500_001i64;
    let other_char = 95_500_002i64;
    let sys_id = 30_002_506u32;
    let ship_id = 1_000_955_001i64;
    let other_ship = 1_000_955_002i64;
    let miner_id = 1_000_955_010i64;
    let cloak_id = 1_000_955_011i64;
    let cap_mjfg_id = 1_000_955_012i64;
    let weather_id = 1_000_955_013i64;
    let cargo_mod_id = 1_000_955_014i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let mut ship_rec = make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 0.0);
        ship_rec.dynamic_attributes.insert(37, 500.0);
        ship_rec.dynamic_attributes.insert(552, 120.0);
        snap.items.insert(ship_id, ship_rec);
        snap.items.insert(other_ship, make_item(other_ship, 587, other_char, sys_id as i64, InventoryFlag::None, 0.0));

        for (mid, tid, flag) in [
            (miner_id, 995_010, InventoryFlag::HiSlot0),
            (cloak_id, 995_011, InventoryFlag::HiSlot1),
            (cap_mjfg_id, 995_012, InventoryFlag::MedSlot0),
            (weather_id, 995_013, InventoryFlag::HiSlot2),
            (cargo_mod_id, 995_010, InventoryFlag::Cargo),
        ] {
            snap.items.insert(mid, make_item(mid, tid, char_id, ship_id, flag, 0.0));
        }
    });

    // Unhandled service/method returns None
    let kw = HashMap::new();
    let mut unauth_sess = SessionState::default();
    let other_svc = "machoNet";
    let other_method = "GetTime";
    assert!(
        handle_mining_and_deactivate(&ctx, &mut unauth_sess, None, other_svc, other_method, &[], &kw)
            .await
            .is_none()
    );
    assert!(call_dogma(&ctx, &mut unauth_sess, "Deactivate", &[PyRep::Integer(miner_id)])
        .await
        .is_err());

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: None,
        ..Default::default()
    };
    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(miner_id)]).await.unwrap_err();
    assert_eq!(e.code, "NoActiveShip");

    sess.ship_id = Some(999_999_999);
    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(miner_id)]).await.unwrap_err();
    assert_eq!(e.code, "ShipNotFound");

    sess.ship_id = Some(other_ship);
    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(miner_id)]).await.unwrap_err();
    assert_eq!(e.code, "ShipOwnershipViolation");

    sess.ship_id = Some(ship_id);
    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(888_888_888)]).await.unwrap_err();
    assert_eq!(e.code, "ModuleNotFound");

    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(cargo_mod_id)]).await.unwrap_err();
    assert_eq!(e.code, "ModuleNotOwnedByShip");

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag, state) in [
        (miner_id, 995_010, InventoryFlag::HiSlot0 as u16, ModuleState::Online),
        (cap_mjfg_id, 995_012, InventoryFlag::MedSlot0 as u16, ModuleState::Online),
        (weather_id, 995_013, InventoryFlag::HiSlot2 as u16, ModuleState::Active),
    ] {
        fit.modules.insert(
            mid,
            FittedModule {
                item_id: mid,
                type_id: tid,
                flag_id: flag,
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
    ctx.store_ship_fitting(ship_id, fit.clone());

    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(cloak_id)]).await.unwrap_err();
    assert_eq!(e.code, "DogmaDeactivateFailed");

    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, ship_id),
            crate::services::tactical_modules::ActiveEntosisLinkState {
                ship_id,
                char_id,
                side_id: 1_000_009,
                module_id: cloak_id,
                target_id: 77_000_001,
                cycle_index: 1,
                cycle_duration_s: 60.0,
                started_unix: 1_800_000_000,
                expires_unix: 1_800_000_060,
                is_capital: false,
            },
        );
    }
    let res_ent = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(cloak_id)]).await.unwrap();
    assert_eq!(res_ent.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));

    fit.modules.insert(
        cloak_id,
        FittedModule {
            item_id: cloak_id,
            type_id: 995_011,
            flag_id: InventoryFlag::HiSlot1 as u16,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit);
    let _ = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(cloak_id)]).await.unwrap();
    assert!(ctx.is_module_in_reactivation_delay(ship_id, cloak_id));

    let res_miner = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(miner_id)]).await.unwrap();
    assert_eq!(res_miner.dict_get("active").and_then(|v| v.as_bool()), Some(false));

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                ..Default::default()
            },
        )
        .await;
    if let Ok(Some(mut cs)) = sys.get_combat_state(ship_id).await {
        cs.mjfg_spool_remaining_s = 4.0;
        let _ = sys.set_combat_state(ship_id, cs).await;
    }
    let e = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(cap_mjfg_id)]).await.unwrap_err();
    assert_eq!(e.code, "CantDeactivateWhileSpooling");

    let _ = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(weather_id)]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items[&ship_id].dynamic_attributes.contains_key(&37));
        assert!(!snap.items[&ship_id].dynamic_attributes.contains_key(&552));
    });
}

#[tokio::test]
async fn overload_rack_nanite_repair_guards_ehe_burnout_and_mining_scan_branches() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_020,
            custom_inv_type(sde, 995_020, 46, 7, "Prop", &[(40, 40.0), (1267, 10.0), (1277, 0.5)], &[]),
        );
        sde.types.insert(995_021, custom_inv_type(sde, 995_021, 60, 7, "Custom EHE", &[(40, 40.0)], &[6484]));
        sde.types.insert(995_022, custom_inv_type(sde, 995_022, 49, 7, "Scanner", &[(197, 10_000.0)], &[]));
    });

    let char_id = 95_500_020i64;
    let sys_id = 30_002_507u32;
    let ship_id = 1_000_955_020i64;
    let hi_mod = 1_000_955_021i64;
    let low_ehe = 1_000_955_022i64;
    let burned_mod = 1_000_955_023i64;
    let undamaged_mod = 1_000_955_024i64;
    let repairable_mod = 1_000_955_025i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.skills.entry(char_id).or_default().insert(
            28879,
            TrainedSkillRecord {
                type_id: 28879,
                level: 4,
                skill_points: 50_000.0,
            },
        );
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 0.0));
        for (mid, tid, flag, dmg) in [
            (hi_mod, 995_022, InventoryFlag::HiSlot0, 0.0),
            (low_ehe, 995_021, InventoryFlag::LoSlot0, 10.0),
            (burned_mod, 995_020, InventoryFlag::LoSlot1, 40.0),
            (undamaged_mod, 995_020, InventoryFlag::MedSlot0, 0.0),
            (repairable_mod, 995_020, InventoryFlag::MedSlot1, 15.0),
        ] {
            snap.items.insert(mid, make_item(mid, tid, char_id, ship_id, flag, dmg));
        }

        for (aid, qty, x) in [
            (1_000_955_101, 0, 2000.0),
            (1_000_955_102, 800, 5000.0),
            (1_000_955_103, 800, 25000.0),
        ] {
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 1,
                    type_id: 1230,
                    quantity: qty,
                    initial_quantity: 1000,
                    position: (x, 0.0, 0.0),
                    radius: 100.0,
                },
            );
        }
    });

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag, state) in [
        (hi_mod, 995_022, InventoryFlag::HiSlot0 as u16, ModuleState::Online),
        (low_ehe, 995_021, InventoryFlag::LoSlot0 as u16, ModuleState::Online),
        (burned_mod, 995_020, InventoryFlag::LoSlot1 as u16, ModuleState::Offline),
        (undamaged_mod, 995_020, InventoryFlag::MedSlot0 as u16, ModuleState::Active),
        (repairable_mod, 995_020, InventoryFlag::MedSlot1 as u16, ModuleState::Online),
    ] {
        fit.modules.insert(
            mid,
            FittedModule {
                item_id: mid,
                type_id: tid,
                flag_id: flag,
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

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    let r0 = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(0)]).await.unwrap();
    assert_eq!(r0.dict_get("rackIndex").and_then(|v| v.as_i64()), Some(0));
    let r2 = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(2)]).await.unwrap();
    assert_eq!(r2.dict_get("rackIndex").and_then(|v| v.as_i64()), Some(2));
    let sr2 = call_dogma(&ctx, &mut sess, "StopOverloadRack", &[PyRep::Integer(low_ehe)]).await.unwrap();
    assert_eq!(sr2.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));

    sess.station_id = Some(60_003_760);
    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(repairable_mod)]).await.unwrap_err();
    assert_eq!(e.code, "CannotRepairModuleWhileDocked");
    sess.station_id = None;

    let e = call_dogma(&ctx, &mut sess, "InitiateModuleRepair", &[PyRep::List(vec![PyRep::Integer(low_ehe)])]).await.unwrap_err();
    assert_eq!(e.code, "ModuleBurnedOut");

    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(burned_mod)]).await.unwrap_err();
    assert_eq!(e.code, "ModuleBurnedOut");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(undamaged_mod);
    });
    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(undamaged_mod)]).await.unwrap_err();
    assert_eq!(e.code, "CannotRepairActiveModule");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&undamaged_mod);
    });

    let ok_zero = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(undamaged_mod)]).await.unwrap();
    assert_eq!(ok_zero.dict_get("pasteConsumed").and_then(|v| v.as_i64()), Some(0));

    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(repairable_mod)]).await.unwrap_err();
    assert_eq!(e.code, "NotEnoughNanitePaste");

    let stop_idle = call_dogma(&ctx, &mut sess, "StopModuleRepair", &[PyRep::Integer(repairable_mod)]).await.unwrap();
    assert_eq!(stop_idle.dict_get("stopped").and_then(|v| v.as_bool()), Some(false));

    let kw = HashMap::new();
    assert!(call_scan(&ctx, &mut sess, &[], &kw).await.is_none());
    let e = call_scan(&ctx, &mut sess, &[PyRep::Integer(low_ehe)], &kw).await.unwrap().unwrap_err();
    assert_eq!(e.code, "ModuleNotFound");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                ..Default::default()
            },
        )
        .await;
    let scan_ok = call_scan(&ctx, &mut sess, &[PyRep::Integer(hi_mod)], &kw).await.unwrap().unwrap();
    assert_eq!(scan_ok.dict_get("count").and_then(|v| v.as_i64()), Some(1));
}
