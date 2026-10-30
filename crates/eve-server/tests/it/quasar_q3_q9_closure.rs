//! Integration tests for remediated Quasar endpoints and store/payment/cosmetic/sovereignty services.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_achievements::AchievementManager;
use eve_server::services::quasar_payment_store_types::{
    save_store_catalog, PaymentTokenRecord, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
};
use std::collections::HashMap;
use std::sync::Arc;

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"q3-q9-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-q3-q9-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

#[derive(Debug, Clone)]
enum RawField {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn parse_raw_proto(mut buf: &[u8]) -> Vec<(u32, RawField)> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let mut tag = 0u64;
        let mut shift = 0;
        loop {
            let b = buf[0];
            buf = &buf[1..];
            tag |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let field_num = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u8;
        match wire_type {
            0 => {
                let mut val = 0u64;
                let mut s = 0;
                loop {
                    let b = buf[0];
                    buf = &buf[1..];
                    val |= ((b & 0x7f) as u64) << s;
                    if b & 0x80 == 0 {
                        break;
                    }
                    s += 7;
                }
                out.push((field_num, RawField::Varint(val)));
            }
            2 => {
                let mut len = 0u64;
                let mut s = 0;
                loop {
                    let b = buf[0];
                    buf = &buf[1..];
                    len |= ((b & 0x7f) as u64) << s;
                    if b & 0x80 == 0 {
                        break;
                    }
                    s += 7;
                }
                let len = len as usize;
                out.push((field_num, RawField::Bytes(buf[..len].to_vec())));
                buf = &buf[len..];
            }
            _ => panic!("Unexpected wire type {wire_type}"),
        }
    }
    out
}

fn decode_rsp_fields(rsp: &EvePublicResponse) -> Vec<(u32, RawField)> {
    parse_raw_proto(&rsp.payload.as_ref().expect("response payload").value)
}

fn raw_u64(fields: &[(u32, RawField)], num: u32) -> Option<u64> {
    fields.iter().find_map(|(n, f)| match f {
        RawField::Varint(v) if *n == num => Some(*v),
        _ => None,
    })
}

fn raw_str(fields: &[(u32, RawField)], num: u32) -> Option<String> {
    fields.iter().find_map(|(n, f)| match f {
        RawField::Bytes(b) if *n == num => std::str::from_utf8(b).ok().map(str::to_string),
        _ => None,
    })
}

fn raw_msg(fields: &[(u32, RawField)], num: u32) -> Option<Vec<(u32, RawField)>> {
    fields.iter().find_map(|(n, f)| match f {
        RawField::Bytes(b) if *n == num => Some(parse_raw_proto(b)),
        _ => None,
    })
}

#[tokio::test]
async fn test_q3_payment_purchase_and_store_catalog_closure() {
    // parity: quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

    // 1. Unseeded store catalog returns empty lists (no hardcoded default fixture)
    let cats_empty = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cats_empty.status_code, STATUS_OK);
    assert!(cats_empty.payload.as_ref().unwrap().value.is_empty());

    // 2. Seed catalog with one offer lacking price_cents and one offer with explicit price_cents
    let school_id = ctx.sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
    let ch = ctx.db.create_character(1, "Test Pilot", 1, 1, 11, school_id, &ctx.sde).unwrap();
    sess.character_id = Some(ch.character_id);
    ctx.db.with_mut_snapshot(|snap| {
        let catalog = StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "Omega".into(),
                description: "Clone state".into(),
            }],
            products: vec![StoreProduct {
                id: 101,
                name: "30 Days Omega".into(),
                description: "Omega time".into(),
                type_id: 3966,
                quantity: 1,
            }],
            offers: vec![
                StoreOffer {
                    id: 5001,
                    name: "PLEX-Only Offer".into(),
                    description: "No fiat price_cents configured".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 5002,
                    name: "Fiat-Priced Offer".into(),
                    description: "Explicit price_cents".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: true,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &catalog);
        let tokens = vec![PaymentTokenRecord {
            id: 77,
            user_id: 1,
            token_str: "tok_test_77".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs.insert(
            "payment_tokens".to_string(),
            serde_json::to_string(&tokens).unwrap(),
        );
    });

    // 2b. Verify store.category.api, store.product.api, store.offer.api, and store.targetaudience.offer.api
    let cats_seeded = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cats_seeded.status_code, STATUS_OK);
    let cats_msg = decode_rsp_fields(&cats_seeded);
    let cat_entry = raw_msg(&cats_msg, 1).expect("seeded category entry");
    assert_eq!(raw_u64(&cat_entry, 1), Some(1)); // sequential category id
    assert_eq!(raw_u64(&cat_entry, 2), Some(1)); // catalog = Ingame

    let mut prod_id_msg = Vec::new();
    encode_varint_field(1, 101, &mut prod_id_msg);
    let mut prod_req = Vec::new();
    encode_bytes_field(1, &prod_id_msg, &mut prod_req);
    let prod_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.product.api.GetRequest",
        prod_req,
    )
    .await;
    assert_eq!(prod_rsp.status_code, STATUS_OK);
    let prod_msg = decode_rsp_fields(&prod_rsp);
    let prod_attr = raw_msg(&prod_msg, 1).expect("product attributes");
    assert_eq!(raw_str(&prod_attr, 1).as_deref(), Some("30 Days Omega"));
    let eve_item = raw_msg(&prod_attr, 2).expect("product item");
    assert_eq!(raw_u64(&eve_item, 1), Some(3966));
    assert_eq!(raw_u64(&eve_item, 2), Some(1));

    let mut prod_miss_id_msg = Vec::new();
    encode_varint_field(1, 999_999, &mut prod_miss_id_msg);
    let mut prod_miss_req = Vec::new();
    encode_bytes_field(1, &prod_miss_id_msg, &mut prod_miss_req);
    let prod_miss = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.product.api.GetRequest",
        prod_miss_req,
    )
    .await;
    assert_eq!(prod_miss.status_code, STATUS_NOT_FOUND);

    let offers_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.offer.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(offers_all.status_code, STATUS_OK);
    let offers_all_msg = decode_rsp_fields(&offers_all);
    let off_entry = raw_msg(&offers_all_msg, 1).expect("non-targeted offer 5001");
    assert_eq!(raw_str(&off_entry, 1).as_deref(), Some("5001"));

    let mut off_id_msg = Vec::new();
    encode_string_field(1, "5002", &mut off_id_msg);
    let mut off_get_req = Vec::new();
    encode_bytes_field(1, &off_id_msg, &mut off_get_req);
    let off_get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.offer.api.GetRequest",
        off_get_req,
    )
    .await;
    assert_eq!(off_get_rsp.status_code, STATUS_OK);
    let off_get_msg = decode_rsp_fields(&off_get_rsp);
    let off_attr = raw_msg(&off_get_msg, 1).expect("offer attributes");
    assert_eq!(raw_str(&off_attr, 1).as_deref(), Some("Fiat-Priced Offer"));
    let price_msg = raw_msg(&off_attr, 12).expect("offer_price");
    assert_eq!(raw_u64(&price_msg, 1), Some(50_000));

    let offers_tgt = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.offer.api.GetAllTargetedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(offers_tgt.status_code, STATUS_OK);
    let offers_tgt_msg = decode_rsp_fields(&offers_tgt);
    let tgt_entry = raw_msg(&offers_tgt_msg, 1).expect("targeted offer 5002");
    assert_eq!(raw_str(&tgt_entry, 1).as_deref(), Some("5002"));

    let ta_list = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ta_list.status_code, STATUS_OK);
    let ta_list_msg = decode_rsp_fields(&ta_list);
    let ta_entry = raw_msg(&ta_list_msg, 1).expect("targeted offer 5002 in targetaudience");
    assert_eq!(raw_u64(&ta_entry, 1), Some(5002));

    // 3. CostRequest and TokenRequest fail closed with STATUS_NOT_IMPLEMENTED when price_cents is None
    let mut cost_no_fiat = Vec::new();
    encode_varint_field(1, 5001, &mut cost_no_fiat);
    let cost_unpriced = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.payment.purchase.api.CostRequest",
        cost_no_fiat,
    )
    .await;
    assert_eq!(cost_unpriced.status_code, STATUS_NOT_IMPLEMENTED);

    let mut unpriced_offer_id_msg = Vec::new();
    encode_string_field(1, "5001", &mut unpriced_offer_id_msg);
    let mut unpriced_order_msg = Vec::new();
    encode_bytes_field(1, &unpriced_offer_id_msg, &mut unpriced_order_msg);
    encode_varint_field(2, 1, &mut unpriced_order_msg);
    let mut tok_id_msg_unpriced = Vec::new();
    encode_varint_field(1, 77, &mut tok_id_msg_unpriced);
    let mut tok_req_unpriced = Vec::new();
    encode_bytes_field(1, &unpriced_order_msg, &mut tok_req_unpriced);
    encode_bytes_field(2, &tok_id_msg_unpriced, &mut tok_req_unpriced);
    let tok_unpriced = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.payment.purchase.api.TokenRequest",
        tok_req_unpriced,
    )
    .await;
    assert_eq!(tok_unpriced.status_code, STATUS_NOT_IMPLEMENTED);
    ctx.db.with_snapshot(|snap| {
        let tokens: Vec<PaymentTokenRecord> = serde_json::from_str(
            snap.service_state_blobs.get("payment_tokens").unwrap(),
        )
        .unwrap();
        assert!(!tokens[0].consumed, "Token must not be consumed on fail-closed TokenRequest");
    });

    // 4. CostRequest and TokenRequest succeed when price_cents is explicitly configured
    let mut cost_fiat = Vec::new();
    encode_varint_field(1, 5002, &mut cost_fiat);
    let cost_ok = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.payment.purchase.api.CostRequest",
        cost_fiat,
    )
    .await;
    assert_eq!(cost_ok.status_code, STATUS_OK);
    assert_eq!(
        cost_ok.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.payment.purchase.api.CostResponse"
    );
    let cost_msg = decode_rsp_fields(&cost_ok);
    let cost_inner = raw_msg(&cost_msg, 1).expect("CostResponse inner cost message");
    assert_eq!(raw_u64(&cost_inner, 1), Some(1_999)); // catalog_amount_in_cents
    assert_eq!(raw_u64(&cost_inner, 2), Some(0)); // tax_amount_in_cents
    assert_eq!(raw_u64(&cost_inner, 3), Some(1_999)); // total_amount_in_cents
    assert_eq!(raw_str(&cost_inner, 5).as_deref(), Some("USD")); // currency

    let mut offer_id_msg = Vec::new();
    encode_string_field(1, "5002", &mut offer_id_msg);
    let mut order_msg = Vec::new();
    encode_bytes_field(1, &offer_id_msg, &mut order_msg);
    encode_varint_field(2, 1, &mut order_msg);
    let mut tok_id_msg = Vec::new();
    encode_varint_field(1, 77, &mut tok_id_msg);
    let mut tok_req = Vec::new();
    encode_bytes_field(1, &order_msg, &mut tok_req);
    encode_bytes_field(2, &tok_id_msg, &mut tok_req);
    let tok_ok = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.payment.purchase.api.TokenRequest",
        tok_req,
    )
    .await;
    assert_eq!(tok_ok.status_code, STATUS_OK, "{}", tok_ok.status_message);

    // 5. Unknown offer ID returns STATUS_NOT_FOUND
    let mut cost_miss = Vec::new();
    encode_varint_field(1, 999_999, &mut cost_miss);
    let miss_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.payment.purchase.api.CostRequest",
        cost_miss,
    )
    .await;
    assert_eq!(miss_rsp.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
async fn test_q4_achievements_sde_and_career_goals_closure() {
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
    let mut unauth = SessionState::new(2);

    // 1. AchievementManager loads real SDE achievements with zero synthetic category/score/rewards
    let mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)));
    assert!(mgr.sde_achievement_count > 0, "SDE must contain achievements");
    for def in &mgr.definitions {
        assert_eq!(def.category_id, 0);
        assert_eq!(def.score, 0);
        assert_eq!(def.reward_isk, 0);
        assert_eq!(def.reward_sp, 0);
    }

    // 2. Achievement category, showcase, and reward endpoints
    let mut cat_id_msg = Vec::new();
    encode_bytes_field(1, &1u32.to_be_bytes(), &mut cat_id_msg);
    let mut cat_req = Vec::new();
    encode_bytes_field(1, &cat_id_msg, &mut cat_req);
    let cat_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.GetRequest",
        cat_req,
    )
    .await;
    assert_eq!(cat_rsp.status_code, STATUS_OK, "{}", cat_rsp.status_message);

    let score_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(score_rsp.status_code, STATUS_OK);

    let unauth_claim = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.achievement.reward.api.ClaimRequest",
        Vec::new(),
    )
    .await;
    assert_ne!(unauth_claim.status_code, STATUS_OK);

    // 3. Career goal endpoints
    let goals_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(goals_rsp.status_code, STATUS_OK);
    assert_eq!(
        goals_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );
}

#[tokio::test]
async fn test_q5_q6_social_economy_and_ai_assistant_fail_closed() {
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

    // 1. AI assistant fails closed with STATUS_NOT_IMPLEMENTED (no generic schema-filler)
    let mut query_payload = Vec::new();
    encode_string_field(1, "How do I fit a Drake?", &mut query_payload);
    let ai_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ai.assistant.chat.api.QueryRequest",
        query_payload,
    )
    .await;
    assert_eq!(ai_rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // 2. Space CDN map-tag checkpoint validates request and returns SDE-backed checkpoint
    let cdn_bad = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.space.api.cdn.GetUpdateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cdn_bad.status_code, STATUS_BAD_REQUEST);

    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req); // no_local_version_available = true
    let cdn_ok = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(cdn_ok.status_code, STATUS_OK);
    assert!(!cdn_ok.payload.as_ref().unwrap().value.is_empty());
}

#[tokio::test]
async fn test_q7_q8_sovereignty_planet_resources_and_mercenary_den() {
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
    let mut unauth = SessionState::new(2);

    // 1. Planet resource definitions & version
    let defs_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    assert!(!defs_rsp.payload.as_ref().unwrap().value.is_empty());

    let ver_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ver_rsp.status_code, STATUS_OK);

    // 2. Planet resource configurations for caller's SDE solar system (and unauth rejection)
    let cfgs_unauth = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cfgs_unauth.status_code, STATUS_UNAUTHORIZED);

    let cfgs_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cfgs_rsp.status_code, STATUS_OK);
    assert!(!cfgs_rsp.payload.as_ref().unwrap().value.is_empty());

    // 3. Mercenary den maximum & owned queries
    let max_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(max_rsp.status_code, STATUS_OK);
    assert_eq!(
        max_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse"
    );
}

#[tokio::test]
async fn test_q9_cosmetics_market_and_skinr_sequencing_closure() {
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

    // 1. Broker fees, tax rate, and buyer fees definition
    let broker_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(broker_rsp.status_code, STATUS_OK);

    let tax_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);

    // 2. GetBuyerFeeRequest rejects empty skin identifier with STATUS_BAD_REQUEST and unknown skin with STATUS_NOT_FOUND
    let empty_buyer = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(empty_buyer.status_code, STATUS_BAD_REQUEST);

    let mut skin_id_msg = Vec::new();
    encode_string_field(1, "nonexistent_skin_hex", &mut skin_id_msg);
    let mut buyer_req = Vec::new();
    encode_bytes_field(2, &skin_id_msg, &mut buyer_req);
    let miss_buyer = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
        buyer_req,
    )
    .await;
    assert_eq!(miss_buyer.status_code, STATUS_NOT_FOUND);

    // 3. Component & SKIN listings and SKINR sequencing job queries
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    let skin_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(skin_rsp.status_code, STATUS_OK);

    let seq_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(seq_rsp.status_code, STATUS_OK);
}
