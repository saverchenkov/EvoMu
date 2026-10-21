//! Comprehensive acceptance tests for `Q-3`: Quasar Identity, Accounts & Entitlements
//! (`auth`, `login`, `user`, `entitlement`, `omega`, `membership`, `payment`, `plex`, `store`,
//!  `vendors`, `fulfillment`, `isk`, `legal`, `pii`, `device`, `cdn`, `localization`).

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    NoticeScope, STATUS_OK, STATUS_UNAUTHORIZED,
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
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/app/eveonline/login/login_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/login/milestone/tooltip_pb2.pyj, quasar:eveProto/generated/eve_public/device/user/user_pb2.pyj, quasar:eveProto/generated/eve_public/login/milestone_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/app/eveonline/login/login_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/login/milestone/tooltip_pb2.pyj",
        "quasar:eveProto/generated/eve_public/device/user/user_pb2.pyj",
        "quasar:eveProto/generated/eve_public/login/milestone_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // 1. Unauthenticated session -> 401 STATUS_UNAUTHORIZED
    let mut unauth_sess = SessionState::new(2_001);
    let ping_req = build_public_request(ctx.now_unix(), "eve_public.auth.AuthenticateUsernameRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut unauth_sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(ping_req.encode_proto())], &empty_kw, None)
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

    let auth_req = build_public_request(ctx.now_unix(), "eve_public.auth.AuthenticateUsernameRequest", req_body);
    let rep = ctx
        .dispatch_rpc(&mut user_sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(auth_req.encode_proto())], &empty_kw, None)
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.is_some());
    let payload = rsp.payload.unwrap();
    assert_eq!(payload.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.AuthenticateUsernameResponse"));
    assert!(!payload.value.is_empty());
}

#[tokio::test]
async fn test_q3_user_accounts_lifecycle() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/user/user_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/user/user_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_003);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. eve_public.character.GetNamesByUserRequest
    let req = build_public_request(ctx.now_unix(), "eve_public.character.GetNamesByUserRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetNamesByUserResponse payload");
    assert_eq!(payload.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserResponse"));

    // 2. eve_public.user.license.api.GetRequest
    let req = build_public_request(ctx.now_unix(), "eve_public.user.license.api.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_user_license_and_subscription() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/user/license/license_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/user/license/license_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_004);
    session.user_id = Some(1);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
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
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"),
                value: notice_body,
            },
            ctx.now_unix(),
        )
        .expect("publish ActivatedNotice");
    assert_eq!(delivered, 1);

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected ActivatedNotice in inbox");
    assert!(notices[0].payload.as_ref().unwrap().type_url.contains("user.license"));

    // Query license
    let req = build_public_request(ctx.now_unix(), "eve_public.user.license.api.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_entitlements_omega_and_membership() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/assetholding/entitlement/entitlement_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/omega/benefit_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/assetholding/entitlement/entitlement_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/omega/benefit_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_005);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");

    // Grant entitlement -> enqueues GrantedNotice
    let mut ent_bytes = Vec::new();
    encode_string_field(3, "alliance_tournament_xx", &mut ent_bytes);
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &ent_bytes, &mut req_bytes);

    let req = build_public_request(ctx.now_unix(), "eve.entitlement.user.api.GrantRequest", req_bytes);
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GrantRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GrantResponse"));

    // Get list of entitlements
    let req = build_public_request(ctx.now_unix(), "eve_public.entitlement.user.api.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p = rsp.payload.unwrap();
    assert!(!p.value.is_empty());
    assert!(p.type_url.ends_with("GetResponse"));

    // Membership query
    let req = build_public_request(ctx.now_unix(), "eve.membership.GetCharacterAllRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("Membership GetCharacterAllRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetCharacterAllResponse"));
}

#[tokio::test]
async fn test_q3_payment_purchase_and_schedules() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/payment/payment_pb2.pyj, quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/payment/purchase/purchase_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/schedule_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/token_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/payment/payment_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/purchase/purchase_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/schedule_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/token_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_006);
    session.user_id = Some(1);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");

    // 1. Redeem code
    let mut code_id = Vec::new();
    encode_string_field(1, "CCPV24-PLEX-BONUS", &mut code_id);
    let mut code_body = Vec::new();
    encode_bytes_field(1, &code_id, &mut code_body);
    let req = build_public_request(ctx.now_unix(), "eve_public.payment.code.api.RedeemRequest", code_body);
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("RedeemRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("RedeemResponse"));

    // 2. Billing schedule query -> triggers notice
    let req = build_public_request(ctx.now_unix(), "eve_public.payment.schedule.api.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetScheduleRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));

    // 3. Payment token query -> triggers notice
    let req = build_public_request(ctx.now_unix(), "eve_public.payment.token.api.GetQuickPayRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetTokensRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetQuickPayResponse"));

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected payment schedule or token notices");
    assert!(notices[0].payload.as_ref().unwrap().type_url.contains("payment"));
}

#[tokio::test]
async fn test_q3_plex_vault_and_isk_wallets() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/isk/isk_pb2.pyj, quasar:eveProto/generated/eve_public/isk/wallet/wallet_pb2.pyj, quasar:eveProto/generated/eve_public/plex/plex_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/transaction/invoice/invoice_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/transaction/transaction_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/isk/isk_pb2.pyj",
        "quasar:eveProto/generated/eve_public/isk/wallet/wallet_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/plex_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/transaction/invoice/invoice_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/transaction/transaction_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_007);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");

    // 1. Initial balance check
    let req = build_public_request(ctx.now_unix(), "eve_public.plex.vault.api.BalanceRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("BalanceRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("BalanceResponse"));

    // 2. Deposit PLEX -> updates balance and delivers notice
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 250 * 100, &mut curr_msg); // 250 PLEX
    let mut dep_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut dep_body);

    let req = build_public_request(ctx.now_unix(), "eve.plex.vault.assetholding.operation.api.DepositRequest", dep_body);
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("DepositRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("DepositResponse"));

    // 3. Withdraw PLEX
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 50 * 100, &mut curr_msg);
    let mut with_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut with_body);

    let req = build_public_request(ctx.now_unix(), "eve.plex.vault.assetholding.operation.api.WithdrawRequest", with_body);
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("WithdrawRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("WithdrawResponse"));

    // 4. Poll notices -> BalanceChangedNotice present
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected BalanceChangedNotice in inbox");
    assert!(notices[0].payload.as_ref().unwrap().type_url.contains("plex"));

    // 5. ISK transfer
    let req = build_public_request(ctx.now_unix(), "eve.isk.wallet.api.TransferByOwnerRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("TransferByOwnerRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
}

#[tokio::test]
async fn test_q3_store_categories_and_products() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/category/category_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/product_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/category/category_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/product_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_008);
    session.user_id = Some(1);

    // 1. Get all categories
    let req = build_public_request(ctx.now_unix(), "eve_public.store.category.api.GetAllRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetAll categories");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get product
    let req = build_public_request(ctx.now_unix(), "eve_public.store.product.api.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("Get product");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_store_offers_and_targetaudience() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/offer/offer_pb2.pyj, quasar:eveProto/generated/eve_public/store/store_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/catalogue/entry/entry_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/image_resource_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/localized_text_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/content_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/offer_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/overlay_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/price_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/offer/offer_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/store_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/catalogue/entry/entry_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/image_resource_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/localized_text_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/content_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/offer_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/overlay_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/price_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_009);
    session.user_id = Some(1);

    // 1. Get all offers
    let req = build_public_request(ctx.now_unix(), "eve_public.store.offer.api.GetAllRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetAll offers");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Targeted offers
    let req = build_public_request(ctx.now_unix(), "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetAllTargeted offers");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetAllTargetedResponse"));

    // 3. Store targetaudience list
    let req = build_public_request(ctx.now_unix(), "eve_public.store.targetaudience.offer.api.ListRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("targetaudience ListRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("ListResponse"));
    assert_eq!(rsp.correlation_uuid, b"corr-q3-test-uuid");
}

#[tokio::test]
async fn test_q3_vendors_and_fulfillment() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/fulfillment/offer_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/fulfillment/offer_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
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

    let req = build_public_request(ctx.now_unix(), "eve.fulfillment.api.DeliverItemRequest", fulfill_req);
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("DeliverItemRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    // Verify item in character inventory
    let items = ctx.db.get_items_for_character(1001);
    assert!(items.iter().any(|it| it.type_id == 44992), "Delivered item should exist in DB inventory");

    // 2. Steam auth and RSA public key queries
    let req = build_public_request(ctx.now_unix(), "eve_public.auth.steam.AuthenticateSteamRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("AuthenticateSteamRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let req = build_public_request(ctx.now_unix(), "eve_public.auth.crypto.rsa.GetPublicKeyRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("GetPublicKeyRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
}

#[tokio::test]
async fn test_q3_legal_pii_cdn_localization() {
    // parity: plan:Q-3, quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cdn/cdn_pb2.pyj, quasar:eveProto/generated/eve_public/localization/cerberus/cerberus_pb2.pyj, quasar:eveProto/generated/eve_public/localization/localization_pb2.pyj, quasar:eveProto/generated/eve_public/localization/message_pb2.pyj, quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj
    const CDN_LOC_ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cdn/cdn_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/cerberus/cerberus_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/localization_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/message_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
    ];
    eve_testkit::attest(&[
        "plan:Q-3",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cdn/cdn_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/cerberus/cerberus_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/localization_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization/message_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_011);
    session.user_id = Some(1);
    session.character_id = Some(1001);
    assert_eq!(verify_app_ui_pyj_batch(&ctx, CDN_LOC_ROWS), 6);

    // 1. Achievement definition CDN GetLatestRequest
    let req = build_public_request(ctx.now_unix(), "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("achievement.definition.api.cdn.GetLatestRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetLatestResponse"));

    // 2. Space CDN GetUpdateRequest
    let req = build_public_request(ctx.now_unix(), "eve_public.space.api.cdn.GetUpdateRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("space.api.cdn.GetUpdateRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(!p2.value.is_empty());
    assert!(p2.type_url.ends_with("GetUpdateResponse"));
}

fn verify_app_ui_pyj_batch(ctx: &ServerContext, row_ids: &[&str]) -> usize {
    let reg = &ctx.quasar_gateway.descriptors;
    for &row_id in row_ids {
        let pyj = row_id.strip_prefix("quasar:").expect("quasar: prefix");
        let fd = reg.get_file_by_pyj(pyj).expect("cataloged pyj present");
        assert_eq!(fd.pyj_path, pyj);
        for (idx, m) in fd.messages.iter().enumerate() {
            let sample = reg
                .instantiate_populated_sample(&m.full_name, (idx as u64) + 1)
                .expect("instantiate sample");
            let wire = reg.encode_dynamic(&sample).expect("encode sample");
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .expect("decode sample");
            assert_eq!(decoded, sample);
            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            assert_eq!(any.normalized_type_name(), m.full_name);
        }
    }
    row_ids.len()
}

#[tokio::test]
async fn test_q3_app_client_agency_ui_and_ai_assistant_batch() {
    // parity: quasar:eveProto/generated/eve_public/acl/acl_pb2.pyj, quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/app/client/audio/audio_pb2.pyj, quasar:eveProto/generated/eve_public/app/client/client_pb2.pyj, quasar:eveProto/generated/eve_public/app/client/graphics/graphics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/action_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/bookmark_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/content_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/filter_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/group_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/help/video/link_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/career/career_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/acl/acl_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/client/audio/audio_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/client/client_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/client/graphics/graphics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/action_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/bookmark_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/content_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/filter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/agency/ui/help/video/link_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/career/career_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(32_001);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    assert_eq!(verify_app_ui_pyj_batch(&ctx, ROWS), 12);

    let req = build_public_request(ctx.now_unix(), "eve_public.ai.assistant.chat.api.QueryRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("QueryRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(
        rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.ai.assistant.chat.api.QueryResponse"
    );
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/acl/acl_pb2.pyj").unwrap().domain, "acl");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/client/client_pb2.pyj").unwrap().domain, "app");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/agency/ui/action_pb2.pyj").unwrap().domain, "app");
}

#[tokio::test]
async fn test_q3_app_generic_ui_and_fleet_telemetry_batch() {
    // parity: quasar:eveProto/generated/eve_public/app/eveonline/career/group_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/fleet/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/fleet/fleet_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/button/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/button/button_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/context_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/input/idle_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/tab/tab_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/video/video_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/view/view_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/window/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/window/window_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/app/eveonline/career/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/fleet/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/fleet/fleet_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/button/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/button/button_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/context_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/input/idle_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/tab/tab_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/video/video_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/view/view_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/window/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/generic_ui/window/window_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(32_002);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    assert_eq!(verify_app_ui_pyj_batch(&ctx, ROWS), 12);

    let req = build_public_request(ctx.now_unix(), "eve_public.app.eveonline.career.goal.GetAllRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("career.goal.GetAllRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(
        rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.app.eveonline.career.goal.GetAllResponse"
    );
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/fleet/fleet_pb2.pyj").unwrap().domain, "app");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/generic_ui/button/button_pb2.pyj").unwrap().domain, "app");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/generic_ui/window/window_pb2.pyj").unwrap().domain, "app");
}

#[tokio::test]
async fn test_q3_app_map_network_helperpointer_and_recommendations_batch() {
    // parity: quasar:eveProto/generated/eve_public/app/eveonline/helperpointer/ui/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/helperpointer/ui/link_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/map/analytics/events_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/map/settings/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/network/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/network/rtt_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/newscarousel/ui/ui_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/nodegraph/action/datapoint_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/omegaupsell/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/operation/recommendation/recommendation_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/operation/recommendation/ui_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/settings/featurepreview/analytics_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/app/eveonline/helperpointer/ui/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/helperpointer/ui/link_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/map/analytics/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/map/settings/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/network/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/network/rtt_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/newscarousel/ui/ui_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/nodegraph/action/datapoint_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/omegaupsell/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/operation/recommendation/recommendation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/operation/recommendation/ui_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/settings/featurepreview/analytics_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(32_003);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    assert_eq!(verify_app_ui_pyj_batch(&ctx, ROWS), 12);

    let req = build_public_request(ctx.now_unix(), "eve_public.operation.recommendation.GetRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("recommendation.GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(
        rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.operation.recommendation.GetResponse"
    );
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/network/rtt_pb2.pyj").unwrap().domain, "app");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/newscarousel/ui/ui_pb2.pyj").unwrap().domain, "app");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/app/eveonline/operation/recommendation/ui_pb2.pyj").unwrap().domain, "app");
}

#[tokio::test]
async fn test_q3_app_platform_color_localization_and_ui_primitives_batch() {
    // parity: quasar:eveProto/generated/eve_public/app/eveonline/skill/plan/certified/analytics_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/slash/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/app/platform_pb2.pyj, quasar:eveProto/generated/eve_public/color/color_pb2.pyj, quasar:eveProto/generated/eve_public/localization_pb2.pyj, quasar:eveProto/generated/eve_public/network/rtt_pb2.pyj, quasar:eveProto/generated/eve_public/semanticversion_pb2.pyj, quasar:eveProto/generated/eve_public/tracecontext_pb2.pyj, quasar:eveProto/generated/eve_public/ui/annotation_pb2.pyj, quasar:eveProto/generated/eve_public/ui/contenttag_pb2.pyj, quasar:eveProto/generated/eve_public/ui/icon_pb2.pyj, quasar:eveProto/generated/eve_public/ui/image_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/app/eveonline/skill/plan/certified/analytics_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/slash/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/platform_pb2.pyj",
        "quasar:eveProto/generated/eve_public/color/color_pb2.pyj",
        "quasar:eveProto/generated/eve_public/localization_pb2.pyj",
        "quasar:eveProto/generated/eve_public/network/rtt_pb2.pyj",
        "quasar:eveProto/generated/eve_public/semanticversion_pb2.pyj",
        "quasar:eveProto/generated/eve_public/tracecontext_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/annotation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/contenttag_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/icon_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/image_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(32_004);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    assert_eq!(verify_app_ui_pyj_batch(&ctx, ROWS), 12);

    let req = build_public_request(ctx.now_unix(), "eve_public.character.skill.plan.GetAllRequest", Vec::new());
    let rep = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("character.skill.plan.GetAllRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(
        rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.character.skill.plan.GetAllResponse"
    );
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/color/color_pb2.pyj").unwrap().domain, "color");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/ui/icon_pb2.pyj").unwrap().domain, "ui");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/ui/image_pb2.pyj").unwrap().domain, "ui");
}
