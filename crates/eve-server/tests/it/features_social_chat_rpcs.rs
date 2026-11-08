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
async fn test_alliance_dictatorial_declare_executor_support_and_persistence() {
    const EXPECTED_MISSING_ERR: &str = "AllianceNotFound";
    let ctx = ServerContext::new();
    let mut unauth_sess = SessionState::new(995);
    assert!(
        rpc(&ctx, &mut unauth_sess, "allianceRegistry", "GetAlliance", &[])
            .await
            .is_err()
    );

    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    // Negative case: non-existent ID raises AllianceNotFound
    let err = rpc(&ctx, &mut sess, "allianceRegistry", "GetAlliance", &[PyRep::Integer(999_999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, EXPECTED_MISSING_ERR);

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
            corp.set_division_balance(1, eve_db::IskCents(2_000_000_000_00));
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

    let init_info = rpc(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(ally_id)],
    )
    .await
    .expect("initial GetAlliance");
    assert_eq!(init_info.dict_get("dictatorial"), Some(&PyRep::Bool(false)));

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

    // Add a second member corporation and verify DeclareExecutorSupport is blocked while dictatorial=true
    let mut sess_member = setup_pilot(&ctx, 90_000_002).await;
    let corp2_rep = rpc(
        &ctx,
        &mut sess_member,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Batch7 Wing".into()),
            PyRep::String("B7WG".into()),
            PyRep::String("Second member corp".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 2");
    let corp2_id = corp2_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .or_else(|| corp2_rep.as_i64())
        .expect("corp2_id");
    sess_member.corporation_id = Some(corp2_id);
    sess_member.alliance_id = Some(ally_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.corporations.get_mut(&corp2_id) {
            c2.alliance_id = Some(ally_id);
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.alliance_id = Some(ally_id);
        }
        if let Some(al) = snap.alliances.get_mut(&ally_id) {
            al.member_corp_ids.insert(corp2_id);
        }
    });

    let vote_err = rpc(
        &ctx,
        &mut sess_member,
        "allianceRegistry",
        "DeclareExecutorSupport",
        &[PyRep::Integer(corp2_id)],
    )
    .await
    .expect_err("DeclareExecutorSupport must fail in a dictatorial alliance");
    assert_eq!(vote_err.code, "AllianceIsDictatorial");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.alliances[&ally_id].executor_corp_id),
        corp_id,
        "executorCorpID must remain unchanged when alliance is dictatorial"
    );

    // Reload from SQLite and verify alliance description, url, and dictatorial=true persist via GetAlliance
    let ctx_reloaded =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_alliance_attrs");
    let mut sess_re = setup_pilot(&ctx_reloaded, PILOT_1).await;
    let info_re = rpc(
        &ctx_reloaded,
        &mut sess_re,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(ally_id)],
    )
    .await
    .expect("GetAlliance after reload");
    assert_eq!(
        info_re.dict_get("description"),
        Some(&PyRep::String("Updated Coalition Charter".into()))
    );
    assert_eq!(
        info_re.dict_get("url"),
        Some(&PyRep::String("https://coalition.example".into()))
    );
    assert_eq!(info_re.dict_get("dictatorial"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn test_war_registry_bind_and_unbind() {
    // parity: rpc:warRegistry.Bind
    // parity: rpc:warRegistry.Unbind
    let ctx = ServerContext::new();
    let mut unauth_sess = SessionState::new(998);
    for (svc, meth) in [("warRegistry", "Bind"), ("warRegistry", "Unbind")] {
        assert!(
            rpc(&ctx, &mut unauth_sess, svc, meth, &[]).await.is_err(),
            "unauthenticated {svc}.{meth} must fail"
        );
    }

    let mut sess = setup_pilot(&ctx, PILOT_1).await;
    let mut sess_other = crate::features_sovereignty_upwell_rpcs::setup_pilot_for_user(
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
}
