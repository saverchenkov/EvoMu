//! Deep branch-coverage and mutation-killing unit tests for `fitting_mining_lasers`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_mining_lasers::{
    compute_nanite_repair_params, handle_mining_and_deactivate,
};
use crate::services::inventory_fitting::fitting_scanners_superweapons;
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
        sde.types.insert(995_010, custom_inv_type(sde, 995_010, 54, 7, "Miner I", &[(40, 40.0)], &[12]));
        sde.types.insert(
            995_011,
            custom_inv_type(sde, 995_011, 353, 7, "Cloak", &[(40, 40.0), (669, 4_000.0)], &[12]),
        );
        sde.types.insert(
            995_012,
            custom_inv_type(sde, 995_012, 1533, 7, "Capital Micro Jump Field Generator", &[(40, 40.0)], &[13]),
        );
        sde.types.insert(
            995_013,
            custom_inv_type(sde, 995_013, 1815, 7, "Weather Generator", &[(40, 40.0)], &[12]),
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
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
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

#[tokio::test]
async fn deactivate_online_mining_undocked_and_effect_id_fallback_ladder() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_030,
            custom_inv_type(sde, 995_030, 54, 7, "Custom Miner", &[(40, 40.0)], &[12]),
        );
        sde.types.insert(
            995_031,
            custom_inv_type(sde, 995_031, 46, 7, "Custom Prop", &[(40, 40.0)], &[13]),
        );
    });

    // Pick a real non-mining, non-"online", non-"defaultEffect" SDE effect to exercise the SDE lookup branch.
    let (sde_eid, sde_ename) = ctx
        .sde
        .effects
        .values()
        .find(|e| {
            !e.name.is_empty()
                && !matches!(e.effect_id, 11..=13 | 16)
                && e.name != "online"
                && e.name != "defaultEffect"
                && !e.name.starts_with("mining")
        })
        .map(|e| (i64::from(e.effect_id), e.name.clone()))
        .expect("seed SDE must have a non-mining effect");

    let char_id = 95_500_030i64;
    let sys_id = 30_002_508u32;
    let ship_id = 1_000_955_030i64;
    let miner_id = 1_000_955_031i64;
    let prop_id = 1_000_955_032i64;
    let online_mod_id = 1_000_955_033i64;

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        let uid = ch.user_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items
            .insert(ship_id, make_item(ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 0.0));
        for (mid, tid, flag) in [
            (miner_id, 995_030, InventoryFlag::HiSlot0),
            (prop_id, 995_031, InventoryFlag::MedSlot0),
            (online_mod_id, 995_031, InventoryFlag::MedSlot1),
        ] {
            snap.items
                .insert(mid, make_item(mid, tid, char_id, ship_id, flag, 0.0));
        }
        uid
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 9_550_030, Some(char_id), true, ctx.now_unix_secs())
        .unwrap();

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag, state) in [
        (miner_id, 995_030, InventoryFlag::HiSlot0 as u16, ModuleState::Active),
        (prop_id, 995_031, InventoryFlag::MedSlot0 as u16, ModuleState::Active),
        (online_mod_id, 995_031, InventoryFlag::MedSlot1 as u16, ModuleState::Online),
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
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.store_ship_fitting(ship_id, fit);

    // 1. "online" early-return NoActiveShip vs happy path
    let mut sess_no_ship = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: None,
        ship_id: None,
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let e = call_dogma(
        &ctx,
        &mut sess_no_ship,
        "Deactivate",
        &[PyRep::Integer(online_mod_id), PyRep::String("online".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "NoActiveShip");

    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: None,
        ship_id: Some(ship_id),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let res_online = call_dogma(
        &ctx,
        &mut sess,
        "Deactivate",
        &[PyRep::Integer(online_mod_id), PyRep::String("online".into())],
    )
    .await
    .unwrap();
    assert_eq!(res_online.dict_get("online").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&online_mod_id].state,
        ModuleState::Offline
    );

    // 2. Mining deactivate with solar_system_id == None hits notify_godma_ship_effect directly
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = call_dogma(
        &ctx,
        &mut sess,
        "Deactivate",
        &[PyRep::Integer(miner_id), PyRep::String("miningLaser".into())],
    )
    .await
    .unwrap();
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        notes.iter().any(|n| {
            n.event == "OnGodmaShipEffect"
                && n.args.first().and_then(|v| v.as_i64()) == Some(miner_id)
                && n.args.get(1).and_then(|v| v.as_i64()) == Some(67)
                && n.args.get(3).and_then(|v| v.as_i64()) == Some(0)
        }),
        "expected OnGodmaShipEffect with miningLaser effect_id=67 when solar_system_id=None: {notes:?}"
    );

    // 3. Test "miningClouds" -> 2726, "mining" -> 17, SDE lookup, "defaultEffect" -> 0, unknown -> 0
    for (eff_name, expected_eid) in [
        ("miningClouds", 2726_i64),
        ("mining", 17_i64),
        (sde_ename.as_str(), sde_eid),
        ("defaultEffect", 0_i64),
        ("completelyUnknownEffectXYZ", 0_i64),
    ] {
        let mut f = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        f.modules.get_mut(&prop_id).unwrap().state = ModuleState::Active;
        ctx.store_ship_fitting(ship_id, f);
        let _ = ctx.drain_notifications_for_char(char_id);

        let _ = call_dogma(
            &ctx,
            &mut sess,
            "Deactivate",
            &[PyRep::Integer(prop_id), PyRep::String(eff_name.into())],
        )
        .await
        .unwrap();
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(
            notes.iter().any(|n| {
                n.event == "OnGodmaShipEffect"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(prop_id)
                    && n.args.get(1).and_then(|v| v.as_i64()) == Some(expected_eid)
            }),
            "expected effect '{eff_name}' to map to effect_id={expected_eid}, notes={notes:?}"
        );
    }
}

#[tokio::test]
async fn stop_overload_and_rack_edges_and_nanite_repair_conflict_refund_done() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_040,
            custom_inv_type(
                sde,
                995_040,
                53,
                7,
                "Overloadable Prop",
                &[(40, 40.0), (1267, 10.0), (1277, 1.0)],
                &[11, 12, 13],
            ),
        );
    });

    let char_id = 95_500_040i64;
    let other_char = 95_500_041i64;
    let sys_id = 30_002_509u32;
    let ship_id = 1_000_955_040i64;
    let other_ship = 1_000_955_041i64;
    let hi_mod = 1_000_955_042i64;
    let med_mod = 1_000_955_043i64;
    let offline_low_mod = 1_000_955_044i64;
    let alien_mod = 1_000_955_045i64;
    let paste_id = 1_000_955_046i64;

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        let uid = ch.user_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.skills.entry(char_id).or_default().insert(
            28879,
            TrainedSkillRecord {
                type_id: 28879,
                level: 0,
                skill_points: 0.0,
            },
        );

        snap.items
            .insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 0.0));
        snap.items.insert(
            other_ship,
            make_item(other_ship, 587, other_char, sys_id as i64, InventoryFlag::None, 0.0),
        );
        snap.items.insert(
            hi_mod,
            make_item(hi_mod, 995_040, char_id, ship_id, InventoryFlag::HiSlot0, 0.0),
        );
        snap.items.insert(
            med_mod,
            make_item(med_mod, 995_040, char_id, ship_id, InventoryFlag::MedSlot0, 10.0),
        );
        snap.items.insert(
            offline_low_mod,
            make_item(offline_low_mod, 995_040, char_id, ship_id, InventoryFlag::LoSlot0, 0.0),
        );
        snap.items.insert(
            alien_mod,
            make_item(alien_mod, 995_040, other_char, ship_id, InventoryFlag::LoSlot1, 5.0),
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668, // Nanite Repair Paste
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(offline_low_mod, false);
        uid
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 9_550_040, Some(char_id), true, ctx.now_unix_secs())
        .unwrap();

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, flag, state, dmg) in [
        (hi_mod, InventoryFlag::HiSlot0 as u16, ModuleState::Overloaded, 0.0),
        (med_mod, InventoryFlag::MedSlot0 as u16, ModuleState::Overloaded, 10.0),
        (offline_low_mod, InventoryFlag::LoSlot0 as u16, ModuleState::Offline, 0.0),
    ] {
        fit.modules.insert(
            mid,
            FittedModule {
                item_id: mid,
                type_id: 995_040,
                flag_id: flag,
                state,
                charge_type_id: None,
                charge_quantity: 0,
                damage_hp: dmg,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }
    fit.recalculate_stats(&ctx.sde);
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.store_ship_fitting(ship_id, fit);

    // A. NoActiveShip guards for StopOverload, OverloadRack, RepairModule, perform_scan
    let mut no_ship_sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: None,
        ship_id: None,
        ..Default::default()
    };
    assert_eq!(
        call_dogma(&ctx, &mut no_ship_sess, "StopOverload", &[PyRep::Integer(hi_mod)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        call_dogma(&ctx, &mut no_ship_sess, "OverloadRack", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        call_dogma(&ctx, &mut no_ship_sess, "RepairModule", &[PyRep::Integer(med_mod)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    let kw = HashMap::new();
    assert_eq!(
        call_scan(&ctx, &mut no_ship_sess, &[PyRep::Integer(hi_mod)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // B. StopOverload (was_cycling = false, solar_system_id = None) -> Overloaded -> Online + notifications
    let mut sess_no_sys = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);
    let res_so = call_dogma(&ctx, &mut sess_no_sys, "StopOverload", &[PyRep::Integer(hi_mod)])
        .await
        .unwrap();
    assert_eq!(res_so.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&hi_mod].state,
        ModuleState::Online
    );
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnHeatRemoved"));
    assert!(notes.iter().any(|n| n.event == "OnModuleAttributeChanges"));

    // StopOverloadRack with solar_system_id = None clears MedSlot0 Overloaded
    let _ = call_dogma(&ctx, &mut sess_no_sys, "StopOverloadRack", &[PyRep::Integer(med_mod)])
        .await
        .unwrap();
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&med_mod].state,
        ModuleState::Online
    );

    // C. OverloadRack(1) hits MedSlot0; OverloadRack(2) skips Offline low module
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let r_med = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(r_med.dict_get("rackIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&med_mod].state,
        ModuleState::Overloaded
    );
    let _ = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(2)]).await.unwrap();
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&offline_low_mod].state,
        ModuleState::Offline
    );

    // D. Active overloaded cycle triggers CannotRepairWhileOverloaded and StopOverload keeps fitting Overloaded until cycle end (was_cycling == true)
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: med_mod,
        module_type_id: 995_040,
        effect_name: "afterburner".into(),
        duration_s: 10.0,
        repeat: 1,
        is_overloaded: true,
        rack_idx: 1,
        ..crate::system_actor::ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "CannotRepairWhileOverloaded");

    let _ = call_dogma(&ctx, &mut sess, "StopOverload", &[PyRep::Integer(med_mod)])
        .await
        .unwrap();
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&med_mod].state,
        ModuleState::Overloaded,
        "StopOverload during active cycle sets pending_overloaded instead of clearing fitting immediately"
    );

    // Clear active cycle before RepairModule ownership & repair tests
    let _ = sys.stop_non_warp_module_cycles(ship_id).await;

    // E. Ownership violations on RepairModule (alien ship & alien module)
    sess.ship_id = Some(other_ship);
    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "ModuleOwnershipViolation");
    sess.ship_id = Some(ship_id);

    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(alien_mod)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "ModuleOwnershipViolation");

    // Take med_mod off overload -> Online
    let _ = call_dogma(&ctx, &mut sess, "StopOverload", &[PyRep::Integer(med_mod)])
        .await
        .unwrap();

    // F. Contract-locked nanite paste is ignored in paste sum -> NotEnoughNanitePaste
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            955_001,
            eve_db::ContractRecord {
                contract_id: 955_001,
                issuer_id: char_id,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![paste_id],
                ..Default::default()
            },
        );
    });
    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "NotEnoughNanitePaste");
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.remove(&955_001);
    });

    // G. Successful RepairModule -> deducts paste, blocks OverloadRack + ModuleAlreadyRepairing, StopRepairModule refunds & emits OnRepairDone
    let rep_ok = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap();
    let paste_consumed = rep_ok.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    assert!(paste_consumed > 0);
    let rem_qty_after_start = ctx.db.with_snapshot(|snap| snap.items[&paste_id].quantity);
    assert_eq!(rem_qty_after_start, 50 - paste_consumed as i32);

    let e = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(1)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "CannotOverheatWhileRepairing");

    let e = call_dogma(&ctx, &mut sess, "RepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap_err();
    assert_eq!(e.code, "ModuleAlreadyRepairing");

    let _ = ctx.drain_notifications_for_char(char_id);
    let stop_ok = call_dogma(&ctx, &mut sess, "StopRepairModule", &[PyRep::Integer(med_mod)])
        .await
        .unwrap();
    assert_eq!(stop_ok.dict_get("stopped").and_then(|v| v.as_bool()), Some(true));
    let refunded = stop_ok.dict_get("pasteRefunded").and_then(|v| v.as_i64()).unwrap();
    assert!(refunded > 0);
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items[&paste_id].quantity),
        rem_qty_after_start + refunded as i32
    );
    let done_notes = ctx.drain_notifications_for_char(char_id);
    assert!(done_notes.iter().any(|n| {
        n.event == "OnRepairDone"
            && n.args
                .first()
                .and_then(|a| match a {
                    PyRep::List(items) => items.first().and_then(|x| x.as_i64()),
                    _ => None,
                })
                == Some(med_mod)
    }));
}

#[tokio::test]
async fn perform_scan_zero_pos_fallback_and_dbuff_attr23_range_multiplier() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_050,
            custom_inv_type(sde, 995_050, 49, 7, "Survey Scanner", &[(197, 10_000.0)], &[]),
        );
    });

    let char_id = 95_500_050i64;
    let sys_id = 30_002_510u32;
    let ship_id = 1_000_955_050i64;
    let hi_mod = 1_000_955_051i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items
            .insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 0.0));
        snap.items
            .insert(hi_mod, make_item(hi_mod, 995_050, char_id, ship_id, InventoryFlag::HiSlot0, 0.0));

        for (aid, x) in [(1_000_955_501, 5_000.0), (1_000_955_502, 22_000.0)] {
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 1,
                    type_id: 1230,
                    quantity: 500,
                    initial_quantity: 500,
                    position: (x, 0.0, 0.0),
                    radius: 100.0,
                },
            );
        }
    });

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        hi_mod,
        FittedModule {
            item_id: hi_mod,
            type_id: 995_050,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.recalculate_stats(&ctx.sde);
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.store_ship_fitting(ship_id, fit);

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Ship ball intentionally NOT spawned -> ship_pos == DVec3::ZERO reveals all non-zero asteroids
    let res_zero = call_scan(&ctx, &mut sess, &[PyRep::Integer(hi_mod)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_zero.dict_get("count").and_then(|v| v.as_i64()), Some(2));

    // 2. Spawn ship ball away from origin -> dist cutoff applies, only 5 km asteroid revealed within 10 km range
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;
    let res_base = call_scan(&ctx, &mut sess, &[PyRep::Integer(hi_mod)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_base.dict_get("count").and_then(|v| v.as_i64()), Some(1));

    // 3. Apply DBuff attr 23 = +150% -> range = 10_000 * 2.5 = 25_000 -> reveals 22 km asteroid as well
    {
        let mut st = fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, ship_id),
            vec![fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id: 23,
                value: 150.0,
                expires_at_unix: ctx.now_unix_secs() + 600,
                source_module_id: hi_mod,
            }],
        );
    }
    let res_buffed = call_scan(&ctx, &mut sess, &[PyRep::Integer(hi_mod)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_buffed.dict_get("count").and_then(|v| v.as_i64()), Some(2));
}

#[tokio::test]
async fn mutants_cov_tp_sig_mult_fallback_effects_and_overload() {
    use eve_dogma::{DogmaItemState, ProjectedEwarEffect};

    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_091,
            custom_inv_type(
                sde,
                995_091,
                379,
                7,
                "Test Target Painter",
                &[(40, 40.0), (54, 30_000.0), (158, 15_000.0), (554, 30.0)],
                &[13],
            ),
        );
        sde.types.insert(
            995_092,
            custom_inv_type(
                sde,
                995_092,
                379,
                7,
                "Micro Target Painter",
                &[(40, 40.0), (54, 100_000.0), (158, 50_000.0), (554, 1e-4)],
                &[13],
            ),
        );
        sde.types.insert(
            995_093,
            custom_inv_type(sde, 995_093, 53, 7, "Dummy Module", &[(40, 40.0)], &[11]),
        );
        sde.types.insert(
            995_094,
            custom_inv_type(
                sde,
                995_094,
                46,
                7,
                "Overload Module",
                &[(40, 40.0), (1267, 10.0), (1277, 1.0)],
                &[12],
            ),
        );
    });

    let (sde_eid, sde_ename) = ctx
        .sde
        .effects
        .values()
        .find(|e| {
            !e.name.is_empty()
                && !matches!(e.effect_id, 11..=13 | 16)
                && e.name != "online"
                && e.name != "defaultEffect"
                && !e.name.starts_with("mining")
        })
        .map(|e| (i64::from(e.effect_id), e.name.clone()))
        .expect("non-mining SDE effect");

    let char_id = 95_590_001_i64;
    let sys_id = 30_002_510_u32;
    let ship_id = 1_000_955_901_i64;
    let tp_target_id = 1_000_955_902_i64;
    let tp1_id = 1_000_955_911_i64;
    let tp2_id = 1_000_955_912_i64;
    let tp3_id = 1_000_955_913_i64;
    let dummy_id = 1_000_955_914_i64;
    let overload_id = 1_000_955_915_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 0.0));
        snap.items.insert(tp_target_id, make_item(tp_target_id, 587, char_id + 1, sys_id as i64, InventoryFlag::None, 0.0));
        snap.items.insert(tp1_id, make_item(tp1_id, 995_091, char_id, ship_id, InventoryFlag::MedSlot0, 0.0));
        snap.items.insert(tp2_id, make_item(tp2_id, 995_091, char_id, ship_id, InventoryFlag::MedSlot1, 0.0));
        snap.items.insert(tp3_id, make_item(tp3_id, 995_092, char_id, ship_id, InventoryFlag::MedSlot2, 0.0));
        snap.items.insert(dummy_id, make_item(dummy_id, 995_093, char_id, ship_id, InventoryFlag::LoSlot0, 0.0));
        snap.items.insert(overload_id, make_item(overload_id, 995_094, char_id, ship_id, InventoryFlag::HiSlot0, 0.0));
        for active_mid in [tp1_id, tp2_id, dummy_id] {
            snap.module_active_states.insert(active_mid);
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for bid in [ship_id, tp_target_id] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: 587,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
                SlimItemMeta::default(),
            )
            .await;
    }

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag, state) in [
        (tp1_id, 995_091, InventoryFlag::MedSlot0 as u16, ModuleState::Active),
        (tp2_id, 995_091, InventoryFlag::MedSlot1 as u16, ModuleState::Active),
        (tp3_id, 995_092, InventoryFlag::MedSlot2 as u16, ModuleState::Online),
        (dummy_id, 995_093, InventoryFlag::LoSlot0 as u16, ModuleState::Active),
        (overload_id, 995_094, InventoryFlag::HiSlot0 as u16, ModuleState::Online),
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
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.store_ship_fitting(ship_id, fit);

    // 1. L166 TP threshold (`tp_mult > 1.0 + 1e-6`): 2 TPs -> deactivate tp1 leaves ~1.30 (kills +->/, > -> ==);
    // deactivate tp2 leaves 1.0 -> None (kills +->*, > -> <); exact 1.0 + 1e-6 -> None (kills > -> >=)
    ctx.upsert_projected_ewar_on_ship(
        ship_id,
        tp_target_id,
        ProjectedEwarEffect {
            source_module_id: tp1_id,
            source_module_type_id: 995_091,
            charge_type_id: None,
            distance: 0.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(char_id, 995_091, &ctx.sde),
        },
    );
    ctx.upsert_projected_ewar_on_ship(
        ship_id,
        tp_target_id,
        ProjectedEwarEffect {
            source_module_id: tp2_id,
            source_module_type_id: 995_091,
            charge_type_id: None,
            distance: 0.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(char_id, 995_091, &ctx.sde),
        },
    );
    {
        let mut active = ctx.active_ewar_starts.write().unwrap();
        active.insert((ship_id, tp1_id), vec![(tp_target_id, "targetPainter")]);
        active.insert((ship_id, tp2_id), vec![(tp_target_id, "targetPainter")]);
    }
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id));
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(tp1_id)]).await.unwrap();
    let cs_one_tp = sys.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert!(cs_one_tp.tp_sig_multiplier.is_some_and(|m| (m - 1.3).abs() < 0.05));

    let _ = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(tp2_id)]).await.unwrap();
    let cs_no_tp = sys.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert_eq!(cs_no_tp.tp_sig_multiplier, None);

    ctx.upsert_projected_ewar_on_ship(
        ship_id,
        tp_target_id,
        ProjectedEwarEffect {
            source_module_id: tp3_id,
            source_module_type_id: 995_092,
            charge_type_id: None,
            distance: 0.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(char_id, 995_092, &ctx.sde),
        },
    );
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship_id, dummy_id), vec![(tp_target_id, "targetPainter")]);
    assert_eq!(ctx.effective_target_painter_multiplier(tp_target_id), 1.0 + 1e-6);
    let _ = call_dogma(&ctx, &mut sess, "Deactivate", &[PyRep::Integer(dummy_id)]).await.unwrap();
    let cs_eps_tp = sys.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert_eq!(cs_eps_tp.tp_sig_multiplier, None);

    // 2. OverloadRack -> StopOverload -> OverloadRack -> StopOverloadRack (kills L352-419 & L421-530)
    let ov_rack_rep = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(overload_id)])
        .await
        .unwrap();
    assert_eq!(ov_rack_rep.dict_get("overloaded"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&overload_id].state,
        ModuleState::Overloaded
    );
    let _ = ctx.drain_notifications_for_char(char_id);
    let stop_ov_rep = call_dogma(&ctx, &mut sess, "StopOverload", &[PyRep::Integer(overload_id)])
        .await
        .unwrap();
    assert_eq!(stop_ov_rep.dict_get("overloaded"), Some(&PyRep::Bool(false)));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnHeatRemoved"));

    let _ = call_dogma(&ctx, &mut sess, "OverloadRack", &[PyRep::Integer(overload_id)])
        .await
        .unwrap();
    let stop_rack_rep = call_dogma(&ctx, &mut sess, "StopOverloadRack", &[PyRep::Integer(overload_id)])
        .await
        .unwrap();
    assert_eq!(stop_rack_rep.dict_get("overloaded"), Some(&PyRep::Bool(false)));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&overload_id].state,
        ModuleState::Online
    );

    // 3. Fallback effect_name match arms (L215, L230-232) when SDE lacks mining* effect names
    let ctx_fb = test_ctx_with_custom(|sde| {
        sde.effects.retain(|_, e| !matches!(e.name.as_str(), "miningLaser" | "miningClouds" | "mining"));
        sde.types.insert(
            995_099,
            custom_inv_type(sde, 995_099, 46, 7, "Plain Mod", &[(40, 40.0)], &[]),
        );
    });
    let mod_fb_id = 1_000_955_999_i64;
    let mut uid_fb = 0_u64;
    ctx_fb.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        uid_fb = ch.user_id;
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 0.0));
        snap.items.insert(mod_fb_id, make_item(mod_fb_id, 995_099, char_id, ship_id, InventoryFlag::MedSlot0, 0.0));
    });
    let _ = ctx_fb.db.acquire_online_lock(uid_fb, 2, Some(char_id), true, ctx_fb.now_unix());
    let mut sess_fb = SessionState {
        user_id: Some(uid_fb),
        character_id: Some(char_id),
        solar_system_id: None,
        ship_id: Some(ship_id),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    for (eff_name, expected_eid) in [
        ("miningLaser", 67_i64),
        ("miningClouds", 2726_i64),
        ("mining", 17_i64),
        (sde_ename.as_str(), sde_eid),
        ("defaultEffect", 0_i64),
    ] {
        let mut f = ShipFittingEngine::from_sde(ship_id, 587, &ctx_fb.sde);
        f.modules.insert(
            mod_fb_id,
            FittedModule {
                item_id: mod_fb_id,
                type_id: 995_099,
                flag_id: InventoryFlag::MedSlot0 as u16,
                state: ModuleState::Active,
                charge_type_id: None,
                charge_quantity: 0,
                damage_hp: 0.0,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        ctx_fb.store_ship_fitting(ship_id, f);
        let _ = ctx_fb.drain_notifications_for_char(char_id);
        let deact_rep = call_dogma(
            &ctx_fb,
            &mut sess_fb,
            "Deactivate",
            &[PyRep::Integer(mod_fb_id), PyRep::String(eff_name.into())],
        )
        .await
        .unwrap();
        assert_eq!(deact_rep.dict_get("overloaded"), Some(&PyRep::Bool(false)));
        assert_eq!(deact_rep.dict_get("moduleID"), Some(&PyRep::Integer(mod_fb_id)));
        let notes = ctx_fb.drain_notifications_for_char(char_id);
        assert!(
            notes.iter().any(|n| {
                n.event == "OnGodmaShipEffect"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(mod_fb_id)
                    && n.args.get(1).and_then(|v| v.as_i64()) == Some(expected_eid)
            }),
            "expected '{eff_name}' -> {expected_eid}, got {notes:?}"
        );
    }
}
