//! Acceptance tests for Quasar Payment, PLEX Vault, Store, Vendors & Legal (Part 2)
//! over `publicGatewaySvc`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_FORBIDDEN, STATUS_NOT_IMPLEMENTED, STATUS_OK,
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

async fn dispatch_req(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> EvePublicResponse {
    let req = build_public_request(ctx.now_unix(), type_name, payload);
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn make_varint_id_request(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    let mut out = Vec::new();
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_str_id_request(s: &str) -> Vec<u8> {
    let mut b = Vec::new();
    encode_string_field(1, s, &mut b);
    let mut out = Vec::new();
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_ta_offer_request(offer_id: u32) -> Vec<u8> {
    let mut entry = Vec::new();
    encode_string_field(1, &offer_id.to_string(), &mut entry);
    let mut id_msg = Vec::new();
    encode_bytes_field(2, &entry, &mut id_msg);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_msg, &mut out);
    out
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
    let mut code_inner = Vec::new();
    encode_string_field(1, "EVE-OMEGA-30D-PROMO", &mut code_inner);
    let mut code_payload = Vec::new();
    encode_bytes_field(1, &code_inner, &mut code_payload);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.code.api.RedeemRequest",
        code_payload,
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
    assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // 2. Billing schedule query
    let uid = session.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        let schedules = vec![eve_server::services::quasar_payment_store_types::PaymentScheduleRecord {
            id: 101,
            user_id: uid,
            schedule_type: 1,
            next_payment_unix: ctx.now_unix() + 30 * 86400,
            offer_id: "1001".to_string(),
            interval_days: 30,
            created_unix: ctx.now_unix(),
        }];
        snap.service_state_blobs.insert("payment_schedules".to_string(), serde_json::to_string(&schedules).unwrap());
    });
    let mut sched_inner = Vec::new();
    encode_varint_field(1, 101, &mut sched_inner);
    let mut sched_payload = Vec::new();
    encode_bytes_field(1, &sched_inner, &mut sched_payload);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.schedule.api.GetRequest",
        sched_payload,
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

    // 3. Add payment token and query it
    let tok_rec = eve_server::services::quasar_payment_store_types::PaymentTokenRecord {
        id: 101,
        user_id: 1,
        token_str: "tok_101".into(),
        is_quick_pay: true,
        consumed: false,
    };
    eve_server::services::quasar_payment_store_types::add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        1,
        tok_rec,
    );

    // Verify token query returns token
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
        .contains("QuickPayAddedNotice"));
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
    // DATA GAP: quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj
    // (NES store catalog is live Quasar backend data not in V24 client FSD)
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut session = SessionState::new(2_008);
    session.user_id = Some(1);

    // 1. Get all categories
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.category.api.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get category with explicit ID 1
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.category.api.GetRequest", make_varint_id_request(1)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p_cat = rsp.payload.unwrap();
    assert!(!p_cat.value.is_empty());
    assert!(p_cat.type_url.ends_with("GetResponse"));

    // 3. Get product with explicit ID 101
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.product.api.GetRequest", make_varint_id_request(101)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(!p2.value.is_empty());
    assert!(p2.type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_store_offers_and_targetaudience() {
    // parity: quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    // DATA GAP: quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj (NES store catalog is live Quasar backend data not in V24 client FSD)
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut session = SessionState::new(2_009);
    session.user_id = Some(1);

    // 1. Get all offers
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get offer with explicit ID 1001
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetRequest", make_str_id_request("1001")).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p_off = rsp.payload.unwrap();
    assert!(!p_off.value.is_empty());
    assert!(p_off.type_url.ends_with("GetResponse"));

    // 3. Targeted offers
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetAllTargetedResponse"));

    // 4. Store targetaudience list
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.targetaudience.offer.api.ListRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("ListResponse"));

    // 5. Store targetaudience get with explicit ID 2001
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.targetaudience.offer.api.GetRequest", make_ta_offer_request(2001)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_vendors_and_fulfillment() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_010);
    session.user_id = Some(1);
    session.character_id = Some(1001);
    ctx.db.with_mut_snapshot(|snap| {
        let mut rec = snap.characters.get(&90_000_001).cloned().unwrap();
        rec.character_id = 1001;
        snap.characters.insert(1001, rec);
    });

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
    let payload = rsp.payload.unwrap();
    assert!(payload.type_url.ends_with("GetLatestResponse"));

    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default().expect("registry");
    let dyn_cdn = reg
        .decode_dynamic("eve_public.achievement.definition.api.cdn.GetLatestResponse", &payload.value)
        .expect("decode GetLatestResponse");
    if let Some(eve_proto::quasar::DynamicValue::Message(cp)) = dyn_cdn.get(1) {
        if let Some(eve_proto::quasar::DynamicValue::String(url)) = cp.get(1) {
            assert!(url.contains("achievements"), "URL should contain achievements, got {url}");
        } else {
            panic!("Expected url string in checkpoint");
        }
        if let Some(eve_proto::quasar::DynamicValue::Message(ver)) = cp.get(2) {
            match ver.get(3) {
                Some(eve_proto::quasar::DynamicValue::Uint32(p)) => assert_eq!(*p, 68),
                Some(eve_proto::quasar::DynamicValue::Uint64(p)) => assert_eq!(*p, 68),
                other => panic!("Expected patch 68 in version, got {other:?}"),
            }
        } else {
            panic!("Expected version in checkpoint");
        }
    } else {
        panic!("Expected checkpoint message");
    }

    // 2. Space CDN GetUpdateRequest (MapTagProvider flatbuffer checkpoint)
    let bad_req = build_public_request(
        ctx.now_unix(),
        "eve_public.space.api.cdn.GetUpdateRequest",
        Vec::new(),
    );
    let bad_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(bad_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("space.api.cdn.GetUpdateRequest empty");
    assert_eq!(extract_response_proto(&bad_rep).status_code, 400);

    let mut no_local_buf = Vec::new();
    eve_proto::quasar::encode_varint_field(2, 1, &mut no_local_buf);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.space.api.cdn.GetUpdateRequest",
        no_local_buf,
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
    assert!(p2.type_url.ends_with("GetUpdateResponse"));
    let dyn_space_cdn = reg
        .decode_dynamic("eve_public.space.api.cdn.GetUpdateResponse", &p2.value)
        .expect("decode space GetUpdateResponse");
    let expected_patch = ctx.sde.solar_systems.len() as u64;
    if let Some(eve_proto::quasar::DynamicValue::Message(cp)) = dyn_space_cdn.get(1) {
        if let Some(eve_proto::quasar::DynamicValue::String(url)) = cp.get(1) {
            assert!(url.contains("maptags/v24.1.") && url.ends_with("/universe.fb"), "unexpected url {url}");
        } else {
            panic!("Expected url in space CDN checkpoint");
        }
        if let Some(eve_proto::quasar::DynamicValue::Message(ver)) = cp.get(2) {
            match ver.get(3) {
                Some(eve_proto::quasar::DynamicValue::Uint32(p)) => assert_eq!(*p as u64, expected_patch),
                Some(eve_proto::quasar::DynamicValue::Uint64(p)) => assert_eq!(*p, expected_patch),
                other => panic!("Expected patch {expected_patch}, got {other:?}"),
            }
        } else {
            panic!("Expected version in space CDN checkpoint");
        }
        assert!(cp.get(3).is_some(), "Expected crc checksum in space CDN checkpoint");
    } else {
        panic!("Expected checkpoint in space GetUpdateResponse");
    }
}
