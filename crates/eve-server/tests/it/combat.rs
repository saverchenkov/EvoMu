//! Combat, fighters, and weapon systems integration tests.

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s2_fighter_mgr_unimplemented_rpc() {
    // parity: plan:S-2, rpc:charUnboundMgr.SelectCharacterID, rpc:fighterMgr.CmdActivateAbilitySlots
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(902);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ball_before) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001");
    assert_eq!(ball_before.id, 100_000_001);

    let s2_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(100_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        s2_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "FighterNotFound"
    ));

    let (_, ball_after) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001 must remain intact in ballpark");
    assert_eq!(ball_after.id, 100_000_001);
}

#[tokio::test]
async fn test_ship_operations_logoff_scoop_drop_and_dirt() {
    // parity: rpc:ship.ResetDirtTimestamp, rpc:ship.SafeLogoff, rpc:ship.AbortSafeLogoff, rpc:ship.Drop, rpc:ship.Scoop, rpc:ship.ScoopToInfrastructureHold, rpc:ship.ScoopToMobileDepotHold
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(903);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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

    // 1. ResetDirtTimestamp and GetDirtTimestamp
    let dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ResetDirtTimestamp",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("ResetDirtTimestamp");
    let dirt_ft = dirt.as_i64().expect("filetime");
    assert!(dirt_ft > 116_444_736_000_000_000);

    // Verify dynamic attribute 1969 was mutated in snapshot
    let dirt_dyn = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_001).and_then(|it| it.dynamic_attributes.get(&1969).copied())
    });
    assert_eq!(dirt_dyn, Some(dirt_ft as f64));

    // Verify GetDirtTimestamp reads the persisted timestamp
    let get_dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "GetDirtTimestamp",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetDirtTimestamp");
    assert_eq!(get_dirt.as_i64(), Some(dirt_ft));

    // 2. SetSessionCheck & clear_memoized
    let sc = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect("SetSessionCheck");
    assert_eq!(sc, PyRep::Bool(true));

    let cm = ctx
        .dispatch_rpc(&mut sess, "ship", None, "clear_memoized", &[], &kw, None)
        .await
        .expect("clear_memoized");
    assert_eq!(cm, PyRep::Bool(true));

    // 3. SafeLogoff & AbortSafeLogoff
    let sl = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None)
        .await
        .expect("SafeLogoff");
    let PyRep::KeyVal(sl_kv) = sl else { panic!("expected KeyVal") };
    assert_eq!(sl_kv.get("safeLogoffActive"), Some(&PyRep::Bool(true)));
    assert_eq!(sl_kv.get("logoffTime"), Some(&PyRep::Integer(30)));
    assert_eq!(sl_kv.get("shipID"), Some(&PyRep::Integer(100_000_001)));

    let asl = ctx
        .dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &kw, None)
        .await
        .expect("AbortSafeLogoff");
    assert_eq!(asl, PyRep::Bool(true));

    // Undock to space for Drop and Scoop tests
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    // Seed item into ship cargo for Drop
    let cargo_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    // 4. Drop (jettison from cargo to space)
    let drop_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::Integer(cargo_item_id)], &kw, None)
        .await
        .expect("Drop");
    let PyRep::KeyVal(drop_kv) = drop_res else { panic!("expected KeyVal") };
    let jetcan_id = drop_kv.get("jetcanID").and_then(|v| v.as_i64()).expect("jetcanID");
    assert!(jetcan_id > 0);

    // 5. Scoop (scoop back to cargo)
    let scoop_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Scoop",
            &[PyRep::Integer(cargo_item_id), PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("Scoop");
    let PyRep::KeyVal(sc_kv) = scoop_res else { panic!("expected KeyVal") };
    assert_eq!(sc_kv.get("flagID"), Some(&PyRep::Integer(5)));

    // 6. ScoopToInfrastructureHold
    let infra_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToInfrastructureHold",
            &[PyRep::Integer(cargo_item_id), PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("ScoopToInfrastructureHold");
    let PyRep::KeyVal(in_kv) = infra_res else { panic!("expected KeyVal") };
    assert_eq!(in_kv.get("flagID"), Some(&PyRep::Integer(185)));

    // 7. ScoopToMobileDepotHold
    let depot_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToMobileDepotHold",
            &[PyRep::Integer(cargo_item_id), PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("ScoopToMobileDepotHold");
    let PyRep::KeyVal(dp_kv) = depot_res else { panic!("expected KeyVal") };
    assert_eq!(dp_kv.get("flagID"), Some(&PyRep::Integer(179)));
}
