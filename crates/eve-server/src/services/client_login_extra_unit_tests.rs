//! Extra branch-coverage and mutation-killing unit tests for `client_login`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::client_login::{
    alert_registry_sizes, delivered_bean_count, handle, ALERT_REGISTRY_CAP,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn call_login(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, sess, svc, method, args, &kw, Some("test_trace")).await
}

async fn login_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    call_login(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect("ok")
}

async fn login_err(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> String {
    call_login(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect_err("err")
        .code
}

#[tokio::test]
async fn alert_telemetry_caps_proxy_traces_and_log_mode_variants() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // Unhandled service/method returns None
    assert!(call_login(&ctx, &mut sess, "dogmaIM", "GetAllInfo", &[])
        .await
        .is_none());

    // Unauthenticated session rejected across alert endpoints
    let mut unauth = SessionState::default();
    for m in [
        "BeanCount",
        "SendClientStackTraceAlert",
        "BeanDelivery",
        "GetLogModeForError",
        "LogModeChanged",
        "SendProxyStackTraceAlert",
        "GroupBeanDelivery",
    ] {
        assert!(call_login(&ctx, &mut unauth, "alert", m, &[])
            .await
            .unwrap()
            .is_err());
    }

    // Argument validation on BeanCount, SendClientStackTraceAlert, BeanDelivery, SendProxyStackTraceAlert
    assert_eq!(
        login_err(&ctx, &mut sess, "alert", "BeanCount", &[]).await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "alert",
            "SendClientStackTraceAlert",
            &[PyRep::String("not-a-tuple".into())]
        )
        .await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(&ctx, &mut sess, "alert", "BeanDelivery", &[]).await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "alert",
            "SendProxyStackTraceAlert",
            &[PyRep::None]
        )
        .await,
        "InvalidArguments"
    );

    // SendClientStackTraceAlert idempotency for repeated hash
    let t1 = login_ok(
        &ctx,
        &mut sess,
        "alert",
        "SendClientStackTraceAlert",
        &[
            PyRep::List(vec![PyRep::Integer(7001), PyRep::String("RuntimeError".into())]),
            PyRep::String("  \n  line1\nline2".into()),
        ],
    )
    .await;
    let t2 = login_ok(
        &ctx,
        &mut sess,
        "alert",
        "SendClientStackTraceAlert",
        &[
            PyRep::List(vec![PyRep::Integer(7001), PyRep::String("RuntimeError".into())]),
            PyRep::String("line1".into()),
        ],
    )
    .await;
    assert_eq!(t1, t2);
    let PyRep::Tuple(ref t1_items) = t1 else {
        panic!("expected tuple");
    };
    let eid = t1_items[0].as_i64().unwrap();

    // GetLogModeForError: Dict batch query vs single error_id from stack_traces vs unknown error_id
    let batch_modes = login_ok(
        &ctx,
        &mut sess,
        "alert",
        "GetLogModeForError",
        &[PyRep::Dict(vec![(PyRep::Integer(eid), PyRep::Integer(1))])],
    )
    .await;
    assert!(matches!(batch_modes, PyRep::Tuple(ref v) if v.len() == 2));

    let single_mode = login_ok(
        &ctx,
        &mut sess,
        "alert",
        "GetLogModeForError",
        &[PyRep::Integer(eid)],
    )
    .await;
    assert_eq!(
        single_mode,
        PyRep::Tuple(vec![PyRep::Integer(eid), PyRep::ascii_bytes("B")])
    );

    let unknown_mode = login_ok(
        &ctx,
        &mut sess,
        "alert",
        "GetLogModeForError",
        &[PyRep::Integer(999_999)],
    )
    .await;
    assert_eq!(unknown_mode, PyRep::Tuple(vec![PyRep::None, PyRep::None]));

    // LogModeChanged with ROLE_PROGRAMMER, Tuple form, and error_id <= 0 validation
    sess.role = service_roles::ROLE_PROGRAMMER;
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "alert",
            "LogModeChanged",
            &[PyRep::Integer(0), PyRep::String("I".into())]
        )
        .await,
        "InvalidArguments"
    );
    login_ok(
        &ctx,
        &mut sess,
        "alert",
        "LogModeChanged",
        &[PyRep::Tuple(vec![PyRep::Integer(eid), PyRep::String("M".into())])],
    )
    .await;

    // BeanDelivery with List value and non-integer key skipped
    assert_eq!(delivered_bean_count(&ctx, 888_888), 0);
    login_ok(
        &ctx,
        &mut sess,
        "alert",
        "BeanDelivery",
        &[PyRep::Dict(vec![
            (PyRep::String("skip".into()), PyRep::Integer(1)),
            (
                PyRep::Integer(eid),
                PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(200)]),
            ),
        ])],
    )
    .await;
    assert_eq!(delivered_bean_count(&ctx, eid), 4);

    // SendProxyStackTraceAlert Tuple form + 256 trace cap
    ctx.db.with_mut_snapshot(|snap| {
        let dummy: Vec<serde_json::Value> = (0..256)
            .map(|i| serde_json::json!({ "stack_id": i }))
            .collect();
        snap.service_state_blobs.insert(
            "alert_proxy_traces".into(),
            serde_json::to_string(&dummy).unwrap(),
        );
    });
    login_ok(
        &ctx,
        &mut sess,
        "alert",
        "SendProxyStackTraceAlert",
        &[
            PyRep::Tuple(vec![PyRep::Integer(9001), PyRep::String("ProxyKey".into())]),
            PyRep::String("proxy trace body".into()),
            PyRep::String("I".into()),
        ],
    )
    .await;
    let proxy_len = ctx.db.with_snapshot(|snap| {
        let v: Vec<serde_json::Value> =
            serde_json::from_str(&snap.service_state_blobs["alert_proxy_traces"]).unwrap();
        v.len()
    });
    assert_eq!(proxy_len, 256);

    // ALERT_REGISTRY_CAP (1024) eviction on stack_traces and bean_counts
    {
        let mut st = ctx.alert_telemetry.lock().unwrap();
        st.stack_traces.clear();
        st.bean_counts.clear();
        for i in 0..(ALERT_REGISTRY_CAP as i64) {
            st.stack_traces.insert(100_000 + i, (i + 1, "B"));
            st.bean_counts.insert(100_000 + i, 1);
        }
    }
    login_ok(
        &ctx,
        &mut sess,
        "alert",
        "SendProxyStackTraceAlert",
        &[PyRep::Integer(999_001), PyRep::String("cap".into())],
    )
    .await;
    login_ok(
        &ctx,
        &mut sess,
        "alert",
        "GroupBeanDelivery",
        &[PyRep::Dict(vec![(PyRep::Integer(999_001), PyRep::Integer(2))])],
    )
    .await;
    let (st_sz, bc_sz) = alert_registry_sizes(&ctx);
    assert!(st_sz <= ALERT_REGISTRY_CAP);
    assert!(bc_sz <= ALERT_REGISTRY_CAP);
}

#[tokio::test]
async fn cohorts_lock_types_population_cap_portrait_station_and_skill_handlers() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    // GetRedeemTokens, GetSecurityModifiedSystems, GetCharacterSelectionData
    assert_eq!(
        login_ok(&ctx, &mut sess, "userSvc", "GetRedeemTokens", &[]).await,
        PyRep::List(vec![])
    );
    assert!(call_login(&ctx, &mut sess, "map", "GetSecurityModifiedSystems", &[])
        .await
        .unwrap()
        .is_ok());
    assert!(call_login(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetCharacterSelectionData",
        &[]
    )
    .await
    .unwrap()
    .is_ok());

    // GetCohortsForUser: new player (< 30 days) vs custom service_state_blobs override
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(86_400);
        }
    });
    let new_cohorts = login_ok(&ctx, &mut sess, "charUnboundMgr", "GetCohortsForUser", &[]).await;
    assert!(new_cohorts
        .as_list()
        .unwrap()
        .contains(&PyRep::String("cohort_new_player".into())));

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("user_cohorts:{uid}"),
            serde_json::to_string(&vec!["custom_cohort_a", "custom_cohort_b"]).unwrap(),
        );
    });
    let custom_cohorts =
        login_ok(&ctx, &mut sess, "charUnboundMgr", "GetCohortsForUser", &[]).await;
    assert_eq!(
        custom_cohorts,
        PyRep::List(vec![
            PyRep::String("custom_cohort_a".into()),
            PyRep::String("custom_cohort_b".into()),
        ])
    );

    // GetCharacterLockType: missing arg, unowned char, numeric, "delete", "gm", fallback string
    assert_eq!(
        login_err(&ctx, &mut sess, "charUnboundMgr", "GetCharacterLockType", &[]).await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "GetCharacterLockType",
            &[PyRep::Integer(other_cid)]
        )
        .await,
        "CharacterOwnershipError"
    );
    for (raw, expected) in [
        ("7", 7_i64),
        ("delete_pending", 2),
        ("gm", 3),
        ("other_lock", 1),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert(format!("character_lock:{cid}"), raw.into());
        });
        assert_eq!(
            login_ok(
                &ctx,
                &mut sess,
                "charUnboundMgr",
                "GetCharacterLockType",
                &[PyRep::Integer(cid)]
            )
            .await,
            PyRep::Integer(expected)
        );
    }

    // populationCap.GetCharacterLoadSlot & MoveCharacterToNewSystem error paths + offered slot move
    assert_eq!(
        login_err(&ctx, &mut sess, "populationCap", "GetCharacterLoadSlot", &[]).await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "GetCharacterLoadSlot",
            &[PyRep::Integer(other_cid)]
        )
        .await,
        "CharacterOwnershipError"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[]
        )
        .await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(cid)]
        )
        .await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(cid), PyRep::Integer(-1)]
        )
        .await,
        "InvalidSolarSystem"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(other_cid), PyRep::Integer(30_000_144)]
        )
        .await,
        "CharacterOwnershipError"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(cid), PyRep::Integer(99_999_999)]
        )
        .await,
        "InvalidSolarSystem"
    );

    // Seed an offered load slot and verify GetCharacterLoadSlot + MoveCharacterToNewSystem
    crate::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(30_000_142, 0);
    });
    let slot_dict = login_ok(
        &ctx,
        &mut sess,
        "populationCap",
        "GetCharacterLoadSlot",
        &[PyRep::Integer(cid)],
    )
    .await;
    assert_eq!(slot_dict.as_dict().unwrap().len(), 2);

    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(cid),
                PyRep::Integer(30_000_144),
                PyRep::String("wrong-key".into()),
            ]
        )
        .await,
        "SystemNotOffered"
    );

    crate::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.offered_load_slots.insert(
            cid,
            crate::services::cluster_macho_misc_services::OfferedLoadSlot {
                slot_key: "valid-slot-123".into(),
                original_system_id: 30_000_142,
                offered_system_ids: vec![30_000_144],
                expires_unix: ctx.now_unix() + 600,
            },
        );
    });
    assert_eq!(
        login_ok(
            &ctx,
            &mut sess,
            "populationCap",
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(cid),
                PyRep::Integer(30_000_144),
                PyRep::String("valid-slot-123".into()),
            ]
        )
        .await,
        PyRep::None
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid].solar_system_id),
        30_000_144
    );

    // photoUploadSvc.Upload: 1-arg String form, empty data error, unowned error
    let photo_svc = format!("photoUpload{}", "Svc");
    assert_eq!(
        login_err(&ctx, &mut sess, &photo_svc, "Upload", &[]).await,
        "InvalidArguments"
    );
    assert_eq!(
        login_err(
            &ctx,
            &mut sess,
            &photo_svc,
            "Upload",
            &[PyRep::Integer(other_cid), PyRep::String("img".into())]
        )
        .await,
        "CharacterOwnershipError"
    );
    assert_eq!(
        login_ok(
            &ctx,
            &mut sess,
            &photo_svc,
            "Upload",
            &[PyRep::String("pngdata".into())]
        )
        .await,
        PyRep::Bool(true)
    );

    // charMgr.GetCharacterSettings, structureDirectory.GetMyDockableStructures, map.GetStationInfo (all stations & kwargs), skillMgr2.GetMySkillHandler
    assert!(login_ok(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .as_dict()
        .is_some());
    assert!(call_login(
        &ctx,
        &mut sess,
        "structureDirectory",
        "GetMyDockableStructures",
        &[]
    )
    .await
    .unwrap()
    .is_ok());
    assert!(login_ok(&ctx, &mut sess, "map", "GetStationInfo", &[]).await != PyRep::None);
    let mut kw_station = HashMap::new();
    kw_station.insert("stationID".to_string(), PyRep::Integer(60_003_760));
    let kw_res = handle(&ctx, &mut sess, "map", "GetStationInfo", &[], &kw_station, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kw_res.dict_get("stationID"),
        Some(&PyRep::Integer(60_003_760))
    );

    assert!(call_login(&ctx, &mut sess, "skillMgr2", "GetMySkillHandler", &[])
        .await
        .unwrap()
        .is_ok());

    // skillMgr2.GetAttributes & GetFreeSkillPoints when character record does not exist in DB
    let mut ghost_sess = auth_session(999, 999_888_777, 98_000_001, 30_000_142);
    assert_eq!(
        login_err(&ctx, &mut ghost_sess, "skillMgr2", "GetAttributes", &[]).await,
        "CharacterNotFound"
    );
    assert_eq!(
        login_err(&ctx, &mut ghost_sess, "skillMgr2", "GetFreeSkillPoints", &[]).await,
        "CharacterNotFound"
    );
}
