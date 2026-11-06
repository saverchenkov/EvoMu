//! Deep branch-coverage and mutation-killing unit tests for `account_services`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::account_services::*;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn call_acct(
    ctx: &ServerContext,
    sess: &mut eve_machonet::SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, service, method, args, kwargs)
        .await
        .expect("account_services method should be handled")
}

#[tokio::test]
async fn save_location_logout_in_space_and_multilogin_blocker_branches() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    // 1. save_location_and_leave_space with no character_id and unknown character_id
    let mut no_char_sess = auth_session(uid, cid, 98_000_001, sys_id);
    no_char_sess.character_id = None;
    assert_eq!(save_location_and_leave_space(&ctx, &no_char_sess).await, (false, 0));

    let ghost_sess = auth_session(uid, 999_888_777, 98_000_001, sys_id);
    assert_eq!(save_location_and_leave_space(&ctx, &ghost_sess).await, (false, 0));

    // 2. Character in space (no station_id / structure_id, active_ship_id > 0) -> removed_from_space = true
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = 100_000_001;
        }
    });
    let mut space_sess = auth_session(uid, cid, 98_000_001, sys_id);
    space_sess.station_id = None;
    space_sess.structure_id = None;
    let (removed, saved_sys) = save_location_and_leave_space(&ctx, &space_sess).await;
    assert!(removed);
    assert_eq!(saved_sys, sys_id);

    // 3. multiLoginBlocker.Login: kwargs computerhash, >1 args error, non-string error, displacement, alpha conflict, suspended
    let mut sess1 = auth_session(uid, cid, 98_000_001, sys_id);
    sess1.session_id = 1001;
    let err = call_acct(
        &ctx,
        &mut sess1,
        "multiLoginBlocker",
        "Login",
        &[PyRep::String("a".into()), PyRep::String("b".into())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidComputerHash");

    let err = call_acct(
        &ctx,
        &mut sess1,
        "multiLoginBlocker",
        "Login",
        &[PyRep::Bytes(b"   ".to_vec())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidComputerHash");

    let err = call_acct(&ctx, &mut sess1, "multiLoginBlocker", "Login", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidComputerHash");

    // Login via kwargs["computerHash"] on session 1001
    let mut kw_hash = HashMap::new();
    kw_hash.insert("computerHash".into(), PyRep::String("box-alpha-1".into()));
    let ok1 = call_acct(&ctx, &mut sess1, "multiLoginBlocker", "Login", &[], &kw_hash)
        .await
        .unwrap();
    assert_eq!(ok1, PyRep::Bool(true));

    // Concurrent login on same account (uid=1) from session 1002 when lock is held by 1001 -> returns Bool(false)
    let mut sess2 = auth_session(uid, cid, 98_000_001, sys_id);
    sess2.session_id = 1002;
    let ok2 = call_acct(
        &ctx,
        &mut sess2,
        "multiLoginBlocker",
        "Login",
        &[PyRep::String("box-alpha-1".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(ok2, PyRep::Bool(false));

    // Second Alpha account (uid=601) attempting to use same computer_hash "box-alpha-1" -> returns false
    ctx.db.with_mut_snapshot(|snap| {
        for ch in snap.characters.values_mut().filter(|c| c.user_id == uid) {
            ch.clone_grade = 1;
        }
        snap.accounts.insert(
            "alpha2".into(),
            AccountRecord {
                user_id: 601,
                username: "alpha2".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let mut alpha2_sess = auth_session(601, 0, 0, sys_id);
    alpha2_sess.session_id = 2002;
    alpha2_sess.character_id = None;
    let conflict = call_acct(
        &ctx,
        &mut alpha2_sess,
        "multiLoginBlocker",
        "Login",
        &[PyRep::String("box-alpha-1".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(conflict, PyRep::Bool(false));

    // Suspended account (finite banned_until_unix > now) -> AccountSuspended
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "suspended_user".into(),
            AccountRecord {
                user_id: 602,
                username: "suspended_user".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: Some(now + 3600),
                ban_reason: Some("temp".into()),
            },
        );
    });
    let mut susp_sess = auth_session(602, 0, 0, sys_id);
    susp_sess.session_id = 2003;
    susp_sess.character_id = None;
    let err = call_acct(
        &ctx,
        &mut susp_sess,
        "multiLoginBlocker",
        "Login",
        &[PyRep::String("box-susp".into())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserBanned");

    // Petitioner delegation through account_services::handle
    assert!(call_acct(&ctx, &mut sess2, "petitioner", "GetCategories", &[], &kw)
        .await
        .is_ok());

    // UserLogOffCharacter without character fails
    let err = call_acct(&ctx, &mut susp_sess, "userSvc", "UserLogOffCharacter", &[], &kw)
        .await
        .unwrap_err();
    assert!(!err.code.is_empty());
}

#[tokio::test]
async fn claim_redeem_tokens_input_variants_ownership_ship_singleton_and_injectors() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    let kw = HashMap::new();

    // 1. Slash command argument validation errors
    let err = call_acct(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/setomega not_a_uid 100".into())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidSlashArgument");

    let err = call_acct(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/setomega 1 not_a_ts".into())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidSlashArgument");

    // 2. CreateRedeemTokenQA with unknown SDE type_id -> InvalidTypeID; negative qty clamped to 1
    let err = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidTypeID");

    let ship_tok = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(587), PyRep::Integer(-5), PyRep::String("Ship".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(ship_tok.dict_get("quantity"), Some(&PyRep::Integer(1)));
    let ship_tok_id = ship_tok.dict_get("tokenID").and_then(PyRep::as_i64).unwrap();

    // 3. ClaimRedeemTokens error branches: CharacterNotSelected, CharacterNotFound, CharacterOwnershipMismatch
    let mut no_char_sess = sess.clone();
    no_char_sess.character_id = None;
    let err = call_acct(
        &ctx,
        &mut no_char_sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(ship_tok_id)])],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CharacterNotSelected");

    let err = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[
            PyRep::List(vec![PyRep::Integer(ship_tok_id)]),
            PyRep::Integer(999_888_777),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CharacterNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        let mut c2 = snap.characters[&cid].clone();
        c2.character_id = other_cid;
        c2.user_id = 777;
        snap.characters.insert(other_cid, c2);
    });
    let err = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[
            PyRep::List(vec![PyRep::Integer(ship_tok_id)]),
            PyRep::Integer(other_cid),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CharacterOwnershipMismatch");

    // 4. Single integer arg matching a character ID sets target_cid and fails with RedeemTokenNotFound
    let err = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::Integer(cid)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "RedeemTokenNotFound");

    // 5. Single integer arg NOT matching a character ID claims that token for session.character_id;
    // ship category (6) creates singleton = true item at structure_id fallback when station_id is None
    let struct_loc = 1_000_000_888_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = Some(struct_loc);
        }
    });
    let claim_ship = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::Integer(ship_tok_id)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(claim_ship.dict_get("stationID"), Some(&PyRep::Integer(struct_loc)));
    let created_ship_id = claim_ship
        .dict_get("createdItemIDs")
        .and_then(|v| v.as_list())
        .unwrap()[0]
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let item = &snap.items[&created_ship_id];
        assert!(item.singleton);
        assert_eq!(item.location_id, struct_loc);
    });

    // 6. Claim via kwargs ("tokens" with Dict entry + "characterID") for non-diminishing injector
    let inj_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| crate::services::implant_plugging::is_non_diminishing_injector(&ctx.sde, t.type_id))
        .map(|t| t.type_id)
        .unwrap_or(34);
    let inj_tok_id = ctx.db.with_mut_snapshot(|snap| {
        enqueue_redeem_token_in_snapshot(snap, uid, inj_type_id, 2, "Injector".into(), None, None)
    });
    let mut kw_claim = HashMap::new();
    kw_claim.insert(
        "tokens".into(),
        PyRep::List(vec![PyRep::Dict(vec![(
            PyRep::String("tokenID".into()),
            PyRep::Integer(inj_tok_id),
        )])]),
    );
    kw_claim.insert("characterID".into(), PyRep::Integer(cid));
    let claim_inj = call_acct(&ctx, &mut sess, "userSvc", "ClaimRedeemTokens", &[], &kw_claim)
        .await
        .unwrap();
    assert_eq!(
        claim_inj
            .dict_get("claimedTokenIDs")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(1)
    );
}

#[tokio::test]
async fn mct_resculpt_stack_decrement_and_session_history_trim_branches() {
    let ctx = test_ctx();
    let uid = 42_u64;
    let cid = 90_000_042_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = cid;
        ch.user_id = uid;
        snap.characters.insert(cid, ch);

        // Stack of 2 MCT certificates (type 48582) and stack of 2 Resculpt certificates (type 33502)
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: 48582,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: 33502,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    // 1. First ActivateSoulboundMultiTraining decrements stack to 1; second removes item
    assert!(call_acct(&ctx, &mut sess, "userSvc", "ActivateSoulboundMultiTraining", &[], &kw)
        .await
        .is_ok());
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&880_001].quantity, 1));

    assert!(call_acct(&ctx, &mut sess, "userSvc", "ActivateSoulboundMultiTraining", &[], &kw)
        .await
        .is_ok());
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&880_001)));

    let upd = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "GetUpdatedMultiCharacterTraining",
        &[],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(upd.as_list().map(|l| l.len()), Some(2));

    // 2. First ActivateCharacterReSculpt decrements stack to 1; second removes item
    assert!(call_acct(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[], &kw)
        .await
        .is_ok());
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&880_002].quantity, 1));
    assert!(call_acct(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[], &kw)
        .await
        .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&880_002));
        assert_eq!(
            snap.service_state_blobs
                .get(&format!("account_resculpt_tokens:{uid}"))
                .map(String::as_str),
            Some("2")
        );
    });

    // 3. GetUserName fallback ("pilot") when user_id has no AccountRecord, and ReportISKSpammer
    let uname = call_acct(&ctx, &mut sess, "userSvc", "GetUserName", &[], &kw)
        .await
        .unwrap();
    assert_eq!(uname, PyRep::String("pilot".into()));

    let spam_note_id = call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ReportISKSpammer",
        &[PyRep::Integer(90_000_001), PyRep::String("spam".into())],
        &kw,
    )
    .await
    .unwrap()
    .as_i64()
    .unwrap();
    assert!(spam_note_id > 0);

    // 4. ProcessEVESessionCreated mismatch when session.character_id == None but requested char belongs to uid 1
    let mut unbound_sess = sess.clone();
    unbound_sess.character_id = None;
    let err = call_acct(
        &ctx,
        &mut unbound_sess,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(90_000_001)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.message, "CharacterDoesNotBelongToUser");

    // 5. ProcessEVESessionCreated trims user_sessions list at 256 entries
    ctx.db.with_mut_snapshot(|snap| {
        let big: Vec<serde_json::Value> = (0..260)
            .map(|i| serde_json::json!({"user_id": uid, "session_id": i}))
            .collect();
        snap.service_state_blobs
            .insert("user_sessions".into(), serde_json::to_string(&big).unwrap());
    });
    assert!(call_acct(
        &ctx,
        &mut sess,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(cid)],
        &kw,
    )
    .await
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        let list: Vec<serde_json::Value> =
            serde_json::from_str(&snap.service_state_blobs["user_sessions"]).unwrap();
        assert_eq!(list.len(), 256);
    });
}
