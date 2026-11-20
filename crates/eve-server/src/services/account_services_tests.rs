//! Unit tests for `account_services` (playtime, subscriptions, redeem tokens, MCT, resculpt, kiringMgr).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord, OwnerNoteRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(crate) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(crate) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(crate) fn auth_session(uid: u64, cid: i64, corp_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::new(100);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s
}

pub(crate) trait PyRepTestExt {
    fn as_list(&self) -> Option<&[PyRep]>;
    fn as_dict(&self) -> Option<&[(PyRep, PyRep)]>;
}

impl PyRepTestExt for PyRep {
    fn as_list(&self) -> Option<&[PyRep]> {
        match self {
            PyRep::List(items) | PyRep::Tuple(items) | PyRep::PySet(items) => Some(items),
            _ => None,
        }
    }

    fn as_dict(&self) -> Option<&[(PyRep, PyRep)]> {
        match self {
            PyRep::Dict(entries) => Some(entries),
            PyRep::Object { args, .. } | PyRep::ObjectEx { header: args, .. } => args.as_dict(),
            _ => None,
        }
    }
}

#[test]
fn store_allocators_playtime_underage_and_subscription_sync() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut store = AccountServicesStore::default();
    assert_eq!(store.alloc_id(), 600_001);
    assert_eq!(store.alloc_id(), 600_002);
    assert_eq!(store.alloc_message_id(), 1);
    assert_eq!(store.alloc_message_id(), 2);

    let uid = 1_u64;
    on_login(&ctx, uid);
    set_user_country(&ctx, uid, "IS");
    with_store(&ctx, |_, st| {
        assert_eq!(st.user_countries.get(&uid).map(String::as_str), Some("IS"));
        st.login_start_unix.insert(uid, ctx.now_unix().saturating_sub(180));
    });

    let mut sess = auth_session(uid, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();
    let accrued = handle_inner(&ctx, &mut sess, "authentication", "AccruedTime", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(accrued.as_i64(), Some(180));

    let mins = handle_inner(&ctx, &mut sess, "userSvc", "GetSessionPlaytimeMinutes", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(mins.as_i64(), Some(3));

    on_logout(&ctx, uid);
    with_store(&ctx, |_, st| {
        assert!(!st.login_start_unix.contains_key(&uid));
        assert!(st.accrued_playtime_secs.get(&uid).copied().unwrap_or(0) >= 180);
    });

    // Underage + Kiring checks
    assert_eq!(
        handle_inner(&ctx, &mut sess, "authentication", "AmUnderage", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    set_underage(&ctx, uid, true);
    assert_eq!(
        handle_inner(&ctx, &mut sess, "authentication", "AmUnderage", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let kiring_cfg = handle_inner(&ctx, &mut sess, "kiringMgr", "GetKiringConfiguration", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(kiring_cfg.dict_get("enabled"), Some(&PyRep::Bool(false)));
    assert_eq!(
        kiring_cfg.dict_get("antiAddictionEnabled"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        kiring_cfg.dict_get("mode"),
        Some(&PyRep::String("live".into()))
    );
    assert_eq!(
        kiring_cfg.dict_get("client_id"),
        Some(&PyRep::String("evomu".into()))
    );
    let endpoints = kiring_cfg.dict_get("endpoints").expect("endpoints key");
    assert_eq!(endpoints.dict_get("mpay"), Some(&PyRep::String(String::new())));
    assert_eq!(endpoints.dict_get("billing"), Some(&PyRep::String(String::new())));
    assert_eq!(
        endpoints.dict_get("redirect_uri"),
        Some(&PyRep::String(String::new()))
    );
    let channels = kiring_cfg.dict_get("channels").expect("channels key");
    assert_eq!(channels.dict_get("login"), Some(&PyRep::String(String::new())));
    assert_eq!(channels.dict_get("pay"), Some(&PyRep::String(String::new())));
    assert_eq!(channels.dict_get("app"), Some(&PyRep::String(String::new())));
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "GetAntiAddictionCode", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "GetSAuthAid", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "GetEveGuardServerAddress", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "kiring:eveguard_server_address".into(),
            "https://eveguard.example.invalid".into(),
        );
    });
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "GetEveGuardServerAddress", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("https://eveguard.example.invalid".into())
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove("kiring:eveguard_server_address");
    });
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "IsMonochromeStyleEnabled", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "ShouldKickCheater", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    set_underage(&ctx, uid, false);
    assert_eq!(
        handle_inner(&ctx, &mut sess, "kiringMgr", "GetAntiAddictionCode", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    // Subscription expiry sync: future vs past vs quasar note max
    let now = ctx.now_unix();
    let (grade_fut, exp_fut) = set_subscription_expiry(&ctx, uid, now + 3600);
    assert_eq!(grade_fut, 2);
    assert_eq!(exp_fut, now + 3600);

    ctx.db.with_mut_snapshot(|snap| {
        let mut qstore = crate::services::quasar_auth_user::QuasarIdentityStore::default();
        qstore.omega_subscriptions.insert(uid, now + 7200);
        snap.owner_notes.insert(
            -8_500_000_000_000_i64,
            OwnerNoteRecord {
                note_id: -8_500_000_000_000_i64,
                character_id: 0,
                item_id: 0,
                label: "quasar".into(),
                note: serde_json::to_string(&qstore).unwrap(),
                timestamp_unix: now,
            },
        );
        assert_eq!(subscription_expiry_for_user(snap, uid), Some(now + 7200));
    });

    let sub_rep = handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetSubscriptionTime", &[], &kw)
        .unwrap()
        .unwrap();
    let expected_omega_ft = ((now + 7200) as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(sub_rep, PyRep::Integer(expected_omega_ft));
    assert_eq!(
        handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetCloneGrade", &[], &kw).unwrap(),
        None
    );

    // Lapsed subscription downgrades to Alpha (1) and GetSubscriptionTime returns None
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.remove(&-8_500_000_000_000_i64);
    });
    let (grade_past, _) = set_subscription_expiry(&ctx, uid, now.saturating_sub(10));
    assert_eq!(grade_past, 1);
    let sub_alpha = handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetSubscriptionTime", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(sub_alpha, PyRep::None);

    // Default-Omega account with no explicit expiry (exp == 0) gets a future Win32 FILETIME integer
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&ACCOUNT_SERVICES_STORE_KEY);
        for ch in snap.characters.values_mut().filter(|c| c.user_id == uid) {
            ch.clone_grade = 2;
        }
    });
    let sub_default_omega = handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetSubscriptionTime", &[], &kw)
        .unwrap()
        .unwrap();
    let expected_fallback_ft = ((now + 30 * 86_400) as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(sub_default_omega, PyRep::Integer(expected_fallback_ft));
}

#[test]
fn slash_commands_redeem_tokens_mct_resculpt_and_user_svc_misc() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Non-GM rejected on slash and CreateRedeemTokenQA
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/setomega 1 9999999999".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, " slash ", "SlashCmd", &[PyRep::String("/other".into())], &kw)
            .unwrap_or(None),
        None
    );

    sess.role = eve_machonet::service_roles::ROLE_GMH;
    let now = ctx.now_unix();
    let omega_res = handle_inner(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/setomega {uid} {}", now + 5000))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(omega_res.dict_get("cloneGrade"), Some(&PyRep::Integer(2)));

    let underage_res = handle_inner(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/setunderage {uid} true"))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(underage_res.dict_get("underage"), Some(&PyRep::Bool(true)));

    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/setomega 999999 100".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "AccountNotFound"
    );

    // CreateRedeemTokenQA + GetRedeemTokens + ClaimRedeemTokens + TrashRedeemTokens
    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "CreateRedeemTokenQA", &[PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidTypeID"
    );
    let created_tok = handle_inner(
        &ctx,
        &mut sess,
        "userSvc",
        "CreateRedeemTokenQA",
        &[
            PyRep::Integer(587),
            PyRep::Integer(1),
            PyRep::String("Free Rifter".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let tok1_id = created_tok.dict_get("tokenID").and_then(|v| v.as_i64()).unwrap();

    // Enqueue a second token and an expired token
    let (tok2_id, tok_exp_id) = ctx.db.with_mut_snapshot(|snap| {
        let t2 = enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            34,
            100,
            "Tritanium".into(),
            Some(60_003_760),
            None,
        );
        let t3 = enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            34,
            50,
            "Expired".into(),
            None,
            Some(1),
        );
        (t2, t3)
    });

    let list_rep = handle_inner(&ctx, &mut sess, "userSvc", "GetRedeemTokens", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(list_rep.as_list().map(|l| l.len()), Some(3));

    // Claiming only expired token fails with RedeemTokenNotFound
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "ClaimRedeemTokens",
            &[PyRep::List(vec![PyRep::Integer(tok_exp_id)]), PyRep::Integer(cid)],
            &kw,
        )
        .unwrap_err()
        .code,
        "RedeemTokenNotFound"
    );

    // Claim tok1_id via KeyVal and tok2_id via Tuple
    let claim_rep = handle_inner(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[
            PyRep::List(vec![
                PyRep::keyval([("tokenID", PyRep::Integer(tok1_id))]),
                PyRep::Tuple(vec![PyRep::Integer(tok2_id), PyRep::Integer(0)]),
            ]),
            PyRep::Integer(cid),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        claim_rep
            .dict_get("claimedTokenIDs")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(2)
    );

    // Trash the remaining expired token
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "TrashRedeemTokens",
            &[PyRep::Integer(tok_exp_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "ReverseRedeem",
            &[PyRep::Integer(tok_exp_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    // ActivateMultiTraining & ActivateCharacterReSculpt
    assert!(handle_inner(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[], &kw).is_err());
    assert!(handle_inner(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[], &kw).is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_001,
            InventoryItemRecord {
                item_id: 770_001,
                type_id: 34189,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            770_002,
            InventoryItemRecord {
                item_id: 770_002,
                type_id: 28662,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mct_act = handle_inner(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(mct_act.dict_get("slotActivated"), Some(&PyRep::Bool(true)));
    let mct_slots = handle_inner(
        &ctx,
        &mut sess,
        "userSvc",
        "GetMultiCharactersTrainingSlots",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(mct_slots.as_dict().is_some_and(|d| d.len() == 1));
    let mct_upd = handle_inner(
        &ctx,
        &mut sess,
        "userSvc",
        "InvalidateMultiCharacterTrainingCache",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mct_upd.as_list().map(|l| l.len()), Some(1));

    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // GetUserName, GetCreateDate, GetUserToken, CastUpvote/Downvote, ReportBot, ProcessEVESessionCreated
    assert!(matches!(
        handle_inner(&ctx, &mut sess, "userSvc", "GetUserName", &[], &kw).unwrap().unwrap(),
        PyRep::String(_)
    ));
    assert!(
        handle_inner(&ctx, &mut sess, "userSvc", "GetCreateDate", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 0
    );
    let PyRep::String(tok_str) =
        handle_inner(&ctx, &mut sess, "userSvc", "GetUserToken", &[], &kw)
            .unwrap()
            .unwrap()
    else {
        panic!("expected String JWT from userSvc.GetUserToken");
    };
    let verified = ctx.db.verify_quasar_bearer_jwt(&tok_str, ctx.now_unix()).unwrap();
    assert_eq!(verified.user_id, 1);
    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "CastUpvote", &[PyRep::String("ui".into())], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "CastDownvote", &[PyRep::String("ui".into())], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "ReportBot",
            &[PyRep::Integer(90_000_002), PyRep::String("macro".into())],
            &kw,
        )
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap()
            > 0
    );
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "ProcessEVESessionCreated",
            &[PyRep::Integer(cid)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert!(handle_inner(
        &ctx,
        &mut sess,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(99_999_999)],
        &kw,
    )
    .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_logout_logoff_and_multilogin_blocker_paths() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();

    // multiLoginBlocker.Login validation and success
    let blank_hash = [PyRep::String("  ".into())];
    let err = handle(&ctx, &mut sess, "multiLoginBlocker", "Login", &blank_hash, &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "InvalidComputerHash");
    let valid_hash = [PyRep::Bytes(b"hw-hash-001".to_vec())];
    let ok = handle(&ctx, &mut sess, "multiLoginBlocker", "Login", &valid_hash, &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok, PyRep::Bool(true));

    // UserLogOffCharacter clears character fields while preserving user_id
    let logoff = handle(&ctx, &mut sess, "userSvc", "UserLogOffCharacter", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(logoff, PyRep::Bool(true));
    assert_eq!(sess.user_id, Some(uid));
    assert_eq!(sess.character_id, None);

    // Re-bind character and test authentication.Logout
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    let logout = handle(&ctx, &mut sess, "authentication", "Logout", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(logout.dict_get("loggedOut"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.user_id, None);
    assert_eq!(sess.character_id, None);

    // Banned account check in multiLoginBlocker
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "banned_user".into(),
            AccountRecord {
                user_id: 500,
                username: "banned_user".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: Some(u64::MAX),
                ban_reason: None,
            },
        );
    });
    let mut banned_sess = auth_session(500, 0, 0, 30_000_142);
    banned_sess.character_id = None;
    let err = handle(
        &ctx,
        &mut banned_sess,
        "multiLoginBlocker",
        "Login",
        &[PyRep::String("hw-hash-002".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "UserBanned");
}

#[test]
fn subscription_and_create_date_reads_do_not_mutate_db() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let kw = HashMap::new();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut alpha_sess = auth_session(999, 90_000_999, 98_000_001, 30_000_142);

    let epoch_before = ctx.db.state_epoch();
    let commits_before = ctx.db.tx_commit_count();
    let dt1 = handle_inner(&ctx, &mut alpha_sess, "userSvc", "GetCreateDate", &[], &kw)
        .unwrap()
        .unwrap();
    let dt2 = handle_inner(&ctx, &mut alpha_sess, "userSvc", "GetCreateDate", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dt1, dt2);
    assert!(dt1.as_i64().unwrap() > 130_000_000_000_000_000);
    assert_eq!(
        handle_inner(&ctx, &mut alpha_sess, "subscriptionMgr", "GetSubscriptionTime", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_inner(&ctx, &mut alpha_sess, "subscriptionMgr", "GetCloneGrade", &[], &kw).unwrap(),
        None
    );

    let omega_dt = handle_inner(&ctx, &mut sess, "userSvc", "GetCreateDate", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(omega_dt.as_i64().unwrap() > 130_000_000_000_000_000);
    let expected_omega_ft = (1_800_000_000_i64 + 30 * 86_400 + 11_644_473_600) * 10_000_000;
    assert_eq!(
        handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetSubscriptionTime", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(expected_omega_ft)
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "subscriptionMgr", "GetCloneGrade", &[], &kw).unwrap(),
        None
    );
    assert_eq!(ctx.db.state_epoch(), epoch_before);
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
}

#[test]
fn subscription_get_subscription_time_is_read_pure_with_quasar_or_lapsed_state() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let kw = HashMap::new();

    // Unauthenticated callers fail closed with zero DB mutations on account/subscription reads.
    let mut unauth = SessionState::default();
    let unauth_epoch = ctx.db.state_epoch();
    let unauth_commits = ctx.db.tx_commit_count();
    for (svc, meth) in [
        ("userSvc", "GetCreateDate"),
        ("subscriptionMgr", "GetSubscriptionTime"),
        ("subscriptionMgr", "GetCloneGrade"),
    ] {
        let err = handle_inner(&ctx, &mut unauth, svc, meth, &[], &kw).unwrap_err();
        assert_eq!(err.code, "Unauthorized", "{svc}.{meth}");
    }
    assert_eq!(ctx.db.state_epoch(), unauth_epoch);
    assert_eq!(ctx.db.tx_commit_count(), unauth_commits);

    let uid = 777_u64;
    let cid = 90_777_001_i64;
    let now = ctx.now_unix();
    let future_exp = now + 7_200;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .get(&90_000_001)
            .expect("base char")
            .clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.name = "QuasarReadonly".into();
        ch.clone_grade = 1;
        ch.create_date_unix = 0;
        snap.characters.insert(cid, ch);
        snap.accounts.insert(
            "quasar_readonly".into(),
            AccountRecord {
                user_id: uid,
                username: "quasar_readonly".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut qstore = crate::services::quasar_auth_user::QuasarIdentityStore::default();
        qstore.omega_subscriptions.insert(uid, future_exp);
        snap.owner_notes.insert(
            -8_500_000_000_000_i64,
            OwnerNoteRecord {
                note_id: -8_500_000_000_000_i64,
                character_id: 0,
                item_id: 0,
                label: "quasar".into(),
                note: serde_json::to_string(&qstore).unwrap(),
                timestamp_unix: now,
            },
        );
    });

    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let _ = ctx.drain_notifications_for_char(cid);
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    let expected_fallback_dt =
        PyRep::Integer((1_700_000_000_i64 + 11_644_473_600) * 10_000_000);
    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "GetCreateDate", &[], &kw)
            .unwrap()
            .unwrap(),
        expected_fallback_dt
    );
    assert_eq!(
        handle_inner(&ctx, &mut sess, "userSvc", "GetCreateDate", &[], &kw)
            .unwrap()
            .unwrap(),
        expected_fallback_dt
    );

    let expected_future_ft =
        PyRep::Integer(((future_exp as i64) + 11_644_473_600) * 10_000_000);
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "subscriptionMgr",
            "GetSubscriptionTime",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        expected_future_ft
    );
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters.get(&cid).expect("char").clone_grade,
            1
        );
        let st = load_store(snap);
        assert!(!st.subscription_expiry.contains_key(&uid));
        assert!(!st.account_create_date.contains_key(&uid));
    });

    // Lapsed Omega in Quasar store with un-synced grade-2 DB char must still read Alpha without mutating DB.
    let past_exp = now.saturating_sub(60);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2;
        }
        let mut qstore = crate::services::quasar_auth_user::QuasarIdentityStore::default();
        qstore.omega_subscriptions.insert(uid, past_exp);
        snap.owner_notes.insert(
            -8_500_000_000_000_i64,
            OwnerNoteRecord {
                note_id: -8_500_000_000_000_i64,
                character_id: 0,
                item_id: 0,
                label: "quasar".into(),
                note: serde_json::to_string(&qstore).unwrap(),
                timestamp_unix: now,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(cid);
    let epoch1 = ctx.db.state_epoch();
    let commits1 = ctx.db.tx_commit_count();
    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "subscriptionMgr",
            "GetSubscriptionTime",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(ctx.db.state_epoch(), epoch1);
    assert_eq!(ctx.db.tx_commit_count(), commits1);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| snap.characters.get(&cid).expect("char").clone_grade),
        2
    );
}

#[test]
fn trash_and_reverse_redeem_tokens_precheck_before_mutating_store() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let (tok1, tok2) = ctx.db.with_mut_snapshot(|snap| {
        let t1 = enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            587,
            1,
            "Rifter".into(),
            None,
            None,
        );
        let t2 = enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            34,
            100,
            "Tritanium".into(),
            None,
            None,
        );
        (t1, t2)
    });
    let _ = ctx.drain_notifications_for_char(cid);
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    for meth in ["TrashRedeemTokens", "ReverseRedeem"] {
        assert_eq!(
            handle_inner(&ctx, &mut sess, "userSvc", meth, &[PyRep::List(vec![])], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(false)
        );
        assert_eq!(
            handle_inner(
                &ctx,
                &mut sess,
                "userSvc",
                meth,
                &[PyRep::List(vec![PyRep::Integer(tok1 + 99_999)])],
                &kw,
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(false)
        );
    }
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_store(s).redeem_tokens.get(&uid).map(|v| v.len())),
        Some(2)
    );

    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "TrashRedeemTokens",
            &[PyRep::List(vec![PyRep::Integer(tok1)])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_store(s).redeem_tokens.get(&uid).map(|v| v.len())),
        Some(1)
    );

    assert_eq!(
        handle_inner(
            &ctx,
            &mut sess,
            "userSvc",
            "ReverseRedeem",
            &[PyRep::List(vec![PyRep::Integer(tok2)])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| load_store(s).redeem_tokens.get(&uid).map(|v| v.len())),
        Some(0)
    );
}

#[cfg(test)]
#[path = "account_services_extra_unit_tests.rs"]
mod account_services_extra_unit_tests;
