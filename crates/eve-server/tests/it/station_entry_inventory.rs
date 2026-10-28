//! Station-entry inventory handles the real V24.01 client requests from its location-bound
//! `invbroker` moniker right after `SelectCharacterID` (T-6 real-client run: both were
//! `UnimplementedRPC` in the captured trace). `invCache.GetInventoryFromId` /
//! `invCache.GetInventory` (`eve/client/script/environment/invCache.pyj:553-626`) call them on
//! `eveMoniker.GetStationInventoryMgr(stationID)` and then `List()` the returned remote object.

use crate::common::*;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CONTAINER_HANGAR: i64 = 10_004;
const TRITANIUM: u32 = 34;

/// Item IDs in an `invbroker.List` CRowset (`PyObjectEx2` whose list holds `DBRow`s).
fn listed_item_ids(rep: &PyRep) -> Vec<i64> {
    let PyRep::ObjectEx { list, .. } = rep else { panic!("invbroker.List must return a CRowset, got {rep:?}") };
    let mut ids: Vec<i64> = list
        .iter()
        .map(|row| {
            let PyRep::DBRow { columns, .. } = row else { panic!("CRowset rows are DBRows") };
            columns.iter().find(|(n, _)| n == "itemID").and_then(|(_, v)| v.as_i64()).expect("itemID column")
        })
        .collect();
    ids.sort_unstable();
    ids
}

fn bound_id_of(rep: &PyRep) -> u64 {
    rep.as_i64().expect("bridge callers get the bound ID as an integer") as u64
}

#[tokio::test]
async fn bound_invbroker_hands_out_ship_character_and_hangar_inventories() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_201);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let user_id = sess.user_id.expect("logged in");
    let ch = ctx.db.get_characters_for_user(user_id)[0].clone();
    let char_id = ch.character_id;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let station_id = ch.station_id.expect("seeded demo character starts docked");
    let ship_id = ch.active_ship_id;

    // A tritanium stack in the ship's cargo: the ship inventory must list it, and tritanium itself
    // (SDE capacity 0) is not an inventory.
    assert_eq!(ctx.sde.types[&TRITANIUM].capacity, 0.0);
    assert!(ctx.sde.types[&ctx.db.get_item(ship_id).expect("ship item").type_id].capacity > 0.0);
    let cargo = ctx.db.add_or_stack_item(char_id, ship_id, InventoryFlag::Cargo, TRITANIUM, 250).expect("cargo stack");

    // Unbound calls are refused: these methods exist only on the location-bound moniker.
    let unbound = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "GetInventoryFromId", &[PyRep::Integer(ship_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("unbound GetInventoryFromId");
    assert_eq!(unbound.code, "InvalidBoundObject");

    // `GetStationInventoryMgr(stationID)` = Moniker('invbroker', (stationID, const.groupStation)).
    let bind = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None], &kw, None)
        .await
        .expect("bind station inventory manager");
    let PyRep::Tuple(parts) = &bind else { panic!("MachoBindObject 2-tuple") };
    let mgr = bound_id_of(&parts[0]);

    // 1. GetInventoryFromId(shipID, passive) -> remote object whose List() is the ship's contents.
    let ship_inv = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(ship_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect("GetInventoryFromId(ship)");
    let ship_bid = bound_id_of(&ship_inv);
    assert_ne!(ship_bid, mgr, "a new remote object is handed out");
    let ship_list = ctx.dispatch_rpc(&mut sess, "invbroker", Some(ship_bid), "List", &[], &kw, None).await.expect("ship List");
    let mut expected_ship: Vec<i64> = ctx
        .db
        .get_items_for_character(char_id)
        .into_iter()
        .filter(|i| i.location_id == ship_id)
        .map(|i| i.item_id)
        .collect();
    expected_ship.sort_unstable();
    assert!(expected_ship.contains(&cargo.item_id));
    assert_eq!(listed_item_ids(&ship_list), expected_ship);

    // 2. The character itself is an inventory (implants/skills live in it).
    let char_inv = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(char_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("GetInventoryFromId(char)");
    let char_bid = bound_id_of(&char_inv);
    let entry = ctx.verify_bound_object(&sess, char_bid).expect("bound to this session");
    assert_eq!((entry.service.as_str(), entry.object_id, entry.character_id), ("invbroker", char_id, char_id));

    // 3. GetInventory(containerHangar, None) -> the station hangar; its List() includes the docked ship.
    let hangar = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventory", &[PyRep::Integer(CONTAINER_HANGAR), PyRep::None], &kw, None)
        .await
        .expect("GetInventory(containerHangar)");
    let hangar_list = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(bound_id_of(&hangar)), "List", &[], &kw, None)
        .await
        .expect("hangar List");
    let hangar_ids = listed_item_ids(&hangar_list);
    assert!(hangar_ids.contains(&ship_id), "docked ship {ship_id} is in the hangar {hangar_ids:?}");
    assert!(!hangar_ids.contains(&cargo.item_id), "cargo stays inside the ship");

    // Error paths: a non-container item, someone else's item, an unsupported special container.
    let not_inv = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(cargo.item_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("tritanium has no inventory");
    assert_eq!(not_inv.code, "InventoryNotAccessible");
    let foreign_ship = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|i| i.owner_id != char_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.capacity > 0.0)).map(|i| i.item_id)
    });
    let foreign = foreign_ship.expect("the seeded world has another pilot's ship");
    let e = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(foreign), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("foreign inventory");
    assert_eq!(e.code, "InventoryNotAccessible");
    let unsupported = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventory", &[PyRep::Integer(10_010), PyRep::None], &kw, None)
        .await
        .expect_err("containerStationCharacters");
    assert_eq!(unsupported.code, "UnimplementedRPC");

    // Undocked, the hangar and station-held containers are no longer reachable through the old station moniker.
    let spare_ship_type = ctx.db.get_item(ship_id).expect("ship").type_id;
    let spare_ship = ctx
        .db
        .add_or_stack_item(char_id, station_id, InventoryFlag::Hangar, spare_ship_type, 1)
        .expect("spare ship in station hangar");
    let docked_spare = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(spare_ship.item_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect("docked spare ship inventory");
    assert_ne!(bound_id_of(&docked_spare), mgr);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
        }
    });
    let undocked = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventory", &[PyRep::Integer(CONTAINER_HANGAR), PyRep::None], &kw, None)
        .await
        .expect_err("undocked hangar");
    assert_eq!(undocked.code, "CharacterNotAtStation");
    let undocked_spare = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(spare_ship.item_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("undocked station-held ship");
    assert_eq!(undocked_spare.code, "InventoryNotAccessible");
}

#[tokio::test]
async fn native_client_gets_a_substruct_object_reference_from_get_inventory_from_id() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_202);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let ch = ctx.db.get_characters_for_user(sess.user_id.unwrap())[0].clone();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(ch.character_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let station_id = ch.station_id.expect("docked");
    let native = Some("machoNet:26000");
    let bind = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None], &kw, native)
        .await
        .expect("bind");
    let PyRep::Tuple(parts) = &bind else { panic!("2-tuple") };
    let mgr = oid_bound_id(&parts[0]);
    let rep = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
            &[PyRep::Integer(ch.active_ship_id), PyRep::Integer(0)], &kw, native)
        .await
        .expect("GetInventoryFromId over machoNet");
    let bid = oid_bound_id(&rep);
    assert_eq!(ctx.verify_bound_object(&sess, bid).expect("bound").object_id, ch.active_ship_id);
}

/// Parses `SubStruct(SubStream((b"N={node}:{boundID}", refID)))` and checks the node and refID.
fn oid_bound_id(rep: &PyRep) -> u64 {
    let PyRep::SubStruct(inner) = rep else { panic!("SubStruct OID expected, got {rep:?}") };
    let PyRep::SubStream(inner) = inner.as_ref() else { panic!("SubStream expected") };
    let PyRep::Tuple(t) = inner.as_ref() else { panic!("(oid, refID) tuple") };
    let oid = t[0].as_str().expect("oid string").to_string();
    let prefix = format!("N={}:", eve_machonet::MACHO_PROXY_NODE_ID);
    assert!(oid.starts_with(&prefix), "oid {oid} must name the proxy node");
    assert!(t[1].as_i64().expect("refID") > 116_444_736_000_000_000, "refID is a Windows filetime");
    oid[prefix.len()..].parse().expect("bound id")
}

fn dbrow_col(row: &PyRep, name: &str) -> PyRep {
    let PyRep::DBRow { columns, .. } = row else { panic!("DBRow expected, got {row:?}") };
    columns.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("column {name}"))
}

#[tokio::test]
async fn station_ui_bootstrap_shapes_match_the_v24_client() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_203);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let ch = ctx.db.get_characters_for_user(sess.user_id.unwrap())[0].clone();
    let char_id = ch.character_id;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let station_id = ch.station_id.expect("docked");
    let st = ctx.sde.stations[&station_id].clone();

    // stationSvc.GetStationItemBits() -> the 4 values of Row(['ownerID','itemID','operationID','stationTypeID']).
    let bits = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "GetStationItemBits", &[], &kw, None).await.expect("bits");
    assert_eq!(
        bits,
        PyRep::Tuple(vec![
            PyRep::Integer(st.owner_corporation_id),
            PyRep::Integer(station_id),
            PyRep::Integer(i64::from(st.operation_id)),
            PyRep::Integer(i64::from(st.station_type_id)),
        ])
    );

    // GetSelfInvItem on the station hangar, the ship, and the character inventory.
    let bind = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None], &kw, None)
        .await
        .expect("bind");
    let PyRep::Tuple(parts) = &bind else { panic!("2-tuple") };
    let mgr = bound_id_of(&parts[0]);
    let hangar = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventory", &[PyRep::Integer(CONTAINER_HANGAR), PyRep::None], &kw, None)
        .await
        .expect("hangar");
    let station_row = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(bound_id_of(&hangar)), "GetSelfInvItem", &[], &kw, None)
        .await
        .expect("hangar GetSelfInvItem");
    assert_eq!(dbrow_col(&station_row, "itemID"), PyRep::Integer(station_id));
    assert_eq!(dbrow_col(&station_row, "typeID"), PyRep::Integer(i64::from(st.station_type_id)));
    assert_eq!(dbrow_col(&station_row, "ownerID"), PyRep::Integer(st.owner_corporation_id));
    assert_eq!(dbrow_col(&station_row, "locationID"), PyRep::Integer(i64::from(st.solar_system_id)));

    let ship_type = ctx.db.get_item(ch.active_ship_id).expect("ship").type_id;
    let ship_inv = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(ch.active_ship_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect("ship inventory");
    let ship_row = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(bound_id_of(&ship_inv)), "GetSelfInvItem", &[], &kw, None)
        .await
        .expect("ship GetSelfInvItem");
    assert_eq!(dbrow_col(&ship_row, "itemID"), PyRep::Integer(ch.active_ship_id));
    assert_eq!(dbrow_col(&ship_row, "typeID"), PyRep::Integer(i64::from(ship_type)));
    assert_eq!(dbrow_col(&ship_row, "ownerID"), PyRep::Integer(char_id));
    assert_eq!(dbrow_col(&ship_row, "locationID"), PyRep::Integer(station_id));

    let free_turrets = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(bound_id_of(&ship_inv)), "GetAvailableTurretSlots", &[], &kw, None)
        .await
        .expect("ship GetAvailableTurretSlots");
    assert!(free_turrets.as_i64().is_some_and(|n| n >= 0));

    let char_inv = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(char_id), PyRep::Integer(0)], &kw, None)
        .await
        .expect("char inventory");
    let char_self = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(bound_id_of(&char_inv)), "GetSelfInvItem", &[], &kw, None)
        .await
        .expect_err("character self item not implemented");
    assert_eq!(char_self.code, "UnimplementedRPC");

    // warRegistry.GetWars(ownerID) on the bound moniker -> {warID: KeyVal} (war_cso iterates itervalues()).
    let corp_id = ctx.db.get_character(char_id).expect("char").corporation_id;
    let war_id = 9_203_001;
    let started = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            war_id,
            eve_db::WarRecord { war_id, aggressor_corp_id: corp_id, defender_corp_id: 98_000_777, mutual: true, started_unix: started, active: true },
        );
    });
    let wbind = ctx
        .dispatch_rpc(&mut sess, "warRegistry", None, "MachoBindObject", &[PyRep::Integer(corp_id), PyRep::None], &kw, None)
        .await
        .expect("bind warRegistry");
    let PyRep::Tuple(wparts) = &wbind else { panic!("2-tuple") };
    let wars = ctx
        .dispatch_rpc(&mut sess, "warRegistry", Some(bound_id_of(&wparts[0])), "GetWars", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWars");
    let PyRep::Dict(entries) = &wars else { panic!("GetWars must return {{warID: war}}, got {wars:?}") };
    let (k, v) = entries.iter().find(|(k, _)| k.as_i64() == Some(war_id)).expect("our war is listed");
    assert_eq!(k, &PyRep::Integer(war_id));
    let PyRep::KeyVal(war) = v else { panic!("war entry is a KeyVal") };
    assert_eq!(war.get("warID"), Some(&PyRep::Integer(war_id)));
    assert_eq!(war.get("declaredByID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(war.get("againstID"), Some(&PyRep::Integer(98_000_777)));
    assert_eq!(war.get("mutual"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn station_entry_handlers_match_v24_client_shapes_and_effective_attributes() {
    // parity: rpc:accessGroupBookmarkMgr.GetMyActiveBookmarks, rpc:ship.GetDirtTimestamp, rpc:userSvc.GetMultiCharactersTrainingSlots, rpc:shipCosmeticsMgr.GetEnabledCosmetics, rpc:nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections, rpc:dynamicBountyMgr.GetOutputForClientSolarSystem, rpc:crimewatch.GetClientStates, rpc:monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_204);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let ch = ctx.db.get_characters_for_user(sess.user_id.unwrap())[0].clone();
    let char_id = ch.character_id;
    let ship_id = ch.active_ship_id;
    let sys_id = ch.solar_system_id;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    // 1. `ProjectDiscovery.initialize_tutorial_status` reads per-character `tutorial_complete`
    //    (matching `get_tutorial_completion_status`) and `ProjectDiscovery.is_enabled` returns true.
    let enabled = ctx.dispatch_rpc(&mut sess, "ProjectDiscovery", None, "is_enabled", &[], &kw, None).await.expect("is_enabled");
    assert_eq!(enabled, PyRep::Bool(true));
    let _ = ctx.dispatch_rpc(&mut sess, "ProjectDiscovery", None, "reset_tutorial", &[], &kw, None).await.expect("reset_tutorial");
    let init_before = ctx
        .dispatch_rpc(&mut sess, "ProjectDiscovery", None, "initialize_tutorial_status", &[], &kw, None)
        .await
        .expect("initialize_tutorial_status before skip");
    let status_before = ctx
        .dispatch_rpc(&mut sess, "ProjectDiscovery", None, "get_tutorial_completion_status", &[], &kw, None)
        .await
        .expect("get_tutorial_completion_status before skip");
    assert_eq!(init_before, PyRep::Bool(false));
    assert_eq!(status_before, PyRep::Bool(false));
    let _ = ctx.dispatch_rpc(&mut sess, "ProjectDiscovery", None, "skip_tutorial", &[], &kw, None).await.expect("skip_tutorial");
    let init_after = ctx
        .dispatch_rpc(&mut sess, "ProjectDiscovery", None, "initialize_tutorial_status", &[], &kw, None)
        .await
        .expect("initialize_tutorial_status after skip");
    assert_eq!(init_after, PyRep::Bool(true));

    // 2. `skillMgr2.GetAttributes` includes plugged-in attribute implant bonuses (`175..=179`).
    let (implant_type_id, bonus_int) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            let b = t.default_attributes.get(&176).copied().unwrap_or(0.0);
            (t.category_id == 20 && b >= 3.0).then_some((t.type_id, b as i64))
        })
        .expect("SDE has an intelligence implant");
    let base_attrs = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetAttributes", &[], &kw, None).await.expect("base GetAttributes");
    let PyRep::Dict(base_entries) = &base_attrs else { panic!("GetAttributes dict") };
    let base_int = base_entries.iter().find(|(k, _)| k.as_i64() == Some(165)).and_then(|(_, v)| v.as_i64()).expect("intelligence 165");
    assert_eq!(base_int, i64::from(ch.neural.intelligence));

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().push(implant_type_id);
    });
    let eff_attrs = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetAttributes", &[], &kw, None).await.expect("effective GetAttributes");
    let PyRep::Dict(eff_entries) = &eff_attrs else { panic!("GetAttributes dict") };
    let eff_int = eff_entries.iter().find(|(k, _)| k.as_i64() == Some(165)).and_then(|(_, v)| v.as_i64()).expect("intelligence 165");
    assert_eq!(eff_int, base_int + bonus_int, "GetAttributes must include plugged-in implant bonus");

    // `skillMgr2.GetMySkillHandler` uses `bound_object_rep` (integer for bridge, SubStruct OID for native).
    let bridge_h = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetMySkillHandler", &[], &kw, None).await.expect("bridge GetMySkillHandler");
    assert_eq!(ctx.verify_bound_object(&sess, bound_id_of(&bridge_h)).expect("bound").object_id, char_id);
    let native_h = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetMySkillHandler", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("native GetMySkillHandler");
    assert_eq!(ctx.verify_bound_object(&sess, oid_bound_id(&native_h)).expect("bound").object_id, char_id);

    // 3. `dogmaIM.GetAllInfo`: `shipInfo`, `charInfo` (`(char_dict, (0, (), (), ()))`), `shipState`, and 2-arg `ProcessGodmaPrimeLocation`.
    let _ = sess.drain_notifications();
    let all_info = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)], &kw, None)
        .await
        .expect("dogmaIM.GetAllInfo");
    let notifs = sess.drain_notifications();
    let (_, prime_args) = notifs.iter().find(|(n, _)| n == "ProcessGodmaPrimeLocation").expect("ProcessGodmaPrimeLocation");
    assert_eq!(prime_args.len(), 2, "Godma.ProcessGodmaPrimeLocation(locationID, data) takes 2 args");
    assert_eq!(prime_args[0], PyRep::Integer(ship_id));
    let PyRep::Dict(ship_info) = all_info.dict_get("shipInfo").expect("shipInfo") else { panic!("shipInfo dict") };
    let (_, ship_entry) = ship_info.iter().find(|(k, _)| k.as_i64() == Some(ship_id)).expect("ship entry in shipInfo");
    assert_eq!(dbrow_col(ship_entry.dict_get("invItem").expect("invItem"), "itemID"), PyRep::Integer(ship_id));
    assert_eq!(dbrow_col(ship_entry.dict_get("invItem").expect("invItem"), "quantity"), PyRep::Integer(-1));

    let PyRep::Tuple(char_info) = all_info.dict_get("charInfo").expect("charInfo") else { panic!("charInfo 2-tuple") };
    assert_eq!(char_info.len(), 2);
    let PyRep::Dict(char_dict) = &char_info[0] else { panic!("charInfo[0] dict") };
    let (_, char_entry) = char_dict.iter().find(|(k, _)| k.as_i64() == Some(char_id)).expect("char entry");
    assert_eq!(dbrow_col(char_entry.dict_get("invItem").expect("char invItem"), "flagID"), PyRep::Integer(57));
    let PyRep::Dict(char_row_attrs) = char_entry.dict_get("attributes").expect("char attributes") else { panic!("char attributes dict") };
    assert_eq!(
        char_row_attrs.iter().find(|(k, _)| k.as_i64() == Some(165)).and_then(|(_, v)| v.as_f64()),
        Some(eff_int as f64),
    );
    assert_eq!(
        char_info[1],
        PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Tuple(vec![]), PyRep::Tuple(vec![]), PyRep::Tuple(vec![])])
    );
    let PyRep::Tuple(ship_state) = all_info.dict_get("shipState").expect("shipState") else { panic!("shipState 4-tuple") };
    assert_eq!(ship_state.len(), 4);

    // `godma.Prime` binds `dogmaIM` to `(stationID, groupStation=15)` and invokes `GetAllInfo(True, True, False)`.
    let station_id = sess.station_id.unwrap_or(60_003_760);
    let dogma_bind = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetAllInfo".into()),
                    PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)]),
                    PyRep::Dict(vec![]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.MachoBindObject bound to station_id");
    let PyRep::Tuple(dogma_bind_t) = &dogma_bind else { panic!("dogmaIM MachoBindObject 2-tuple") };
    assert_eq!(dogma_bind_t[1].dict_get("shipID"), Some(&PyRep::Integer(ship_id)));

    // 4. `charMgr.GetSettingsInfo` -> `(code_bytes, 0)` (Placebo-verified Python 2.7 code object).
    let settings_info = ctx.dispatch_rpc(&mut sess, "charMgr", None, "GetSettingsInfo", &[], &kw, None).await.expect("GetSettingsInfo");
    let PyRep::Tuple(si) = &settings_info else { panic!("GetSettingsInfo 2-tuple") };
    assert_eq!(si.len(), 2);
    assert_eq!(si[1], PyRep::Integer(0));
    let PyRep::Bytes(code_bytes) = &si[0] else { panic!("code bytes") };
    assert_eq!(code_bytes.first().copied(), Some(b'c'), "marshalled code object starts with 'c'");
    assert!(code_bytes.windows(b"SettingsInfo".len()).any(|w| w == b"SettingsInfo"));

    // 5. `agentMgr.GetAgents` -> 10-column rowset with `solarsystemID` (lowercase 's' in system).
    let agents = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &kw, None).await.expect("GetAgents");
    let PyRep::Rowset { columns, .. } = &agents else { panic!("GetAgents rowset") };
    assert_eq!(columns.len(), 10);
    assert!(columns.iter().any(|c| c == "solarsystemID"), "Agents.__GetAllAgents filters by solarsystemID: {columns:?}");
    assert!(!columns.iter().any(|c| c == "solarSystemID"));

    // 6. `fwWarzoneSolarsystem.GetLocalOccupationState` -> `(solarsystemID, None)` in non-FW Jita (`30000142`),
    // and `(solarsystemID, KeyVal)` in low-sec FW warzone Asabona (`30000012`).
    let occ_jita = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &kw, None)
        .await
        .expect("GetLocalOccupationState in Jita");
    assert_eq!(
        occ_jita,
        PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::None]),
        "Non-FW high-sec solar system must return (solarsystemID, None)"
    );
    let fw_sys_id: u32 = 30_000_012; // Asabona (0.32 low-sec FW warzone)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = fw_sys_id;
    });
    sess.solar_system_id = Some(fw_sys_id);
    let occ_fw = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &kw, None)
        .await
        .expect("GetLocalOccupationState in Asabona");
    let PyRep::Tuple(occ_t) = &occ_fw else { panic!("GetLocalOccupationState 2-tuple") };
    assert_eq!(occ_t.len(), 2);
    assert_eq!(occ_t[0], PyRep::Integer(i64::from(fw_sys_id)));
    assert_eq!(occ_t[1].dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(fw_sys_id))));
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id;
    });
    sess.solar_system_id = Some(sys_id);

    // 7. `accessGroupBookmarkMgr.GetMyActiveBookmarks` -> `(folders, bookmarks, subfolders)` 3-tuple of lists.
    let folder = ctx
        .dispatch_rpc(&mut sess, "bookmark", None, "AddFolder", &[PyRep::String("Nav".into())], &kw, None)
        .await
        .expect("AddFolder");
    let folder_id = folder.dict_get("folderID").and_then(|v| v.as_i64()).expect("folderID");
    let bm = ctx
        .dispatch_rpc(
            &mut sess,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Spot".into()), PyRep::String("Note".into()), PyRep::Integer(folder_id)],
            &kw,
            None,
        )
        .await
        .expect("BookmarkLocation");
    let bm_id = bm.dict_get("bookmarkID").and_then(|v| v.as_i64()).expect("bookmarkID");
    let acl_bms = ctx
        .dispatch_rpc(&mut sess, "accessGroupBookmarkMgr", None, "GetMyActiveBookmarks", &[], &kw, None)
        .await
        .expect("GetMyActiveBookmarks");
    let PyRep::Tuple(bm_tuple) = &acl_bms else { panic!("GetMyActiveBookmarks 3-tuple") };
    assert_eq!(bm_tuple.len(), 3);
    let PyRep::List(folders) = &bm_tuple[0] else { panic!("folders list") };
    let PyRep::List(bookmarks) = &bm_tuple[1] else { panic!("bookmarks list") };
    assert!(folders.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(folder_id))));
    let our_bm = bookmarks.iter().find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(bm_id))).expect("bookmark present");
    assert_eq!(our_bm.dict_get("locationID"), Some(&PyRep::Integer(i64::from(sys_id))));

    // 8. `ship.GetDirtTimestamp` -> Windows filetime for an existing ship and `ItemNotFound` for a missing ID.
    let dirt = ctx
        .dispatch_rpc(&mut sess, "ship", None, "GetDirtTimestamp", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("GetDirtTimestamp");
    assert!(dirt.as_i64().expect("filetime") > 116_444_736_000_000_000);
    let missing_dirt = ctx
        .dispatch_rpc(&mut sess, "ship", None, "GetDirtTimestamp", &[PyRep::Integer(999_999_999)], &kw, None)
        .await
        .expect_err("missing ship");
    assert_eq!(missing_dirt.code, "ItemNotFound");

    // 9. `crimewatch.GetClientStates`, `monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount`,
    // `fighterMgr.GetFightersForShip`, and `userSvc.GetMultiCharactersTrainingSlots`.
    let cw_states = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "GetClientStates", &[], &kw, None)
        .await
        .expect("crimewatch.GetClientStates");
    let PyRep::Tuple(cw_t) = &cw_states else { panic!("crimewatch.GetClientStates 4-tuple") };
    assert_eq!(cw_t.len(), 4);
    assert_eq!(
        cw_t[0],
        PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Integer(100), PyRep::None]),
            PyRep::Tuple(vec![PyRep::Integer(200), PyRep::None]),
            PyRep::Tuple(vec![PyRep::Integer(400), PyRep::None]),
            PyRep::Tuple(vec![PyRep::Integer(300), PyRep::None]),
            PyRep::Tuple(vec![PyRep::Integer(500), PyRep::None]),
        ])
    );

    let ach_info = ctx
        .dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "GetCompletedAchievementsAndClientEventCount", &[], &kw, None)
        .await
        .expect("GetCompletedAchievementsAndClientEventCount");
    assert_eq!(ach_info.dict_get("completedDict"), Some(&PyRep::Dict(vec![])));
    assert_eq!(ach_info.dict_get("eventDict"), Some(&PyRep::Dict(vec![])));

    let fighters = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "GetFightersForShip", &[], &kw, None)
        .await
        .expect("fighterMgr.GetFightersForShip");
    assert_eq!(
        fighters,
        PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::Dict(vec![])])
    );

    let mct = ctx
        .dispatch_rpc(&mut sess, "userSvc", None, "GetMultiCharactersTrainingSlots", &[], &kw, None)
        .await
        .expect("userSvc.GetMultiCharactersTrainingSlots");
    assert_eq!(mct, PyRep::Dict(vec![]));

    // 10. Additional station-entry RPCs: `nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections`,
    // `skillMgr2.CheckAndSendNotifications`, `agentMgr.GetReplayTimestamp`,
    // `shipCosmeticsMgr.GetEnabledCosmetics`, `contractProxy.GetLoginInfo`, and
    // `dynamicBountyMgr.GetOutputForClientSolarSystem`.
    let injections = ctx
        .dispatch_rpc(
            &mut sess,
            "nonDiminishingInjectionMgr",
            None,
            "GetAvailableNonDiminishingInjections",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAvailableNonDiminishingInjections");
    assert_eq!(injections, PyRep::Dict(vec![]));

    let skill_notifs = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "CheckAndSendNotifications", &[], &kw, None)
        .await
        .expect("skillMgr2.CheckAndSendNotifications");
    assert_eq!(skill_notifs, PyRep::None);

    let replay_ts = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetReplayTimestamp", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("agentMgr.GetReplayTimestamp");
    assert_eq!(replay_ts, PyRep::None);

    let enabled_cosmetics = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "GetEnabledCosmetics",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("shipCosmeticsMgr.GetEnabledCosmetics");
    assert_eq!(enabled_cosmetics, PyRep::Dict(vec![]));

    let contract_login = ctx
        .dispatch_rpc(&mut sess, "contractProxy", None, "GetLoginInfo", &[], &kw, None)
        .await
        .expect("contractProxy.GetLoginInfo");
    assert_eq!(contract_login.dict_get("assignedToMe"), Some(&PyRep::List(vec![])));
    assert_eq!(contract_login.dict_get("needsAttention"), Some(&PyRep::List(vec![])));
    assert_eq!(contract_login.dict_get("inProgress"), Some(&PyRep::List(vec![])));

    let dbs = ctx
        .dispatch_rpc(&mut sess, "dynamicBountyMgr", None, "GetOutputForClientSolarSystem", &[], &kw, None)
        .await
        .expect("dynamicBountyMgr.GetOutputForClientSolarSystem");
    assert_eq!(dbs, PyRep::Tuple(vec![PyRep::Real(1.35), PyRep::Bool(false)]));
}

#[tokio::test]
async fn native_client_undock_and_space_entry_delivers_set_state_and_destiny_updates() {
    // parity: rpc:ship.Undock, rpc:beyonce.GetFormations, rpc:beyonce.UpdateStateRequest, feat:navigation.undock, destiny:SetState, notify:DoDestinyUpdate
    eve_testkit::attest(&[
        "rpc:ship.Undock",
        "rpc:beyonce.GetFormations",
        "rpc:beyonce.UpdateStateRequest",
        "feat:navigation.undock",
        "destiny:SetState",
        "notify:DoDestinyUpdate",
    ]);

    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket,
        CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType, MACHO_PROXY_NODE_ID,
    };
    use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    let ctx_server = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_server);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let client_ver = PyRep::Tuple(vec![
        PyRep::Integer(170_472),
        PyRep::Integer(496),
        PyRep::Integer(0),
        PyRep::Real(24.01),
        PyRep::Integer(3_624_425),
        PyRep::ascii_bytes("V24.01@ccp"),
    ]);
    write_framed_pyrep(&mut s, &client_ver).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let ok_cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(ok_cc.as_str(), Some("OK CC"));

    let challenge = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &challenge.to_pyrep())
        .await
        .unwrap();
    let pw_ver_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(pw_ver_rep, PyRep::Integer(2));
    let srv_hs_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&srv_hs_rep).expect("CryptoServerHandshake");
    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_framed_pyrep(&mut s, &hs_res.to_pyrep()).await.unwrap();

    // Ack + SessionInitialStateNotification + SessionChangeNotification
    let _ack = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        init_pkt.packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let _sess_chg0 = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let send_call = |call_id: u64,
                     service: &str,
                     bound_oid: Option<String>,
                     method: &str,
                     args: Vec<PyRep>,
                     kwargs: Vec<(PyRep, PyRep)>| {
        let inner_tuple = if let Some(oid_str) = bound_oid {
            PyRep::Tuple(vec![
                PyRep::ascii_bytes(&oid_str),
                PyRep::ascii_bytes(method),
                PyRep::Tuple(args),
                PyRep::Dict(kwargs),
            ])
        } else {
            PyRep::Tuple(vec![
                PyRep::ascii_bytes(method),
                PyRep::Tuple(args),
                PyRep::Dict(kwargs),
            ])
        };
        let is_obj = i64::from(matches!(inner_tuple, PyRep::Tuple(ref t) if t.len() == 4));
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(service.into()),
                call_id: 0,
            },
            user_id: Some(1001),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(is_obj),
                PyRep::SubStream(Box::new(inner_tuple)),
            ])]),
            named_payload: HashMap::new(),
        }
    };

    // 1. SelectCharacterID(90_000_001)
    let sel_pkt = send_call(
        1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(90_000_001)],
        vec![],
    );
    write_framed_pyrep(&mut s, &sel_pkt.to_pyrep()).await.unwrap();
    let sel_rsp = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    let sel_sess = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        sel_sess.packet_type,
        MachoPacketType::SessionChangeNotification
    );

    let ch = ctx.db.get_character(90_000_001).expect("character");
    let station_id = ch.station_id.expect("starts docked in Jita 4-4");
    let ship_id = ch.active_ship_id;

    // Fit an offline module onto the ship so we can verify `onlineModules=True` in `ship.Undock`.
    let mod_item = ctx
        .db
        .add_or_stack_item(90_000_001, ship_id, InventoryFlag::LoSlot0, 2048, 1)
        .expect("fit Damage Control II");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_item.item_id, false);
    });

    // 2. Bind station `ship` moniker (`Moniker('ship', (stationID, 15))`) and call `ship.Undock(shipID, False, onlineModules=True)`
    let bind_ship_pkt = send_call(
        2,
        "ship",
        None,
        "MachoBindObject",
        vec![
            PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]),
            PyRep::None,
        ],
        vec![],
    );
    write_framed_pyrep(&mut s, &bind_ship_pkt.to_pyrep())
        .await
        .unwrap();
    let bind_ship_rsp = loop {
        let pkt =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        if pkt.packet_type != MachoPacketType::Notification {
            break pkt;
        }
    };
    assert_eq!(bind_ship_rsp.packet_type, MachoPacketType::CallRsp);
    let PyRep::Tuple(rsp_outer) = &bind_ship_rsp.payload else {
        panic!("CallRsp payload tuple");
    };
    let PyRep::SubStream(rsp_sub) = &rsp_outer[0] else {
        panic!("CallRsp SubStream");
    };
    let PyRep::Tuple(bind_ship_parts) = rsp_sub.as_ref() else {
        panic!("MachoBindObject 2-tuple");
    };
    let ship_moniker_bid = oid_bound_id(&bind_ship_parts[0]);
    let ship_oid_str = format!("N={MACHO_PROXY_NODE_ID}:{ship_moniker_bid}");

    let undock_pkt = send_call(
        3,
        "ship",
        Some(ship_oid_str),
        "Undock",
        vec![PyRep::Integer(ship_id), PyRep::Bool(false)],
        vec![(PyRep::ascii_bytes("onlineModules"), PyRep::Bool(true))],
    );
    write_framed_pyrep(&mut s, &undock_pkt.to_pyrep())
        .await
        .unwrap();
    let undock_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let undock_sess =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        undock_sess.packet_type,
        MachoPacketType::SessionChangeNotification
    );

    // Verify module was put online by `onlineModules=True` and character is now in space in Jita (`30000142`)
    let is_mod_online = ctx.db.with_snapshot(|snap| {
        snap.module_online_states
            .get(&mod_item.item_id)
            .copied()
            .unwrap_or(false)
    });
    assert!(
        is_mod_online,
        "ship.Undock(..., onlineModules=True) must put fitted modules online"
    );
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().station_id, None);

    // 3. Space entry: `Michelle.AddBallpark` binds `beyonce` (`(30000142, 6)`), calls `GetFormations`, then `UpdateStateRequest`
    let bind_beyonce_pkt = send_call(
        4,
        "beyonce",
        None,
        "MachoBindObject",
        vec![
            PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(6)]),
            PyRep::None,
        ],
        vec![],
    );
    write_framed_pyrep(&mut s, &bind_beyonce_pkt.to_pyrep())
        .await
        .unwrap();
    let bind_beyonce_rsp = loop {
        let pkt =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        if pkt.packet_type != MachoPacketType::Notification {
            break pkt;
        }
    };
    assert_eq!(bind_beyonce_rsp.packet_type, MachoPacketType::CallRsp);
    let PyRep::Tuple(b_outer) = &bind_beyonce_rsp.payload else {
        panic!("CallRsp payload tuple");
    };
    let PyRep::SubStream(b_sub) = &b_outer[0] else {
        panic!("CallRsp SubStream");
    };
    let PyRep::Tuple(b_parts) = b_sub.as_ref() else {
        panic!("MachoBindObject 2-tuple");
    };
    let beyonce_bid = oid_bound_id(&b_parts[0]);
    let beyonce_oid_str = format!("N={MACHO_PROXY_NODE_ID}:{beyonce_bid}");

    let formations_pkt = send_call(
        5,
        "beyonce",
        Some(beyonce_oid_str.clone()),
        "GetFormations",
        vec![],
        vec![],
    );
    write_framed_pyrep(&mut s, &formations_pkt.to_pyrep())
        .await
        .unwrap();
    let formations_rsp = loop {
        let pkt =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        if pkt.packet_type != MachoPacketType::Notification {
            break pkt;
        }
    };
    assert_eq!(formations_rsp.packet_type, MachoPacketType::CallRsp);
    let PyRep::Tuple(f_outer) = &formations_rsp.payload else {
        panic!("CallRsp payload tuple");
    };
    let PyRep::SubStream(f_sub) = &f_outer[0] else {
        panic!("CallRsp SubStream");
    };
    let PyRep::Tuple(formations) = f_sub.as_ref() else {
        panic!("GetFormations must return a 2-tuple of formations for _destiny.so LoadFormations");
    };
    assert_eq!(formations.len(), 2);
    let PyRep::Tuple(diamond) = &formations[0] else {
        panic!("formation 0 must be a (name, positions) 2-tuple");
    };
    assert_eq!(diamond[0].as_str(), Some("Diamond"));
    let PyRep::Tuple(diamond_slots) = &diamond[1] else {
        panic!("formation positions must be a 4-tuple of (x, y, z) 3-tuples");
    };
    assert_eq!(diamond_slots.len(), 4);

    // 4. `beyonce.UpdateStateRequest()` -> returns `CallRsp(None)` followed immediately by `DoDestinyUpdate` `SetState`
    let usr_pkt = send_call(
        6,
        "beyonce",
        Some(beyonce_oid_str.clone()),
        "UpdateStateRequest",
        vec![],
        vec![],
    );
    write_framed_pyrep(&mut s, &usr_pkt.to_pyrep())
        .await
        .unwrap();
    let usr_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(usr_rsp.packet_type, MachoPacketType::CallRsp);

    let destiny_notif =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(destiny_notif.packet_type, MachoPacketType::Notification);
    let (bcast_id, notif_args) = destiny_notif
        .extract_broadcast_notification()
        .expect("3-layer GPCS Notification payload");
    assert_eq!(bcast_id, "DoDestinyUpdate");
    assert_eq!(notif_args.len(), 2);
    assert_eq!(notif_args[1], PyRep::Bool(false));
    let PyRep::List(updates_list) = &notif_args[0] else {
        panic!("DoDestinyUpdate arg[0] must be a List of (stamp, (funcName, args))");
    };
    assert_eq!(updates_list.len(), 1);
    let PyRep::Tuple(stamp_and_call) = &updates_list[0] else {
        panic!("(stamp, call)");
    };
    let PyRep::Tuple(func_and_args) = &stamp_and_call[1] else {
        panic!("(funcName, args)");
    };
    assert_eq!(func_and_args[0].as_str(), Some("SetState"));
    let PyRep::Tuple(set_state_args) = &func_and_args[1] else {
        panic!("SetState args tuple");
    };
    assert_eq!(
        set_state_args[0].dict_get("ego"),
        Some(&PyRep::Integer(ship_id))
    );
    let Some(PyRep::Bytes(state_bytes)) = set_state_args[0].dict_get("state") else {
        panic!("SetState KeyVal must contain binary 'state' buffer");
    };
    let (pkt_type, _stamp, decoded_balls) =
        eve_destiny::wire::decode_add_balls(state_bytes).expect("decode SetState binary buffer");
    assert_eq!(pkt_type, 0);
    assert!(decoded_balls.iter().any(|b| b.id == ship_id));
    assert!(decoded_balls.iter().any(|b| b.id == station_id));

    // 5. In-space movement command (`beyonce.CmdGotoDirection`) immediately delivers a `DoDestinyUpdate` notification
    let goto_pkt = send_call(
        7,
        "beyonce",
        Some(beyonce_oid_str),
        "CmdGotoDirection",
        vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
        vec![],
    );
    write_framed_pyrep(&mut s, &goto_pkt.to_pyrep())
        .await
        .unwrap();
    let mut goto_rsp = None;
    let mut has_goto = false;
    for _ in 0..15 {
        if let Ok(Ok(Some(rep))) = tokio::time::timeout(std::time::Duration::from_millis(500), read_framed_pyrep(&mut s)).await {
            if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
                if pkt.packet_type == MachoPacketType::CallRsp {
                    goto_rsp = Some(pkt);
                } else if pkt.packet_type == MachoPacketType::Notification {
                    if let Some((bcast, args)) = pkt.extract_broadcast_notification() {
                        if bcast == "DoDestinyUpdate" {
                            if let Some(PyRep::List(updates)) = args.get(0) {
                                if updates.iter().any(|u| matches!(u, PyRep::Tuple(sc) if matches!(&sc[1], PyRep::Tuple(fa) if fa[0].as_str() == Some("GotoDirection")))) {
                                    has_goto = true;
                                }
                            }
                        }
                    }
                }
                if goto_rsp.is_some() && has_goto {
                    break;
                }
            }
        }
    }
    assert!(goto_rsp.is_some(), "CmdGotoDirection must receive CallRsp");
    assert!(
        has_goto,
        "CmdGotoDirection must emit GotoDirection in DoDestinyUpdate"
    );
}
