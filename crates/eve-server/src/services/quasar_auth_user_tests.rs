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
