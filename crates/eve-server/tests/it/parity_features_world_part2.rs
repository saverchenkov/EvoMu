//! Verified runtime integration tests for world parity features (Part 2: Quasar Public Gateway Domains).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    decode_zigzag64, encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::quasar_wire::{decode_fields, ProtoWireValue};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
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

async fn send_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_val: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"),
            value: payload_val,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-world-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
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
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_quasar_public_gateway_domains_part_a() {
    // parity: feat:quasar.app
    // parity: feat:quasar.campaign
    // parity: feat:quasar.character
    // parity: feat:quasar.space
    eve_testkit::attest(&[
        "feat:quasar.app",
        "feat:quasar.campaign",
        "feat:quasar.character",
        "feat:quasar.space",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    sess.character_id = Some(90_000_001);
    sess.user_id = Some(1);

    // 1. quasar.app (event routed via publicGatewaySvc.SendEvent)
    let mut app_payload = Vec::new();
    encode_varint_field(1, 101, &mut app_payload);
    let app_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        correlation_uuid: b"q-app-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.activitytracker.ui.node.Selected"),
            value: app_payload,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-world-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let app_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[PyRep::Bytes(app_req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("publicGatewaySvc.SendEvent quasar.app");
    assert_eq!(app_rep, PyRep::Bool(true));

    // 3. quasar.campaign
    let camp_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.campaign.api.GetAllRequest",
        Vec::new(),
        b"q-camp-01",
    )
    .await;
    assert_eq!(camp_rsp.status_code, STATUS_OK, "quasar.campaign: {}", camp_rsp.status_message);
    let camp_any = camp_rsp.payload.as_ref().expect("campaign payload");
    assert_eq!(
        camp_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.GetAllResponse")
    );
    let camp_fields = decode_fields(&camp_any.value).expect("decode campaign GetAllResponse");
    assert!(camp_fields.iter().all(|(f, _)| *f == 1));

    // 4. quasar.character
    let mut char_payload = Vec::new();
    let mut user_id_msg = Vec::new();
    encode_varint_field(1, 1, &mut user_id_msg);
    encode_bytes_field(1, &user_id_msg, &mut char_payload);
    let char_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.character.GetNamesByUserRequest",
        char_payload,
        b"q-char-01",
    )
    .await;
    assert_eq!(char_rsp.status_code, STATUS_OK, "quasar.character: {}", char_rsp.status_message);
    let char_any = char_rsp.payload.as_ref().expect("character payload");
    assert_eq!(
        char_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserResponse")
    );
    let char_fields = decode_fields(&char_any.value).expect("decode GetNamesByUserResponse");
    let first_char_bytes = char_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("character entry");
    let first_char_entry = decode_fields(first_char_bytes).expect("decode character entry");
    let char_name = first_char_entry
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (2, ProtoWireValue::Bytes(b)) => std::str::from_utf8(b).ok().map(str::to_string),
            _ => None,
        })
        .expect("character name");
    assert!(!char_name.is_empty(), "character name must be non-empty");

    // 5. quasar.space (GetUpdateRequest with no_local_version_available = true)
    let mut space_payload = Vec::new();
    encode_varint_field(2, 1, &mut space_payload);
    let space_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.space.api.cdn.GetUpdateRequest",
        space_payload,
        b"q-space-01",
    )
    .await;
    assert_eq!(space_rsp.status_code, STATUS_OK, "quasar.space: {}", space_rsp.status_message);
    let space_any = space_rsp.payload.as_ref().expect("space payload");
    assert_eq!(
        space_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.space.api.cdn.GetUpdateResponse")
    );
    let space_fields = decode_fields(&space_any.value).expect("decode cdn GetUpdateResponse");
    let cp_bytes = space_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("cdn update checkpoint");
    let cp_fields = decode_fields(cp_bytes).expect("decode checkpoint");
    let cdn_url = cp_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => std::str::from_utf8(b).ok().map(str::to_string),
            _ => None,
        })
        .expect("cdn url");
    assert!(cdn_url.starts_with("http://127.0.0.1:26004/cdn/maptags/v24.1."));
}

#[tokio::test]
async fn test_quasar_public_gateway_domains_part_b() {
    // parity: feat:quasar.payment
    // parity: feat:quasar.plex
    // parity: feat:quasar.store
    // parity: feat:quasar.user
    eve_testkit::attest(&[
        "feat:quasar.payment",
        "feat:quasar.plex",
        "feat:quasar.store",
        "feat:quasar.user",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);
    let mut sess = SessionState::new(40_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    sess.character_id = Some(90_000_001);
    sess.user_id = Some(1);

    // 1. quasar.plex
    let plex_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.plex.vault.api.BalanceRequest",
        Vec::new(),
        b"q-plex-01",
    )
    .await;
    assert_eq!(plex_rsp.status_code, STATUS_OK, "quasar.plex: {}", plex_rsp.status_message);
    let plex_any = plex_rsp.payload.as_ref().expect("plex payload");
    assert_eq!(
        plex_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceResponse")
    );
    let plex_fields = decode_fields(&plex_any.value).expect("decode BalanceResponse");
    let curr_bytes = plex_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("plex currency message");
    let curr_fields = decode_fields(curr_bytes).expect("decode Currency");
    let total_cents = curr_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Varint(raw)) => Some(decode_zigzag64(*raw)),
            _ => None,
        })
        .unwrap_or(0);
    assert_eq!(total_cents, 50_000);

    // 2. quasar.store
    let store_cat_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
        b"q-store-01",
    )
    .await;
    assert_eq!(store_cat_rsp.status_code, STATUS_OK, "quasar.store: {}", store_cat_rsp.status_message);
    let cat_any = store_cat_rsp.payload.as_ref().expect("store category payload");
    assert_eq!(
        cat_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse")
    );
    let cat_fields = decode_fields(&cat_any.value).expect("decode category GetAllResponse");
    assert!(!cat_fields.is_empty(), "expected seeded NES store categories");

    let store_offer_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.store.offer.api.GetAllRequest",
        Vec::new(),
        b"q-store-02",
    )
    .await;
    assert_eq!(store_offer_rsp.status_code, STATUS_OK, "quasar.store offers: {}", store_offer_rsp.status_message);
    let off_any = store_offer_rsp.payload.as_ref().expect("store offer payload");
    assert_eq!(
        off_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.offer.api.GetAllResponse")
    );
    let off_fields = decode_fields(&off_any.value).expect("decode offer GetAllResponse");
    assert!(!off_fields.is_empty(), "expected seeded NES store offers");

    // 3. quasar.user
    let user_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.user.license.api.GetRequest",
        Vec::new(),
        b"q-user-01",
    )
    .await;
    assert_eq!(user_rsp.status_code, STATUS_OK, "quasar.user: {}", user_rsp.status_message);
    let user_any = user_rsp.payload.as_ref().expect("user license payload");
    assert_eq!(
        user_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.GetResponse")
    );
    let user_fields = decode_fields(&user_any.value).expect("decode user license GetResponse");
    let lic_bytes = user_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("license attributes");
    let lic_fields = decode_fields(lic_bytes).expect("decode license attributes");
    let no_expiry = lic_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Varint(raw)) => Some(*raw),
            _ => None,
        })
        .expect("no_expiry_date");
    assert_eq!(no_expiry, 0);

    // 4. quasar.payment
    let pay_rsp = send_quasar(
        &ctx,
        &mut sess,
        "eve_public.payment.token.api.GetQuickPayRequest",
        Vec::new(),
        b"q-pay-01",
    )
    .await;
    assert_eq!(pay_rsp.status_code, STATUS_OK, "quasar.payment: {}", pay_rsp.status_message);
    let pay_any = pay_rsp.payload.as_ref().expect("payment payload");
    assert_eq!(
        pay_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.GetQuickPayResponse")
    );
    let pay_fields = decode_fields(&pay_any.value).expect("decode GetQuickPayResponse");
    assert_eq!(pay_fields.len(), 0);
}
