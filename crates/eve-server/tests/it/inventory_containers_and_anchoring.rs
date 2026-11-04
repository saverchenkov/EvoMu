//! Inventory containers, ALSC audit logging, container content restrictions,
//! freight container space rules, jetcan rules, and secure container anchoring.
//!
//! Parity rows:
//! - `feat:inventory.audit-log-container-events`
//! - `feat:inventory.container-contents-restrictions`
//! - `feat:inventory.freight-container-space-rules`
//! - `feat:inventory.jetcan-capacity-and-despawn`
//! - `feat:inventory.secure-container-anchoring`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const CHAR1_ID: i64 = 90_000_001;
const CHAR2_ID: i64 = 90_000_002;
const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYS_ID: u32 = 30_000_142;

async fn select_pilot(ctx: &ServerContext, session_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn rpc_kw(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep], kwargs: &[(&str, PyRep)]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw: HashMap<String, PyRep> = kwargs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn seed_item(snap: &mut eve_db::WorldStateSnapshot, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() });
}

async fn spawn_ship(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_combat(
        Ball { id, type_id, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(type_id),
    ).await;
}

#[tokio::test]
async fn test_audit_log_container_events_and_protection() {
    // parity: feat:inventory.audit-log-container-events
    eve_testkit::attest(&["feat:inventory.audit-log-container-events"]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 60_011, "demo", "demo-secret", CHAR1_ID).await;
    let mut sess2 = select_pilot(&ctx, 60_012, "other", "other-secret", CHAR2_ID).await;

    let (alsc_id, item_inside_id) = (820_001i64, 820_002i64);
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, alsc_id, 17363, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
        seed_item(snap, item_inside_id, 34, CHAR1_ID, alsc_id, InventoryFlag::None, 1000, false);
    });

    // 1. Configure ALSC: lockByDefault = true, passwordPerAction = true
    let _ = rpc_kw(&ctx, &mut sess1, "invbroker", "ALSCConfigSet", &[PyRep::Integer(alsc_id), PyRep::String("alsc-secret-pw".into())], &[
        ("lockByDefault", PyRep::Bool(true)), ("passwordPerAction", PyRep::Bool(true)), ("configBitmask", PyRep::Integer(3)),
    ]).await.expect("ALSCConfigSet");

    // 2. ALSCConfigGet returns lockByDefault and passwordPerAction
    let cfg_get = rpc(&ctx, &mut sess1, "invbroker", "ALSCConfigGet", &[PyRep::Integer(alsc_id)]).await.expect("ALSCConfigGet");
    assert_eq!(cfg_get.dict_get("hasPassword").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(cfg_get.dict_get("lockByDefault").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(cfg_get.dict_get("passwordPerAction").and_then(|v| v.as_bool()), Some(true));

    // 3. Rename container via invbroker.SetLabel (records ALSCActionRename)
    rpc(&ctx, &mut sess1, "invbroker", "SetLabel", &[PyRep::Integer(alsc_id), PyRep::String("My Vault".into())]).await.expect("SetLabel");

    // 4. Lock item inside ALSC (requires passwordPerAction)
    let bad_pw = rpc_kw(&ctx, &mut sess1, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(item_inside_id)])], &[("password", PyRep::String("wrong".into()))]).await.expect_err("ALSCLockItems wrong pw");
    assert_eq!(bad_pw.code, "ALSCPasswordNeededToLock");

    rpc_kw(&ctx, &mut sess1, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(item_inside_id)])], &[("password", PyRep::String("alsc-secret-pw".into()))]).await.expect("ALSCLockItems");

    // Unlock item inside ALSC
    rpc_kw(&ctx, &mut sess1, "invbroker", "ALSCUnlockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(item_inside_id)])], &[("password", PyRep::String("alsc-secret-pw".into()))]).await.expect("ALSCUnlockItems");

    // 5. Password failure on SetPassword
    let set_pw_err = rpc(&ctx, &mut sess1, "invbroker", "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("wrong-old".into()), PyRep::String("new-pass".into())]).await.expect_err("SetPassword wrong old pw");
    assert_eq!(set_pw_err.code, "InvalidPassword");

    // 6. Non-owner / unauthorized corp pilot accessing ALSCLogGet is denied
    let log_denied = rpc_kw(&ctx, &mut sess2, "invbroker", "ALSCLogGet", &[PyRep::Integer(alsc_id)], &[("password", PyRep::String("alsc-secret-pw".into()))]).await.expect_err("Other pilot cannot view ALSC logs");
    assert_eq!(log_denied.code, "ALSCAccessDenied");

    // 7. Owner retrieves logs
    let logs_rep = rpc_kw(&ctx, &mut sess1, "invbroker", "ALSCLogGet", &[PyRep::Integer(alsc_id)], &[("password", PyRep::String("alsc-secret-pw".into()))]).await.expect("ALSCLogGet");
    let PyRep::List(entries) = logs_rep.dict_get("entries").expect("entries list") else { panic!("expected list"); };
    assert!(entries.len() >= 4, "Must have recorded config changes, renames, locks, password failure");

    // 8. Containers with log entries younger than 3 weeks cannot be repackaged, reprocessed, or trashed
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&item_inside_id); });
    let repackage_err = rpc(&ctx, &mut sess1, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(alsc_id)])]).await.expect_err("Repackage with log");
    assert_eq!(repackage_err.code, "ContainerHasActiveAuditLog");

    let reproc_err = rpc(&ctx, &mut sess1, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(alsc_id)])]).await.expect_err("Reprocess with log");
    assert_eq!(reproc_err.code, "ContainerHasActiveAuditLog");

    let trash_err = rpc(&ctx, &mut sess1, "invbroker", "TrashItems", &[PyRep::List(vec![PyRep::Integer(alsc_id)])]).await.expect_err("Trash with log");
    assert_eq!(trash_err.code, "ContainerHasActiveAuditLog");
}

#[tokio::test]
async fn test_container_contents_restrictions_and_manual_reload() {
    // parity: feat:inventory.container-contents-restrictions
    eve_testkit::attest(&["feat:inventory.container-contents-restrictions"]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_021, "demo", "demo-secret", CHAR1_ID).await;

    let (can_id, inside_item_id, ship_id) = (821_001i64, 821_002i64, 821_010i64);
    let (cargo_can_id, cargo_ammo_id, weapon_id) = (821_011i64, 821_012i64, 821_013i64);

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, can_id, 3465, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
        seed_item(snap, inside_item_id, 34, CHAR1_ID, can_id, InventoryFlag::None, 500, false);
        seed_item(snap, ship_id, 587, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
        seed_item(snap, weapon_id, 484, CHAR1_ID, ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(snap, cargo_can_id, 3465, CHAR1_ID, ship_id, InventoryFlag::Cargo, 1, true);
        seed_item(snap, cargo_ammo_id, 178, CHAR1_ID, cargo_can_id, InventoryFlag::None, 200, false);

        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(JITA_STATION_ID);
        }
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = Some(JITA_STATION_ID);

    // 1. Repackage item inside container fails with ItemInsideContainer
    let err_repack = rpc(&ctx, &mut sess, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(inside_item_id)])]).await.expect_err("Repackage inside container");
    assert_eq!(err_repack.code, "ItemInsideContainer");

    // 2. Repair item inside container fails with ItemInsideContainer
    let err_repair = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStation", &[PyRep::List(vec![PyRep::Integer(inside_item_id)])]).await.expect_err("Repair inside container");
    assert_eq!(err_repair.code, "ItemInsideContainer");

    // 3. Contract creation with item inside container fails with ItemInsideContainer
    let err_contract = rpc(&ctx, &mut sess, "contractProxy", "CreateContract", &[
        PyRep::String("ItemExchange".into()), PyRep::Integer(1), PyRep::Integer(86400),
        PyRep::Real(100_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0),
        PyRep::None, PyRep::None, PyRep::Integer(JITA_STATION_ID), PyRep::Integer(JITA_STATION_ID),
        PyRep::List(vec![PyRep::Integer(inside_item_id)]), PyRep::List(vec![]),
    ]).await.expect_err("Contract item inside container");
    assert_eq!(err_contract.code, "ItemInsideContainer");

    // 4. Market sell order with item inside container fails with ItemInsideContainer
    let err_sell = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[
        PyRep::List(vec![
            PyRep::Dict(vec![
                (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
                (PyRep::String("typeID".into()), PyRep::Integer(34)),
                (PyRep::String("itemID".into()), PyRep::Integer(inside_item_id)),
                (PyRep::String("price".into()), PyRep::Real(10.0)),
                (PyRep::String("quantity".into()), PyRep::Integer(100)),
                (PyRep::String("duration".into()), PyRep::Integer(1)),
            ])
        ])
    ]).await.expect_err("Sell order inside container");
    assert_eq!(err_sell.code, "ItemInsideContainer");

    // 5. Deliver item inside container fails with ItemInsideContainer
    let err_deliv = rpc(&ctx, &mut sess, "invbroker", "DeliverToCorpHangar", &[PyRep::List(vec![PyRep::Integer(inside_item_id)]), PyRep::Integer(1)]).await.expect_err("Deliver inside container");
    assert_eq!(err_deliv.code, "ItemInsideContainer");

    // 6. Ammunition inside container in cargo is NOT consumable by auto-reload
    let consumable = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&cargo_ammo_id).unwrap();
        snap.is_item_consumable_by(it, CHAR1_ID, Some(JITA_STATION_ID), Some(ship_id))
    });
    assert!(!consumable, "Ammo in container cannot be auto-reloaded");

    // 7. Manual LoadAmmo succeeds and loads ammo from inside container into weapon
    rpc(&ctx, &mut sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(weapon_id)]), PyRep::List(vec![PyRep::Integer(cargo_ammo_id)])]).await.expect("LoadAmmo from container");

    let loaded = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 178)
    });
    assert!(loaded, "Weapon should now contain loaded ammo");
}

#[tokio::test]
async fn test_freight_container_space_rules_and_holds() {
    // parity: feat:inventory.freight-container-space-rules
    eve_testkit::attest(&["feat:inventory.freight-container-space-rules"]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_031, "demo", "demo-secret", CHAR1_ID).await;

    let (ship_id, freight_can_id, standard_can_id) = (822_001i64, 822_002i64, 822_003i64);
    let sys_id = 30_000_139u32;

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, ship_id, 20183, CHAR1_ID, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(snap, freight_can_id, 24445, CHAR1_ID, ship_id, InventoryFlag::Cargo, 1, false); // Packaged Freight Container (group 649)
        seed_item(snap, standard_can_id, 3465, CHAR1_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;

    spawn_ship(&ctx, sys_id, ship_id, 20183).await;

    // 1. Jettison/drop packaged freight container into space: automatically assembles itself (singleton = true)
    rpc(&ctx, &mut sess, "ship", "Drop", &[PyRep::List(vec![PyRep::Integer(freight_can_id)])]).await.expect("Drop freight container");
    let is_assembled = ctx.db.with_snapshot(|snap| {
        snap.items.get(&freight_can_id).map(|i| i.singleton && i.location_id == sys_id as i64).unwrap_or(false)
    });
    assert!(is_assembled, "Launched freight container must automatically assemble (singleton = true)");

    // 2. Freight container cannot be anchored
    let anchor_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(freight_can_id)]).await.expect_err("Anchor freight container");
    assert_eq!(anchor_err.code, "CannotAnchorFreightContainer");

    // 3. Freight container cannot be password protected
    let pw_err = rpc(&ctx, &mut sess, "invbroker", "SetPassword", &[PyRep::Integer(freight_can_id), PyRep::Integer(1), PyRep::String("pw".into()), PyRep::String("".into())]).await.expect_err("Password on freight container");
    assert_eq!(pw_err.code, "CannotPasswordProtectFreightContainer");

    // 4. Scooping into small ship hold with insufficient cargo fails with CapacityExceeded
    let small_ship_id = 822_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, small_ship_id, 587, CHAR1_ID, sys_id as i64, InventoryFlag::None, 1, true);
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = small_ship_id;
        }
    });
    spawn_ship(&ctx, sys_id, small_ship_id, 587).await;
    sess.ship_id = Some(small_ship_id);

    let scoop_err = rpc(&ctx, &mut sess, "ship", "Scoop", &[PyRep::Integer(freight_can_id)]).await.expect_err("Scoop into small ship");
    assert_eq!(scoop_err.code, "CapacityExceeded");

    // 5. Scooping into freighter hold succeeds and stays assembled (singleton = true)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = ship_id;
        }
    });
    sess.ship_id = Some(ship_id);
    rpc(&ctx, &mut sess, "ship", "Scoop", &[PyRep::Integer(freight_can_id)]).await.expect("Scoop into freighter");
    let stays_assembled = ctx.db.with_snapshot(|snap| {
        snap.items.get(&freight_can_id).map(|i| i.singleton && i.location_id == ship_id).unwrap_or(false)
    });
    assert!(stays_assembled, "Scooped freight container must remain assembled");

    // 6. Only freight containers allowed assembled in freighter and jump freighter holds
    let move_std_err = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(standard_can_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect_err("Standard container in freighter");
    assert_eq!(move_std_err.code, "OnlyFreightContainersAllowedInFreighters");
}

#[tokio::test]
async fn test_jetcan_capacity_and_despawn_rules() {
    // parity: feat:inventory.jetcan-capacity-and-despawn
    eve_testkit::attest(&["feat:inventory.jetcan-capacity-and-despawn"]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_041, "demo", "demo-secret", CHAR1_ID).await;

    let (ship_id, ore_id, oversize_ore_id) = (823_001i64, 823_002i64, 823_003i64);
    let sys_id = 30_000_139u32;

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, ship_id, 20183, CHAR1_ID, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(snap, ore_id, 1230, CHAR1_ID, ship_id, InventoryFlag::Cargo, 50_000, false); // 5,000 m3 <= 55,000 m3
        seed_item(snap, oversize_ore_id, 1230, CHAR1_ID, ship_id, InventoryFlag::Cargo, 600_000, false); // 60,000 m3 > 55,000 m3
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;

    spawn_ship(&ctx, sys_id, ship_id, 20183).await;

    // 1. Jettisoning items exceeding 55,000 m3 capacity fails with CapacityExceeded
    let cap_err = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(oversize_ore_id)])]).await.expect_err("Jettison > 55,000 m3");
    assert_eq!(cap_err.code, "CapacityExceeded");

    // 2. Jettisoning <= 55,000 m3 succeeds and creates jetcan (type 23) with 55,000 m3 capacity
    let jet_rep = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_id)])]).await.expect("Jettison within 55,000 m3");
    let jetcan_id = match &jet_rep {
        PyRep::Tuple(parts) => parts.first().and_then(|p| match p {
            PyRep::List(ids) => ids.first().and_then(|id| id.as_i64()),
            PyRep::Integer(id) => Some(*id),
            _ => None,
        }),
        PyRep::Integer(id) => Some(*id),
        _ => None,
    }.expect("jetcan item ID");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&jetcan_id).map(|i| i.type_id)), Some(23));

    // 3. Jetcan cannot be scooped into a ship hold
    let scoop_err = rpc(&ctx, &mut sess, "ship", "Scoop", &[PyRep::Integer(jetcan_id)]).await.expect_err("Scoop jetcan");
    assert_eq!(scoop_err.code, "CannotPutContainerInContainer");

    // 4. Despawn lifetime: despawns as soon as emptied
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ore_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Loot ore from jetcan");
    assert!(ctx.db.get_item(jetcan_id).is_none(), "Empty jetcan must despawn immediately");
}

#[tokio::test]
async fn test_secure_container_anchoring_rules_and_security() {
    // parity: feat:inventory.secure-container-anchoring
    eve_testkit::attest(&["feat:inventory.secure-container-anchoring"]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_051, "demo", "demo-secret", CHAR1_ID).await;

    let (ship_id, cont_id) = (824_001i64, 824_002i64);
    let low_high_sec = ctx.sde.solar_systems.values().find(|s| s.security_status >= 0.45 && s.security_status < 0.75 && s.solar_system_id < 31_000_000).map(|s| s.solar_system_id).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, ship_id, 587, CHAR1_ID, JITA_SYS_ID as i64, InventoryFlag::None, 1, true);
        seed_item(snap, cont_id, 3467, CHAR1_ID, ship_id, InventoryFlag::Cargo, 1, true);
        let sk = snap.skills.entry(CHAR1_ID).or_default();
        sk.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = JITA_SYS_ID;
            ch.station_id = None;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(JITA_SYS_ID);
    sess.station_id = None;

    spawn_ship(&ctx, JITA_SYS_ID, ship_id, 587).await;

    // 1. Refused in sec >= 0.8 with CantAnchorInHighSec08
    let hs08_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_id)]).await.expect_err("Secure container in >= 0.8");
    assert_eq!(hs08_err.code, "CantAnchorInHighSec08");

    // 2. In sec < 0.8 (e.g. 0.70): anchoring succeeds
    sess.solar_system_id = Some(low_high_sec);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR1_ID) { ch.solar_system_id = low_high_sec; }
        if let Some(sh) = snap.items.get_mut(&ship_id) { sh.location_id = low_high_sec as i64; }
    });
    spawn_ship(&ctx, low_high_sec, ship_id, 587).await;

    let anchor_rep = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_id)]).await.expect("posMgr.AnchorStructure in sec < 0.8");
    let now_u = ctx.now_unix();
    // 3. Anchored container lasts 30 days
    assert_eq!(anchor_rep.dict_get("despawnUnix"), Some(&PyRep::Integer((now_u + 30 * 86_400) as i64)));

    // Opening resets timer
    ctx.advance_secs(100);
    let open_now = ctx.now_unix();
    rpc(&ctx, &mut sess, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(cont_id)]).await.expect("OpenShippingUnitStorage");

    let updated_start = ctx.db.with_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.get(&cont_id).and_then(|m| m.anchor_start_unix)
    });
    assert_eq!(updated_start, Some(open_now), "Opening anchored container must reset anchor start timer");

    // 4. Attacking container in high-sec brings Criminal flag and CONCORD response
    let sec_status = ctx.sde.solar_systems.get(&low_high_sec).unwrap().security_status;
    let criminal_eval = ctx.crimewatch.write().unwrap().handle_combat_aggression_with_pod(
        CHAR2_ID, CHAR1_ID, ship_id, low_high_sec, sec_status, false, false, open_now,
    );
    assert!(
        matches!(criminal_eval, eve_server::combat::crimewatch::AggressionEvaluation::CriminalFlagTriggered { .. }),
        "Attacking secure container in high-sec must trigger Criminal flag"
    );

    // 5. Anchored secure containers are immune to probes
    let scan_results = common::probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
    let found_container = scan_results.iter().any(|r| {
        r.dict_get("targetID").and_then(|v| v.as_i64()) == Some(cont_id)
            || r.dict_get("name").and_then(|v| v.as_str()) == Some("Small Secure Container")
    });
    assert!(!found_container, "Anchored secure container must be immune to probes");
}
