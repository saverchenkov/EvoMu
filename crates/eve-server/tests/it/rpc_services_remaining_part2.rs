//! Integration tests for verified solid RPC parity rows across `activityMgr`,
//! `charMgr`, `devIndexManager`, `expertSystemMgr`, `i2`, `search`, and `abyssalMgr`.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const EXPERT_SYSTEM_TYPE_ID: u32 = 57203;

async fn setup_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
// parity: rpc:activityMgr.ActivitySeen
// parity: rpc:activityMgr.GetActivityBadgeStatuses
// parity: rpc:charMgr.DeleteCharacterSetting
// parity: rpc:charMgr.SaveCharacterSetting
// parity: rpc:devIndexManager.GetDevelopmentIndicesForSystem
// parity: rpc:expertSystemMgr.ConsumeExpertSystem
// parity: rpc:expertSystemMgr.GetMyExpertSystems
// parity: rpc:expertSystemMgr.RemoveMyExpertSystem
// parity: rpc:i2.GetItem
// parity: rpc:search.Query
// parity: rpc:search.QuickQuery
async fn test_rpc_solid_activity_charmgr_expert_devindex_i2_search() {
    eve_testkit::attest(&[
        "rpc:activityMgr.ActivitySeen",
        "rpc:activityMgr.GetActivityBadgeStatuses",
        "rpc:charMgr.DeleteCharacterSetting",
        "rpc:charMgr.SaveCharacterSetting",
        "rpc:devIndexManager.GetDevelopmentIndicesForSystem",
        "rpc:expertSystemMgr.ConsumeExpertSystem",
        "rpc:expertSystemMgr.GetMyExpertSystems",
        "rpc:expertSystemMgr.RemoveMyExpertSystem",
        "rpc:i2.GetItem",
        "rpc:search.Query",
        "rpc:search.QuickQuery",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 41_101, char_id).await;

    // 1. activityMgr: ActivitySeen, GetActivityBadgeStatuses
    assert_eq!(
        rpc(&ctx, &mut sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(42)]).await.unwrap(),
        PyRep::Bool(true)
    );
    let badges = rpc(&ctx, &mut sess, "activityMgr", "GetActivityBadgeStatuses", &[]).await.unwrap();
    assert_eq!(badges, PyRep::List(vec![PyRep::Integer(42)]));

    // 2. charMgr: SaveCharacterSetting, DeleteCharacterSetting
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("overview_tab".into()), PyRep::Integer(3)],
    )
    .await
    .unwrap();
    let settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[]).await.unwrap();
    assert_eq!(settings.dict_get("overview_tab"), Some(&PyRep::Integer(3)));
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("overview_tab".into())]
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // 3. devIndexManager: GetDevelopmentIndicesForSystem
    let dev_indices = rpc(
        &ctx,
        &mut sess,
        "devIndexManager",
        "GetDevelopmentIndicesForSystem",
        &[PyRep::Integer(30_000_142)],
    )
    .await
    .unwrap();
    assert!(dev_indices.dict_get("military").is_some());
    assert!(dev_indices.dict_get("industrial").is_some());
    assert!(dev_indices.dict_get("strategic").is_some());

    // 4. expertSystemMgr: ConsumeExpertSystem, GetMyExpertSystems, RemoveMyExpertSystem
    let init_es = rpc(&ctx, &mut sess, "expertSystemMgr", "GetMyExpertSystems", &[]).await.unwrap();
    assert_eq!(init_es, PyRep::Dict(vec![]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_572_031,
            eve_db::InventoryItemRecord {
                item_id: 880_572_031,
                type_id: EXPERT_SYSTEM_TYPE_ID,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let consumed = rpc(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "ConsumeExpertSystem",
        &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)],
    )
    .await
    .unwrap();
    assert_eq!(consumed.dict_get("consumed"), Some(&PyRep::Bool(true)));

    let my_es = rpc(&ctx, &mut sess, "expertSystemMgr", "GetMyExpertSystems", &[]).await.unwrap();
    assert!(matches!(my_es, PyRep::Dict(ref d) if !d.is_empty()));

    let removed = rpc(
        &ctx,
        &mut sess,
        "expertSystemMgr",
        "RemoveMyExpertSystem",
        &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)],
    )
    .await
    .unwrap();
    assert_eq!(removed, PyRep::Bool(true));

    // 5. i2: GetItem
    // rpc-lint: allow legacy i2 service in parity ledger
    let item_err = rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(880_572_031)])
        .await
        .unwrap_err(); // was consumed and removed above
    assert_eq!(item_err.code, "CustomError");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_572_032,
            eve_db::InventoryItemRecord {
                item_id: 880_572_032,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    // rpc-lint: allow legacy i2 service in parity ledger
    let tritanium = rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(880_572_032)]).await.unwrap();
    assert_eq!(tritanium.dict_get("itemID"), Some(&PyRep::Integer(880_572_032)));
    assert_eq!(tritanium.dict_get("typeID"), Some(&PyRep::Integer(34)));

    // 6. search: Query, QuickQuery
    let PyRep::Dict(q_res) = rpc(&ctx, &mut sess, "search", "Query", &[PyRep::String("jita".into())]).await.unwrap() else {
        panic!("Expected Dict")
    };
    assert!(!q_res.is_empty());
    let PyRep::List(qq_list) = rpc(&ctx, &mut sess, "search", "QuickQuery", &[PyRep::String("jita".into())]).await.unwrap() else {
        panic!("Expected List")
    };
    assert!(qq_list.contains(&PyRep::Integer(30_000_142)));
}

#[tokio::test]
// parity: rpc:abyssalMgr.AbyssalEndGateActivation
// parity: rpc:abyssalMgr.AbyssalEntranceGateActivation
// parity: rpc:abyssalMgr.AbyssalGateActivation
// parity: rpc:abyssalMgr.ClientIsReady
async fn test_rpc_solid_abyssal_mgr_lifecycle() {
    eve_testkit::attest(&[
        "rpc:abyssalMgr.AbyssalEndGateActivation",
        "rpc:abyssalMgr.AbyssalEntranceGateActivation",
        "rpc:abyssalMgr.AbyssalGateActivation",
        "rpc:abyssalMgr.ClientIsReady",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 41_106, char_id).await;

    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
    let ship_id = sess.ship_id.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            999_477_651,
            eve_db::InventoryItemRecord {
                item_id: 999_477_651,
                type_id: 47_765,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ab_in = rpc(
        &ctx,
        &mut sess,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(47_765)],
    )
    .await
    .unwrap();
    let r1_npcs: Vec<i64> = match ab_in.dict_get("npcBallIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
        _ => vec![],
    };
    assert_eq!(
        rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.unwrap().dict_get("ready"),
        Some(&PyRep::Bool(true))
    );
    let ab_sys = ctx.systems.get_or_spawn(32_000_001);
    for id in r1_npcs {
        let _ = ab_sys.remove_ball(id).await;
    }
    let ab_g1 = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1)]).await.unwrap();
    let r2_npcs: Vec<i64> = match ab_g1.dict_get("npcBallIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
        _ => vec![],
    };
    for id in r2_npcs {
        let _ = ab_sys.remove_ball(id).await;
    }
    assert_eq!(
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(2)])
            .await
            .unwrap()
            .dict_get("exitedAbyssalSpace"),
        Some(&PyRep::Bool(true))
    );
}
