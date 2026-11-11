//! Targeted edge-path and error-branch integration tests for:
//! 1. Dogma / Fitting (`dogmaIM`, `charFittingMgr`, `corpFittingMgr`, `invbroker.Add` fitting checks)
//! 2. Inventory / Containers (`invbroker`, `ALSC*`, `AssembleCargoContainer`, `ClearCharges`, `RepackageItems`, `TrashItems`)
//! 3. Space Destiny / Bookmarks (`dogmaIM` targeting, `beyonce` warp/gate/subwarp, `spaceComponentSvc`, `bookmark`)

use crate::common;
use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, InventoryFlag, InventoryItemRecord, SavedFittingRecord,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR1_ID: i64 = 90_000_001;
const CHAR2_ID: i64 = 90_000_002;
const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYS_ID: u32 = 30_000_142;
const JITA_GATE_ID: i64 = 50_001_248;
const PERIMETER_GATE_ID: i64 = 50_002_183;
const ZARZAKH_SYS_ID: u32 = 30_100_000;
const CONTAINER_HANGAR: i64 = 10_004;

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

async fn login_and_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    let kw = HashMap::new();
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn bound_id_of(rep: &PyRep) -> u64 {
    rep.as_i64().expect("bound object id integer") as u64
}

async fn bind_station_inv_mgr(
    ctx: &ServerContext,
    sess: &mut SessionState,
    station_id: i64,
) -> u64 {
    let kw = HashMap::new();
    let bind = ctx
        .dispatch_rpc(
            sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject invbroker");
    let PyRep::Tuple(parts) = &bind else {
        panic!("MachoBindObject must return 2-tuple, got {bind:?}");
    };
    bound_id_of(&parts[0])
}

async fn bind_inv_id(
    ctx: &ServerContext,
    sess: &mut SessionState,
    mgr_bid: u64,
    item_id: i64,
) -> u64 {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(item_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetInventoryFromId");
    bound_id_of(&rep)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_overheat_nanite_online_and_session_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select(&ctx, 5201, "demo", "demo-secret", CHAR1_ID).await;
    let ship_id = 100_000_001i64;

    // Seed modules on Rifter: Afterburner (439), Small Armor Rep (523), Damage Control I (2046), Cloak (11370)
    let mod_ab = 520_101i64;
    let mod_rep = 520_102i64;
    let mod_dc = 520_103i64;
    let mod_cloak = 520_104i64;
    let mod_gun = 520_106i64;
    for tid in [439u32, 484, 523, 2046, 11370, 28668] {
        grant_skills_for_type(&ctx, CHAR1_ID, tid);
    }
    seed_item(&ctx, mod_ab, 439, CHAR1_ID, ship_id, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, mod_rep, 523, CHAR1_ID, ship_id, InventoryFlag::LoSlot0, 1, true);
    seed_item(&ctx, mod_dc, 2046, CHAR1_ID, ship_id, InventoryFlag::LoSlot1, 1, true);
    seed_item(&ctx, mod_cloak, 11370, CHAR1_ID, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, mod_gun, 484, CHAR1_ID, ship_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, 520_105, 28668, CHAR1_ID, ship_id, InventoryFlag::Cargo, 50, false);
    ctx.invalidate_ship_fitting(ship_id);

    // 1. SetSessionCheck mismatch & clear
    let mismatch_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "SetSessionCheck",
            &[PyRep::Dict(vec![(
                PyRep::String("shipid".into()),
                PyRep::Integer(999_999),
            )])],
            &kw,
            None,
        )
        .await
        .expect_err("SetSessionCheck wrong shipid must fail");
    assert_eq!(mismatch_err.code, "SessionCheckMismatch");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetSessionCheck", &[PyRep::None], &kw, None)
        .await
        .expect("SetSessionCheck None resets");

    // 2. QueryAttributeValue / QueryAllAttributesForItem / FullyDescribeAttribute access denied on foreign/invalid item
    let foreign_item = 520_199i64;
    seed_item(&ctx, foreign_item, 439, CHAR2_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    for method in ["QueryAttributeValue", "QueryAllAttributesForItem", "FullyDescribeAttribute"] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                method,
                &[PyRep::Integer(foreign_item), PyRep::Integer(9)],
                &kw,
                None,
            )
            .await
            .expect_err("foreign dogma query must fail");
        assert_eq!(err.code, "AccessDenied");
    }

    // 3. ChangeDroneSettings validation error + valid update
    let drone_bad = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ChangeDroneSettings",
            &[PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect_err("non-dict drone settings must fail");
    assert_eq!(drone_bad.code, "InvalidArgument");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![(
            PyRep::String("focusFire".into()),
            PyRep::Integer(1),
        )])],
        &kw,
        None,
    )
    .await
    .expect("valid drone settings dict succeeds");

    // 4. Docked InitiateModuleRepairMany -> CannotRepairNaniteWhileDocked
    let repair_docked = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "InitiateModuleRepairMany",
            &[PyRep::List(vec![PyRep::Integer(mod_ab)])],
            &kw,
            None,
        )
        .await
        .expect_err("Nanite repair while docked must fail");
    assert_eq!(repair_docked.code, "CannotRepairNaniteWhileDocked");

    // ShipOwnershipViolation on SetModuleOnline / TakeModuleOffline for foreign ship
    let foreign_ship = 520_198i64;
    let foreign_mod = 520_197i64;
    seed_item(&ctx, foreign_ship, 587, CHAR2_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, foreign_mod, 439, CHAR2_ID, foreign_ship, InventoryFlag::MedSlot0, 1, true);
    let own_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "SetModuleOnline",
            &[PyRep::Integer(foreign_ship), PyRep::Integer(foreign_mod)],
            &kw,
            None,
        )
        .await
        .expect_err("foreign SetModuleOnline must fail");
    assert_eq!(own_err.code, "ShipOwnershipViolation");

    // 5. Undock into Jita to test space Activate / Overload / Online / Nanite guards
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let missing_mod = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(999_888_777), PyRep::String("afterburner".into())],
            &kw,
            None,
        )
        .await
        .expect_err("missing module");
    assert_eq!(missing_mod.code, "ModuleNotFound");

    // TakeModuleOffline & verify ModuleOffline on Activate, then NotEnoughCapacitorToOnline when cap < 75%
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "TakeModuleOffline",
        &[PyRep::Integer(ship_id), PyRep::Integer(mod_ab)],
        &kw,
        None,
    )
    .await
    .expect("TakeModuleOffline");
    let offline_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_ab)], &kw, None)
        .await
        .expect_err("offline module Activate must fail");
    assert_eq!(offline_err.code, "ModuleOffline");

    let jita = ctx.systems.get_or_spawn(JITA_SYS_ID);
    let mut combat = jita.get_combat_state(ship_id).await.unwrap().unwrap();
    let orig_cap = combat.max_capacitor.max(100.0);
    combat.max_capacitor = orig_cap;
    combat.capacitor = orig_cap * 0.2;
    jita.set_combat_state(ship_id, combat.clone()).await.unwrap();
    let low_cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "SetModuleOnline",
            &[PyRep::Integer(ship_id), PyRep::Integer(mod_ab)],
            &kw,
            None,
        )
        .await
        .expect_err("online <75% cap in space must fail");
    assert_eq!(low_cap_err.code, "NotEnoughCapacitorToOnline");

    combat.capacitor = orig_cap;
    jita.set_combat_state(ship_id, combat.clone()).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "SetModuleOnline",
        &[PyRep::Integer(ship_id), PyRep::Integer(mod_ab)],
        &kw,
        None,
    )
    .await
    .expect("online with full cap");

    // Burned out module guards: Activate & SetModuleOnline fail with ModuleBurnedOut
    let _ = ctx.get_or_rebuild_ship_fitting(CHAR1_ID, ship_id).unwrap();
    {
        let mut fits = ctx.ship_fittings.write().unwrap();
        let fit = fits.get_mut(&ship_id).unwrap();
        let slot = fit.modules.get_mut(&mod_ab).unwrap();
        slot.damage_hp = slot.max_hp.max(40.0);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_ab)
            .unwrap()
            .dynamic_attributes
            .insert(attr_ids::DAMAGE, 100.0);
    });
    let burned_act = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_ab)], &kw, None)
        .await
        .expect_err("burned out module Activate");
    assert_eq!(burned_act.code, "ModuleBurnedOut");
    let burned_online = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "SetModuleOnline",
            &[PyRep::Integer(ship_id), PyRep::Integer(mod_ab)],
            &kw,
            None,
        )
        .await
        .expect_err("burned out SetModuleOnline");
    assert_eq!(burned_online.code, "ModuleBurnedOut");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_ab)
            .unwrap()
            .dynamic_attributes
            .remove(&attr_ids::DAMAGE);
    });

    // Partial damage + InitiateModuleRepairMany -> ModuleBeingRepaired / CannotOverheatWhileRepairing
    {
        let mut fits = ctx.ship_fittings.write().unwrap();
        let fit = fits.get_mut(&ship_id).unwrap();
        let slot = fit.modules.get_mut(&mod_ab).unwrap();
        slot.damage_hp = 5.0;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_ab)
            .unwrap()
            .dynamic_attributes
            .insert(attr_ids::DAMAGE, 5.0);
    });
    let rep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "InitiateModuleRepairMany",
            &[PyRep::List(vec![PyRep::Integer(mod_ab)])],
            &kw,
            None,
        )
        .await
        .expect("InitiateModuleRepairMany starts repair");
    if let PyRep::Tuple(parts) = &rep_res {
        assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(mod_ab)]));
    } else {
        panic!("expected tuple from InitiateModuleRepairMany: {rep_res:?}");
    }
    let repair_act = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_ab)], &kw, None)
        .await
        .expect_err("repairing module Activate");
    assert_eq!(repair_act.code, "ModuleBeingRepaired");
    let repair_ovh = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mod_rep)], &kw, None)
        .await
        .expect_err("other module Overload while repairing");
    assert_eq!(repair_ovh.code, "CannotOverheatWhileRepairing");

    jita.cancel_ship_nanite_repairs_sync(ship_id, false);
    {
        let mut fits = ctx.ship_fittings.write().unwrap();
        let fit = fits.get_mut(&ship_id).unwrap();
        fit.modules.get_mut(&mod_ab).unwrap().damage_hp = 0.0;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_ab)
            .unwrap()
            .dynamic_attributes
            .remove(&attr_ids::DAMAGE);
    });

    // Overload mod_rep (active overloaded cycle) -> InitiateModuleRepairMany rejects CannotRepairWhileOverheating
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mod_rep)], &kw, None)
        .await
        .expect("Overload mod_rep starts active overloaded cycle");
    let nanite_while_ovh = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "InitiateModuleRepairMany",
            &[PyRep::List(vec![PyRep::Integer(mod_ab)])],
            &kw,
            None,
        )
        .await
        .expect_err("nanite while overheating must fail");
    assert_eq!(nanite_while_ovh.code, "CannotRepairWhileOverheating");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(mod_rep)],
        &kw,
        None,
    )
    .await
    .expect("Deactivate mod_rep");

    // Damage Control Overload succeeds & can deactivate
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mod_dc)], &kw, None)
        .await
        .expect("Overload Damage Control");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_dc)], &kw, None)
        .await
        .expect("Deactivate Damage Control");

    // Tethered guard on offensive module Activate
    combat.is_tethered = true;
    jita.set_combat_state(ship_id, combat.clone()).await.unwrap();
    let tether_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_gun)], &kw, None)
        .await
        .expect_err("tethered offensive Activate must fail");
    assert_eq!(tether_err.code, "CantActivateWhileTethered");
    combat.is_tethered = false;

    // Cloaked guard on non-cloak module Activate
    combat.is_cloaked = true;
    jita.set_combat_state(ship_id, combat.clone()).await.unwrap();
    let cloaked_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_rep)], &kw, None)
        .await
        .expect_err("cloaked non-cloak Activate");
    assert_eq!(cloaked_err.code, "CannotActivateCloaked");
    combat.is_cloaked = false;
    jita.set_combat_state(ship_id, combat.clone()).await.unwrap();

    // In-Warp guard on Activate
    let (_, mut ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.mode = MovementMode::Warp {
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        origin: ship_ball.position,
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        elapsed_warp_time: 1.0,
        phase: eve_destiny::WarpPhase::Cruising,
    };
    jita.spawn_ball_with_combat(ship_ball.clone(), combat.clone()).await.unwrap();
    let warp_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_rep)], &kw, None)
        .await
        .expect_err("in-warp Activate must fail");
    assert_eq!(warp_err.code, "CantActivateInWarp");

    // Abyssal space cloak rejection (system >= 32_000_000)
    ship_ball.mode = MovementMode::Stop;
    jita.spawn_ball_with_combat(ship_ball.clone(), combat.clone()).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.solar_system_id = 32_000_001;
        }
    });
    let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
    abyss_sys.spawn_ball_with_combat(ship_ball, combat).await.unwrap();
    let abyss_cloak = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_cloak)], &kw, None)
        .await
        .expect_err("cloak in abyssal must fail");
    assert_eq!(abyss_cloak.code, "CannotCloakInAbyssalSpace");

    // Repair service at unknown station -> StationServiceUnavailable
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.solar_system_id = JITA_SYS_ID;
            ch.station_id = Some(60_999_001);
        }
    });
    let no_rep_svc = ctx
        .dispatch_rpc(
            &mut sess,
            "repairSvc",
            None,
            "GetRepairQuotes",
            &[PyRep::List(vec![PyRep::Integer(ship_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("station without repair service must fail");
    assert_eq!(no_rep_svc.code, "StationServiceUnavailable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_fitting_validation_charges_and_fitting_mgr_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select(&ctx, 5202, "demo", "demo-secret", CHAR1_ID).await;
    let ship_id = 100_000_001i64;

    for tid in [439u32, 484, 185, 222, 3538, 33894] {
        grant_skills_for_type(&ctx, CHAR1_ID, tid);
    }

    // 1. Add fitting edges: stack module, non-fittable item, oversize rig, CPU/PG exceed, occupied slot, damaged, weapons timer
    let stacked_ab = 520_201i64;
    let tritanium = 520_202i64;
    let med_rig = 520_203i64;
    let large_rep = 520_204i64;
    let ab1 = 520_205i64;
    let ab2 = 520_206i64;
    let gun1 = 520_207i64;
    let bad_ammo = 520_208i64;
    seed_item(&ctx, stacked_ab, 439, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 2, false);
    seed_item(&ctx, tritanium, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, med_rig, 33894, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, large_rep, 3538, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, ab1, 439, CHAR1_ID, ship_id, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, ab2, 439, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, gun1, 484, CHAR1_ID, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, bad_ammo, 222, CHAR1_ID, ship_id, InventoryFlag::Cargo, 20, false);
    ctx.invalidate_ship_fitting(ship_id);

    let stack_fit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(stacked_ab),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("stacked module fit");
    assert_eq!(stack_fit_err.code, "CannotFitStackedModule");

    let non_mod_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(tritanium),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-module fit");
    assert_eq!(non_mod_err.code, "ItemNotFittableModule");

    let occ_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ab2),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("occupied slot");
    assert_eq!(occ_err.code, "SlotAlreadyOccupied");

    let rig_size_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(med_rig),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::RigSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("medium rig on frigate");
    assert_eq!(rig_size_err.code, "FittingValidationFailed");

    let pg_cpu_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(large_rep),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("large armor rep on Rifter exceeds PG");
    assert_eq!(pg_cpu_err.code, "FittingValidationFailed");

    // 2. Charge loading errors via invbroker.Add: NoWeaponFitted & IncompatibleAmmo
    let no_wpn = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(bad_ammo),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("charge into empty high slot");
    assert_eq!(no_wpn.code, "NoWeaponFitted");

    let incompat_ammo = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(bad_ammo),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::HiSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("hybrid charge into projectile autocannon");
    assert_eq!(incompat_ammo.code, "IncompatibleAmmo");

    // Damaged EHE refit error (effect 6484)
    let ehe_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.effect_ids.contains(&6484))
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("EHE module in SDE");
    grant_skills_for_type(&ctx, CHAR1_ID, ehe_tid);
    let damaged_ehe = 520_209i64;
    seed_item(&ctx, damaged_ehe, ehe_tid, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&damaged_ehe)
            .unwrap()
            .dynamic_attributes
            .insert(attr_ids::DAMAGE, 10.0);
    });
    let damaged_fit = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(damaged_ehe),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("damaged EHE cannot fit");
    assert_eq!(damaged_fit.code, "CannotRefitDamagedModule");

    // Undock for space refit locks: CannotFitInSpace + CannotRefitWithWeaponsTimer
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let fit_space_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ab2),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("fit in space without depot");
    assert_eq!(fit_space_err.code, "CannotFitInSpace");

    let now_secs = ctx.now_unix_secs();
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(CHAR1_ID)
        .or_default()
        .weapon_timer_until_unix = now_secs + 60;
    let wpn_timer_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ab2),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("refit in space with weapons timer");
    assert_eq!(wpn_timer_err.code, "CannotRefitWithWeaponsTimer");
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(CHAR1_ID)
        .or_default()
        .weapon_timer_until_unix = 0;

    // 3. charFittingMgr / corpFittingMgr validation & limits
    let make_fitting_kv = |name: &str, ship_t: i64, items: Vec<PyRep>| {
        PyRep::keyval([
            ("name", PyRep::String(name.into())),
            ("description", PyRep::String("desc".into())),
            ("shipTypeID", PyRep::Integer(ship_t)),
            ("fitData", PyRep::List(items)),
        ])
    };
    let valid_item = || PyRep::Tuple(vec![PyRep::Integer(439), PyRep::Integer(19), PyRep::Integer(1)]);

    let bad_ship = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR1_ID),
                make_fitting_kv("Bad Ship", 34, vec![valid_item()]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Tritanium ship type");
    assert_eq!(bad_ship.code, "InvalidShipType");

    let bad_flag = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR1_ID),
                make_fitting_kv(
                    "Bad Flag",
                    587,
                    vec![PyRep::Tuple(vec![
                        PyRep::Integer(439),
                        PyRep::Integer(4),
                        PyRep::Integer(1),
                    ])],
                ),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("flag 4 Hangar in fitData");
    assert_eq!(bad_flag.code, "InvalidFittingFlag");

    let zero_qty = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR1_ID),
                make_fitting_kv(
                    "Zero Qty",
                    587,
                    vec![PyRep::Tuple(vec![
                        PyRep::Integer(439),
                        PyRep::Integer(19),
                        PyRep::Integer(0),
                    ])],
                ),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("zero qty in fitData");
    assert_eq!(zero_qty.code, "InvalidFittingItem");

    let bad_qty = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR1_ID),
                make_fitting_kv(
                    "Bad Qty",
                    587,
                    vec![PyRep::Tuple(vec![
                        PyRep::Integer(439),
                        PyRep::Integer(19),
                        PyRep::Integer(2),
                    ])],
                ),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("qty != 1 in fitted slot");
    assert_eq!(bad_qty.code, "InvalidFittingQuantity");

    let bad_owner = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR2_ID),
                make_fitting_kv("Wrong Owner", 587, vec![valid_item()]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("fitting owner mismatch");
    assert_eq!(bad_owner.code, "UserError");
    assert!(bad_owner.message.contains("FittingOwnerMismatch"));

    // corpFittingMgr without fitting role -> CorpRoleDenied
    let corp_id = 98_520_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Fitting Edge Corp".into(); __corp.ticker = "FEC".into(); __corp.ceo_id = CHAR2_ID; __corp.ensure_wallet_divisions(); __corp },
        );
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.corporation_id = corp_id;
        }
        snap.corp_role_assignments.remove(&(corp_id, CHAR1_ID));
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;
    let corp_denied = ctx
        .dispatch_rpc(
            &mut sess,
            "corpFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(corp_id),
                make_fitting_kv("Corp Denied", 587, vec![valid_item()]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("corp fitting without role");
    assert_eq!(corp_denied.code, "CorpRoleDenied");

    // 250-fitting personal cap + UpdateNameAndDescription + DeleteManyFittings
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..250i64 {
            let fid = 520_500 + i;
            snap.saved_fittings.insert(
                fid,
                SavedFittingRecord {
                    fitting_id: fid,
                    owner_id: CHAR1_ID,
                    ship_type_id: 587,
                    name: format!("Fit {i}"),
                    description: String::new(),
                    items: vec![(439, InventoryFlag::MedSlot0, 1)],
                },
            );
        }
        snap.next_entity_id = 521_000;
    });
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(CHAR1_ID),
                make_fitting_kv("Fit 251", 587, vec![valid_item()]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("251st fitting exceeds OwnerMaxFittings");
    assert_eq!(cap_err.code, "OwnerMaxFittings");

    let missing_upd = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(999_777),
                PyRep::Integer(CHAR1_ID),
                PyRep::String("New".into()),
                PyRep::String("D".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("update missing fitting");
    assert_eq!(missing_upd.code, "FittingNotFound");

    let empty_upd_name = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(520_500),
                PyRep::Integer(CHAR1_ID),
                PyRep::String("   ".into()),
                PyRep::String("Updated".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("empty name on UpdateNameAndDescription must fail");
    assert_eq!(empty_upd_name.code, "InvalidFittingName");

    ctx.dispatch_rpc(
        &mut sess,
        "charFittingMgr",
        None,
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(520_500),
            PyRep::Integer(CHAR1_ID),
            PyRep::String("Renamed".into()),
            PyRep::String("Updated".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("UpdateNameAndDescription succeeds");

    ctx.dispatch_rpc(
        &mut sess,
        "charFittingMgr",
        None,
        "DeleteManyFittings",
        &[
            PyRep::Integer(CHAR1_ID),
            PyRep::List(vec![PyRep::Integer(520_500), PyRep::Integer(520_501)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("DeleteManyFittings frees slots");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invbroker_split_multiadd_holds_and_item_ops_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select(&ctx, 5203, "demo", "demo-secret", CHAR1_ID).await;
    let ship_id = 100_000_001i64;

    // 1. Bound invbroker checks
    let bad_bound = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(999_999_888),
            "GetSelfInvItem",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("invalid bound id");
    assert_eq!(bad_bound.code, "InvalidBoundObject");

    let mgr_bid = bind_station_inv_mgr(&ctx, &mut sess, JITA_STATION_ID).await;
    let ship_bid = bind_inv_id(&ctx, &mut sess, mgr_bid, ship_id).await;
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(ship_bid),
        "GetSelfInvItem",
        &[],
        &kw,
        None,
    )
    .await
    .expect("GetSelfInvItem");
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(ship_bid),
        "GetAvailableTurretSlots",
        &[],
        &kw,
        None,
    )
    .await
    .expect("GetAvailableTurretSlots");

    let missing_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(199_555_444), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("GetInventoryFromId missing");
    assert_eq!(missing_inv.code, "FakeItemNotFound");

    let bind_denied = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(60_003_761), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MachoBindObject for other station denied");
    assert_eq!(bind_denied.code, "BindAuthorizationDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.station_id = Some(60_003_761);
        }
    });
    let wrong_station = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventory",
            &[PyRep::Integer(CONTAINER_HANGAR), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect_err("GetInventory wrong station");
    assert!(
        wrong_station.code == "CharacterNotAtStation" || wrong_station.code == "InvalidBoundObject",
        "unexpected code: {}",
        wrong_station.code
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });

    let bad_cont = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventory",
            &[PyRep::Integer(99_999), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect_err("GetInventory unsupported container");
    assert_eq!(bad_cont.code, "UnimplementedRPC");

    // 2. SplitStack error edges
    let trit_stack = 520_301i64;
    let trit_sing = 520_302i64;
    seed_item(&ctx, trit_stack, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 100, false);
    seed_item(&ctx, trit_sing, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    for (iid, qty) in [(trit_stack, 0i64), (trit_stack, 100), (trit_stack, i64::MAX), (trit_sing, 1), (999_111, 5)] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "SplitStack",
                &[PyRep::Integer(iid), PyRep::Integer(qty)],
                &kw,
                None,
            )
            .await
            .expect_err("bad SplitStack must fail");
        assert_eq!(err.code, "SplitFailed");
    }

    // 3. MultiAdd / Add container, courier, lock, capacity, SMA, FleetHangar, and freighter rules
    let courier_item = 520_303i64;
    let locked_item = 520_304i64;
    let pkg_can = 520_305i64;
    let asm_can = 520_306i64;
    let huge_trit = 520_307i64;
    let orca_id = 520_308i64;
    let pkg_rifter = 520_309i64;
    let asm_rifter = 520_310i64;
    let cargo_trit = 520_311i64;
    let charon_id = 520_312i64;
    let other_orca = 520_313i64;
    seed_item(&ctx, courier_item, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 10, false);
    seed_item(&ctx, locked_item, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Locked, 10, false);
    seed_item(&ctx, pkg_can, 3296, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, asm_can, 3296, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, huge_trit, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 200_000, false);
    seed_item(&ctx, orca_id, 28606, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, pkg_rifter, 587, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, asm_rifter, 587, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cargo_trit, 34, CHAR1_ID, asm_rifter, InventoryFlag::Cargo, 5, false);
    seed_item(&ctx, charon_id, 20185, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, other_orca, 28606, CHAR2_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(courier_item, "courier_cargo:123".into());
    });

    // MultiAdd rejects fitting flags
    let fit_multi = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(trit_stack)]),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::HiSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MultiAdd to fitting flag");
    assert_eq!(fit_multi.code, "InvalidLocation");

    // MultiAdd rejects active ship
    let active_multi = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(ship_id)]),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MultiAdd active ship");
    assert_eq!(active_multi.code, "CannotMoveActiveShip");

    let lock_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(locked_item)]),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MultiAdd locked item");
    assert_eq!(lock_err.code, "ItemLocked");

    let courier_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(courier_item)]),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MultiAdd courier cargo into CorpSAG1");
    assert_eq!(courier_err.code, "CannotDeliverCourierCargo");

    let pkg_can_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(trit_stack)]),
                PyRep::Integer(pkg_can),
                PyRep::Integer(InventoryFlag::None as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("move into packaged container");
    assert_eq!(pkg_can_err.code, "ContainerNotAssembled");

    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(huge_trit)]),
                PyRep::Integer(asm_can),
                PyRep::Integer(InventoryFlag::None as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("exceed container capacity");
    assert_eq!(cap_err.code, "CapacityExceeded");

    let can_in_can = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(pkg_can)]),
                PyRep::Integer(asm_can),
                PyRep::Integer(InventoryFlag::None as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("container in container");
    assert_eq!(can_in_can.code, "CannotPutContainerInContainer");

    let non_freight_can = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(asm_can)]),
                PyRep::Integer(charon_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("standard container in freighter");
    assert_eq!(non_freight_can.code, "OnlyFreightContainersAllowedInFreighters");

    // SMA & FleetHangar guards
    let no_sma = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(asm_rifter)]),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::ShipHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Rifter has no SMA");
    assert_eq!(no_sma.code, "NoShipMaintenanceBay");

    let cargo_in_sma = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(trit_stack)]),
                PyRep::Integer(orca_id),
                PyRep::Integer(InventoryFlag::ShipHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("cargo in Orca SMA");
    assert_eq!(cargo_in_sma.code, "CannotStoreCargoInSMA");

    let pkg_in_sma = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(pkg_rifter)]),
                PyRep::Integer(orca_id),
                PyRep::Integer(InventoryFlag::ShipHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("packaged ship in Orca SMA");
    assert_eq!(pkg_in_sma.code, "OnlyAssembledShipsInSMA");

    let full_ship_sma = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(asm_rifter)]),
                PyRep::Integer(orca_id),
                PyRep::Integer(InventoryFlag::ShipHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("ship carrying non-charge cargo into SMA");
    assert_eq!(full_ship_sma.code, "ShipHasNonChargeCargo");

    let no_fh = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(trit_stack)]),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::FleetHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Rifter has no fleet hangar");
    assert_eq!(no_fh.code, "NoFleetHangar");

    let foreign_fh = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(trit_stack)]),
                PyRep::Integer(other_orca),
                PyRep::Integer(InventoryFlag::FleetHangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("foreign Orca fleet hangar");
    assert_eq!(foreign_fh.code, "FleetHangarAccessDenied");

    // Stale item version on Add
    let mut ver_kw = HashMap::new();
    ver_kw.insert("expected_version".into(), PyRep::Integer(999));
    let ver_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(trit_stack),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &ver_kw,
            None,
        )
        .await
        .expect_err("stale item version on Add");
    assert_eq!(ver_err.code, "ItemVersionMismatch");

    // MultiMerge empty error
    let empty_merge = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MultiMerge", &[PyRep::List(vec![])], &kw, None)
        .await
        .expect_err("empty MultiMerge");
    assert_eq!(empty_merge.code, "StackFailed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn containers_alsc_clear_charges_repackage_and_trash_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select(&ctx, 5204, "demo", "demo-secret", CHAR1_ID).await;
    let ship_id = 100_000_001i64;

    let pkg_can_stack = 520_401i64;
    let non_can = 520_402i64;
    let alsc_id = 520_403i64;
    let alsc_item = 520_404i64;
    let weapon_id = 520_405i64;
    let loaded_ammo = 520_406i64;
    let cargo_filler = 520_407i64;
    let wrap_id = 520_408i64;
    let wrap_item = 520_409i64;
    let asset_safety_item = 520_410i64;

    seed_item(&ctx, pkg_can_stack, 3296, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 3, false);
    seed_item(&ctx, non_can, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, alsc_id, 17363, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, alsc_item, 34, CHAR1_ID, alsc_id, InventoryFlag::None, 50, false);
    seed_item(&ctx, weapon_id, 484, CHAR1_ID, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, loaded_ammo, 185, CHAR1_ID, ship_id, InventoryFlag::HiSlot0, 40, false);
    seed_item(&ctx, cargo_filler, 34, CHAR1_ID, ship_id, InventoryFlag::Cargo, 14_000, false);
    seed_item(&ctx, wrap_id, 3468, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, wrap_item, 34, CHAR1_ID, wrap_id, InventoryFlag::None, 25, false);
    seed_item(&ctx, asset_safety_item, 34, CHAR1_ID, JITA_STATION_ID, InventoryFlag::from_u16(36), 5, false);

    // 1. ValidateItemCanBeOpened / AssembleCargoContainer
    let open_pkg = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "ValidateItemCanBeOpened", &[PyRep::Integer(pkg_can_stack)], &kw, None)
        .await
        .expect_err("open packaged container");
    assert_eq!(open_pkg.code, "CantOpenPackagedItem");

    let open_non = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "ValidateItemCanBeOpened", &[PyRep::Integer(non_can)], &kw, None)
        .await
        .expect_err("open Tritanium");
    assert_eq!(open_non.code, "NotAContainer");

    let asm_non = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "AssembleCargoContainer", &[PyRep::Integer(non_can)], &kw, None)
        .await
        .expect_err("assemble non-container");
    assert_eq!(asm_non.code, "NotAContainer");

    // Assemble stack of 3 -> splits 1 assembled, leaves qty=2 packaged
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "AssembleCargoContainer", &[PyRep::Integer(pkg_can_stack)], &kw, None)
        .await
        .expect("AssembleCargoContainer splits stack");
    let pkg_rem = ctx.db.with_snapshot(|s| s.items.get(&pkg_can_stack).unwrap().quantity);
    assert_eq!(pkg_rem, 2);

    // 2. ClearCharges: full cargo -> CapacityExceeded; active module -> ModuleIsActive
    let clear_full = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "ClearCharges", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("ClearCharges full cargo");
    assert_eq!(clear_full.code, "CapacityExceeded");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&cargo_filler);
        snap.module_active_states.insert(weapon_id);
    });
    let clear_active = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "ClearCharges", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("ClearCharges active weapon");
    assert_eq!(clear_active.code, "ModuleIsActive");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&weapon_id);
    });

    // 3. ALSC lock + SetLabel audit log + TrashItems ContainerHasActiveAuditLog & RepackageItems edges
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "ALSCLockItems",
        &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(alsc_item)])],
        &kw,
        None,
    )
    .await
    .expect("ALSCLockItems");

    let trash_alsc_locked = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(alsc_item)])],
            &kw,
            None,
        )
        .await
        .expect_err("trash ALSC locked item");
    assert_eq!(trash_alsc_locked.code, "ItemLocked");

    // Renaming ALSC records an AlscLogEntry -> TrashItems on ALSC container fails with ContainerHasActiveAuditLog
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "SetLabel",
        &[PyRep::Integer(alsc_id), PyRep::String("Audit Box".into())],
        &kw,
        None,
    )
    .await
    .expect("SetLabel records ALSC audit log");
    let trash_audit = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(alsc_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("trash container with active audit log");
    assert_eq!(trash_audit.code, "ContainerHasActiveAuditLog");

    // Trash fitted weapon & Asset Safety item
    let trash_fitted = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(weapon_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("trash fitted item");
    assert_eq!(trash_fitted.code, "CannotTrashFittedItem");

    seed_item(&ctx, asset_safety_item, 34, CHAR2_ID, JITA_STATION_ID, InventoryFlag::Hangar, 5, false);
    let trash_foreign = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(asset_safety_item)])],
            &kw,
            None,
        )
        .await
        .expect_err("trash foreign item");
    assert_eq!(trash_foreign.code, "DeleteFailed");

    let trash_empty = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![])],
            &kw,
            None,
        )
        .await
        .expect_err("trash empty list");
    assert_eq!(trash_empty.code, "DeleteFailed");

    // RepackageItems: active ship, fitted module, inside container
    let repack_active = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(ship_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("repackage active ship");
    assert_eq!(repack_active.code, "CannotRepackageActiveShip");

    let repack_fitted = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(weapon_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("repackage fitted module");
    assert_eq!(repack_fitted.code, "CanNotUnassembleInThisItemLoc");

    let repack_cont = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(alsc_item)])],
            &kw,
            None,
        )
        .await
        .expect_err("repackage inside container");
    assert_eq!(repack_cont.code, "ItemInsideContainer");

    // BreakPlasticWrap on non-wrap vs valid Plastic Wrap
    let bad_wrap = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "BreakPlasticWrap", &[PyRep::Integer(non_can)], &kw, None)
        .await
        .expect_err("BreakPlasticWrap non-wrap");
    assert_eq!(bad_wrap.code, "InvalidPlasticWrap");
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw, None)
        .await
        .expect("BreakPlasticWrap succeeds");
    let moved_loc = ctx.db.with_snapshot(|s| s.items.get(&wrap_item).unwrap().location_id);
    assert_eq!(moved_loc, JITA_STATION_ID);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_targeting_warp_gate_subwarp_and_component_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = login_and_select(&ctx, 5205, "demo", "demo-secret", CHAR1_ID).await;
    let ship_id = 100_000_001i64;

    // 1. Docked beyonce errors
    let docked_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(JITA_GATE_ID), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("warp while docked");
    assert_eq!(docked_warp.code, "NotInSpace");

    let docked_gate = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(JITA_GATE_ID)],
            &kw,
            None,
        )
        .await
        .expect_err("gate jump while docked");
    assert_eq!(docked_gate.code, "ShipDocked");

    let docked_stop = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStop", &[], &kw, None)
        .await
        .expect_err("CmdStop while docked");
    assert_eq!(docked_stop.code, "NotInSpace");

    // 2. Undock into Jita
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let jita = ctx.systems.get_or_spawn(JITA_SYS_ID);
    let (_, mut ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let mut ship_combat = jita.get_combat_state(ship_id).await.unwrap().unwrap();

    // AdminGetBubbleID requires admin/GM role
    let admin_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "AdminGetBubbleID", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("AdminGetBubbleID non-admin");
    assert_eq!(admin_err.code, "InsufficientRoles");

    // beyonce.CallComponentFromClient errors
    let comp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[PyRep::Integer(ship_id), PyRep::String("bogusComponent".into()), PyRep::String("act".into())],
            &kw,
            None,
        )
        .await
        .expect_err("unsupported component");
    assert_eq!(comp_err.code, "UnsupportedComponentCall");

    // 3. Subwarp navigation errors (unbound fails BoundObjectRequired; bound tests nav validation)
    let unbound_nav = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("unbound CmdGotoDirection must fail");
    assert_eq!(unbound_nav.code, "BoundObjectRequired");

    let bind_beyonce = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(JITA_SYS_ID)), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject beyonce");
    let PyRep::Tuple(b_parts) = &bind_beyonce else {
        panic!("MachoBindObject beyonce tuple expected");
    };
    let beyonce_bid = bound_id_of(&b_parts[0]);

    let bad_dir = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_bid),
            "CmdGotoDirection",
            &[PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("non-finite direction");
    assert_eq!(bad_dir.code, "InvalidArguments");

    let bad_spd = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bid), "CmdSetSpeedFraction", &[PyRep::Real(-0.5)], &kw, None)
        .await
        .expect_err("negative speed fraction");
    assert_eq!(bad_spd.code, "InvalidSpeedFraction");

    let self_orbit = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_bid),
            "CmdOrbit",
            &[PyRep::Integer(ship_id), PyRep::Real(1000.0)],
            &kw,
            None,
        )
        .await
        .expect_err("self orbit");
    assert_eq!(self_orbit.code, "InvalidTarget");

    let missing_follow = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_bid),
            "CmdFollowBall",
            &[PyRep::Integer(999_888_111), PyRep::Real(500.0)],
            &kw,
            None,
        )
        .await
        .expect_err("follow missing ball");
    assert_eq!(missing_follow.code, "TargetNotFound");

    let neg_range = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_bid),
            "CmdOrbit",
            &[PyRep::Integer(JITA_GATE_ID), PyRep::Real(-10.0)],
            &kw,
            None,
        )
        .await
        .expect_err("negative orbit range");
    assert_eq!(neg_range.code, "InvalidRange");

    // Bastion / immobilization check (33400 Bastion Module I)
    let mod_bastion = 520_501i64;
    seed_item(&ctx, mod_bastion, 33400, CHAR1_ID, ship_id, InventoryFlag::HiSlot2, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_bastion);
    });
    ship_ball.max_velocity = 0.0;
    ship_combat.module_immobilized = true;
    jita.spawn_ball_with_combat(ship_ball.clone(), ship_combat.clone()).await.unwrap();
    let bastion_nav = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .expect_err("cannot move while bastion active");
    assert_eq!(bastion_nav.code, "CantMoveWhileModuleActive");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_bastion);
    });
    ship_ball.max_velocity = 300.0;
    ship_combat.module_immobilized = false;
    jita.spawn_ball_with_combat(ship_ball.clone(), ship_combat.clone()).await.unwrap();

    // 4. Targeting edge cases (self, missing, out of range, jammed, tethered)
    let tgt_far = 520_510i64;
    let tgt_tethered = 520_511i64;
    jita.spawn_ball_with_combat(
        Ball {
            id: tgt_far,
            type_id: 587,
            position: ship_ball.position + DVec3::new(1_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .unwrap();
    jita.spawn_ball_with_combat(
        Ball {
            id: tgt_tethered,
            type_id: 587,
            position: ship_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            is_tethered: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let self_lock_obo = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "AddTargetOBO",
            &[PyRep::Integer(ship_id), PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("self target lock OBO");
    assert_eq!(self_lock_obo.code, "TargetingFailed");

    let missing_lock = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(999_999_999)], &kw, None)
        .await
        .expect_err("missing target ball");
    assert_eq!(missing_lock.code, "TargetingFailed");

    let far_lock = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(tgt_far)], &kw, None)
        .await
        .expect_err("out of maxTargetRange");
    assert_eq!(far_lock.code, "TargetingFailed");

    let tether_lock = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(tgt_tethered)], &kw, None)
        .await
        .expect_err("tethered target lock");
    assert_eq!(tether_lock.code, "TargetInvulnerable");

    ship_combat.ecm_jammed_remaining_s = 30.0;
    jita.set_combat_state(ship_id, ship_combat.clone()).await.unwrap();
    let jammed_lock = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(tgt_far)], &kw, None)
        .await
        .expect_err("jammed target lock");
    assert_eq!(jammed_lock.code, "ShipJammed");
    ship_combat.ecm_jammed_remaining_s = 0.0;
    jita.set_combat_state(ship_id, ship_combat.clone()).await.unwrap();

    // 5. Warp & Gate edges
    let self_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(ship_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("warp to self");
    assert_eq!(self_warp.code, "InvalidTarget");

    let invalid_dest_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(tgt_tethered), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("non-celestial warp target");
    assert_eq!(invalid_dest_warp.code, "InvalidWarpDestination");

    let bad_dist_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION_ID), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("warp distance < 150km");
    assert_eq!(bad_dist_warp.code, "WarpDistanceTooShort");

    ship_combat.warp_scramble_strength = 2;
    jita.set_combat_state(ship_id, ship_combat.clone()).await.unwrap();
    let scram_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(JITA_GATE_ID), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("warp while scrambled");
    assert_eq!(scram_warp.code, "WarpScrambled");
    ship_combat.warp_scramble_strength = 0;

    ship_combat.capacitor = 0.0;
    jita.set_combat_state(ship_id, ship_combat.clone()).await.unwrap();
    let low_cap_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(JITA_GATE_ID), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect_err("warp with 0 capacitor");
    assert_eq!(low_cap_warp.code, "NotEnoughCapacitor");
    ship_combat.capacitor = ship_combat.max_capacitor;
    jita.set_combat_state(ship_id, ship_combat.clone()).await.unwrap();

    // Gate jump checks: unknown gate, wrong system gate, in-warp, capital ship
    let bad_gate = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStargateJump", &[PyRep::Integer(59_999_999)], &kw, None)
        .await
        .expect_err("unknown stargate");
    assert_eq!(bad_gate.code, "UnknownStargate");

    let wrong_sys_gate = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStargateJump", &[PyRep::Integer(PERIMETER_GATE_ID)], &kw, None)
        .await
        .expect_err("gate in wrong solar system");
    assert_eq!(wrong_sys_gate.code, "StargateWrongSystem");

    ship_ball.mode = MovementMode::Warp {
        destination: DVec3::new(1e7, 0.0, 0.0),
        origin: ship_ball.position,
        warp_speed_au_s: 3.0,
        total_distance: 1e7,
        elapsed_warp_time: 1.0,
        phase: eve_destiny::WarpPhase::Cruising,
    };
    jita.spawn_ball_with_combat(ship_ball.clone(), ship_combat.clone()).await.unwrap();
    let sess_timer_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStargateJump", &[PyRep::Integer(JITA_GATE_ID)], &kw, None)
        .await
        .expect_err("gate jump during 10s undock session-change timer");
    assert_eq!(sess_timer_err.code, "SessionChangeInProgress");

    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        common::TEST_CLOCK_BASE_UNIX + 15,
    )));
    let warp_gate = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStargateJump", &[PyRep::Integer(JITA_GATE_ID)], &kw, None)
        .await
        .expect_err("gate jump while in warp");
    assert_eq!(warp_gate.code, "ShipInWarp");

    // Capital ship (Archon 23757) cannot jump stargate
    let archon_id = 520_599i64;
    seed_item(&ctx, archon_id, 23757, CHAR1_ID, JITA_SYS_ID as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = archon_id;
        }
    });
    sess.ship_id = Some(archon_id);
    jita.spawn_ball_with_combat(
        Ball {
            id: archon_id,
            type_id: 23757,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 10.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .unwrap();
    let cap_gate = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdStargateJump", &[PyRep::Integer(JITA_GATE_ID)], &kw, None)
        .await
        .expect_err("capital ship cannot jump stargate");
    assert_eq!(cap_gate.code, "CapitalShipCannotJumpStargate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bookmarks_crud_limits_folders_and_batch_transfer_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess1 = login_and_select(&ctx, 5206, "demo", "demo-secret", CHAR1_ID).await;
    let mut sess2 = login_and_select(&ctx, 5207, "other", "other-secret", CHAR2_ID).await;

    // 1. InvalidBookmarkLabel & CantbookmarkEveSystem (Zarzakh) & FolderNotFound
    let bad_lbl = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::Integer(JITA_GATE_ID), PyRep::Integer(CHAR1_ID), PyRep::String("   ".into())],
            &kw,
            None,
        )
        .await
        .expect_err("whitespace bookmark label");
    assert_eq!(bad_lbl.code, "InvalidBookmarkLabel");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.solar_system_id = ZARZAKH_SYS_ID;
        }
    });
    let zarzakh_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[
                PyRep::Integer(ZARZAKH_SYS_ID as i64),
                PyRep::Integer(CHAR1_ID),
                PyRep::String("No Zarzakh".into()),
                PyRep::String("comment".into()),
                PyRep::Integer(4156),
                PyRep::Integer(ZARZAKH_SYS_ID as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("bookmark in Zarzakh");
    assert_eq!(zarzakh_err.code, "CantbookmarkEveSystem");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.solar_system_id = JITA_SYS_ID;
        }
    });

    let missing_folder = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[
                PyRep::Integer(JITA_GATE_ID),
                PyRep::Integer(999_888),
                PyRep::String("Valid".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-existent folder");
    assert_eq!(missing_folder.code, "FolderNotFound");

    // 2. Folder cap (5 personal folders max) & NoCorporation
    for i in 0..5 {
        ctx.dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "AddFolder",
            &[PyRep::String(format!("Folder {i}"))],
            &kw,
            None,
        )
        .await
        .expect("AddFolder under limit");
    }
    let folder_cap = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "AddFolder",
            &[PyRep::String("Folder 6".into())],
            &kw,
            None,
        )
        .await
        .expect_err("6th personal folder exceeds limit");
    assert_eq!(folder_cap.code, "MaxPersonalFoldersReached");

    // Shared folder without corp (args = [name, is_shared=true])
    sess1.corporation_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.corporation_id = 0;
        }
    });
    let no_corp_folder = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "AddFolder",
            &[PyRep::String("Corp Shared".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("shared folder without corporation");
    assert_eq!(no_corp_folder.code, "NoCorporation");

    // 3. Personal bookmark limit (500) + Update/Delete AccessDenied + Batch transfer limits (>20)
    let mut seeded_ids = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..500i64 {
            let bmid = 520_900 + i;
            seeded_ids.push(bmid);
            snap.bookmarks.insert(
                bmid,
                BookmarkRecord {
                    bookmark_id: bmid,
                    owner_id: CHAR1_ID,
                    label: format!("BM {i}"),
                    solar_system_id: JITA_SYS_ID,
                    x: 100.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: Some(JITA_GATE_ID),
                    memo: String::new(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
        snap.next_entity_id = 522_000;
        snap.bookmark_folders.insert(
            777_001,
            BookmarkFolderRecord {
                folder_id: 777_001,
                owner_id: CHAR2_ID,
                folder_name: "OtherPrivate".into(),
                is_shared: false,
                ..Default::default()
            },
        );
    });

    let max_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[
                PyRep::Integer(JITA_GATE_ID),
                PyRep::Integer(CHAR1_ID),
                PyRep::String("501st BM".into()),
                PyRep::String("".into()),
                PyRep::Integer(16),
                PyRep::Integer(JITA_SYS_ID as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("501st personal bookmark exceeds limit");
    assert_eq!(max_bm.code, "MaxBookmarksReached");

    // Pilot 2 cannot edit or delete Pilot 1's bookmark
    let edit_denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "bookmark",
            None,
            "UpdateBookmark",
            &[PyRep::Integer(seeded_ids[0]), PyRep::Integer(CHAR1_ID), PyRep::String("Hijack".into())],
            &kw,
            None,
        )
        .await
        .expect_err("foreign UpdateBookmark must fail");
    assert_eq!(edit_denied.code, "AccessDenied");

    let del_denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "bookmark",
            None,
            "DeleteBookmarks",
            &[PyRep::List(vec![PyRep::Integer(seeded_ids[0])])],
            &kw,
            None,
        )
        .await
        .expect_err("foreign DeleteBookmarks must fail");
    assert_eq!(del_denied.code, "AccessDenied");

    // CopyBookmarksToFolderAndSubfolder > 20 items -> BookmarkCopyBatchLimitExceeded
    let too_many: Vec<PyRep> = seeded_ids[0..21].iter().map(|&id| PyRep::Integer(id)).collect();
    let batch_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "CopyBookmarksToFolderAndSubfolder",
            &[PyRep::List(too_many), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err(">20 copy batch fails");
    assert_eq!(batch_err.code, "BookmarkCopyBatchLimitExceeded");

    // Move to foreign private folder -> BookmarkFolderAccessDenied
    let folder_acl_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "MoveBookmarksToFolderAndSubfolder",
            &[PyRep::List(vec![PyRep::Integer(seeded_ids[0])]), PyRep::Integer(777_001)],
            &kw,
            None,
        )
        .await
        .expect_err("move to foreign folder must fail");
    assert_eq!(folder_acl_err.code, "BookmarkFolderAccessDenied");
}
