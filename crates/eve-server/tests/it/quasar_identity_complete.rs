//! Acceptance tests for Quasar Identity, Accounts, Entitlements, Payment, PLEX Vault & Store
//! over `publicGatewaySvc`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    NoticeScope, STATUS_FORBIDDEN, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn build_public_request(now_unix: u64, type_name: &str, payload_value: Vec<u8>) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-q3-test-uuid".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_value,
        }),
        external_origin: "eve-v24-client".into(),
        application_instance_uuid: b"app-q3-test-inst".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

#[tokio::test]
async fn test_q3_auth_login_device() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();

    // 1. Unauthenticated session -> 401 STATUS_UNAUTHORIZED
    let mut unauth_sess = SessionState::new(2_001);
    let ping_req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.AuthenticateUsernameRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ping_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_UNAUTHORIZED);
    assert_eq!(rsp.correlation_uuid, b"corr-q3-test-uuid");

    // 2. Authenticated user session -> AuthenticateUsernameRequest succeeds
    let mut user_sess = SessionState::new(2_002);
    user_sess.user_id = Some(1);

    let mut creds = Vec::new();
    encode_string_field(1, "test_pilot", &mut creds);
    let mut req_body = Vec::new();
    encode_bytes_field(1, &creds, &mut req_body);

    let auth_req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.AuthenticateUsernameRequest",
        req_body,
    );
    let rep = ctx
        .dispatch_rpc(
            &mut user_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(auth_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("AuthenticateUsernameResponse payload");
    assert_eq!(
        payload.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.AuthenticateUsernameResponse")
    );
    assert!(!payload.value.is_empty());
}

#[tokio::test]
async fn test_q3_user_accounts_lifecycle() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_003);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. eve_public.character.GetNamesByUserRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.character.GetNamesByUserRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetNamesByUserResponse payload");
    assert_eq!(
        payload.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserResponse")
    );

    // 2. eve_public.user.license.api.GetRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.user.license.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_user_license_and_subscription() {
    // parity: quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_004);
    session.user_id = Some(1);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("Subscribe");

    // Publish ActivatedNotice and poll notices
    let mut notice_body = Vec::new();
    encode_varint_field(1, 1, &mut notice_body);
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Broadcast,
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"
                ),
                value: notice_body,
            },
            ctx.now_unix(),
        )
        .expect("publish ActivatedNotice");
    assert_eq!(delivered, 1);

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected ActivatedNotice in inbox");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("user.license"));

    // Query license
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.user.license.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetResponse payload");
    assert!(payload.type_url.ends_with("GetResponse"));
    assert!(!payload.value.is_empty());
}

#[tokio::test]
async fn test_q3_entitlements_omega_and_membership() {
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_005);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. Grant entitlement -> enqueues GrantedNotice
    let mut ent_bytes = Vec::new();
    encode_string_field(3, "alliance_tournament_xx", &mut ent_bytes);
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &ent_bytes, &mut req_bytes);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.entitlement.user.api.GrantRequest",
        req_bytes.clone(),
    );
    let rep_unauth = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GrantRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // With GM role, GrantRequest succeeds
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GrantRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
    session.role = 0;

    // 2. Get list of user & character entitlements
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.user.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-q3-test-uuid");
    let ent_payload = rsp.payload.unwrap();
    assert_eq!(
        ent_payload.normalized_type_name(),
        "eve_public.entitlement.user.api.GetResponse"
    );
    assert!(!ent_payload.value.is_empty());

    let char_ent_req = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.character.GetAllRequest",
        Vec::new(),
    );
    let char_ent_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(char_ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("character.GetAllRequest");
    let char_ent_rsp = extract_response_proto(&char_ent_rep);
    assert_eq!(char_ent_rsp.status_code, STATUS_OK);
    assert_eq!(
        char_ent_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.entitlement.character.GetAllResponse"
    );

    // 4. Admin Grant/Revoke on ship alliancelogo and corplogo (non-GM forbidden, GM allowed)
    let unpriv = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(unpriv.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("unpriv admin GrantRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_FORBIDDEN);

    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for admin_req in [
        "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
        "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
        "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
        "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
    ] {
        let req = build_public_request(ctx.now_unix(), admin_req, Vec::new());
        let rep = ctx
            .dispatch_rpc(
                &mut session,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect(admin_req);
        assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
    }
}

#[tokio::test]
async fn test_q3_payment_purchase_and_schedules() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_006);
    session.user_id = Some(1);

    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("Subscribe");

    // 1. Payment code redeem
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.code.api.RedeemRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("RedeemRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(
        rsp.payload.unwrap().normalized_type_name(),
        "eve_public.payment.code.api.RedeemResponse"
    );

    // 2. Billing schedule query -> triggers schedule notice
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.schedule.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetScheduleRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));

    // 3. Payment token query -> triggers token notice
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.token.api.GetQuickPayRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetQuickPayRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetQuickPayResponse"));

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected payment schedule or token notices");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("payment"));
}

#[tokio::test]
async fn test_q3_plex_vault_and_isk_wallets() {
    // parity: quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_007);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("Subscribe");

    // 1. Initial balance check
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.plex.vault.api.BalanceRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("BalanceRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("BalanceResponse"));

    // 2. Deposit PLEX -> unprivileged must fail with 403, GM succeeds
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 250 * 100, &mut curr_msg);
    let mut dep_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut dep_body);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        dep_body.clone(),
    );
    let rep_unauth = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("DepositRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // Give GM role to session
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("DepositRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    // 3. Withdraw PLEX
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 50 * 100, &mut curr_msg);
    let mut with_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut with_body);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        with_body,
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("WithdrawRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    // 4. Poll notices -> BalanceChangedNotice present
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected BalanceChangedNotice in inbox");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("plex"));
}

#[tokio::test]
async fn test_q3_store_categories_and_products() {
    // parity: quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_008);
    session.user_id = Some(1);

    // 1. Get all categories
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetAll categories");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get product
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.store.product.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("Get product");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(!p2.value.is_empty());
    assert!(p2.type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_store_offers_and_targetaudience() {
    // parity: quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_009);
    session.user_id = Some(1);

    // 1. Get all offers
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.store.offer.api.GetAllRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetAll offers");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Targeted offers
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.store.offer.api.GetAllTargetedRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetAllTargeted offers");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetAllTargetedResponse"));

    // 3. Store targetaudience list
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("targetaudience ListRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("ListResponse"));
}

#[tokio::test]
async fn test_q3_vendors_and_fulfillment() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_010);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. Deliver item via fulfillment
    let mut item_type = Vec::new();
    encode_varint_field(1, 44992, &mut item_type);
    let mut fulfill_req = Vec::new();
    encode_bytes_field(2, &item_type, &mut fulfill_req);
    encode_varint_field(3, 10, &mut fulfill_req);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.fulfillment.api.DeliverItemRequest",
        fulfill_req.clone(),
    );
    let rep_unauth = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("DeliverItemRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // With GM role, DeliverItemRequest succeeds
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("DeliverItemRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let items = ctx.db.get_items_for_character(1001);
    assert!(
        items.iter().any(|it| it.type_id == 44992),
        "Delivered item should exist in DB inventory"
    );

    // 2. Steam auth and RSA public key queries
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.steam.AuthenticateSteamRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("AuthenticateSteamRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.crypto.rsa.GetPublicKeyRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetPublicKeyRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
}

#[tokio::test]
async fn test_q3_legal_pii_cdn_localization() {
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj, quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_011);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. Achievement definition CDN GetLatestRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("achievement.definition.api.cdn.GetLatestRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetLatestResponse"));

    // 2. Space CDN GetUpdateRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.space.api.cdn.GetUpdateRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("space.api.cdn.GetUpdateRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(!p2.value.is_empty());
    assert!(p2.type_url.ends_with("GetUpdateResponse"));
}
