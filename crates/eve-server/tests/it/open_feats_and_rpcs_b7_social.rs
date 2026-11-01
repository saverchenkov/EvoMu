//! Integration tests for open `rpc:` parity rows in social, notification, alliance, corp, and war domains (Batch 7).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const STATION_ID: i64 = 60_003_760;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.balance = eve_db::IskCents(500_000_000_00);
            ch.station_id = Some(STATION_ID);
        }
    });
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

#[tokio::test]
async fn test_notification_mgr_iterkeys_and_persistence() {
    // parity: rpc:notificationMgr.iterkeys
    let ctx = ServerContext::new();
    let mut unauth_sess = SessionState::new(999);
    assert!(
        rpc(&ctx, &mut unauth_sess, "notificationMgr", "iterkeys", &[])
            .await
            .is_err(),
        "unauthenticated notificationMgr.iterkeys must fail"
    );

    let mut sess2 = setup_pilot(&ctx, PILOT_2).await;

    let n1 = ctx.create_character_notification(PILOT_1, PILOT_2, 14, 9, "contact: added");
    let n2 = ctx.create_character_notification(PILOT_1, PILOT_2, 2, 5, "war: declared");

    let keys_rep = rpc(&ctx, &mut sess2, "notificationMgr", "iterkeys", &[])
        .await
        .expect("notificationMgr.iterkeys");
    let PyRep::List(keys) = keys_rep else {
        panic!("expected List from notificationMgr.iterkeys, got {keys_rep:?}");
    };
    let initial_ids: Vec<i64> = keys.iter().filter_map(|k| k.as_i64()).collect();
    assert!(initial_ids.contains(&n1.notification_id));
    assert!(initial_ids.contains(&n2.notification_id));
    let first_id = n1.notification_id;
    let second_id = n2.notification_id;

    // Delete the first notification and verify iterkeys reflects the mutation
    rpc(
        &ctx,
        &mut sess2,
        "notificationMgr",
        "DeleteNotifications",
        &[PyRep::List(vec![PyRep::Integer(first_id)])],
    )
    .await
    .expect("DeleteNotifications");

    let keys_after = rpc(&ctx, &mut sess2, "notificationMgr", "iterkeys", &[])
        .await
        .expect("notificationMgr.iterkeys after delete");
    let PyRep::List(keys_after_list) = keys_after else {
        panic!("expected List after delete");
    };
    let remaining_ids: Vec<i64> = keys_after_list.iter().filter_map(|k| k.as_i64()).collect();
    assert!(!remaining_ids.contains(&first_id));
    assert!(remaining_ids.contains(&second_id));

    // Reload from SQLite and verify iterkeys persists
    let ctx_reloaded =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_notif_iterkeys");
    let mut sess2_re = setup_pilot(&ctx_reloaded, PILOT_2).await;
    let keys_re = rpc(&ctx_reloaded, &mut sess2_re, "notificationMgr", "iterkeys", &[])
        .await
        .expect("notificationMgr.iterkeys after reload");
    let PyRep::List(keys_re_list) = keys_re else {
        panic!("expected List after reload");
    };
    let reloaded_ids: Vec<i64> = keys_re_list.iter().filter_map(|k| k.as_i64()).collect();
    assert!(!reloaded_ids.contains(&first_id));
    assert!(reloaded_ids.contains(&second_id));
}

#[tokio::test]
async fn test_alliance_registry_description_url_dictatorial_and_persistence() {
    // parity: rpc:allianceRegistry.description
    // parity: rpc:allianceRegistry.dictatorial
    // parity: rpc:allianceRegistry.url
    const EXPECTED_MISSING_ERR: &str = "AllianceNotFound";
    let ctx = ServerContext::new();
    let mut unauth_sess = SessionState::new(995);
    assert!(
        rpc(&ctx, &mut unauth_sess, "allianceRegistry", "description", &[])
            .await
            .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth_sess, "allianceRegistry", "dictatorial", &[])
            .await
            .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth_sess, "allianceRegistry", "url", &[])
            .await
            .is_err()
    );

    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    // Negative case: non-existent ID raises AllianceNotFound
    for err in [
        rpc(&ctx, &mut sess, "allianceRegistry", "description", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err(),
        rpc(&ctx, &mut sess, "allianceRegistry", "dictatorial", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err(),
        rpc(&ctx, &mut sess, "allianceRegistry", "url", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err(),
    ] {
        assert_eq!(err.code, EXPECTED_MISSING_ERR);
    }

    // Create a player corp and an alliance
    let corp_rep = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Batch7 Vanguard".into()),
            PyRep::String("B7VG".into()),
            PyRep::String("Initial corp desc".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .or_else(|| corp_rep.as_i64())
        .expect("corp_id");
    sess.corporation_id = Some(corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.master_wallet = eve_db::IskCents(2_000_000_000_00);
            if let Some(div) = corp.wallet_divisions.first_mut() {
                div.balance = eve_db::IskCents(2_000_000_000_00);
            }
        }
    });

    let ally_rep = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Batch7 Coalition".into()),
            PyRep::String("B7CL".into()),
            PyRep::String("Initial alliance desc".into()),
            PyRep::String("https://initial.example".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let ally_id = ally_rep
        .dict_get("allianceID")
        .and_then(|v| v.as_i64())
        .or_else(|| ally_rep.as_i64())
        .expect("ally_id");
    sess.alliance_id = Some(ally_id);

    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "dictatorial",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("initial dictatorial"),
        PyRep::Bool(false)
    );

    // Mutate alliance description, url, allowWar, and dictatorial=true via UpdateAlliance
    rpc(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "UpdateAlliance",
        &[
            PyRep::String("Updated Coalition Charter".into()),
            PyRep::String("https://coalition.example".into()),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("UpdateAlliance");

    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "description",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("allianceRegistry.description"),
        PyRep::String("Updated Coalition Charter".into())
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "url",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("allianceRegistry.url"),
        PyRep::String("https://coalition.example".into())
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "dictatorial",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("allianceRegistry.dictatorial"),
        PyRep::Bool(true)
    );

    // Reload from SQLite and verify alliance description, url, and dictatorial=true persist
    let ctx_reloaded =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_alliance_attrs");
    let mut sess_re = setup_pilot(&ctx_reloaded, PILOT_1).await;
    assert_eq!(
        rpc(
            &ctx_reloaded,
            &mut sess_re,
            "allianceRegistry",
            "description",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("description after reload"),
        PyRep::String("Updated Coalition Charter".into())
    );
    assert_eq!(
        rpc(
            &ctx_reloaded,
            &mut sess_re,
            "allianceRegistry",
            "url",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("url after reload"),
        PyRep::String("https://coalition.example".into())
    );
    assert_eq!(
        rpc(
            &ctx_reloaded,
            &mut sess_re,
            "allianceRegistry",
            "dictatorial",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("dictatorial after reload"),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_war_and_corp_registry_bind_and_state_accessors() {
    // parity: rpc:warRegistry.Bind
    // parity: rpc:warRegistry.Unbind
    // parity: rpc:warStatisticMgr.Bind
    // parity: rpc:corpRegistry.columns
    // parity: rpc:corpRegistry.header
    // parity: rpc:corpRegistry.itervalues
    // parity: rpc:corpmgr.deleted
    // parity: rpc:corpmgr.description
    let ctx = ServerContext::new();
    let mut unauth_sess = SessionState::new(998);
    for (svc, meth) in [
        ("warRegistry", "Bind"),
        ("warRegistry", "Unbind"),
        ("warStatisticMgr", "Bind"),
        ("corpRegistry", "columns"),
        ("corpRegistry", "header"),
        ("corpRegistry", "itervalues"),
        ("corpmgr", "deleted"),
        ("corpmgr", "description"),
    ] {
        assert!(
            rpc(&ctx, &mut unauth_sess, svc, meth, &[]).await.is_err(),
            "unauthenticated {svc}.{meth} must fail"
        );
    }

    let mut sess = setup_pilot(&ctx, PILOT_1).await;
    let mut sess_other = crate::open_feats_and_rpcs_b7::setup_pilot_for_user(
        &ctx,
        77_092,
        "other",
        "other-secret",
        PILOT_2,
    )
    .await;

    // Bind & Unbind warRegistry, verify wrong-session Unbind fails, and Bind warStatisticMgr
    let war_bind = rpc(&ctx, &mut sess, "warRegistry", "Bind", &[])
        .await
        .expect("warRegistry.Bind");
    let war_bound_id = war_bind.as_i64().expect("warRegistry bound_id");
    assert!(war_bound_id > 0);
    assert!(
        rpc(
            &ctx,
            &mut sess_other,
            "warRegistry",
            "Unbind",
            &[PyRep::Integer(war_bound_id)],
        )
        .await
        .is_err(),
        "another session must not unbind PILOT_1's warRegistry bound object"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "warRegistry",
            "Unbind",
            &[PyRep::Integer(war_bound_id)],
        )
        .await
        .expect("warRegistry.Unbind"),
        PyRep::None
    );
    assert!(!ctx
        .bound_objects
        .read()
        .unwrap()
        .contains_key(&(war_bound_id as u64)));

    let stat_target = sess.corporation_id.unwrap_or(1_000_001);
    let stat_bind = rpc(
        &ctx,
        &mut sess,
        "warStatisticMgr",
        "Bind",
        &[PyRep::Integer(stat_target)],
    )
    .await
    .expect("warStatisticMgr.Bind");
    let stat_bound_id = stat_bind.as_i64().expect("warStatisticMgr bound_id") as u64;
    assert!(stat_bound_id > 0);
    assert!(ctx.bound_objects.read().unwrap().contains_key(&stat_bound_id));

    // Create a player corp and verify corpRegistry + corpmgr state accessors
    let corp_rep = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Batch7 Logistics".into()),
            PyRep::String("B7LG".into()),
            PyRep::String("State-backed corp description".into()),
            PyRep::Real(0.10),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .or_else(|| corp_rep.as_i64())
        .expect("corp_id");

    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "columns", &[])
            .await
            .expect("corpRegistry.columns"),
        PyRep::List(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ])
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "header", &[])
            .await
            .expect("corpRegistry.header"),
        PyRep::Tuple(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ])
    );

    let itervals = rpc(&ctx, &mut sess, "corpRegistry", "itervalues", &[])
        .await
        .expect("corpRegistry.itervalues");
    let PyRep::List(corp_rows) = itervals else {
        panic!("expected List from corpRegistry.itervalues");
    };
    let expected_tuple = PyRep::Tuple(vec![
        PyRep::Integer(corp_id),
        PyRep::String("Batch7 Logistics".into()),
        PyRep::String("B7LG".into()),
    ]);
    assert!(
        corp_rows.contains(&expected_tuple),
        "corpRegistry.itervalues must include created corp"
    );

    assert_eq!(
        rpc(&ctx, &mut sess, "corpmgr", "deleted", &[PyRep::Integer(corp_id)])
            .await
            .expect("corpmgr.deleted active"),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "corpmgr",
            "deleted",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .expect("corpmgr.deleted missing"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "corpmgr",
            "description",
            &[PyRep::Integer(corp_id)],
        )
        .await
        .expect("corpmgr.description"),
        PyRep::String("State-backed corp description".into())
    );

    // Reload from SQLite and verify persistence of corpRegistry.itervalues and corpmgr.description
    let ctx_reloaded =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_corp_war_attrs");
    let mut sess_re = setup_pilot(&ctx_reloaded, PILOT_1).await;
    let itervals_re = rpc(&ctx_reloaded, &mut sess_re, "corpRegistry", "itervalues", &[])
        .await
        .expect("corpRegistry.itervalues after reload");
    let PyRep::List(re_rows) = itervals_re else {
        panic!("expected List after reload");
    };
    assert!(re_rows.contains(&expected_tuple));
    assert_eq!(
        rpc(
            &ctx_reloaded,
            &mut sess_re,
            "corpmgr",
            "description",
            &[PyRep::Integer(corp_id)],
        )
        .await
        .expect("corpmgr.description after reload"),
        PyRep::String("State-backed corp description".into())
    );
}
