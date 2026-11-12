//! Comprehensive branch-coverage and mutation-killing tests for `quasar_auth_user.rs`.

use crate::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED, STATUS_UNAUTHORIZED,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_auth_user::{
    activate_user_omega_license, expire_user_omega_license, read_identity_store, register_handlers,
};
use eve_db::{AccountRecord, ContractRecord, IndustryJobRecord, IskCents, MarketOrderRecord};
use eve_proto::quasar::{
    encode_string_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, WireMessage,
};
use std::sync::Arc;

fn user_ctx(uid: Option<i32>, cid: Option<i64>, is_admin: bool) -> AuthoritativeContext {
    let role = if is_admin { u64::MAX } else { 0 };
    let mut ctx = AuthoritativeContext::for_session(
        uid,
        cid.map(|c| c as u32),
        Some(1_000_009),
        role,
        "tranquility",
        vec![1],
    );
    if is_admin {
        ctx = ctx.with_service_roles(u64::MAX);
    }
    ctx
}

#[test]
fn omega_subscription_stacking_and_username_password_auth_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    register_handlers(router, Arc::clone(db));

    db.provision_gm_account("alice_user", "secret_pass", 91_000_001, "AlicePilot");
    let alice_uid = db.with_snapshot(|s| s.characters[&91_000_001].user_id);

    // 1. Activate omega when none active -> base = now_unix (1_000) + 500 = 1_500
    let exp1 = activate_user_omega_license(db, &router.pending_notices, alice_uid, 500, 1_000);
    assert_eq!(exp1, 1_500);
    assert_eq!(db.with_snapshot(|s| s.characters[&91_000_001].clone_grade), 2);

    // 2. Activate omega again while still active (now_unix = 1_200 < 1_500) -> stacks onto 1_500 -> 2_000
    let exp2 = activate_user_omega_license(db, &router.pending_notices, alice_uid, 500, 1_200);
    assert_eq!(exp2, 2_000);
    let ent_count = read_identity_store(db, |st| {
        st.user_entitlements
            .get(&alice_uid)
            .map(|v| v.iter().filter(|e| e.eq_ignore_ascii_case("omega")).count())
            .unwrap_or(0)
    });
    assert_eq!(ent_count, 1);

    // 3. Expire omega -> clone_grade = 1 and subscription = 0
    expire_user_omega_license(db, &router.pending_notices, alice_uid);
    assert_eq!(db.with_snapshot(|s| s.characters[&91_000_001].clone_grade), 1);

    // 4. AuthenticateUsernameRequest across all credential encoding shapes and failure branches
    let handlers = router.handlers.read().unwrap();
    let auth_fn = &handlers["eve_public.auth.AuthenticateUsernameRequest"];
    let anon = user_ctx(None, None, false);
    let dummy_any = GoogleAny::default();

    // Empty credentials -> STATUS_UNAUTHORIZED
    let empty_msg = DynamicQuasarMessage::new("Auth");
    assert_eq!(auth_fn(&anon, &dummy_any, &empty_msg).unwrap_err().0, STATUS_UNAUTHORIZED);

    // Flat fields (1 = username, 2 = wrong password) -> STATUS_UNAUTHORIZED
    let mut bad_pw = DynamicQuasarMessage::new("Auth");
    bad_pw.fields.insert(1, DynamicValue::String("alice_user".into()));
    bad_pw.fields.insert(2, DynamicValue::String("wrong_pass".into()));
    assert_eq!(auth_fn(&anon, &dummy_any, &bad_pw).unwrap_err().0, STATUS_UNAUTHORIZED);

    // Flat fields valid
    let mut flat_ok = DynamicQuasarMessage::new("Auth");
    flat_ok.fields.insert(1, DynamicValue::String("alice_user".into()));
    flat_ok.fields.insert(2, DynamicValue::String("secret_pass".into()));
    let rsp1 = auth_fn(&anon, &dummy_any, &flat_ok).unwrap();
    assert!(String::from_utf8_lossy(&rsp1.value).contains("evomu-auth-token-alice_user"));

    // Nested creds Message with Bytes and String in enc.get(2)
    for inner_val in [
        DynamicValue::Bytes(b"secret_pass".to_vec()),
        DynamicValue::String("secret_pass".into()),
    ] {
        let mut enc = DynamicQuasarMessage::new("Enc");
        enc.fields.insert(2, inner_val);
        let mut creds = DynamicQuasarMessage::new("Creds");
        creds.fields.insert(1, DynamicValue::String("alice_user".into()));
        creds.fields.insert(2, DynamicValue::Message(Box::new(enc)));
        let mut outer = DynamicQuasarMessage::new("Auth");
        outer.fields.insert(1, DynamicValue::Message(Box::new(creds)));
        assert!(auth_fn(&anon, &dummy_any, &outer).is_ok());
    }

    // Nested creds Message with direct String(2) and direct Bytes(2)
    for pw_val in [
        DynamicValue::String("secret_pass".into()),
        DynamicValue::Bytes(b"secret_pass".to_vec()),
    ] {
        let mut cm = DynamicQuasarMessage::new("Creds");
        cm.fields.insert(1, DynamicValue::String("alice_user".into()));
        cm.fields.insert(2, pw_val);
        let mut om = DynamicQuasarMessage::new("Auth");
        om.fields.insert(1, DynamicValue::Message(Box::new(cm)));
        assert!(auth_fn(&anon, &dummy_any, &om).is_ok());
    }
}

#[test]
fn federated_steam_epic_twitch_and_legal_agreement_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    register_handlers(router, Arc::clone(db));
    let handlers = router.handlers.read().unwrap();

    let anon = user_ctx(None, None, false);
    let u1 = user_ctx(Some(10), Some(90_000_010), false);
    let u2 = user_ctx(Some(20), Some(90_000_020), false);
    let admin = user_ctx(Some(1), Some(90_000_001), true);
    let dummy_any = GoogleAny::default();

    for h_name in [
        "eve_public.user.license.api.GetRequest",
        "eve.legal.agreement.api.GetLatestRequest",
        "eve.user.legal.agreement.api.AcceptRequest",
        "eve_public.auth.steam.AuthenticateSteamRequest",
        "eve.vendors.epic.api.GetUserByEpicUserIDRequest",
        "eve.vendors.epic.api.GetEntitlementsRequest",
        "eve.id.federate.twitch.api.CompleteLinkingRequest",
    ] {
        assert_eq!(
            handlers[h_name](&anon, &dummy_any, &DynamicQuasarMessage::new("E")).unwrap_err().0,
            STATUS_UNAUTHORIZED
        );
    }

    // AcceptRequest: field 3, raw ASCII fallback, and empty fallback ("eula_v24")
    let mut f3_msg = DynamicQuasarMessage::new("Acc");
    f3_msg.fields.insert(3, DynamicValue::String("custom_eula_3".into()));
    assert!(handlers["eve.user.legal.agreement.api.AcceptRequest"](&u1, &dummy_any, &f3_msg).is_ok());

    let raw_ascii_any = GoogleAny {
        type_url: "type.googleapis.com/eve.user.legal.agreement.api.AcceptRequest".into(),
        value: b"raw_ascii_eula".to_vec(),
    };
    assert!(handlers["eve.user.legal.agreement.api.AcceptRequest"](
        &u1,
        &raw_ascii_any,
        &DynamicQuasarMessage::new("Acc")
    )
    .is_ok());
    assert!(handlers["eve.user.legal.agreement.api.AcceptRequest"](
        &u1,
        &dummy_any,
        &DynamicQuasarMessage::new("Acc")
    )
    .is_ok());

    let accepted = read_identity_store(db, |st| st.accepted_agreements[&10].clone());
    assert_eq!(accepted, vec!["custom_eula_3", "raw_ascii_eula", "eula_v24"]);

    // Steam Link & Auth branches
    assert_eq!(
        handlers["eve.vendors.steam.user.api.admin.LinkRequest"](
            &u1,
            &dummy_any,
            &DynamicQuasarMessage::new("L")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let st_fallback = handlers["eve_public.auth.steam.AuthenticateSteamRequest"](
        &u1,
        &dummy_any,
        &DynamicQuasarMessage::new("S"),
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&st_fallback.value).contains("evomu-steam-token-10"));

    let mut st_unlinked = DynamicQuasarMessage::new("S");
    st_unlinked.fields.insert(1, DynamicValue::String("steam_missing".into()));
    assert_eq!(
        handlers["eve_public.auth.steam.AuthenticateSteamRequest"](&u1, &dummy_any, &st_unlinked)
            .unwrap_err()
            .0,
        STATUS_UNAUTHORIZED
    );

    // Link steam_u1 to user 10 via wire-encoded bytes
    let mut link_wire = Vec::new();
    encode_string_field(1, "steam_u1", &mut link_wire);
    encode_varint_field(2, 10, &mut link_wire);
    let link_any = GoogleAny { type_url: String::new(), value: link_wire };
    assert!(handlers["eve.vendors.steam.user.api.admin.LinkRequest"](
        &admin,
        &link_any,
        &DynamicQuasarMessage::new("Empty")
    )
    .is_ok());

    let mut st_u1 = DynamicQuasarMessage::new("S");
    st_u1.fields.insert(1, DynamicValue::String("steam_u1".into()));
    assert_eq!(
        handlers["eve_public.auth.steam.AuthenticateSteamRequest"](&u2, &dummy_any, &st_u1)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Epic account binding & duplicate entitlement conflict
    let mut epic_cross = DynamicQuasarMessage::new("Epic");
    epic_cross.fields.insert(1, DynamicValue::String("epic_10".into()));
    epic_cross.fields.insert(2, DynamicValue::Int64(10));
    assert_eq!(
        handlers["eve.vendors.epic.api.GetUserByEpicUserIDRequest"](&u2, &dummy_any, &epic_cross)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert!(handlers["eve.vendors.epic.api.GetUserByEpicUserIDRequest"](
        &u1,
        &dummy_any,
        &epic_cross
    )
    .is_ok());

    let mut deliv_msg = DynamicQuasarMessage::new("Deliv");
    deliv_msg.fields.insert(1, DynamicValue::String("epic_10".into()));
    deliv_msg.fields.insert(2, DynamicValue::String("grant_unique_1".into()));
    deliv_msg.fields.insert(3, DynamicValue::Uint32(44992));
    deliv_msg.fields.insert(4, DynamicValue::Sint32(2));
    assert!(
        handlers["eve.vendors.epic.api.DeliverEntitlementRequest"](&admin, &dummy_any, &deliv_msg)
            .is_ok()
    );
    assert_eq!(
        handlers["eve.vendors.epic.api.DeliverEntitlementRequest"](&admin, &dummy_any, &deliv_msg)
            .unwrap_err()
            .0,
        STATUS_CONFLICT
    );

    // Twitch linking, lookup, and drops
    let mut tw_cross = DynamicQuasarMessage::new("Tw");
    tw_cross.fields.insert(1, DynamicValue::String("tw_u1".into()));
    tw_cross.fields.insert(2, DynamicValue::Sint64(10));
    assert_eq!(
        handlers["eve.id.federate.twitch.api.CompleteLinkingRequest"](&u2, &dummy_any, &tw_cross)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve.id.federate.twitch.api.GetRequest"](&u1, &dummy_any, &tw_cross)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve.vendors.twitch.entitlement.api.DropEventReceived"](
            &admin, &dummy_any, &tw_cross
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    assert!(
        handlers["eve.id.federate.twitch.api.CompleteLinkingRequest"](&u1, &dummy_any, &tw_cross)
            .is_ok()
    );
    let get_empty = handlers["eve.id.federate.twitch.api.GetEveIDRequest"](
        &u1,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let wm = WireMessage::decode(&get_empty.value).unwrap();
    assert_eq!(wm.fields[0].value.as_varint(), Some(10));

    let mut drop_msg = DynamicQuasarMessage::new("Drop");
    drop_msg.fields.insert(1, DynamicValue::String("tw_u1".into()));
    drop_msg.fields.insert(2, DynamicValue::String("drop_77".into()));
    assert!(handlers["eve.id.federate.twitch.api.VerifyExternalGrantRequest"](
        &admin, &dummy_any, &drop_msg
    )
    .is_ok());
    assert_eq!(
        handlers["eve.id.federate.twitch.api.VerifyExternalGrantRequest"](
            &admin, &dummy_any, &drop_msg
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );
}

#[test]
fn character_transfer_validation_guards_and_execution_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    register_handlers(router, Arc::clone(db));
    let handlers = router.handlers.read().unwrap();
    let validate_fn = &handlers["eve.user.character.transfer.ValidateRequest"];
    let queue_fn = &handlers["eve.user.character.transfer.QueueRequest"];
    let dummy_any = GoogleAny::default();

    let src_uid = 500u64;
    let dst_uid = 501u64;
    let char_id = 95_000_001i64;

    db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "src_acc".into(),
            AccountRecord {
                user_id: src_uid,
                username: "src_acc".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.accounts.insert(
            "dst_acc".into(),
            AccountRecord {
                user_id: dst_uid,
                username: "dst_acc".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: Some(2_000_000_000),
                ban_reason: Some("test".into()),
            },
        );
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = src_uid;
        ch.corporation_id = 98_000_001;
        ch.balance = IskCents(50_000);
        ch.plex_balance = 0;
        snap.characters.insert(char_id, ch);
    });

    let src_ctx = user_ctx(Some(src_uid as i32), Some(char_id), false);
    let other_ctx = user_ctx(Some(999), Some(char_id), false);
    let no_char_ctx = user_ctx(Some(src_uid as i32), None, false);
    assert_eq!(
        validate_fn(&no_char_ctx, &dummy_any, &DynamicQuasarMessage::new("X")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let make_xfer_msg = |cid: u64, dest: u64, token: Option<&str>| {
        let mut m = DynamicQuasarMessage::new("Xfer");
        m.fields.insert(1, DynamicValue::Uint64(cid));
        m.fields.insert(2, DynamicValue::Uint64(dest));
        if let Some(t) = token {
            m.fields.insert(3, DynamicValue::String(t.into()));
        }
        m
    };

    assert_eq!(
        validate_fn(&src_ctx, &dummy_any, &make_xfer_msg(999_999_999, dst_uid, None))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        validate_fn(&other_ctx, &dummy_any, &make_xfer_msg(char_id as u64, dst_uid, None))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        queue_fn(&src_ctx, &dummy_any, &make_xfer_msg(char_id as u64, src_uid, None))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        validate_fn(&src_ctx, &dummy_any, &make_xfer_msg(char_id as u64, 888_888, None))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        validate_fn(&src_ctx, &dummy_any, &make_xfer_msg(char_id as u64, dst_uid, None))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Unban destination account, test consent token mismatch
    db.with_mut_snapshot(|snap| {
        snap.accounts.get_mut("dst_acc").unwrap().banned_until_unix = None;
        let _ = snap
            .kv_mut(eve_db::Ns::CharTransferConsentToken)
            .set(dst_uid as i64, "", &"expected_secret");
    });
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("wrong_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    // Player corporation -> STATUS_PRECONDITION_FAILED
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    // Move to NPC corp 1_000_009, still < 1,000 ISK -> STATUS_PRECONDITION_FAILED
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009;
    });
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    // Give 5,000 ISK; test active market order, active contract, active industry job
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(500_000);
        snap.market_orders.insert(
            7001,
            MarketOrderRecord {
                order_id: 7001,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        snap.contracts.insert(
            8001,
            ContractRecord {
                contract_id: 8001,
                issuer_id: char_id,
                status: "outstanding".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    db.with_mut_snapshot(|snap| {
        snap.contracts.clear();
        snap.industry_jobs.insert(
            9001,
            IndustryJobRecord {
                job_id: 9001,
                installer_id: char_id,
                status: "active".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    db.with_mut_snapshot(|snap| snap.industry_jobs.clear());
    assert_eq!(
        queue_fn(
            &src_ctx,
            &dummy_any,
            &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret"))
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    let src_uid = db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.plex_balance = 1_200;
        ch.user_id
    });
    let queued = queue_fn(
        &src_ctx,
        &dummy_any,
        &make_xfer_msg(char_id as u64, dst_uid, Some("expected_secret")),
    )
    .unwrap();
    assert!(queued.type_url.ends_with("QueueResponse"));
    db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.user_id, dst_uid);
        assert_eq!(ch.plex_balance, 0);
        assert_eq!(snap.unified_plex_balance_for_user(src_uid), 200);
    });
}

#[test]
fn federated_and_transfer_handlers_registration_and_exact_1000_plex_fee() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    register_handlers(router, Arc::clone(db));

    let handlers = router.handlers.read().unwrap();
    for expected in [
        "eve.vendors.steam.user.api.admin.LinkRequest",
        "eve_public.auth.steam.AuthenticateSteamRequest",
        "eve.vendors.epic.api.GetUserByEpicUserIDRequest",
        "eve.id.federate.twitch.api.CompleteLinkingRequest",
        "eve.user.character.transfer.ValidateRequest",
        "eve.user.character.transfer.QueueRequest",
    ] {
        assert!(handlers.contains_key(expected), "missing registered handler {expected}");
    }

    let src_uid = 610_u64;
    let dst_uid = 611_u64;
    let char_id = 95_000_111_i64;
    db.with_mut_snapshot(|snap| {
        for (uname, uid) in [("src_610", src_uid), ("dst_611", dst_uid)] {
            snap.accounts.insert(
                uname.into(),
                AccountRecord {
                    user_id: uid,
                    username: uname.into(),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = src_uid;
        ch.corporation_id = 1_000_009;
        ch.balance = IskCents(500_000);
        ch.plex_balance = 0;
        snap.characters.insert(char_id, ch);
        assert_eq!(snap.adjust_unified_plex(char_id, 999), Some(999));
        let _ = snap
            .kv_mut(eve_db::Ns::CharTransferConsentToken)
            .set(dst_uid as i64, "", &"tok_ok");
    });

    let src_ctx = user_ctx(Some(src_uid as i32), Some(char_id), false);
    let dummy_any = GoogleAny::default();
    let mut xfer_msg = DynamicQuasarMessage::new("QueueXfer");
    xfer_msg.fields.insert(1, DynamicValue::Uint64(char_id as u64));
    xfer_msg.fields.insert(2, DynamicValue::Uint64(dst_uid));
    xfer_msg.fields.insert(3, DynamicValue::String("tok_ok".into()));

    let queue_fn = &handlers["eve.user.character.transfer.QueueRequest"];
    // 999 PLEX is 1 short of 1,000 PLEX fee -> must reject and leave account PLEX untouched.
    assert_eq!(
        queue_fn(&src_ctx, &dummy_any, &xfer_msg).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
    db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].user_id, src_uid);
        assert_eq!(snap.unified_plex_balance_for_user(src_uid), 999);
    });

    // Top up +501 PLEX -> 1,500 PLEX vault -> queue deducts exact -1,000 PLEX leaving 500 PLEX.
    db.with_mut_snapshot(|snap| {
        assert_eq!(snap.adjust_unified_plex(char_id, 501), Some(1_500));
    });
    router.pending_notices.write().unwrap().clear();

    let queued = queue_fn(&src_ctx, &dummy_any, &xfer_msg).expect("1500 PLEX transfer succeeds");
    assert!(queued.type_url.ends_with("QueueResponse"));

    db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.user_id, dst_uid);
        assert_eq!(snap.unified_plex_balance_for_user(src_uid), 500);
        assert!(snap
            .kv(eve_db::Ns::CharTransferLastUnix)
            .get::<u64>(char_id, "")
            .is_some());
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "character_transfer"));
    });
    let notices = router.pending_notices.read().unwrap();
    assert!(notices.iter().any(|(scope, any)| {
        *scope == crate::quasar_gateway::NoticeScope::User(dst_uid as i32)
            && any
                .type_url
                .ends_with("eve.user.character.transfer.api.Completed")
    }));
}
