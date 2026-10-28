//! Regression tests for the RPCs the real V24.01 client calls between the handshake and the
//! character selection screen (T-6 `journey:launch_login`), plus the V24 `ErrorResponse` body.
//! Each one was an `UnimplementedRPC`, `CharacterNotSelected` or unmarshal error in a captured run.

use crate::common::*;
use eve_machonet::{wrapped_runtime_error_body, SessionState, MACHONETERR_WRAPPEDEXCEPTION};
use eve_proto::{marshal, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        _ => None,
    }
}

#[test]
fn error_response_body_wraps_a_pickled_runtime_error() {
    // machoNet: ErrorResponse(originalCommand, code, payload); ExceptionWrapperGPCS re-raises
    // LoadsSanitized(payload[0]) when code == MACHONETERR_WRAPPEDEXCEPTION.
    let body = wrapped_runtime_error_body(6, "UnimplementedRPC: config.X is not yet implemented");
    let PyRep::Tuple(items) = &body else { panic!("body must be a tuple") };
    assert_eq!(items[0], PyRep::Integer(6));
    assert_eq!(items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    assert_eq!(MACHONETERR_WRAPPEDEXCEPTION, 2);
    let PyRep::Tuple(payload) = &items[2] else { panic!("payload must be a tuple") };
    let PyRep::Bytes(pickled) = &payload[0] else { panic!("payload[0] must be a marshal string") };
    let exc = marshal::decode(pickled).expect("pickled exception decodes");
    let PyRep::ObjectEx { is_type_2, header, .. } = exc else { panic!("ObjectEx1 expected") };
    assert!(!is_type_2);
    let PyRep::Tuple(h) = *header else { panic!("header tuple") };
    assert_eq!(h[0].as_str(), Some("exceptions.RuntimeError"));
    assert_eq!(
        h[1],
        PyRep::Tuple(vec![PyRep::ascii_bytes("UnimplementedRPC: config.X is not yet implemented")])
    );
}

#[tokio::test]
async fn login_path_rpcs_answer_before_character_selection() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_101);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    assert!(sess.character_id.is_none(), "these calls happen before SelectCharacterID");
    let kw = HashMap::new();
    let call = |svc: &'static str, m: &'static str, a: Vec<PyRep>| (svc, m, a);

    // alert telemetry: unknown hash -> (None, None); after a trace is sent the hash maps to an errorID.
    let hash = 0x5EED_1234_i64;
    for (svc, m, a) in [call("alert", "BeanCount", vec![PyRep::Integer(hash)])] {
        let r = ctx.dispatch_rpc(&mut sess, svc, None, m, &a, &kw, None).await.unwrap();
        assert_eq!(r, PyRep::Tuple(vec![PyRep::None, PyRep::None]));
    }
    let stack_id = PyRep::Tuple(vec![PyRep::Integer(hash), PyRep::ascii_bytes("k")]);
    let sent = ctx
        .dispatch_rpc(&mut sess, "alert", None, "SendClientStackTraceAlert",
            &[stack_id, PyRep::ascii_bytes("EXCEPTION #1 ...\nline2"), PyRep::ascii_bytes("Error"), PyRep::None],
            &kw, None)
        .await
        .unwrap();
    let PyRep::Tuple(t) = &sent else { panic!("(errorID, logMode)") };
    let error_id = t[0].as_i64().expect("errorID assigned");
    assert_eq!(t[1].as_str(), Some("B"));
    let again = ctx
        .dispatch_rpc(&mut sess, "alert", None, "BeanCount", &[PyRep::Integer(hash)], &kw, None)
        .await
        .unwrap();
    assert_eq!(again, sent, "BeanCount now returns the registered (errorID, logMode)");
    let before = eve_server::services::client_login::delivered_bean_count(&ctx, error_id);
    let beans = PyRep::Dict(vec![(PyRep::Integer(error_id), PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(0)]))]);
    let d = ctx.dispatch_rpc(&mut sess, "alert", None, "BeanDelivery", &[beans], &kw, None).await.unwrap();
    assert_eq!(d, PyRep::None);
    assert_eq!(eve_server::services::client_login::delivered_bean_count(&ctx, error_id), before + 3);

    // Saturating addition prevents i64 overflow on repeated large counts, and registry sizes are
    // strictly capped at ALERT_REGISTRY_CAP (1024) (T6H-P1-1).
    let sat_id = 0x7000_0000_i64;
    for delta in [i64::MAX - 2, 10] {
        let sat_beans = PyRep::Dict(vec![(
            PyRep::Integer(sat_id),
            PyRep::Tuple(vec![PyRep::Integer(delta), PyRep::Integer(0)]),
        )]);
        ctx.dispatch_rpc(&mut sess, "alert", None, "BeanDelivery", &[sat_beans], &kw, None)
            .await
            .unwrap();
    }
    assert_eq!(eve_server::services::client_login::delivered_bean_count(&ctx, sat_id), i64::MAX);

    let cap = eve_server::services::client_login::ALERT_REGISTRY_CAP;
    for i in 0..(cap + 32) {
        let h = 0x7000_0000_i64 + i as i64;
        let cap_stack = PyRep::Tuple(vec![PyRep::Integer(h), PyRep::ascii_bytes("cap")]);
        ctx.dispatch_rpc(
            &mut sess,
            "alert",
            None,
            "SendClientStackTraceAlert",
            &[cap_stack, PyRep::ascii_bytes("trace"), PyRep::ascii_bytes("Error"), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap();
        let cap_beans = PyRep::Dict(vec![(
            PyRep::Integer(h),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)]),
        )]);
        ctx.dispatch_rpc(&mut sess, "alert", None, "BeanDelivery", &[cap_beans], &kw, None)
            .await
            .unwrap();
    }
    let (trace_len, bean_len) = eve_server::services::client_login::alert_registry_sizes(&ctx);
    assert_eq!(trace_len, cap);
    assert_eq!(bean_len, cap);

    let planets = ctx.dispatch_rpc(&mut sess, "config", None, "GetBlackListedPlanets", &[], &kw, None).await.unwrap();
    assert_eq!(planets, PyRep::List(Vec::new()));
    let old = ctx.dispatch_rpc(&mut sess, "config", None, "GetOldStationData", &[], &kw, None).await.unwrap();
    assert_eq!(old, PyRep::None);
    let npe = ctx.dispatch_rpc(&mut sess, "air_npe", None, "is_air_npe_enabled", &[], &kw, None).await.unwrap();
    assert_eq!(npe, PyRep::Bool(!ctx.sde.node_graphs.is_empty()));
    assert_eq!(npe, PyRep::Bool(true), "the real SDE ships AIR NPE node graphs");
    let desc = ctx.dispatch_rpc(&mut sess, "invbroker", None, "GetItemDescriptor", &[], &kw, None).await.unwrap();
    assert_eq!(desc, eve_proto::DBRowDescriptor::inv_item_header().to_pyrep());
}

#[tokio::test]
async fn character_selection_data_lists_the_accounts_characters() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_102);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let user_id = sess.user_id.expect("logged in");
    let expected = ctx.db.get_characters_for_user(user_id);
    assert!(!expected.is_empty(), "demo fixture owns a character");
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterSelectionData", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::Tuple(parts) = &rep else { panic!("4-tuple") };
    assert_eq!(parts.len(), 4);
    let PyRep::List(user_rows) = &parts[0] else { panic!("userDetails list") };
    assert_eq!(kv_get(&user_rows[0], "characterSlots"), Some(&PyRep::Integer(3)));
    assert_eq!(kv_get(&user_rows[0], "userName"), Some(&PyRep::String("demo".into())));
    assert_eq!(parts[1], PyRep::Tuple(vec![PyRep::None, PyRep::None]));
    let PyRep::List(chars) = &parts[2] else { panic!("characterDetails list") };
    assert_eq!(chars.len(), expected.len());
    let c0 = &expected[0];
    let row = chars
        .iter()
        .find(|r| kv_get(r, "characterID") == Some(&PyRep::Integer(c0.character_id)))
        .expect("row for the first character");
    assert_eq!(kv_get(row, "corporationID"), Some(&PyRep::Integer(c0.corporation_id)));
    assert_eq!(kv_get(row, "solarSystemID"), Some(&PyRep::Integer(i64::from(c0.solar_system_id))));
    assert_eq!(kv_get(row, "balance"), Some(&PyRep::Real(c0.balance.to_wire_f64())));
    assert_eq!(kv_get(row, "raceID"), Some(&PyRep::Integer(i64::from(c0.race_id))));
    assert!(matches!(parts[3], PyRep::List(_)));
}

#[test]
fn keyval_and_rowsets_use_v24_module_paths() {
    // The V24 client has no `util` module; blue imports these classes by module path.
    let enc = marshal::encode(&PyRep::keyval([("a", PyRep::Integer(1))]));
    let needle = b"utillib.KeyVal";
    assert!(enc.windows(needle.len()).any(|w| w == needle));
    assert!(!enc.windows(11).any(|w| w == b"util.KeyVal"));
    let back = marshal::decode(&enc).unwrap();
    assert_eq!(kv_get(&back, "a"), Some(&PyRep::Integer(1)));
    let d = eve_proto::DBRowDescriptor::inv_item_header().to_pyrep();
    let denc = marshal::encode(&d);
    let ss = b"eve.common.script.sys.eveCfg.StackSize";
    assert!(denc.windows(ss.len()).any(|w| w == ss));
}

#[tokio::test]
async fn alert_rpcs_enforce_auth_bounds_and_saturating_counts() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // Unauthenticated callers must be rejected on all three alert endpoints (T6H-P1-1).
    let mut unauth = SessionState::new(9_103);
    for (m, args) in [
        ("BeanCount", vec![PyRep::Integer(42)]),
        (
            "SendClientStackTraceAlert",
            vec![
                PyRep::Tuple(vec![PyRep::Integer(42), PyRep::ascii_bytes("k")]),
                PyRep::ascii_bytes("trace"),
                PyRep::ascii_bytes("Error"),
                PyRep::None,
            ],
        ),
        ("BeanDelivery", vec![PyRep::Dict(Vec::new())]),
    ] {
        let err = ctx
            .dispatch_rpc(&mut unauth, "alert", None, m, &args, &kw, None)
            .await
            .expect_err("unauthenticated alert call must fail");
        assert_eq!(err.code, "Unauthorized", "alert.{m} must require auth");
    }

    let mut sess = SessionState::new(9_104);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // Negative counts in BeanDelivery must be rejected with InvalidArguments.
    let neg_beans = PyRep::Dict(vec![(
        PyRep::Integer(777_001),
        PyRep::Tuple(vec![PyRep::Integer(-5), PyRep::Integer(0)]),
    )]);
    let err = ctx
        .dispatch_rpc(&mut sess, "alert", None, "BeanDelivery", &[neg_beans], &kw, None)
        .await
        .expect_err("negative bean count must fail");
    assert_eq!(err.code, "InvalidArguments");
    assert_eq!(eve_server::services::client_login::delivered_bean_count(&ctx, 777_001), 0);
}

#[tokio::test]
async fn character_selection_screen_and_entry_rpcs_work_end_to_end() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_105);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let user_id = sess.user_id.expect("logged in");
    let chars = ctx.db.get_characters_for_user(user_id);
    let char_id = chars[0].character_id;
    let solar_system_id = chars[0].solar_system_id;
    let kw = HashMap::new();

    // 1. config.GetAverageMarketPrices derives {typeID: KeyVal(typeID, averagePrice, adjustedPrice)}
    let prices = ctx
        .dispatch_rpc(&mut sess, "config", None, "GetAverageMarketPrices", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::Dict(price_entries) = &prices else {
        panic!("GetAverageMarketPrices must return a Dict")
    };
    assert!(!price_entries.is_empty(), "seeded market orders populate average prices");
    let (first_tid, first_kv) = &price_entries[0];
    let tid = first_tid.as_i64().expect("integer typeID key");
    assert_eq!(kv_get(first_kv, "typeID"), Some(&PyRep::Integer(tid)));
    let avg = kv_get(first_kv, "averagePrice").and_then(|v| v.as_f64()).expect("averagePrice");
    let adj = kv_get(first_kv, "adjustedPrice").and_then(|v| v.as_f64()).expect("adjustedPrice");
    assert!(avg > 0.0 && adj > 0.0);

    // 2. Character-selection screen RPCs before SelectCharacterID
    let camp = ctx
        .dispatch_rpc(&mut sess, "loginCampaignManager", None, "get_client_campaign_state", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(camp, PyRep::None);

    let seasonal = ctx
        .dispatch_rpc(&mut sess, "seasonalLoginCampaignManager", None, "get_active_campaign", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(seasonal, PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));

    let cohorts = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCohortsForUser", &[], &kw, None)
        .await
        .unwrap();
    assert!(matches!(cohorts, PyRep::List(ref l) if !l.is_empty()));

    let tokens = ctx
        .dispatch_rpc(&mut sess, "userSvc", None, "GetRedeemTokens", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(tokens, PyRep::List(Vec::new()));

    let downgrade = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharOmegaDowngradeStatus", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(downgrade, PyRep::Bool(false));

    let mod_sys = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSecurityModifiedSystems", &[], &kw, None)
        .await
        .unwrap();
    let expected_mod_sys = eve_proto::DBRowDescriptor::new([
        ("solarSystemID", eve_proto::DbColumnType::I4),
        ("security", eve_proto::DbColumnType::R8),
        ("modifiedSecurity", eve_proto::DbColumnType::R8),
    ])
    .build_crowset(Vec::new());
    assert_eq!(mod_sys, expected_mod_sys);

    // 3. Character entry: populationCap moniker resolve + bind + GetCharacterLoadSlot & GetCharacterLockType
    let node_id = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MachoResolveObject",
            &[PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(1373)])],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    assert_eq!(node_id, PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));

    let bind_rsp = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(1373)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetCharacterLoadSlot".into()),
                    PyRep::Tuple(vec![PyRep::Integer(char_id)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    let PyRep::Tuple(bind_parts) = &bind_rsp else { panic!("MachoBindObject 2-tuple") };
    assert!(
        matches!(bind_parts[0], PyRep::SubStruct(_)),
        "native machoNet MachoBindObject must return a SubStruct OID"
    );
    assert_eq!(
        bind_parts[1],
        PyRep::Dict(vec![(PyRep::Integer(i64::from(solar_system_id)), PyRep::None)])
    );

    // Unowned character ID is rejected on both populationCap and GetCharacterLockType.
    let unowned_err = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterLockType",
            &[PyRep::Integer(char_id + 999_999)],
            &kw,
            None,
        )
        .await
        .expect_err("unowned character must be rejected");
    assert_eq!(unowned_err.code, "CharacterOwnershipError");

    let lock_type = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterLockType",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(lock_type, PyRep::None);
}

#[tokio::test]
async fn provision_starter_characters_creates_character_for_configured_account() {
    let ctx = ServerContext::new();
    ctx.db.provision_player_account("starter_acct", "starter_pw");
    let mut cfg = eve_server::config::ServerConfig::default();
    cfg.player_accounts = vec![eve_server::config::PlayerAccountConfig {
        username: "starter_acct".into(),
        password_token: "starter_pw".into(),
        starter_character: Some(eve_server::config::StarterCharacterConfig {
            name: "Starter Pilot".into(),
            bloodline_id: 1,
            gender: 0,
            ancestry_id: 11,
            school_id: 17,
            ..Default::default()
        }),
    }];
    let created = eve_server::starter_characters::provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created.len(), 1);
    let acct = ctx.db.authenticate("starter_acct", "starter_pw").unwrap();
    let chars = ctx.db.get_characters_for_user(acct.user_id);
    assert_eq!(chars.len(), 1);
    assert_eq!(chars[0].character_id, created[0]);
    assert_eq!(chars[0].name, "Starter Pilot");

    // Second invocation is a no-op because the account now owns a character.
    let again = eve_server::starter_characters::provision_starter_characters(&ctx, &cfg).await;
    assert!(again.is_empty());
}

#[tokio::test]
async fn test_p1_track_c_regressions_issues_23_27_31_45() {
    let ctx1 = ServerContext::new();
    let ctx2 = ServerContext::new();
    let kw = HashMap::new();

    // 1. Issue #27: MAX_MACHO_FRAME_LEN matches MAX_FRAME_SIZE (4 MiB)
    assert_eq!(
        eve_server::macho_listener::MAX_MACHO_FRAME_LEN,
        eve_proto::marshal::MAX_FRAME_SIZE,
        "Issue #27: macho_listener MAX_MACHO_FRAME_LEN must be capped at MAX_FRAME_SIZE (4 MiB)"
    );

    // 2. Issue #23: corpRegistry.UnBind enforces session ownership
    let mut sess1 = SessionState::new(41_001);
    login_pilot(&ctx1, &mut sess1, "demo", "demo-secret").await;
    sess1.character_id = Some(90_000_001);
    sess1.corporation_id = Some(1_000_009);
    let bound_id = ctx1
        .bind_object(&sess1, "corpRegistry", 1_000_009)
        .expect("bind corpRegistry");

    let mut sess2 = SessionState::new(41_002);
    login_pilot(&ctx1, &mut sess2, "other", "other-secret").await;
    sess2.character_id = Some(90_000_002);
    sess2.corporation_id = Some(1_000_009);

    let cross_unbind = ctx1
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "UnBind",
            &[PyRep::Integer(bound_id as i64)],
            &kw,
            None,
        )
        .await;
    assert!(
        cross_unbind.is_err(),
        "Issue #23: Another session must not be able to UnBind sess1's bound object"
    );
    let self_unbind = ctx1
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UnBind",
            &[PyRep::Integer(bound_id as i64)],
            &kw,
            None,
        )
        .await;
    assert!(
        self_unbind.is_ok(),
        "Issue #23: Owning session must be able to UnBind its own bound object"
    );

    // 3. Issue #31: Global config and alert telemetry do not leak across ServerContext instances
    let mut gm1 = sess1.clone();
    gm1.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    ctx1.dispatch_rpc(
        &mut gm1,
        "machoNet",
        None,
        "SetGlobalConfigValue",
        &[PyRep::String("testKey31".into()), PyRep::String("val1".into())],
        &kw,
        None,
    )
    .await
    .expect("SetGlobalConfigValue on ctx1");
    assert!(
        !ctx2.global_config_values.read().unwrap().contains_key("testKey31"),
        "Issue #31: SetGlobalConfigValue on ctx1 must not pollute ctx2"
    );

    // 4. Issue #45: DeleteAllianceApplication requires Director/CEO, and GetBulletins rejects foreign corp IDs
    sess2.corp_role = 0;
    let del_app_err = ctx1
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "DeleteAllianceApplication",
            &[PyRep::Integer(99_000_001)],
            &kw,
            None,
        )
        .await;
    assert!(
        del_app_err.is_err(),
        "Issue #45: Unprivileged line member must not be able to call DeleteAllianceApplication"
    );

    let foreign_bulletins = ctx1
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "GetBulletins",
            &[PyRep::Integer(98_999_999)],
            &kw,
            None,
        )
        .await;
    assert!(
        foreign_bulletins.is_err(),
        "Issue #45: GetBulletins for a foreign corporation must be rejected"
    );
}
