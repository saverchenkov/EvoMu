use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

#[test]
fn identity_store_omega_license_and_auth_user_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    register_handlers(router, Arc::clone(db));

    let exp = activate_user_omega_license(db, &router.pending_notices, 1, 86_400, 1_000);
    assert_eq!(exp, 87_400);
    assert_eq!(
        read_identity_store(db, |st| st.omega_subscriptions.get(&1).copied()),
        Some(87_400)
    );

    with_identity_store(db, |st| {
        st.user_emails.insert(1, "pilot@evomu.local".into());
    });
    assert_eq!(
        read_identity_store(db, |st| st.user_emails.get(&1).cloned()),
        Some("pilot@evomu.local".to_string())
    );

    expire_user_omega_license(db, &router.pending_notices, 1);
    assert_eq!(
        read_identity_store(db, |st| st.omega_subscriptions.get(&1).copied()),
        Some(0)
    );

    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    )
    .with_service_roles(u64::MAX);
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    let rsa = handlers["eve_public.auth.crypto.rsa.GetPublicKeyRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(rsa.type_url.ends_with("GetPublicKeyResponse"));

    let lic = handlers["eve_public.user.license.api.GetRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(lic.type_url.ends_with("eve_public.user.license.api.GetResponse"));

    let mut acc_msg = DynamicQuasarMessage::new("Accept");
    acc_msg.fields.insert(1, DynamicValue::String("eula_v24".into()));
    let acc = handlers["eve.user.legal.agreement.api.AcceptRequest"](&admin_ctx, &dummy_any, &acc_msg).unwrap();
    assert!(acc.type_url.ends_with("AcceptResponse"));

    let latest = handlers["eve.legal.agreement.api.GetLatestRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(latest.type_url.ends_with("GetLatestResponse"));

    let mut link_msg = DynamicQuasarMessage::new("Link");
    link_msg.fields.insert(1, DynamicValue::String("steam_123".into()));
    link_msg.fields.insert(2, DynamicValue::Uint64(1));
    let _ = handlers["eve.vendors.steam.user.api.admin.LinkRequest"](&admin_ctx, &dummy_any, &link_msg).unwrap();

    let mut steam_auth = DynamicQuasarMessage::new("SteamAuth");
    steam_auth.fields.insert(1, DynamicValue::String("steam_123".into()));
    let st_resp = handlers["eve_public.auth.steam.AuthenticateSteamRequest"](&admin_ctx, &dummy_any, &steam_auth).unwrap();
    assert!(st_resp.type_url.ends_with("AuthenticateSteamResponse"));

    let mut epic_msg = DynamicQuasarMessage::new("Epic");
    epic_msg.fields.insert(1, DynamicValue::String("epic_123".into()));
    let _ = handlers["eve.vendors.epic.api.GetUserByEpicUserIDRequest"](&admin_ctx, &dummy_any, &epic_msg).unwrap();
    let _ = handlers["eve.vendors.epic.api.DeliverEntitlementRequest"](&admin_ctx, &dummy_any, &epic_msg).unwrap();
    let _ = handlers["eve.vendors.epic.api.GetEntitlementsRequest"](&admin_ctx, &dummy_any, &epic_msg).unwrap();

    let mut tw_msg = DynamicQuasarMessage::new("Twitch");
    tw_msg.fields.insert(1, DynamicValue::String("tw_123".into()));
    let _ = handlers["eve.id.federate.twitch.api.CompleteLinkingRequest"](&admin_ctx, &dummy_any, &tw_msg).unwrap();
    let _ = handlers["eve.id.federate.twitch.api.GetRequest"](&admin_ctx, &dummy_any, &tw_msg).unwrap();
    let _ = handlers["eve.vendors.twitch.entitlement.api.DropReceived"](&admin_ctx, &dummy_any, &tw_msg).unwrap();
    let _ = handlers["eve.user.character.transfer.GetPendingByUserRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
}

#[test]
fn omega_user_license_notices_and_get_request_use_v2402_license_type() {
    use eve_proto::quasar::QuasarDescriptorRegistry;

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    register_handlers(router, Arc::clone(db));

    let exp = activate_user_omega_license(db, &router.pending_notices, 1, 86_400, 1_700_000_000);
    let act_notice = router
        .pending_notices
        .write()
        .unwrap()
        .drain(..)
        .find(|(_, any)| any.type_url.ends_with("eve_public.user.license.api.ActivatedNotice"))
        .expect("ActivatedNotice queued")
        .1;
    let act_dec = reg
        .decode_dynamic(&act_notice.type_url, &act_notice.value)
        .expect("decode ActivatedNotice");
    let act_ident = match act_dec.get(1) {
        Some(DynamicValue::Message(m)) => m.as_ref(),
        other => panic!("expected license Identifier, got {other:?}"),
    };
    assert_eq!(
        act_ident.get(1),
        Some(&DynamicValue::String("eve_clonestate_omega".into()))
    );
    let act_attr = match act_dec.get(2) {
        Some(DynamicValue::Message(m)) => m.as_ref(),
        other => panic!("expected license Attributes, got {other:?}"),
    };
    let act_exp = match act_attr.get(2) {
        Some(DynamicValue::Message(m)) => m.get(1).cloned(),
        other => panic!("expected expiry_date, got {other:?}"),
    };
    assert_eq!(act_exp, Some(DynamicValue::Int64(exp as i64)));
    let act_mod = match act_attr.get(3) {
        Some(DynamicValue::Message(m)) => m.get(1).cloned(),
        other => panic!("expected last_modified, got {other:?}"),
    };
    assert_eq!(act_mod, Some(DynamicValue::Int64(1_700_000_000)));

    let auth_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![],
        1_700_000_000,
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    for req_lt in ["eve_clonestate_omega", "omega", ""] {
        let mut req_msg = DynamicQuasarMessage::new("eve_public.user.license.api.GetRequest");
        if !req_lt.is_empty() {
            let mut ident = DynamicQuasarMessage::new("eve_public.user.license.Identifier");
            ident.set(1, DynamicValue::String(req_lt.into()));
            req_msg.set(1, DynamicValue::Message(Box::new(ident)));
        }
        let ok_any = handlers["eve_public.user.license.api.GetRequest"](
            &auth_ctx,
            &dummy_any,
            &req_msg,
        )
        .unwrap();
        let ok_dec = reg
            .decode_dynamic(&ok_any.type_url, &ok_any.value)
            .expect("decode user.license GetResponse");
        let ok_attr = match ok_dec.get(1) {
            Some(DynamicValue::Message(m)) => m.as_ref(),
            other => panic!("expected license Attributes, got {other:?}"),
        };
        let lm_sec = match ok_attr.get(3) {
            Some(DynamicValue::Message(m)) => m.get(1).cloned(),
            other => panic!("expected last_modified, got {other:?}"),
        };
        assert_eq!(lm_sec, Some(DynamicValue::Int64(1_700_000_000)));
    }

    let mut bogus_req = DynamicQuasarMessage::new("eve_public.user.license.api.GetRequest");
    let mut bogus_ident = DynamicQuasarMessage::new("eve_public.user.license.Identifier");
    bogus_ident.set(1, DynamicValue::String("unknown_license".into()));
    bogus_req.set(1, DynamicValue::Message(Box::new(bogus_ident)));
    let bogus_err = handlers["eve_public.user.license.api.GetRequest"](
        &auth_ctx,
        &dummy_any,
        &bogus_req,
    )
    .unwrap_err();
    assert_eq!(bogus_err.0, crate::quasar_gateway::STATUS_NOT_FOUND);

    expire_user_omega_license(db, &router.pending_notices, 1);
    let exp_notice = router
        .pending_notices
        .write()
        .unwrap()
        .drain(..)
        .find(|(_, any)| any.type_url.ends_with("eve_public.user.license.api.ExpiredNotice"))
        .expect("ExpiredNotice queued")
        .1;
    let exp_dec = reg
        .decode_dynamic(&exp_notice.type_url, &exp_notice.value)
        .expect("decode ExpiredNotice");
    let exp_ident = match exp_dec.get(1) {
        Some(DynamicValue::Message(m)) => m.as_ref(),
        other => panic!("expected license Identifier, got {other:?}"),
    };
    assert_eq!(
        exp_ident.get(1),
        Some(&DynamicValue::String("eve_clonestate_omega".into()))
    );
}

#[test]
fn omega_user_license_get_request_last_modified_is_deterministic_across_clock_advance() {
    use eve_proto::quasar::QuasarDescriptorRegistry;

    let base_time = 1_700_000_000;
    let (ctx, _clock) = test_ctx_and_clock(base_time);
    let db = &ctx.db;
    let router = &ctx.quasar_gateway;
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    register_handlers(router, Arc::clone(db));

    let dummy_any = GoogleAny::default();
    let req_msg = DynamicQuasarMessage::new("eve_public.user.license.api.GetRequest");
    let get_bytes = |ts: u64| {
        let auth_ctx = AuthoritativeContext::for_session_at(
            Some(1),
            Some(90_000_001),
            Some(1_000_009),
            0,
            "tranquility",
            vec![],
            ts,
        );
        let handlers = router.handlers.read().unwrap();
        handlers["eve_public.user.license.api.GetRequest"](&auth_ctx, &dummy_any, &req_msg)
            .unwrap()
            .value
    };

    let _ = activate_user_omega_license(db, &router.pending_notices, 1, 86_400, base_time);
    let before_act = get_bytes(base_time);
    let after_act = get_bytes(base_time + 1_800);
    assert_eq!(before_act, after_act);

    // Direct out-of-band identity-store writes must invalidate any prior last_modified
    // timestamp once and persist the newly stamped active timestamp across clock advances.
    with_identity_store(db, |st| {
        st.omega_subscriptions.insert(1, base_time + 172_800);
    });
    assert!(read_identity_store(db, |st| {
        !st.omega_license_last_modified.contains_key(&1)
    }));
    let before_direct = get_bytes(base_time + 2_000);
    assert_ne!(before_direct, before_act);
    let after_direct = get_bytes(base_time + 3_800);
    assert_eq!(after_direct, before_direct);

    let dec = reg
        .decode_dynamic("eve_public.user.license.api.GetResponse", &after_direct)
        .expect("decode GetResponse");
    let attr = match dec.get(1) {
        Some(DynamicValue::Message(m)) => m.as_ref(),
        other => panic!("expected license Attributes, got {other:?}"),
    };
    let lm_sec = match attr.get(3) {
        Some(DynamicValue::Message(m)) => m.get(1).cloned(),
        other => panic!("expected last_modified, got {other:?}"),
    };
    assert_eq!(lm_sec, Some(DynamicValue::Int64((base_time + 2_000) as i64)));
}
