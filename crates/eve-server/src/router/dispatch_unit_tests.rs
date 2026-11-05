//! Unit and mutation-killing tests for `router::dispatch`
//! (WAL error hooks, PyRep summary formatting, session revocation, feature flags,
//! GM/QA role gates, `timeControl`, `MachoResolveObject`, `MachoBindObject`, and mining site seeding).

use super::*;
use crate::router::ServerContext;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn wal_error_hooks_and_format_pyrep_summary_variants() {
    ensure_wal_error_hook_installed();
    assert!(!record_dispatch_wal_error("outside_scope"));
    assert!(current_dispatch_wal_error().is_none());
    assert!(take_dispatch_wal_error().is_none());

    DISPATCH_WAL_ERROR
        .scope(std::cell::RefCell::new(None), async {
            assert!(record_dispatch_wal_error("first_err"));
            assert!(record_dispatch_wal_error("second_err_ignored"));
            assert_eq!(current_dispatch_wal_error().as_deref(), Some("first_err"));
            assert_eq!(take_dispatch_wal_error().as_deref(), Some("first_err"));
            assert!(current_dispatch_wal_error().is_none());
        })
        .await;

    assert_eq!(format_pyrep_summary(&PyRep::None), "None");
    assert_eq!(format_pyrep_summary(&PyRep::Bool(true)), "Bool(true)");
    assert_eq!(format_pyrep_summary(&PyRep::Integer(42)), "Int(42)");
    assert_eq!(format_pyrep_summary(&PyRep::Real(3.14159)), "Real(3.14)");
    assert_eq!(
        format_pyrep_summary(&PyRep::String("hello".into())),
        "Str[5]"
    );
    assert_eq!(
        format_pyrep_summary(&PyRep::List(vec![PyRep::None, PyRep::None])),
        "List[2]"
    );
    assert_eq!(
        format_pyrep_summary(&PyRep::Tuple(vec![PyRep::Integer(1)])),
        "Tuple[1]"
    );
    assert_eq!(format_pyrep_summary(&PyRep::Dict(Vec::new())), "Dict[0]");
    assert_eq!(
        format_pyrep_summary(&PyRep::Object {
            type_name: "util.KeyVal".into(),
            args: Box::new(PyRep::None),
        }),
        "Obj(util.KeyVal)"
    );
    assert_eq!(format_pyrep_summary(&PyRep::Token("tok".into())), "PyRep");
}

#[tokio::test]
async fn dispatch_rpc_feature_flags_role_gates_and_time_control() {
    let mut ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.feature_flags.enable_living_world = false;
    ctx.feature_flags.enable_quasar_gateway = false;

    let mut sess = SessionState::new(94_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // 1. FeatureDisabled gates for taleMgr and publicGatewaySvc
    let tale_err = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tale_err.code, "FeatureDisabled");

    let q_err = ctx
        .dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(q_err.code, "FeatureDisabled");

    ctx.feature_flags.enable_living_world = true;
    ctx.feature_flags.enable_quasar_gateway = true;

    // 2. GM & QA role gates
    sess.role = 0;
    let gm_err = ctx
        .dispatch_rpc(&mut sess, "slash", None, "SlashCmd", &[PyRep::String("/healself".into())], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(gm_err.code, "InsufficientRole");

    let qa_err = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(qa_err.code, "InsufficientRole");

    let tc_err = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(5)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tc_err.code, "InsufficientRole");

    // Grant GM role and test AdvanceClock (negative vs valid) and ClearSessionTimer
    sess.role = service_roles::ROLE_GMH;
    let neg_err = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(-5)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(neg_err.code, "InvalidArgument");

    let before_unix = ctx.now_unix();
    let adv_ok = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(15)], &kw, None)
        .await
        .expect("AdvanceClock");
    assert_eq!(adv_ok, PyRep::Bool(true));
    assert!(ctx.now_unix() >= before_unix + 15);

    let clr_ok = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "ClearSessionTimer", &[], &kw, None)
        .await
        .expect("ClearSessionTimer");
    assert_eq!(clr_ok, PyRep::Bool(true));
}

#[tokio::test]
async fn macho_resolve_bind_object_and_session_revocation() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(94_010);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // 1. MachoResolveObject
    let node_rep = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .expect("MachoResolveObject");
    assert_eq!(node_rep, PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));

    // 2. MachoBindObject with integer, string, tuple, and piggybacked sub-call
    let bind_int = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject int");
    assert!(matches!(bind_int, PyRep::Tuple(_)));

    let bind_str = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::String("solarsystem".into()), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject str");
    assert!(matches!(bind_str, PyRep::Tuple(_)));

    let bind_tuple_piggy = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetSkills".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("MachoBindObject with piggybacked GetSkills");
    if let PyRep::Tuple(parts) = bind_tuple_piggy {
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[1], PyRep::Dict(_)));
    } else {
        panic!("expected 2-tuple");
    }

    // Invalid MachoBindObject argument
    let bind_err = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoBindObject", &[PyRep::None], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(bind_err.code, "InvalidArgument");

    // 3. Session displacement when another session holds the online lock ("MultiLoginBlocked")
    ctx.db
        .acquire_online_lock(1, 99_999, Some(90_000_001), true, ctx.now_unix())
        .expect("other session lock");
    let displaced = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(displaced.code, "MultiLoginBlocked");

    // 4. Account ban / session revocation clears session state
    ctx.db.with_mut_snapshot(|snap| {
        for acc in snap.accounts.values_mut().filter(|a| a.user_id == 1) {
            acc.banned_until_unix = Some(u64::MAX);
        }
    });
    let banned = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(banned.code, "UserBanned");
    assert_eq!(sess.user_id, None);
    assert_eq!(sess.character_id, None);
}

#[tokio::test]
async fn seed_system_mining_sites_and_sov_prospecting_respawn() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142;

    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    // Second call hits the already_seeded fast path
    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;

    let has_anoms_or_sigs = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.contains_key(&sys_id) || snap.cosmic_signatures.contains_key(&sys_id)
    });
    assert!(has_anoms_or_sigs);
}
