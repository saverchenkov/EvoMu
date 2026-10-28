//! Upwell Structure Clone Bay, Service Modules, Repairshop, Access Profiles, Directory,
//! Fitting & Hull Types parity tests (`plan_upwell_structures_closure_part2.rs`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const GUEST: i64 = 90_000_002;
const OUTSIDER: i64 = 90_000_003;
const CORP_GRUNT: i64 = 90_000_004;
const JITA: u32 = 30_000_142;
const JITA_44: i64 = 60_003_760;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn inv_add(ctx: &ServerContext, sess: &mut SessionState, item_id: i64, target_loc: i64, flag: InventoryFlag, online: bool) -> Result<PyRep, BridgeRpcError> {
    let mut kw = HashMap::new();
    kw.insert("online".into(), PyRep::Bool(online));
    ctx.dispatch_rpc(sess, "invbroker", None, "Add", &[PyRep::Integer(item_id), PyRep::Integer(target_loc), PyRep::Integer(flag as i64)], &kw, None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }
fn kv_bool(rep: &PyRep, key: &str) -> bool { rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false) }

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Services Corp".into()), PyRep::String("USVC".into())]).await.expect("AddCorporation");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_services_clone_bay_modules_and_repairshop() {
    // parity: feat:structures.service-clone-bay, feat:structures.service-modules, feat:structures.repairshop-service-quotes-and-restoration
    eve_testkit::attest(&[
        "feat:structures.service-clone-bay",
        "feat:structures.service-modules",
        "feat:structures.repairshop-service-quotes-and-restoration",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7203).await;
    let st_pos = open_space(&ctx, JITA, 2.0);
    move_ship(&ctx, &sess, st_pos).await;
    let st_id = online_structure(&ctx, &mut sess, 35832, "Services Astrahus").await;
    let ship_id = sess.ship_id.unwrap();

    // 1. Service module fitting via live invbroker.Add RPC, fuel-gated activation via invbroker.Add (flag 133), and 1-arg OnStructureServiceChanged.
    let clone_mod = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    let fit_res = inv_add(&ctx, &mut sess, clone_mod, st_id, InventoryFlag::ServiceSlot0, false).await.expect("FitModule via invbroker.Add");
    assert!(!kv_bool(&fit_res, "online"));
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect_err("no fuel").code, "InsufficientStructureFuelToOnline");

    let online_fuel = (attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_ONLINE) * 0.75).round() as i32;
    let hourly_fuel = (attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_PER_HOUR) * 0.75).round() as i32;
    let short_fuel = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, online_fuel - 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(short_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(i64::from(online_fuel - 1))]).await.expect("short fuel");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect_err("539 < 540").code, "InsufficientStructureFuelToOnline");
    let fuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 1 + hourly_fuel * 2);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(i64::from(1 + hourly_fuel * 2))]).await.expect("Fuel bay via invbroker.Add");
    let _ = ctx.drain_notifications_for_char(OWNER);
    let on_res = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect("OnlineModule");
    assert!(kv_bool(&on_res, "online"));
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, (hourly_fuel * 2) as u32, "SetModuleOnline must deduct 540 startup fuel blocks");
    let reproc_mod = grant(&ctx, OWNER, ship_id, 35899, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, reproc_mod, st_id, InventoryFlag::ServiceSlot1, false).await.expect("fit reproc");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(reproc_mod)]).await.expect_err("16 < 2160").code, "InsufficientStructureFuelToOnline");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(st_id), PyRep::Integer(2)]).await.expect("has service"), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(OWNER).into_iter().any(|n| n.event == "OnStructureServiceChanged" && n.args == vec![PyRep::Integer(st_id)]), "OnStructureServiceChanged must emit exact 1-arg [structure_id]");

    // 2. Clone Bay: jump clone + medical home station + live pod-death respawn into Upwell Clone Bay structure!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });
    assert!(kv_i64(&rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[]).await.expect("InstallCloneInStructure"), "jumpCloneID") > 0);
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(st_id)]).await.expect("SetHomeStation").as_i64(), Some(st_id));

    // Leave ship in structure hangar, undock in capsule, self-destruct capsule in space -> respawns INSIDE st_id!
    rpc(&ctx, &mut sess, "ship", "LeaveShip", &[]).await.expect("LeaveShip in structure");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock capsule");
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.expect("Start self-destruct");
    ctx.advance_virtual_clock(125);
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "GetHomeStation", &[]).await.expect("Sync session after pod self-destruct").as_i64(), Some(st_id));
    assert_eq!(sess.structure_id, Some(st_id), "Pod death with online Clone Bay must respawn pilot inside structure");
    assert_eq!(sess.station_id, None);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&OWNER).unwrap();
        assert_eq!((ch.structure_id, ch.station_id, ch.home_station_id), (Some(st_id), None, st_id));
        assert_eq!(snap.items.get(&ch.active_ship_id).map(|i| i.location_id), Some(st_id));
    });
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Re-board ship in structure");

    // Advance 100 hours so fuel runs dry; Clone Bay goes offline, structure drops to LowPower, and pod death falls back to starter school NPC station!
    ctx.advance_virtual_clock(100 * 3_600);
    let st_low = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert!(st_low.fuel_blocks < hourly_fuel as u32);
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[]).await.expect_err("offline jc").code.as_str(), "NoCloneBayService" | "CloneBayServiceOffline"));
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(st_id)]).await.expect_err("offline home").code, "CloneBayServiceOffline");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(st_id), PyRep::Integer(2)]).await.expect("svc off"), PyRep::Bool(false));

    rpc(&ctx, &mut sess, "ship", "LeaveShip", &[]).await.expect("LeaveShip for offline pod test");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock capsule offline");
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.expect("Start self-destruct 2");
    ctx.advance_virtual_clock(125);
    let _ = rpc(&ctx, &mut sess, "charMgr", "GetHomeStation", &[]).await.expect("Sync session after offline Clone Bay self-destruct");
    assert_eq!(sess.structure_id, None, "Offline Clone Bay must not respawn pilot inside structure");
    let fallback_station = sess.station_id.expect("Fallback starter school NPC station");
    assert!(fallback_station < 1_000_000_000_000);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.characters.get(&OWNER).unwrap().home_station_id, fallback_station, "home_station_id reset to starter school station"));

    // Return pilot to st_id and re-board ship_id for repairSvc tests.
    let pod_after = sess.ship_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None; ch.structure_id = Some(st_id); ch.solar_system_id = JITA;
        snap.items.get_mut(&pod_after).unwrap().location_id = st_id;
    });
    sess.station_id = None; sess.structure_id = Some(st_id); sess.solar_system_id = Some(JITA);
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Board ship_id");

    // 3. repairSvc: LowPower/Abandoned/NoCore/ACL gates, undocked hangar rejection, container rejection, CorpSAG role gate, 100% owner-corp tax, and free 0% repair!
    let mod_id = grant(&ctx, OWNER, ship_id, 3170, InventoryFlag::HiSlot0, 1);
    let drone_id = grant(&ctx, OWNER, ship_id, 2454, InventoryFlag::DroneBay, 1);
    let owner_corp_id = ctx.db.with_snapshot(|snap| snap.structures.get(&st_id).unwrap().owner_corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 500);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(50_000_000.0);
        snap.corporations.get_mut(&owner_corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(1_000_000.0));
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.location_id = st_id;
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 50.0);
        sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 100.0);
        snap.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 20.0);
        snap.items.get_mut(&drone_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 15.0);
        snap.items.get_mut(&drone_id).unwrap().dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 25.0);
    });

    // While st_id is LowPower (unfueled), both GetRepairQuotes and RepairItemsInStructure fail with StructureServiceOffline!
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("low power quote").code, "StructureServiceOffline");
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("low power repair").code, "StructureServiceOffline");

    // NaN/negative budget, Abandoned, and QuantumCoreRequired rejections.
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(f64::NAN)]).await.expect_err("NaN").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(-5.0)]).await.expect_err("neg").code, "InvalidArgument");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("abandoned").code, "StructureAbandoned");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false;
    });
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("no core").code, "QuantumCoreRequired");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true);

    // Refuel st_id and re-online clone_mod so st_id is FullPower!
    let refuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(refuel_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("refuel");
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect("re-online clone bay");

    // ACL-blocked outsider, undocked hangar item, container-nested item, and restricted CorpSAG2 without role are all rejected.
    let out_ship = grant(&ctx, OUTSIDER, st_id, 606, InventoryFlag::Hangar, 1);
    let mut out_sess = SessionState::new(7209);
    out_sess.user_id = Some(3); out_sess.character_id = Some(OUTSIDER); out_sess.corporation_id = Some(1000009); out_sess.solar_system_id = Some(JITA); out_sess.structure_id = Some(st_id); out_sess.ship_id = Some(out_ship);
    let box_id = grant(&ctx, OWNER, st_id, 17366, InventoryFlag::Hangar, 1);
    let boxed_mod = grant(&ctx, OWNER, box_id, 3170, InventoryFlag::Cargo, 1);
    let corp_div2_mod = grant(&ctx, owner_corp_id, st_id, 3170, InventoryFlag::CorpSAG2, 1);
    let mut grunt_rep_sess = SessionState::new(7210);
    grunt_rep_sess.user_id = Some(4); grunt_rep_sess.character_id = Some(CORP_GRUNT); grunt_rep_sess.corporation_id = Some(owner_corp_id); grunt_rep_sess.solar_system_id = Some(JITA); grunt_rep_sess.structure_id = Some(st_id); grunt_rep_sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = OUTSIDER; ch.corporation_id = 1000009; ch.active_ship_id = out_ship;
        snap.characters.insert(OUTSIDER, ch);
        let mut cg = snap.characters.get(&OWNER).unwrap().clone();
        cg.character_id = CORP_GRUNT; cg.corporation_id = owner_corp_id; cg.structure_id = Some(st_id);
        snap.characters.insert(CORP_GRUNT, cg);
        snap.corporations.get_mut(&owner_corp_id).unwrap().members.insert(CORP_GRUNT);
        snap.structure_docking_acl_denied.entry(st_id).or_default().insert(OUTSIDER);
        for iid in [out_ship, boxed_mod, corp_div2_mod] {
            snap.items.get_mut(&iid).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        }
    });
    assert_eq!(rpc(&ctx, &mut out_sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(out_ship)])]).await.expect_err("acl blocked").code, "StructureRepairAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(boxed_mod)])]).await.expect_err("inside container").code, "ItemInsideContainer");
    assert_eq!(rpc(&ctx, &mut grunt_rep_sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(corp_div2_mod)])]).await.expect_err("corp role denied").code, "CorpRoleDenied");
    let hangar_mod = grant(&ctx, OWNER, st_id, 3170, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&hangar_mod).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 10.0); });
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock to test tethered hangar repair rejection");
    move_ship(&ctx, &sess, st_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = ctx.systems.get_or_spawn(JITA).step_manual(1.0).await;
    if let Ok(mut bp) = ctx.systems.get_or_spawn(JITA).ballpark.write() { bp.combat_states.entry(ship_id).or_default().is_tethered = true; }
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(hangar_mod)])]).await.expect_err("undocked hangar repair").code, "NotDockedInStructure");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Re-dock after tethered hangar check");
    ctx.db.with_mut_snapshot(|snap| {
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 50.0);
        sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 100.0);
    });

    // At 5% structure repair tax: baseCost is 0.0, totalRepairCost == serviceCharge > 0.0, and 100% of paid ISK credits owner corp division 1!
    let quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("GetRepairQuotes");
    let PyRep::Dict(q_entries) = quotes else { panic!("expected dict") };
    let (_, PyRep::List(q_rows)) = &q_entries[0] else { panic!("expected quote list") };
    for iid in [ship_id, mod_id, drone_id] {
        assert!(q_rows.iter().any(|r| kv_i64(r, "itemID") == iid && r.dict_get("baseCost").and_then(|v| v.as_f64()) == Some(0.0) && r.dict_get("totalRepairCost").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0));
    }
    let expected_tax_isk: f64 = q_rows.iter().filter_map(|r| r.dict_get("serviceCharge").and_then(|v| v.as_f64())).sum();
    assert!(expected_tax_isk > 0.0);

    let jita_sys = ctx.systems.get_or_spawn(JITA);
    if let Ok(mut bp) = jita_sys.ballpark.write() {
        let cs = bp.combat_states.entry(ship_id).or_insert_with(eve_destiny::CombatState::default);
        cs.max_shield_hp = 500.0; cs.shield_hp = 50.0;
    }
    let rep_res = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("RepairItemsInStructure");
    let paid = rep_res.dict_get("totalCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((paid - expected_tax_isk).abs() < 1e-2, "Upwell repair must charge only the structure owner tax");
    ctx.db.with_snapshot(|snap| {
        for iid in [ship_id, mod_id, drone_id] {
            let it = snap.items.get(&iid).unwrap();
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::DAMAGE) && !it.dynamic_attributes.contains_key(&attr_ids::ARMOR_DAMAGE));
        }
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - (50_000_000.0 - paid)).abs() < 1e-2);
        let corp = snap.corporations.get(&owner_corp_id).unwrap();
        assert!((corp.division_balance(1).to_wire_f64() - (1_000_000.0 + paid)).abs() < 1e-2, "100% of structure repair fee credited to owner corp");
        let cj = snap.corp_wallet_journals.get(&(owner_corp_id, 1)).expect("corp div 1 journal");
        assert!(cj.iter().any(|e| e.ref_type == "RepairBill" && (e.amount - paid).abs() < 1e-2));
    });

    // At 0% structure repair tax: Upwell repair quote AND repair execution are completely FREE (0.0 ISK)!
    let corp_bal_before_zero = ctx.db.with_snapshot(|snap| snap.corporations.get(&owner_corp_id).unwrap().division_balance(1).to_wire_f64());
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 0);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 30.0);
        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(40_000_000.0);
    });
    let base_quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("0% quotes");
    let PyRep::Dict(bq_entries) = base_quotes else { panic!("expected dict") };
    let (_, PyRep::List(bq_rows)) = &bq_entries[0] else { panic!("expected list") };
    assert_eq!(bq_rows[0].dict_get("totalRepairCost").and_then(|v| v.as_f64()), Some(0.0), "0% tax Upwell repair quote is 0.0 ISK");
    let free_rep = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("0% repair");
    assert_eq!(free_rep.dict_get("totalCostISK").and_then(|v| v.as_f64()), Some(0.0), "0% tax Upwell repair costs 0.0 ISK");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.get(&ship_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - 40_000_000.0).abs() < 1e-4);
        assert!((snap.corporations.get(&owner_corp_id).unwrap().division_balance(1).to_wire_f64() - corp_bal_before_zero).abs() < 1e-4);
    });
}

#[tokio::test]
async fn test_upwell_profiles_directory_fitting_and_types() {
    // parity: feat:structures.structure-fitting, feat:structures.structure-types
    eve_testkit::attest(&[
        "feat:structures.structure-fitting",
        "feat:structures.structure-types",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7204).await;

    // 1. Structure types & SDE profiles across Citadels, Engineering Complexes, Refineries, FLEX, Faction Fortizars, and Palatine Keepstar.
    for (tid, name, exp_sh, exp_ar, exp_hu, hi, med, low, svc, rig) in [
        (35832u32, "Astrahus", 3600000.0, 1800000.0, 1800000.0, 4, 4, 3, 3, 3),
        (35833, "Fortizar", 6750000.0, 5400000.0, 21600000.0, 6, 5, 4, 5, 3),
        (35834, "Keepstar", 33750000.0, 27000000.0, 108000000.0, 8, 6, 5, 7, 3),
        (35825, "Raitaru", 2400000.0, 1200000.0, 1200000.0, 3, 2, 1, 3, 3),
        (35826, "Azbel", 4500000.0, 3600000.0, 14400000.0, 4, 3, 2, 5, 3),
        (35827, "Sotiyo", 22500000.0, 18000000.0, 72000000.0, 6, 4, 3, 6, 3),
        (35835, "Athanor", 2400000.0, 1200000.0, 1200000.0, 3, 3, 1, 3, 3),
        (35836, "Tatara", 4500000.0, 3600000.0, 14400000.0, 5, 4, 3, 5, 3),
    ] {
        let p = life::structure_profile(&ctx, tid).unwrap_or_else(|| panic!("missing profile for {name}"));
        assert_eq!((p.shield_hp, p.armor_hp, p.hull_hp), (exp_sh, exp_ar, exp_hu));
        assert_eq!((p.high_slots, p.mid_slots, p.low_slots, p.service_slots, p.rig_slots), (hi, med, low, svc, rig));
    }
    for tid in [35840, 35841, 37534] {
        assert_eq!(life::structure_profile(&ctx, tid).unwrap().hull_class, life::HullClass::Flex);
    }
    assert!(life::structure_profile(&ctx, 35834).unwrap().disallow_in_highsec);
    let raitaru_p = life::structure_profile(&ctx, 35825).unwrap();
    assert!(raitaru_p.eng_time_bonus_pct != 0.0 && raitaru_p.eng_mat_bonus_pct != 0.0);
    for fid in 47512..=47516 {
        let fp = life::structure_profile(&ctx, fid).expect("faction fortizar");
        assert_eq!((fp.hull_class, fp.high_slots, fp.rig_slots), (life::HullClass::Large, 7, 3));
        assert!(fp.fuel_bonus_pct < 0.0 && (7..=8).contains(&fp.service_slots));
    }
    let palatine = life::structure_profile(&ctx, 40340).expect("palatine");
    assert_eq!((palatine.hull_class, palatine.fuel_bonus_pct, palatine.service_slots), (life::HullClass::XLarge, -25.0, 8));
    assert_eq!((life::structure_profile(&ctx, 35835).unwrap().refining_yield_bonus, life::structure_profile(&ctx, 35836).unwrap().refining_yield_bonus), (2.0, 5.5));

    move_ship(&ctx, &sess, open_space(&ctx, JITA, 5.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 35834, "Jita Keepstar").await.expect_err("hs keepstar").code, "CannotAnchorInHighSec");
    assert!(matches!(anchor(&ctx, &mut sess, 35840, "Jita Pharolux").await.expect_err("hs pharolux").code.as_str(), "CannotAnchorInHighSec" | "CannotAnchorCynoBeaconInHighSec"));

    // 2. Live invbroker.Add fitting, rig destruction on unfit, authorization, and state/HP locks.
    let st_id = online_structure(&ctx, &mut sess, 35832, "Fitting & Profiles Astrahus").await;
    let ship_id = sess.ship_id.unwrap();
    let owner_corp = ctx.db.with_snapshot(|snap| snap.structures.get(&st_id).unwrap().owner_corp_id);

    let siege_mod = grant(&ctx, OWNER, ship_id, 4292, InventoryFlag::Cargo, 1);
    assert_eq!(inv_add(&ctx, &mut sess, siege_mod, st_id, InventoryFlag::HiSlot0, false).await.expect_err("siege mod").code, "ModuleCannotFitStructureType");

    let rig_id = grant(&ctx, OWNER, ship_id, 37146, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::RigSlot0, false).await.expect("fit rig");
    assert_eq!(inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::RigSlot1, false).await.expect_err("move rig").code, "CannotMoveFittedRig");
    assert!(kv_bool(&inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::Hangar, false).await.expect("unfit rig"), "destroyed"));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&rig_id)));

    let high_mod = grant(&ctx, OWNER, ship_id, 35922, InventoryFlag::Cargo, 1);
    let mid_mod = grant(&ctx, OWNER, ship_id, 35947, InventoryFlag::Cargo, 1);
    let low_mod = grant(&ctx, OWNER, ship_id, 35959, InventoryFlag::Cargo, 1);
    let guarded_rig = grant(&ctx, OWNER, ship_id, 37146, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, high_mod, st_id, InventoryFlag::HiSlot0, false).await.expect("fit high");
    inv_add(&ctx, &mut sess, mid_mod, st_id, InventoryFlag::MedSlot0, false).await.expect("fit mid");
    inv_add(&ctx, &mut sess, low_mod, st_id, InventoryFlag::LoSlot0, false).await.expect("fit low");
    inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::RigSlot0, false).await.expect("fit guarded rig");

    let mut guest_sess = SessionState::new(7205);
    guest_sess.user_id = Some(2); guest_sess.character_id = Some(GUEST); guest_sess.corporation_id = Some(1000009); guest_sess.solar_system_id = Some(JITA);
    let mut outsider_sess = SessionState::new(7206);
    outsider_sess.user_id = Some(3); outsider_sess.character_id = Some(OUTSIDER); outsider_sess.corporation_id = Some(1000009); outsider_sess.solar_system_id = Some(JITA); outsider_sess.station_id = Some(JITA_44);
    let mut grunt_sess = SessionState::new(7207);
    grunt_sess.user_id = Some(4); grunt_sess.character_id = Some(CORP_GRUNT); grunt_sess.corporation_id = Some(owner_corp); grunt_sess.solar_system_id = Some(JITA); grunt_sess.structure_id = Some(st_id); grunt_sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch_g = snap.characters.get(&OWNER).cloned().unwrap();
        ch_g.character_id = GUEST; ch_g.corporation_id = 1000009; snap.characters.insert(GUEST, ch_g);
        let mut ch_o = snap.characters.get(&OWNER).cloned().unwrap();
        ch_o.character_id = OUTSIDER; ch_o.corporation_id = 1000009; ch_o.station_id = Some(JITA_44); ch_o.structure_id = None; snap.characters.insert(OUTSIDER, ch_o);
        let mut ch_m = snap.characters.get(&OWNER).cloned().unwrap();
        ch_m.character_id = CORP_GRUNT; ch_m.corporation_id = owner_corp; ch_m.structure_id = Some(st_id); snap.characters.insert(CORP_GRUNT, ch_m);
        snap.corporations.get_mut(&owner_corp).unwrap().members.insert(CORP_GRUNT);
    });

    let owner_ship_rig = grant(&ctx, OWNER, ship_id, 31358, InventoryFlag::RigSlot0, 1);
    assert_eq!(inv_add(&ctx, &mut outsider_sess, owner_ship_rig, JITA_44, InventoryFlag::Hangar, false).await.expect_err("foreign ship rig").code, "ItemOwnershipViolation");
    assert_eq!(inv_add(&ctx, &mut outsider_sess, guarded_rig, JITA_44, InventoryFlag::Hangar, false).await.expect_err("outsider struct rig").code, "StructureOwnershipViolation");
    assert_eq!(inv_add(&ctx, &mut grunt_sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("grunt struct rig").code, "InsufficientCorpRoles");

    let extra_high = grant(&ctx, OWNER, ship_id, 35922, InventoryFlag::Cargo, 1);
    let fut = ctx.now_unix() + 100_000;
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_ARMOR_REINFORCED.into(); s.reinforced_until_unix = fut; });
    assert_eq!(inv_add(&ctx, &mut sess, extra_high, st_id, InventoryFlag::HiSlot1, false).await.expect_err("armor refit").code, "StructureFittingLockedInState");
    assert_eq!(inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("armor rig unfit").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_HULL_REINFORCED.into(); s.reinforced_until_unix = fut; });
    assert_eq!(inv_add(&ctx, &mut sess, extra_high, st_id, InventoryFlag::HiSlot1, false).await.expect_err("hull refit").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_SHIELD_VULNERABLE.into(); s.reinforced_until_unix = 0; s.shield_hp = 100.0; });
    let svc_mod = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    assert_eq!(inv_add(&ctx, &mut sess, svc_mod, st_id, InventoryFlag::ServiceSlot0, false).await.expect_err("damaged refit").code, "StructureDamagedCannotRefit");
    assert_eq!(inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("damaged rig unfit").code, "StructureDamagedCannotRefit");

    let max_sh = life::structure_profile(&ctx, 35832).unwrap().shield_hp;
    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().shield_hp = max_sh; });
    let fit_fuel = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fit_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("fuel via invbroker.Add");
    inv_add(&ctx, &mut sess, svc_mod, st_id, InventoryFlag::ServiceSlot0, true).await.expect("fit & online service via invbroker.Add");

    ctx.advance_virtual_clock(3_600);
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, 4_452);

    // 3. Access groups, structure profiles (via structureSettings), and Structure Directory ACL & fuelBlocks masking.
    let group_id = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Allied Pilots".into()), PyRep::String("ACL group".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "groupID"))).unwrap();
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(GUEST)])]).await.expect("AddMembers");
    let profile_id = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Public Market Profile".into()), PyRep::String("Taxed market".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "profileID"))).unwrap();
    rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(profile_id)]).await.expect("ChangeProfiles");
    let settings_payload = PyRep::Dict(vec![
        (PyRep::String("corpOnly".into()), PyRep::Bool(false)),
        (PyRep::String("accessGroups".into()), PyRep::List(vec![PyRep::Integer(group_id)])),
        (PyRep::String("blocked".into()), PyRep::List(vec![PyRep::Integer(OUTSIDER)])),
        (PyRep::String("services".into()), PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(0.025))])),
    ]);
    rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id), settings_payload]).await.expect("SaveProfileSettings");

    assert!((rpc(&ctx, &mut guest_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(st_id), PyRep::Integer(2)]).await.unwrap().as_f64().unwrap() - 0.025).abs() < 1e-6);
    assert_eq!(rpc(&ctx, &mut outsider_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(st_id), PyRep::Integer(2)]).await.unwrap(), PyRep::None);
    assert_eq!(rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::Integer(4_452)));
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));
    assert_eq!(rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));

    outsider_sess.solar_system_id = Some(30_002_718); outsider_sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.solar_system_id = 30_002_718; c.station_id = None; });
    assert_eq!(rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect_err("remote blocked").code, "StructureAccessDenied");
    outsider_sess.solar_system_id = Some(JITA); outsider_sess.station_id = Some(JITA_44);
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.solar_system_id = JITA; c.station_id = Some(JITA_44); });
    let local_blocked = rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("in-system blocked");
    assert_eq!(local_blocked.dict_get("services"), Some(&PyRep::Dict(vec![])));

    let PyRep::Dict(g_map) = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructures", &[]).await.unwrap() else { panic!("dict") };
    let entry = g_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("entry");
    assert_eq!(kv_str(entry, "powerState"), "FullPower");
    assert_eq!(entry.dict_get("fuelBlocks"), Some(&PyRep::None));
    let PyRep::Dict(o_map) = rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructures", &[]).await.unwrap() else { panic!("dict") };
    assert!(!o_map.iter().any(|(k, _)| k.as_i64() == Some(st_id)));

    // 4. Delayed profile application (300s) + defense ACL (setting ID 17) + V24 3-arg OnSessionChanged verification.
    let profile_id_2 = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Profile 2".into()), PyRep::String("Second".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "profileID"))).unwrap();
    rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(profile_id_2)]).await.expect("ChangeProfiles 2");
    ctx.advance_virtual_clock(305);
    let _ = rpc(&ctx, &mut sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(1)]).await.unwrap();
    let def_group_id = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Gunners".into()), PyRep::String("Defense ACL".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "groupID"))).unwrap();
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_group_id), PyRep::List(vec![PyRep::Integer(OUTSIDER)])]).await.unwrap();
    let profile_rows = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(group_id), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_group_id), PyRep::Integer(1)]),
    ]);
    rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id_2), profile_rows]).await.unwrap();
    guest_sess.structure_id = Some(st_id);
    assert_eq!(rpc(&ctx, &mut guest_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect_err("docking guest denied").code, "DefenseAccessDenied");
    outsider_sess.structure_id = Some(st_id); outsider_sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.station_id = None; c.structure_id = Some(st_id); });
    assert!(rpc(&ctx, &mut outsider_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_ok());
    assert!(rpc(&ctx, &mut outsider_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.is_ok());

    sess.structure_id = Some(st_id);
    let _ = ctx.drain_notifications_for_char(OWNER);
    let prev_ship = sess.ship_id.unwrap();
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("Owner TakeControl");
    let take_evts = ctx.drain_notifications_for_char(OWNER);
    let sc_take = take_evts.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged on TakeControl");
    assert_eq!(sc_take.args.len(), 3, "OnSessionChanged must use exact 3-arg V24 shape (isRemote, sess, changes)");
    assert_eq!(sc_take.args[0], PyRep::Bool(false));
    assert_eq!(sc_take.args[2], PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(prev_ship), PyRep::Integer(st_id)]))]));

    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.expect("Owner ReleaseControl");
    let rel_evts = ctx.drain_notifications_for_char(OWNER);
    let sc_rel = rel_evts.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged on ReleaseControl");
    assert_eq!(sc_rel.args.len(), 3);
    assert_eq!(sc_rel.args[2], PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(prev_ship)]))]));

    // 5. Reinforce change delay per security band & full shield/armor gate.
    assert_eq!(eve_server::services::station_structure::structure_upwell_ops::reinforce_change_delay_secs(&ctx, JITA), 30 * 86_400);
    assert_eq!(eve_server::services::station_structure::structure_upwell_ops::reinforce_change_delay_secs(&ctx, 30_000_157), 7 * 86_400);

    // 6. Symmetric SpecializedFuelBay (flag 133) role/item enforcement across Add & MultiAdd, JettisonStructureFuel type preservation, and Ansiblex Liquid Ozone via invbroker.Add.
    let guest_ship = grant(&ctx, GUEST, st_id, 606, InventoryFlag::Hangar, 1);
    let grunt_ship = grant(&ctx, CORP_GRUNT, st_id, 606, InventoryFlag::Hangar, 1);
    guest_sess.ship_id = Some(guest_ship); guest_sess.station_id = None; guest_sess.structure_id = Some(st_id);
    grunt_sess.ship_id = Some(grunt_ship); grunt_sess.station_id = None; grunt_sess.structure_id = Some(st_id);
    ctx.db.with_mut_snapshot(|snap| {
        let cg = snap.characters.get_mut(&GUEST).unwrap(); cg.station_id = None; cg.structure_id = Some(st_id); cg.active_ship_id = guest_ship;
        let cm = snap.characters.get_mut(&CORP_GRUNT).unwrap(); cm.station_id = None; cm.structure_id = Some(st_id); cm.active_ship_id = grunt_ship;
    });
    let guest_fb = grant(&ctx, GUEST, guest_ship, 4247, InventoryFlag::Cargo, 100);
    let grunt_fb = grant(&ctx, CORP_GRUNT, grunt_ship, 4247, InventoryFlag::Cargo, 100);
    assert_eq!(rpc(&ctx, &mut guest_sess, "invbroker", "Add", &[PyRep::Integer(guest_fb), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("guest Add").code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut guest_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(guest_fb)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("guest MultiAdd").code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "Add", &[PyRep::Integer(grunt_fb), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("grunt Add").code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(grunt_fb)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("grunt MultiAdd").code, "CorpRoleDenied");
    let bay_fb_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == st_id && i.flag == InventoryFlag::SpecializedFuelBay && i.type_id == 4247).unwrap().item_id);
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "Add", &[PyRep::Integer(bay_fb_id), PyRep::Integer(grunt_ship), PyRep::Integer(5), PyRep::Integer(10)]).await.expect_err("grunt withdraw Add").code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(bay_fb_id)]), PyRep::Integer(grunt_ship), PyRep::Integer(5)]).await.expect_err("grunt withdraw MultiAdd").code, "CorpRoleDenied");

    let heavy_water = grant(&ctx, OWNER, ship_id, 16272, InventoryFlag::Cargo, 100);
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(heavy_water), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("heavy water Add").code, "InvalidInventoryFlag");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(heavy_water)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("heavy water MultiAdd").code, "InvalidInventoryFlag");

    let oz_item = grant(&ctx, OWNER, ship_id, 16273, InventoryFlag::Cargo, 700);
    let gas_item = grant(&ctx, OWNER, ship_id, 81143, InventoryFlag::Cargo, 300);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(oz_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(700)]).await.expect("ozone Add");
    rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(gas_item)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect("gas MultiAdd");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, 4_452, "Ozone and reagents must never inflate fuel_blocks");
    let jet_gas = rpc(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(gas_item)])]).await.expect("jettison gas");
    let jet_oz = rpc(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::Integer(16273), PyRep::Integer(100)]).await.expect("jettison ozone");
    let (gas_cid, oz_cid) = (kv_i64(&jet_gas, "containerID"), kv_i64(&jet_oz, "containerID"));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.values().any(|i| i.location_id == gas_cid && i.type_id == 81143 && i.quantity == 300), "Jettisoned reagent must preserve type_id 81143");
        assert!(snap.items.values().any(|i| i.location_id == oz_cid && i.type_id == 16273 && i.quantity == 100), "Jettisoned ozone must preserve type_id 16273");
        assert_eq!(snap.structures[&st_id].fuel_blocks, 4_452);
    });

    // Ansiblex Jump Bridge fueled via live invbroker.Add from in-space ship consumes Liquid Ozone on CmdJumpThroughStructureStargate.
    let (jb_src, jb_dst, src_sys, dst_sys) = (995_000_001i64, 995_000_002i64, 30_004_712u32, 30_004_713u32);
    ctx.db.with_mut_snapshot(|snap| {
        for (jid, sys_id, nm) in [(jb_src, src_sys, "HED-GP Ansiblex"), (jb_dst, dst_sys, "Rancer Ansiblex")] {
            snap.structures.insert(jid, UpwellStructureRecord {
                structure_id: jid, owner_corp_id: owner_corp, type_id: 35841, solar_system_id: sys_id,
                name: nm.into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
                shield_hp: 500_000.0, armor_hp: 500_000.0, hull_hp: 500_000.0, fuel_blocks: 1_000, reinforced_until_unix: 0,
                last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
            });
        }
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_src, jb_dst);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None; ch.structure_id = None; ch.solar_system_id = src_sys;
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(src_sys);
    });
    sess.station_id = None; sess.structure_id = None; sess.solar_system_id = Some(src_sys);
    let _ = ctx.systems.get_or_spawn(src_sys).spawn_ball_with_combat(eve_destiny::Ball { id: ship_id, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1.0e6, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    ctx.advance_secs(12);
    assert_eq!(rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_src)]).await.expect_err("no ozone").code, "InsufficientLiquidOzone");
    let jb_oz = grant(&ctx, OWNER, ship_id, 16273, InventoryFlag::Cargo, 600);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(jb_oz), PyRep::Integer(jb_src), PyRep::Integer(133), PyRep::Integer(600)]).await.expect("in-space ozone Add");
    let jb_res = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_src)]).await.expect("jump through Ansiblex");
    assert_eq!(kv_i64(&jb_res, "solarSystemID"), i64::from(dst_sys));
    ctx.db.with_snapshot(|snap| {
        let rem_oz: i32 = snap.items.values().filter(|i| i.location_id == jb_src && i.flag == InventoryFlag::SpecializedFuelBay && i.type_id == 16273).map(|i| i.quantity).sum();
        assert_eq!(rem_oz, 547, "Ansiblex jump must deduct mass-based Liquid Ozone loaded via invbroker.Add");
    });
}
