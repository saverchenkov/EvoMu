//! Parity test suite for core RPC services batch:
//! `lookupSvc`, `charUnboundMgr`, `pvpFilamentMgr`, `mailingListsMgr`, and `map`.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{DBRowDescriptor, PyRep};
use eve_server::router::ServerContext;

fn as_list(rep: &PyRep) -> &[PyRep] {
    match rep {
        PyRep::List(ref l) => l.as_slice(),
        _ => panic!("expected List, got {rep:?}"),
    }
}

fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
    match rep {
        PyRep::Dict(ref d) => d.as_slice(),
        _ => panic!("expected Dict, got {rep:?}"),
    }
}

fn crowset_header_has_column(rep: &PyRep, col_name: &str) -> bool {
    if let PyRep::ObjectEx {
        is_type_2: true,
        header,
        ..
    } = rep
    {
        if let PyRep::Tuple(items) = header.as_ref() {
            if let Some(PyRep::Dict(dict)) = items.get(1) {
                for (k, v) in dict {
                    if k.as_str() == Some("header") {
                        if let Some(desc) = DBRowDescriptor::from_pyrep(v) {
                            return desc.column_names().iter().any(|c| c == col_name);
                        }
                    }
                }
            }
        }
    }
    false
}

#[tokio::test]
async fn test_rpc_lookup_svc_batch() {
    // parity: rpc:lookupSvc.LookupCharacters, rpc:lookupSvc.LookupCorporations, rpc:lookupSvc.LookupEvePlayerCharacters, rpc:lookupSvc.LookupFactions, rpc:lookupSvc.LookupKnownLocationsByGroup, rpc:lookupSvc.LookupLocationsByGroup, rpc:lookupSvc.LookupNoneNPCAccountOwners, rpc:lookupSvc.LookupOwners, rpc:lookupSvc.LookupPCOwners, rpc:lookupSvc.LookupWarableCorporationsOrAlliances
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_001,
            eve_db::CorporationRecord {
                corporation_id: 98_000_001,
                name: "Federation Defense Corp".into(),
                ticker: "FED".into(),
                ceo_id: 90_000_001,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents::from_isk(1_000_000),
                members: HashSet::from([90_000_001]),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
            },
        );
    });

    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. LookupCharacters
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCharacters",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupCharacters");
    let chars = as_list(&res);
    assert_eq!(chars.is_empty(), false);
    let char_name = chars[0]
        .dict_get("characterName")
        .and_then(PyRep::as_str)
        .expect("characterName");
    assert_eq!(char_name.to_ascii_lowercase().contains("aura"), true);

    // 2. LookupEvePlayerCharacters
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupEvePlayerCharacters",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupEvePlayerCharacters");
    let pc_chars = as_list(&res);
    assert_eq!(pc_chars.is_empty(), false);
    let cid = pc_chars[0]
        .dict_get("characterID")
        .and_then(PyRep::as_i64)
        .expect("characterID");
    assert_eq!(cid >= 90_000_000, true);

    // 3. LookupCorporations
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCorporations",
            &[PyRep::String("Federation".into())],
            &kw,
            None,
        )
        .await
        .expect("LookupCorporations");
    let corps = as_list(&res);
    assert_eq!(corps.is_empty(), false);
    let corp_type = corps[0]
        .dict_get("corporationType")
        .and_then(PyRep::as_i64)
        .expect("corporationType");
    assert_eq!(corp_type == 1 || corp_type == 2, true);

    // 4. LookupFactions
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupFactions",
            &[PyRep::String("Caldari".into())],
            &kw,
            None,
        )
        .await
        .expect("LookupFactions");
    let factions = as_list(&res);
    assert_eq!(factions.is_empty(), false);
    let faction_name = factions[0]
        .dict_get("factionName")
        .and_then(PyRep::as_str)
        .expect("factionName");
    assert_eq!(faction_name.contains("Caldari"), true);

    // 5. LookupKnownLocationsByGroup
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupKnownLocationsByGroup",
            &[
                PyRep::Integer(5),
                PyRep::String("Jita".into()),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("LookupKnownLocationsByGroup");
    let locs = as_list(&res);
    assert_eq!(locs.is_empty(), false);
    let gid = locs[0]
        .dict_get("groupID")
        .and_then(PyRep::as_i64)
        .expect("groupID");
    assert_eq!(gid, 5);
    let item_name = locs[0]
        .dict_get("itemName")
        .and_then(PyRep::as_str)
        .expect("itemName");
    assert_eq!(item_name, "Jita");

    // 6. LookupLocationsByGroup
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupLocationsByGroup",
            &[
                PyRep::Integer(5),
                PyRep::String("Jita".into()),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("LookupLocationsByGroup");
    let locs2 = as_list(&res);
    assert_eq!(locs2.is_empty(), false);
    let item_name2 = locs2[0]
        .dict_get("itemName")
        .and_then(PyRep::as_str)
        .expect("itemName");
    assert_eq!(item_name2, "Jita");

    // 7. LookupNoneNPCAccountOwners
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupNoneNPCAccountOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupNoneNPCAccountOwners");
    let non_npc = as_list(&res);
    assert_eq!(non_npc.is_empty(), false);
    let gid_non_npc = non_npc[0]
        .dict_get("groupID")
        .and_then(PyRep::as_i64)
        .expect("groupID");
    assert_eq!(gid_non_npc == 1 || gid_non_npc == 2, true);

    // 8. LookupOwners
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupOwners");
    let owners = as_list(&res);
    assert_eq!(owners.is_empty(), false);
    let owner_name = owners[0]
        .dict_get("ownerName")
        .and_then(PyRep::as_str)
        .expect("ownerName");
    assert_eq!(owner_name.to_ascii_lowercase().contains("aura"), true);

    // 9. LookupPCOwners
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupPCOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupPCOwners");
    let pc_owners = as_list(&res);
    assert_eq!(pc_owners.is_empty(), false);
    let oid = pc_owners[0]
        .dict_get("ownerID")
        .and_then(PyRep::as_i64)
        .expect("ownerID");
    assert_eq!(oid >= 90_000_000, true);

    // 10. LookupWarableCorporationsOrAlliances
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupWarableCorporationsOrAlliances",
            &[PyRep::String("Federation".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let warable = as_list(&res);
    assert_eq!(warable.is_empty(), false);
    let gid_war = warable[0]
        .dict_get("groupID")
        .and_then(PyRep::as_i64)
        .expect("groupID");
    assert_eq!(gid_war == 2 || gid_war == 32, true);
}

#[tokio::test]
async fn test_rpc_char_unbound_mgr_batch() {
    // parity: rpc:charUnboundMgr.CancelCharacterDeletePrepare, rpc:charUnboundMgr.DeleteCharacter, rpc:charUnboundMgr.GetCharOmegaDowngradeStatus, rpc:charUnboundMgr.GetCharacterLockType, rpc:charUnboundMgr.GetCohortsForUser, rpc:charUnboundMgr.GetNumCharacters, rpc:charUnboundMgr.GetQAStarterSystemIDs, rpc:charUnboundMgr.PrepareCharacterForDelete, rpc:charUnboundMgr.ToggleValidation, rpc:charUnboundMgr.UpdateCharacterBloodline, rpc:charUnboundMgr.UpdateCharacterGender
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. GetNumCharacters
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetNumCharacters", &[], &kw, None)
        .await
        .expect("GetNumCharacters");
    let num_chars = match &res {
        PyRep::Tuple(v) => v.first().and_then(PyRep::as_i64).expect("num characters int"),
        other => other.as_i64().expect("num characters int"),
    };
    assert_eq!(num_chars >= 1, true);

    // 2. GetCharOmegaDowngradeStatus
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharOmegaDowngradeStatus",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharOmegaDowngradeStatus");
    assert_eq!(res.as_bool(), Some(false));

    // 3. GetCharacterLockType
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterLockType",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterLockType");
    assert_eq!(matches!(res, PyRep::None), true);

    // 4. GetCohortsForUser
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCohortsForUser", &[], &kw, None)
        .await
        .expect("GetCohortsForUser");
    let cohorts = as_list(&res);
    assert_eq!(cohorts.is_empty(), true);

    // 5. GetQAStarterSystemIDs
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetQAStarterSystemIDs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetQAStarterSystemIDs");
    let starter_ids = as_list(&res);
    assert_eq!(starter_ids.len(), 4);
    assert_eq!(starter_ids[0].as_i64(), Some(30_000_142));

    // 6. ToggleValidation
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "ToggleValidation", &[], &kw, None)
        .await
        .expect("ToggleValidation");
    assert_eq!(res.as_bool(), Some(true));

    // 7. UpdateCharacterBloodline (unauthorized fails, owned succeeds)
    let unauth_res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterBloodline",
            &[PyRep::Integer(99_999_999), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unauth_res.is_err(), true);

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterBloodline",
            &[PyRep::Integer(90_000_001), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("UpdateCharacterBloodline");
    assert_eq!(res.as_bool(), Some(true));
    let bloodline = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().bloodline_id);
    assert_eq!(bloodline, 2);

    // 8. UpdateCharacterGender (unauthorized fails, owned succeeds)
    let unauth_res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterGender",
            &[PyRep::Integer(99_999_999), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unauth_res.is_err(), true);

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterGender",
            &[PyRep::Integer(90_000_001), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("UpdateCharacterGender");
    assert_eq!(res.as_bool(), Some(true));
    let gender = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().gender);
    assert_eq!(gender, 1);

    // 9. PrepareCharacterForDelete
    let temp_char_id = 90_999_999;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get(&90_000_001).cloned() {
            let mut temp = c;
            temp.character_id = temp_char_id;
            temp.name = "Temp Delete Candidate".into();
            snap.characters.insert(temp_char_id, temp);
        }
    });

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "PrepareCharacterForDelete",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("PrepareCharacterForDelete");
    let del_date = res.as_i64().expect("deleteDate");
    assert_eq!(del_date > 0, true);

    // 10. CancelCharacterDeletePrepare
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CancelCharacterDeletePrepare",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("CancelCharacterDeletePrepare");
    assert_eq!(res.as_bool(), Some(true));

    // 11. DeleteCharacter
    // Prepare again
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "PrepareCharacterForDelete",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("PrepareCharacterForDelete 2");

    // Immediate deletion without countdown elapsed must fail
    let premature_del = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "DeleteCharacter",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(premature_del.is_err(), true);

    // Simulate 10-hour biomass delay elapsing by advancing preparation timestamp
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = eve_server::services::character_lifecycle::load_store(snap);
        store.delete_prepared_unix.insert(temp_char_id, ctx.now_unix().saturating_sub(36000 + 10));
        eve_server::services::character_lifecycle::save_store(snap, &store);
    });

    // Delete succeeds after delay expires
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "DeleteCharacter",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("DeleteCharacter");
    assert_eq!(matches!(res, PyRep::KeyVal(_) | PyRep::Bool(true)), true);
    let is_biomassed = ctx
        .db
        .with_snapshot(|s| s.characters.get(&temp_char_id).unwrap().user_id == 0);
    assert_eq!(is_biomassed, true);
}

#[tokio::test]
async fn test_rpc_pvp_filament_mgr_batch() {
    // parity: rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation, rpc:pvpFilamentMgr.GetActiveEvents, rpc:pvpFilamentMgr.GetAllEvents, rpc:pvpFilamentMgr.GetCharacterStatistics, rpc:pvpFilamentMgr.GetLeaderboard, rpc:pvpFilamentMgr.GetMostRecentEvent, rpc:pvpFilamentMgr.GetNextEventDate, rpc:pvpFilamentMgr.JoinPVPQueue, rpc:pvpFilamentMgr.LeavePVPQueue
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });
    // 1. JoinPVPQueue (verify inventory check, consumption, and persistent state)
    let filament_item_id = 88_888_001;
    let hangar_filament_id = 88_888_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            filament_item_id,
            eve_db::InventoryItemRecord {
                item_id: filament_item_id, type_id: 56000, owner_id: 90_000_001,
                location_id: 100_000_001, flag: eve_db::InventoryFlag::Cargo, quantity: 1, ..Default::default()
            },
        );
        snap.items.insert(
            hangar_filament_id,
            eve_db::InventoryItemRecord {
                item_id: hangar_filament_id, type_id: 56000, owner_id: 90_000_001,
                location_id: 100_000_001, flag: eve_db::InventoryFlag::Hangar, quantity: 1, ..Default::default()
            },
        );
    });

    // Non-positive item_id fails immediately
    let zero_item_res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(0), PyRep::Integer(0)], &kw, None)
        .await;
    assert_eq!(zero_item_res.is_err(), true);

    // Filament in Hangar flag is rejected
    let hangar_res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(hangar_filament_id), PyRep::Integer(56000)], &kw, None)
        .await;
    assert_eq!(hangar_res.is_err(), true);
    assert_eq!(hangar_res.unwrap_err().code, "ItemNotInCargo");

    // Unowned item fails
    let unowned_res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(99_999_999), PyRep::Integer(56000)], &kw, None)
        .await;
    assert_eq!(unowned_res.is_err(), true);

    // Owned item succeeds and consumes filament
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(filament_item_id), PyRep::Integer(56000)],
            &kw,
            None,
        )
        .await
        .expect("JoinPVPQueue");
    assert_eq!(res, PyRep::Integer(30));

    // Verify item consumed
    let item_consumed = ctx.db.with_snapshot(|s| !s.items.contains_key(&filament_item_id));
    assert_eq!(item_consumed, true);

    // Verify queue persistent state in SQLite-backed store
    let in_queue = ctx.db.with_snapshot(|snap| {
        let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
        store.queue.contains_key(&90_000_001)
    });
    assert_eq!(in_queue, true, "Character must be present in queue state after JoinPVPQueue");

    // 2. LeavePVPQueue
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "LeavePVPQueue", &[], &kw, None)
        .await
        .expect("LeavePVPQueue");
    assert_eq!(matches!(res, PyRep::None), true);

    // Verify queue removal persistent state in store
    let in_queue_after = ctx.db.with_snapshot(|snap| {
        let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
        store.queue.contains_key(&90_000_001)
    });
    assert_eq!(in_queue_after, false, "Character must be removed from queue state after LeavePVPQueue");

    // Seed authentic match statistics
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
        store.stats.insert(90_000_001, (5, 1, 50, 6, 12000.0));
        eve_server::services::pvp_filament_mgr::save_pvp_store(snap, &store);
    });

    // 3. GetAllEvents
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetAllEvents", &[], &kw, None)
        .await
        .expect("GetAllEvents");
    let all_events = as_dict(&res);
    assert_eq!(all_events.len(), 3);
    let ev_name = all_events[0]
        .1
        .dict_get("name")
        .and_then(PyRep::as_str)
        .expect("event name");
    assert_eq!(ev_name, "Frigate Proving Grounds");

    // 4. GetActiveEvents
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetActiveEvents", &[], &kw, None)
        .await
        .expect("GetActiveEvents");
    let act_events = as_dict(&res);
    assert_eq!(act_events.len(), 1);
    let match_type = act_events[0]
        .1
        .dict_get("matchTypeID")
        .and_then(PyRep::as_i64)
        .expect("matchTypeID");
    assert_eq!(match_type, 1);

    // 5. GetMostRecentEvent
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "GetMostRecentEvent",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMostRecentEvent");
    let ev_id = res
        .dict_get("eventID")
        .and_then(PyRep::as_i64)
        .expect("eventID");
    assert_eq!(ev_id, 2);

    // 6. GetNextEventDate
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "GetNextEventDate",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetNextEventDate");
    let next_date = res.as_i64().expect("next event date");
    assert_eq!(next_date > 0, true);

    // 7. GetLeaderboard (returns seeded authentic match record)
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetLeaderboard", &[], &kw, None)
        .await
        .expect("GetLeaderboard");
    let lb = as_list(&res);
    assert_eq!(lb.is_empty(), false);
    let score = lb[0].dict_get("score").and_then(PyRep::as_i64).expect("score");
    assert_eq!(score, 50);

    // 8. GetCharacterStatistics (returns authentic statistics)
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "GetCharacterStatistics",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterStatistics");
    let stats_cid = res
        .dict_get("characterID")
        .and_then(PyRep::as_i64)
        .expect("characterID");
    assert_eq!(stats_cid, 90_000_001);
    let wins = res
        .dict_get("wins")
        .and_then(PyRep::as_i64)
        .expect("wins");
    assert_eq!(wins, 5);
    let dmg = res
        .dict_get("damageDealt")
        .and_then(PyRep::as_f64)
        .expect("damageDealt");
    assert_eq!(dmg, 12000.0);

    // 9. AbyssalPVPEndGateActivation
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "AbyssalPVPEndGateActivation",
            &[PyRep::Integer(70001)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalPVPEndGateActivation");
    let gid = res
        .dict_get("gateID")
        .and_then(PyRep::as_i64)
        .expect("gateID");
    assert_eq!(gid, 70001);
    let activated = res
        .dict_get("activated")
        .and_then(PyRep::as_bool)
        .expect("activated");
    assert_eq!(activated, true);
}

#[tokio::test]
async fn test_rpc_mailing_lists_mgr_batch() {
    // parity: rpc:mailingListsMgr.Delete, rpc:mailingListsMgr.GetInfo, rpc:mailingListsMgr.GetSettings, rpc:mailingListsMgr.Leave, rpc:mailingListsMgr.SendWelcomeMail, rpc:mailingListsMgr.SetDefaultAccess, rpc:mailingListsMgr.SetEntitiesAccess, rpc:mailingListsMgr.SetMembersClear
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Create mailing list to manage
    let create_res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "Create",
            &[PyRep::String("Alliance Fleet Broadcast".into())],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let list_id = create_res
        .dict_get("listID")
        .and_then(PyRep::as_i64)
        .expect("created listID");
    assert_eq!(list_id > 0, true);

    // 1. GetInfo
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "GetInfo",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("GetInfo");
    let lid = res
        .dict_get("listID")
        .and_then(PyRep::as_i64)
        .expect("listID");
    assert_eq!(lid, list_id);
    let list_name = res
        .dict_get("name")
        .and_then(PyRep::as_str)
        .expect("list name");
    assert_eq!(list_name, "Alliance Fleet Broadcast");

    // 2. SetDefaultAccess
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "SetDefaultAccess",
            &[PyRep::Integer(list_id), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("SetDefaultAccess");
    assert_eq!(
        res.dict_get("defaultAccess").and_then(PyRep::as_i64),
        Some(2)
    );

    // 3. SetEntitiesAccess
    let entity_access_dict = vec![(PyRep::Integer(90_000_002), PyRep::Integer(1))];
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "SetEntitiesAccess",
            &[PyRep::Integer(list_id), PyRep::Dict(entity_access_dict)],
            &kw,
            None,
        )
        .await
        .expect("SetEntitiesAccess");
    assert_eq!(
        res.dict_get("listID").and_then(PyRep::as_i64),
        Some(list_id)
    );

    // 4. GetSettings
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "GetSettings",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("GetSettings");
    let def_access = res
        .dict_get("defaultAccess")
        .and_then(PyRep::as_i64)
        .expect("defaultAccess");
    assert_eq!(def_access, 2);

    // 5. SetMembersClear
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "SetMembersClear",
            &[
                PyRep::Integer(list_id),
                PyRep::List(vec![PyRep::Integer(90_000_001)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("SetMembersClear");
    assert_eq!(res.as_bool(), Some(true));

    // 6. SendWelcomeMail
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "SendWelcomeMail",
            &[PyRep::Integer(list_id), PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("SendWelcomeMail");
    assert_eq!(res.as_bool(), Some(true));

    // 7. Leave
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "Leave",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("Leave");
    assert_eq!(res.as_bool(), Some(true));

    // 8. Delete
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "Delete",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("Delete");
    assert_eq!(res.as_bool(), Some(true));

    // Verify persistent deletion: GetInfo must fail on deleted mailing list
    let get_after_del = ctx
        .dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "GetInfo",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await;
    assert!(get_after_del.is_err(), "GetInfo must fail on deleted mailing list");
}

#[tokio::test]
async fn test_rpc_map_svc_part1_batch() {
    // parity: rpc:map.GetBeaconCount, rpc:map.GetCurrentSovData, rpc:map.GetDeadspaceComplexMap, rpc:map.GetEdencomFortressSystems, rpc:map.GetEdencomMinorVictorySystems, rpc:map.GetFacWarZoneInfo, rpc:map.GetIncursionGlobalReport
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        // Seed cyno beacon structure (type 35841) in system 30000142
        snap.structures.insert(
            77_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 77_000_001,
                type_id: 35841,
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_001,
                name: "Jita Cyno Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // Seed sov system state with alliance
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "Northern Coalition".into(),
                short_name: "NC".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: HashSet::from([98_000_001]),
            },
        );
        snap.system_states.insert(
            30_000_142,
            eve_db::SystemDynamicState {
                solar_system_id: 30_000_142,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. GetBeaconCount
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetBeaconCount", &[], &kw, None)
        .await
        .expect("GetBeaconCount");
    let beacons = as_dict(&res);
    assert_eq!(beacons.is_empty(), false);
    let count = beacons[0].1.as_i64().expect("beacon count int");
    assert_eq!(count >= 1, true);

    // 2. GetConstellationLPData
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetConstellationLPData",
            &[PyRep::Integer(20_000_020)],
            &kw,
            None,
        )
        .await
        .expect("GetConstellationLPData");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "solarSystemID"), true);

    // 3. GetCurrentSovData
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetCurrentSovData", &[], &kw, None)
        .await
        .expect("GetCurrentSovData");
    let sov_rows = as_list(&res);
    assert_eq!(sov_rows.is_empty(), false);
    let row = &sov_rows[0];
    assert_eq!(
        row.dict_get("solarSystemID").and_then(PyRep::as_i64),
        Some(30_000_142)
    );
    assert_eq!(
        row.dict_get("allianceID").and_then(PyRep::as_i64),
        Some(99_000_001)
    );

    // 4. GetDeadspaceAgentsMap
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetDeadspaceAgentsMap",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDeadspaceAgentsMap");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "agentID"), true);
    let agent_rows = res.crowset_rows().expect("agent rows");
    assert_eq!(agent_rows.is_empty(), false);

    // 5. GetDeadspaceComplexMap
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetDeadspaceComplexMap",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDeadspaceComplexMap");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "dungeonID"), true);

    // 6. GetEdencomFortressSystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetEdencomFortressSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetEdencomFortressSystems");
    assert_eq!(matches!(&res, PyRep::List(_)), true);

    // 7. GetEdencomMinorVictorySystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetEdencomMinorVictorySystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetEdencomMinorVictorySystems");
    assert_eq!(matches!(&res, PyRep::List(_)), true);

    // 8. GetFacWarZoneInfo
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetFacWarZoneInfo",
            &[PyRep::Integer(500_001)],
            &kw,
            None,
        )
        .await
        .expect("GetFacWarZoneInfo");
    let max_pts = res
        .dict_get("maxWarZonePoints")
        .and_then(PyRep::as_i64)
        .expect("maxWarZonePoints");
    assert_eq!(max_pts, 100_000);

    // 9. GetHistory
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetHistory", &[], &kw, None)
        .await
        .expect("GetHistory");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "solarSystemID"), true);

    // 10. GetIncursionGlobalReport
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetIncursionGlobalReport",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetIncursionGlobalReport");
    assert_eq!(matches!(&res, PyRep::List(_)), true);
}

#[tokio::test]
async fn test_rpc_map_svc_part2_batch() {
    // parity: rpc:map.GetIndustryJobsOverLast24Hours, rpc:map.GetMyExtraMapInfo, rpc:map.GetMyExtraMapInfoAgents, rpc:map.GetRecentSovActivity, rpc:map.GetSecurityModifiedSystems, rpc:map.GetSolarSystemVisits, rpc:map.GetStationCount, rpc:map.GetStationInfo, rpc:map.GetSystemsInIncursions, rpc:map.GetTriglavianMinorVictorySystems
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            77_001,
            eve_db::IndustryJobRecord {
                job_id: 77_001,
                installer_id: 90_000_001,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_001,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 5,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 1700000000,
            },
        );
    });

    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. GetIndustryJobsOverLast24Hours
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetIndustryJobsOverLast24Hours",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetIndustryJobsOverLast24Hours");
    if let PyRep::Rowset { columns, rows } = &res {
        assert_eq!(columns.iter().any(|c| c == "solarSystemID"), true);
        assert_eq!(columns.iter().any(|c| c == "noOfJobs"), true);
        assert_eq!(rows.is_empty(), false);
        assert_eq!(rows[0][1].as_i64(), Some(5));
    } else {
        panic!("expected Rowset for GetIndustryJobsOverLast24Hours");
    }

    // 2. GetMyExtraMapInfo
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetMyExtraMapInfo", &[], &kw, None)
        .await
        .expect("GetMyExtraMapInfo");
    let extra_map = as_list(&res);
    assert_eq!(extra_map.is_empty(), false);
    let my_cid = extra_map[0]
        .dict_get("characterID")
        .and_then(PyRep::as_i64)
        .expect("characterID");
    assert_eq!(my_cid, 90_000_001);

    // 3. GetMyExtraMapInfoAgents (queries authentic agent standings)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.insert(3011394, 5.0);
        }
    });

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetMyExtraMapInfoAgents",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyExtraMapInfoAgents");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "agentID"), true);
    let agent_rows = res.crowset_rows().expect("agents rows");
    assert_eq!(agent_rows.len(), 1);
    assert_eq!(agent_rows[0].dbrow_get("agentID").and_then(PyRep::as_i64), Some(3011394));

    // 4. GetRecentSovActivity (queries authentic system states)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(30_000_142).or_default().sov_alliance_id = Some(99_000_001);
    });

    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetRecentSovActivity", &[], &kw, None)
        .await
        .expect("GetRecentSovActivity");
    let sov_act = as_list(&res);
    assert_eq!(sov_act.is_empty(), false);
    let owner_id = sov_act[0]
        .dict_get("ownerID")
        .and_then(PyRep::as_i64)
        .expect("ownerID");
    assert_eq!(owner_id, 99_000_001);

    // 5. GetSecurityModifiedSystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetSecurityModifiedSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSecurityModifiedSystems");
    assert_eq!(res.is_crowset(), true);
    assert_eq!(crowset_header_has_column(&res, "solarSystemID"), true);
    assert_eq!(crowset_header_has_column(&res, "modifiedSecurity"), true);

    // 6. GetSolarSystemVisits
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("GetSolarSystemVisits");
    let visits = as_dict(&res);
    assert_eq!(visits.is_empty(), false);
    let visit_count = visits[0].1.as_i64().expect("visit count int");
    assert_eq!(visit_count, 1);

    // 7. GetStationCount
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetStationCount", &[], &kw, None)
        .await
        .expect("GetStationCount");
    let stations = as_dict(&res);
    assert_eq!(stations.is_empty(), false);
    let st_count = stations[0].1.as_i64().expect("station count int");
    assert_eq!(st_count > 0, true);

    // 8. GetStationInfo
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetStationInfo", &[], &kw, None)
        .await
        .expect("GetStationInfo");
    if let PyRep::Rowset { columns, rows } = &res {
        assert_eq!(columns.iter().any(|c| c == "stationID"), true);
        assert_eq!(rows.is_empty(), false);
    } else {
        panic!("expected Rowset for GetStationInfo");
    }

    // 9. GetSystemsInIncursions
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetSystemsInIncursions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSystemsInIncursions");
    assert_eq!(matches!(&res, PyRep::List(_)), true);

    // 10. GetTriglavianMinorVictorySystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetTriglavianMinorVictorySystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTriglavianMinorVictorySystems");
    assert_eq!(matches!(&res, PyRep::List(_)), true);
}
