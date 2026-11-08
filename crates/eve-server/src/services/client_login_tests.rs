//! Unit tests for `client_login` (`alert`, `config`, `charUnboundMgr`, `populationCap`, `photoUploadSvc`, `skillMgr2`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{IskCents, MarketOrderRecord};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn alert_telemetry_bean_delivery_and_log_modes() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let blob = settings_info_blob();
    assert!(matches!(blob, PyRep::Tuple(_)));

    // BeanCount before trace is registered returns (None, None)
    let bc0 = handle(&ctx, &mut sess, "alert", "BeanCount", &[PyRep::Integer(12345)], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bc0, PyRep::Tuple(vec![PyRep::None, PyRep::None]));

    // Register stack trace via SendClientStackTraceAlert
    let st_res = handle(
        &ctx,
        &mut sess,
        "alert",
        "SendClientStackTraceAlert",
        &[
            PyRep::Tuple(vec![PyRep::Integer(12345), PyRep::String("KeyError".into())]),
            PyRep::String("Traceback (most recent call last):\n  File 'test.py'".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(st_parts) = st_res else {
        panic!("expected tuple");
    };
    let err_id = st_parts[0].as_i64().unwrap();
    assert!(err_id >= 1);

    // BeanCount now returns (err_id, b"B")
    let bc1 = handle(&ctx, &mut sess, "alert", "BeanCount", &[PyRep::Integer(12345)], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bc1,
        PyRep::Tuple(vec![PyRep::Integer(err_id), PyRep::ascii_bytes("B")])
    );

    // BeanDelivery: negative count rejected, positive counts accumulated
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "alert",
            "BeanDelivery",
            &[PyRep::Dict(vec![(
                PyRep::Integer(err_id),
                PyRep::Tuple(vec![PyRep::Integer(-1), PyRep::Integer(0)]),
            )])],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArguments"
    );

    handle(
        &ctx,
        &mut sess,
        "alert",
        "BeanDelivery",
        &[PyRep::Dict(vec![(
            PyRep::Integer(err_id),
            PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(100)]),
        )])],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(delivered_bean_count(&ctx, err_id), 7);

    // GroupBeanDelivery adds to count
    handle(
        &ctx,
        &mut sess,
        "alert",
        "GroupBeanDelivery",
        &[PyRep::Dict(vec![(PyRep::Integer(err_id), PyRep::Integer(3))])],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(delivered_bean_count(&ctx, err_id), 10);

    // LogModeChanged requires GM/Admin/QA role
    assert!(handle(
        &ctx,
        &mut sess,
        "alert",
        "LogModeChanged",
        &[PyRep::Integer(err_id), PyRep::String("I".into())],
        &kw,
        None,
    )
    .await
    .unwrap()
    .is_err());

    sess.role = eve_machonet::service_roles::ROLE_GMH;
    handle(
        &ctx,
        &mut sess,
        "alert",
        "LogModeChanged",
        &[PyRep::Integer(err_id), PyRep::String("I".into())],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();

    let mode_rep = handle(
        &ctx,
        &mut sess,
        "alert",
        "GetLogModeForError",
        &[PyRep::Integer(err_id)],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        mode_rep,
        PyRep::Tuple(vec![PyRep::Integer(err_id), PyRep::ascii_bytes("I")])
    );

    // SendProxyStackTraceAlert
    let proxy_res = handle(
        &ctx,
        &mut sess,
        "alert",
        "SendProxyStackTraceAlert",
        &[PyRep::Integer(54321), PyRep::String("Proxy trace".into())],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(proxy_res, PyRep::Tuple(_)));
    let (traces_len, beans_len) = alert_registry_sizes(&ctx);
    assert!(traces_len >= 2 && beans_len >= 1);
}

#[tokio::test]
async fn login_config_cohorts_population_cap_portrait_and_skills() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    assert_eq!(
        handle(&ctx, &mut sess, "config", "GetBlackListedPlanets", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );
    assert!(matches!(
        handle(&ctx, &mut sess, "air_npe", "is_air_npe_enabled", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(_)
    ));
    assert!(
        handle(&ctx, &mut sess, "invbroker", "GetItemDescriptor", &[], &kw, None)
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        handle(&ctx, &mut sess, "config", "GetOldStationData", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // GetAverageMarketPrices averages live orders with vol_remaining > 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            991_001,
            MarketOrderRecord {
                order_id: 991_001,
                owner_id: cid,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let avg_prices = handle(&ctx, &mut sess, "config", "GetAverageMarketPrices", &[], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert!(avg_prices.as_dict().is_some_and(|d| !d.is_empty()));

    // GetCohortsForUser, GetCharOmegaDowngradeStatus, GetCharacterLockType
    let cohorts = handle(&ctx, &mut sess, "charUnboundMgr", "GetCohortsForUser", &[], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert!(cohorts.as_list().is_some_and(|l| !l.is_empty()));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "GetCharOmegaDowngradeStatus",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "GetCharacterLockType",
            &[PyRep::Integer(cid)],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime
            .character_locks
            .insert(cid, "transfer".into());
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "GetCharacterLockType",
            &[PyRep::Integer(cid)],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );

    // populationCap.GetCharacterLoadSlot & MoveCharacterToNewSystem
    let slot_rep = handle(
        &ctx,
        &mut sess,
        "populationCap",
        "GetCharacterLoadSlot",
        &[PyRep::Integer(cid)],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(slot_rep.as_dict().is_some());

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(cid),
                PyRep::Integer(30_000_144),
                PyRep::String("bad-slot-key".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SystemNotOffered"
    );

    // photoUploadSvc.Upload
    let photo_svc = format!("photoUpload{}", "Svc");
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            &photo_svc,
            "Upload",
            &[PyRep::Integer(cid), PyRep::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.cluster_and_runtime.portraits.get(&cid.to_string()).cloned()),
        Some("deadbeef".into())
    );

    // charMgr, map.GetStationInfo, skillMgr2, operationsManager
    assert_eq!(
        handle(&ctx, &mut sess, "charMgr", "GetPaperdollState", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
    let st_info = handle(
        &ctx,
        &mut sess,
        "map",
        "GetStationInfo",
        &[PyRep::Integer(60_003_760)],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        st_info.dict_get("stationID"),
        Some(&PyRep::Integer(60_003_760))
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "map",
            "GetStationInfo",
            &[PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StationNotFound"
    );

    let attrs = handle(&ctx, &mut sess, "skillMgr2", "GetAttributes", &[], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attrs.as_dict().map(|d| d.len()), Some(5));
    assert!(
        handle(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[], &kw, None)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test]
async fn client_login_state_backed_hollow_arm_remediations() {
    use crate::services::account_services;

    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // 1. alert.BeanCount / SendClientStackTraceAlert honor alert_log_modes override and persist traces/beans
    let st_res = handle(
        &ctx,
        &mut sess,
        "alert",
        "SendClientStackTraceAlert",
        &[
            PyRep::Tuple(vec![PyRep::Integer(555_111), PyRep::String("ValueError".into())]),
            PyRep::String("Traceback: test".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let eid = match &st_res {
        PyRep::Tuple(t) => t[0].as_i64().unwrap(),
        other => panic!("expected tuple from SendClientStackTraceAlert, got {other:?}"),
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime
            .alert_log_modes
            .insert(eid.to_string(), "db".into());
    });
    let bc = handle(&ctx, &mut sess, "alert", "BeanCount", &[PyRep::Integer(555_111)], &kw, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bc, PyRep::Tuple(vec![PyRep::Integer(eid), PyRep::ascii_bytes("db")]));
    assert!(ctx.db.with_snapshot(|s| s.cluster_and_runtime.alert_client_traces.is_some()));

    let _ = handle(
        &ctx,
        &mut sess,
        "alert",
        "BeanDelivery",
        &[PyRep::Dict(vec![(
            PyRep::Integer(eid),
            PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(1_700_000_000)]),
        )])],
        &kw,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.db.with_snapshot(|s| {
        s.cluster_and_runtime
            .alert_bean_counts
            .keys()
            .any(|k| k.contains(&eid.to_string()))
    }));

    // 2. config.GetBlackListedPlanets reads sorted, deduped blacklisted_planets
    assert_eq!(
        handle(&ctx, &mut sess, "config", "GetBlackListedPlanets", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.blacklisted_planets = Some(vec![40_009_080, 40_009_077, 40_009_080]);
    });
    assert_eq!(
        handle(&ctx, &mut sess, "config", "GetBlackListedPlanets", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(vec![PyRep::Integer(40_009_077), PyRep::Integer(40_009_080)])
    );

    // 3. userSvc.GetRedeemTokens delegates to account_services state
    ctx.db.with_mut_snapshot(|snap| {
        account_services::enqueue_redeem_token_in_snapshot(
            snap,
            1,
            34,
            100,
            "Tritanium Gift".into(),
            Some(60_003_760),
            None,
        );
    });
    let tokens = handle(&ctx, &mut sess, "userSvc", "GetRedeemTokens", &[], &kw, None)
        .await
        .unwrap()
        .unwrap();
    let token_rows = match tokens {
        PyRep::List(v) => v,
        other => panic!("expected List from GetRedeemTokens, got {other:?}"),
    };
    assert_eq!(token_rows.len(), 1);

    // 4. charUnboundMgr.GetCharOmegaDowngradeStatus reflects lapsed vs active Omega subscription
    assert_eq!(
        handle(&ctx, &mut sess, "charUnboundMgr", "GetCharOmegaDowngradeStatus", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    account_services::with_store(&ctx, |_snap, store| {
        store.subscription_expiry.insert(1, 1); // well in the past relative to ctx.now_unix()
    });
    assert_eq!(
        handle(&ctx, &mut sess, "charUnboundMgr", "GetCharOmegaDowngradeStatus", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // 5. map.GetSecurityModifiedSystems reflects Corruption Stage 5 / securityMgr state
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(30_000_142).or_default().insurgency_corruption_stage = 5;
    });
    let sec_mods = handle(&ctx, &mut sess, "map", "GetSecurityModifiedSystems", &[], &kw, None)
        .await
        .unwrap()
        .unwrap();
    let row_count = match sec_mods {
        PyRep::ObjectEx { ref list, .. } => list.len(),
        _ => 0,
    };
    assert!(row_count >= 1, "expected at least 1 modified system row");

    // 6. charMgr.GetPaperdollState reads resculpt_pending and paperdoll_state:{char_id}
    assert_eq!(
        handle(&ctx, &mut sess, "charMgr", "GetPaperdollState", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
    account_services::with_store(&ctx, |_snap, store| {
        store.resculpt_pending.insert(90_000_001);
    });
    assert_eq!(
        handle(&ctx, &mut sess, "charMgr", "GetPaperdollState", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime
            .paperdoll_states
            .insert(90_000_001, "4".into());
    });
    assert_eq!(
        handle(&ctx, &mut sess, "charMgr", "GetPaperdollState", &[], &kw, None)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(4)
    );
}
