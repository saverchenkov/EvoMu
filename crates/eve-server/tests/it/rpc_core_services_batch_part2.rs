//! Parity test suite for core RPC services batch (Part 2: `pvpFilamentMgr` and `mailingListsMgr`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
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
                item_id: filament_item_id,
                type_id: 53965,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_filament_id,
            eve_db::InventoryItemRecord {
                item_id: hangar_filament_id,
                type_id: 53965,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // Non-positive item_id fails immediately
    let zero_item_res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(0), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(zero_item_res.is_err());

    // Filament in Hangar flag is rejected
    let hangar_res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(hangar_filament_id), PyRep::Integer(53965)],
            &kw,
            None,
        )
        .await;
    assert!(hangar_res.is_err());
    assert_eq!(hangar_res.unwrap_err().code, "ItemNotInCargo");

    // Unowned item fails
    let unowned_res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(99_999_999), PyRep::Integer(53965)],
            &kw,
            None,
        )
        .await;
    assert!(unowned_res.is_err());

    // Owned item succeeds and consumes filament
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(filament_item_id), PyRep::Integer(53965)],
            &kw,
            None,
        )
        .await
        .expect("JoinPVPQueue");
    assert_eq!(res, PyRep::Integer(30));

    let item_consumed = ctx.db.with_snapshot(|s| !s.items.contains_key(&filament_item_id));
    assert!(item_consumed);

    let in_queue = ctx.db.with_snapshot(|snap| {
        let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
        store.queue.contains_key(&90_000_001)
    });
    assert!(in_queue, "Character must be present in queue state after JoinPVPQueue");

    // 2. LeavePVPQueue
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "LeavePVPQueue", &[], &kw, None)
        .await
        .expect("LeavePVPQueue");
    assert!(matches!(res, PyRep::None));

    let in_queue_after = ctx.db.with_snapshot(|snap| {
        let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
        store.queue.contains_key(&90_000_001)
    });
    assert!(
        !in_queue_after,
        "Character must be removed from queue state after LeavePVPQueue"
    );

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
    assert!(next_date > 0);

    // 7. GetLeaderboard
    let res = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetLeaderboard", &[], &kw, None)
        .await
        .expect("GetLeaderboard");
    let lb = as_list(&res);
    assert!(!lb.is_empty());
    let score = lb[0].dict_get("score").and_then(PyRep::as_i64).expect("score");
    assert_eq!(score, 50);

    // 8. GetCharacterStatistics
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
    let wins = res.dict_get("wins").and_then(PyRep::as_i64).expect("wins");
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
    let gid = res.dict_get("gateID").and_then(PyRep::as_i64).expect("gateID");
    assert_eq!(gid, 70001);
    let activated = res
        .dict_get("activated")
        .and_then(PyRep::as_bool)
        .expect("activated");
    assert!(activated);
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
        .as_i64()
        .or_else(|| create_res.dict_get("listID").and_then(PyRep::as_i64))
        .expect("created listID");
    assert!(list_id > 0);

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
    let lid = res.dict_get("listID").and_then(PyRep::as_i64).expect("listID");
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
    assert_eq!(res.dict_get("defaultAccess").and_then(PyRep::as_i64), Some(2));

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
    assert_eq!(res.dict_get("listID").and_then(PyRep::as_i64), Some(list_id));

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
            &[
                PyRep::Integer(list_id),
                PyRep::String("Welcome".into()),
                PyRep::String("Rules".into()),
            ],
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
