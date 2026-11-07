//! Live integration tests for Quasar Pirate Insurgency, Payment/Store, Sovereignty, and Space CDN.

use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::quasar_wire::{decode_fields, ProtoWireValue};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, init_insurgency_campaign, FACTION_GURISTAS,
};
use eve_server::services::quasar_payment_store_types::{
    add_user_quick_pay_token, PaymentTokenRecord,
};

use crate::common;

const PILOT_1: i64 = 91_000_001;
const FACTION_CALDARI: i64 = 500_001;
const TURNUR: u32 = 30_002_086;
const TYPE_MAGMATIC_GAS: u32 = 81_143;
const TYPE_STABILITY_GEN: u64 = 87_950;
const TYPE_SOV_HUB: i64 = 32_458;
const TYPE_SKYHOOK: i64 = 81_080;
const TYPE_MERC_DEN: i64 = 85_230;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    let prev = sess.role;
    sess.role |= service_roles::ROLE_GMH | service_roles::ROLE_GM_DEFAULT;
    let res = rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash `{cmd}` failed: {e:?}"));
    sess.role = prev;
    res
}

async fn docked_pilot(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(31);
    sess
}

async fn quasar_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
    admin: bool,
) -> EvePublicResponse {
    let prev = sess.role;
    if admin {
        sess.role |= service_roles::ROLE_GM_DEFAULT;
    }
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"quasar-live-batch-b".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-live-b".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("SendRequest");
    sess.role = prev;
    let PyRep::KeyVal(entries) = rep else { panic!("expected KeyVal, got {rep:?}"); };
    for (k, v) in entries {
        if k == "response_proto" {
            if let PyRep::Bytes(b) = v {
                return EvePublicResponse::decode_proto(&b).expect("decode EvePublicResponse");
            }
        }
    }
    panic!("missing response_proto");
}

async fn subscribe_notices(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    rpc(ctx, sess, "publicGatewaySvc", "Subscribe", &[]).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver")
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p);
        }
    }
    out
}

fn id_msg(field_num: u32, id: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn wire_field(buf: &[u8], num: u32) -> Option<Vec<u8>> {
    decode_fields(buf).ok()?.into_iter().find_map(|(n, v)| match v {
        ProtoWireValue::Bytes(b) if n == num => Some(b),
        _ => None,
    })
}

fn wire_varint(buf: &[u8], num: u32) -> Option<u64> {
    decode_fields(buf).ok()?.into_iter().find_map(|(n, v)| match v {
        ProtoWireValue::Varint(x) if n == num => Some(x),
        _ => None,
    })
}

fn wire_str(buf: &[u8], num: u32) -> Option<String> {
    wire_field(buf, num).and_then(|b| String::from_utf8(b).ok())
}

#[tokio::test]
async fn test_quasar_pirate_insurgency_live() {
    // parity: feat:quasar.pirate
    // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.pirate",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    common::pin_clock(&ctx);

    let mut pirate = common::gm_pilot_in_space(&ctx, 71_001, PILOT_1, 587, TURNUR).await;
    let mut rx = subscribe_notices(&ctx, &mut pirate.sess).await;
    let _ = drain_notices(&mut rx);

    init_insurgency_campaign(&ctx, 1, FACTION_GURISTAS, FACTION_CALDARI, TURNUR, vec![TURNUR]);
    add_system_corruption(&ctx, 1, TURNUR, 250);
    add_system_suppression(&ctx, 1, TURNUR, 210);
    let _ = drain_notices(&mut rx);

    assert_eq!(
        quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", id_msg(1, 99_999_999), false).await.status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", id_msg(1, 99_999_999), false).await.status_code,
        STATUS_NOT_FOUND
    );

    let c_info = quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", id_msg(1, u64::from(TURNUR)), false).await;
    assert_eq!(c_info.status_code, STATUS_OK);
    assert_eq!(wire_varint(&c_info.payload.unwrap().value, 4), Some(2));

    let c_th = quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.corruption.api.GetStageThresholdsRequest", Vec::new(), false).await;
    assert_eq!(c_th.status_code, STATUS_OK);
    assert!(!c_th.payload.unwrap().value.is_empty());

    let s_info = quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", id_msg(1, u64::from(TURNUR)), false).await;
    assert_eq!(s_info.status_code, STATUS_OK);
    assert_eq!(wire_varint(&s_info.payload.unwrap().value, 2), Some(2));

    let s_th = quasar_call(&ctx, &mut pirate.sess, "eve_public.pirate.suppression.api.GetStageThresholdsRequest", Vec::new(), false).await;
    assert_eq!(s_th.status_code, STATUS_OK);
    assert!(!s_th.payload.unwrap().value.is_empty());

    add_system_corruption(&ctx, 1, TURNUR, 10);
    add_system_suppression(&ctx, 1, TURNUR, 10);
    let inc_notices = drain_notices(&mut rx);
    assert!(inc_notices.iter().any(|n| n.normalized_type_name() == "eve_public.pirate.corruption.api.IncreasedNotice"));
    assert!(inc_notices.iter().any(|n| n.normalized_type_name() == "eve_public.pirate.suppression.api.IncreasedNotice"));

    add_system_corruption(&ctx, 1, TURNUR, -10);
    add_system_suppression(&ctx, 1, TURNUR, -10);
    let set_notices = drain_notices(&mut rx);
    assert!(set_notices.iter().any(|n| n.normalized_type_name() == "eve_public.pirate.corruption.api.SetNotice"));
    assert!(set_notices.iter().any(|n| n.normalized_type_name() == "eve_public.pirate.suppression.api.SetNotice"));
}

#[tokio::test]
async fn test_quasar_payment_and_store_catalog_live() {
    // parity: quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    common::pin_clock(&ctx);

    let mut sess = docked_pilot(&ctx, 72_001, PILOT_1).await;
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    let uid = sess.user_id.unwrap_or(1) as u64;
    let mut rx = subscribe_notices(&ctx, &mut sess).await;
    let _ = drain_notices(&mut rx);

    let mut cat_attr = Vec::new();
    encode_string_field(1, "Omega", &mut cat_attr);
    let mut cat_req = Vec::new();
    encode_bytes_field(1, &cat_attr, &mut cat_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve.store.admin.api.CreateCategoryRequest", cat_req, true).await.status_code, STATUS_OK);

    let mut prod_attr = Vec::new();
    encode_string_field(1, "Skill Extractor", &mut prod_attr);
    let mut eve_item = Vec::new();
    encode_bytes_field(1, &id_msg(1, 40_519)[2..], &mut eve_item);
    encode_varint_field(2, 1, &mut eve_item);
    encode_bytes_field(2, &eve_item, &mut prod_attr);
    let mut prod_req = Vec::new();
    encode_varint_field(1, 1, &mut prod_req);
    encode_bytes_field(2, &prod_attr, &mut prod_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve.store.admin.api.CreateProductRequest", prod_req, true).await.status_code, STATUS_OK);

    for (name, targeted) in [("30 Days Omega", false), ("Targeted Omega", true)] {
        let mut off_attr = Vec::new();
        encode_string_field(1, name, &mut off_attr);
        let mut fiat_p = Vec::new();
        encode_string_field(1, "USD", &mut fiat_p);
        encode_varint_field(2, 1_999, &mut fiat_p);
        encode_bytes_field(3, &fiat_p, &mut off_attr);
        let mut plex_p = Vec::new();
        encode_varint_field(1, 50_000, &mut plex_p);
        encode_bytes_field(5, &plex_p, &mut off_attr);
        if targeted {
            encode_varint_field(7, 1, &mut off_attr);
        }
        encode_string_field(16, "featured", &mut off_attr);
        encode_bytes_field(17, &id_msg(1, 1)[2..], &mut off_attr);
        let mut prod_ref = Vec::new();
        encode_bytes_field(1, &id_msg(1, 101)[2..], &mut prod_ref);
        encode_bytes_field(18, &prod_ref, &mut off_attr);
        let mut off_req = Vec::new();
        encode_bytes_field(1, &off_attr, &mut off_req);
        assert_eq!(quasar_call(&ctx, &mut sess, "eve.store.admin.api.CreateOfferRequest", off_req, true).await.status_code, STATUS_OK);
    }

    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.category.api.GetAllRequest", Vec::new(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.category.api.GetRequest", id_msg(1, 1), false).await.status_code, STATUS_OK);
    let prod_rsp = quasar_call(&ctx, &mut sess, "eve_public.store.product.api.GetRequest", id_msg(1, 101), false).await;
    assert_eq!(prod_rsp.status_code, STATUS_OK);
    assert_eq!(wire_str(&wire_field(&prod_rsp.payload.unwrap().value, 1).unwrap(), 1).as_deref(), Some("Skill Extractor"));

    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.offer.api.GetAllRequest", Vec::new(), false).await.status_code, STATUS_OK);
    let mut off_inner = Vec::new();
    encode_string_field(1, "1001", &mut off_inner);
    let mut off_get = Vec::new();
    encode_bytes_field(1, &off_inner, &mut off_get);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.offer.api.GetRequest", off_get, false).await.status_code, STATUS_OK);

    let mut page = Vec::new();
    encode_varint_field(1, 10, &mut page);
    let mut ta_list = Vec::new();
    encode_bytes_field(1, &page, &mut ta_list);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.targetaudience.offer.api.ListRequest", ta_list, false).await.status_code, STATUS_OK);

    let (mut u_ident, mut c_ident, mut ta_ident, mut ta_get) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, uid, &mut u_ident);
    encode_string_field(1, "1002", &mut c_ident);
    encode_bytes_field(1, &u_ident, &mut ta_ident);
    encode_bytes_field(2, &c_ident, &mut ta_ident);
    encode_bytes_field(1, &ta_ident, &mut ta_get);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get, false).await.status_code, STATUS_OK);

    let cost_rsp = quasar_call(&ctx, &mut sess, "eve_public.payment.purchase.api.CostRequest", id_msg(1, 1001), false).await;
    assert_eq!(cost_rsp.status_code, STATUS_OK);
    assert_eq!(wire_varint(&wire_field(&cost_rsp.payload.unwrap().value, 1).unwrap(), 1), Some(1_999));

    add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        uid,
        PaymentTokenRecord { id: 77, user_id: uid, token_str: "tok_live_77".into(), is_quick_pay: true, consumed: false },
    );
    let _ = drain_notices(&mut rx);

    let (mut order_off, mut order, mut tok_ident, mut tok_req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "VEVC-1001", &mut order_off);
    encode_bytes_field(1, &order_off, &mut order);
    encode_varint_field(2, 1, &mut order);
    encode_varint_field(1, 77, &mut tok_ident);
    encode_bytes_field(1, &order, &mut tok_req);
    encode_bytes_field(2, &tok_ident, &mut tok_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.payment.purchase.api.TokenRequest", tok_req, false).await.status_code, STATUS_OK);

    let mut by_type_req = Vec::new();
    encode_varint_field(1, 1, &mut by_type_req);
    let by_type_rsp = quasar_call(&ctx, &mut sess, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type_req, false).await;
    assert_eq!(by_type_rsp.status_code, STATUS_OK);
    let sched_id = wire_varint(&wire_field(&by_type_rsp.payload.unwrap().value, 1).unwrap(), 1).expect("schedule id");
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.payment.schedule.api.GetRequest", id_msg(1, sched_id), false).await.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_sov_hub_skyhook_mercden_and_space_cdn_live() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    common::pin_clock(&ctx);

    let (null_sys, planet) = ctx.sde.solar_systems.values()
        .filter(|s| s.security_status < 0.0 && eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .find_map(|s| {
            ctx.sde.celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.group_id == 7 && c.type_id == 11)
                .map(|p| (s.solar_system_id, p))
        })
        .expect("claimable nullsec with temperate planet");
    let planet_id = planet.celestial_id;

    let mut sess = docked_pilot(&ctx, 73_001, PILOT_1).await;
    slash(&ctx, &mut sess, "/giveskill 3363 5").await;
    slash(&ctx, &mut sess, "/giveskill 85233 5").await;
    slash(&ctx, &mut sess, "/giveisk 2000000000").await;
    let c_rep = rpc(&ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Quasar Sov Corp".into()), PyRep::String("QSOV".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let corp_id = c_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.advance_secs(31);
    rpc(&ctx, &mut sess, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(1_500_000_000.0), PyRep::String("ally".into())]).await.expect("GiveCash");
    let a_rep = rpc(&ctx, &mut sess, "corpRegistry", "CreateAlliance", &[PyRep::String("Quasar Sov Alliance".into()), PyRep::String("QALLY".into())]).await.expect("CreateAlliance");
    let ally_id = a_rep.dict_get("allianceID").and_then(|v| v.as_i64()).or_else(|| a_rep.as_i64()).expect("ally_id");
    ctx.advance_secs(31);

    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    slash(&ctx, &mut sess, &format!("/tr {null_sys}")).await;
    let ship_id = sess.ship_id.expect("ship_id");
    let mut rx = subscribe_notices(&ctx, &mut sess).await;

    slash(&ctx, &mut sess, &format!("/createitem {TYPE_SOV_HUB} 1")).await;
    let hub_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_SOV_HUB), PyRep::String("Sov Hub".into())]).await.expect("Anchor Hub");
    let hub_id = hub_rep.as_i64().or_else(|| hub_rep.dict_get("structureID").and_then(|v| v.as_i64())).expect("hub_id");
    eve_server::services::sov_services::claim_system_sovereignty(&ctx, null_sys, ally_id, corp_id, hub_id).expect("claim_system_sovereignty");

    ctx.systems.get_or_spawn(null_sys).set_ball_position(ship_id, planet.position + glam::DVec3::new(10_000.0, 0.0, 0.0)).await;
    slash(&ctx, &mut sess, &format!("/createitem {TYPE_SKYHOOK} 1")).await;
    let sh_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_SKYHOOK), PyRep::Integer(planet_id), PyRep::String("Skyhook".into())]).await.expect("Anchor Skyhook");
    let skyhook_id = sh_rep.as_i64().or_else(|| sh_rep.dict_get("structureID").and_then(|v| v.as_i64())).expect("skyhook_id");

    let skyhook_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&skyhook_id].position));
    ctx.systems.get_or_spawn(null_sys).set_ball_position(ship_id, skyhook_pos + glam::DVec3::new(1_000.0, 0.0, 0.0)).await;
    slash(&ctx, &mut sess, &format!("/createitem {TYPE_MERC_DEN} 1")).await;
    let md_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERC_DEN), PyRep::Integer(skyhook_id), PyRep::Integer(planet_id)]).await.expect("Anchor MercDen");
    let mercden_id = md_rep.as_i64().or_else(|| md_rep.dict_get("structureID").and_then(|v| v.as_i64())).expect("mercden_id");
    let _ = drain_notices(&mut rx);

    // 1. Hub Fuel
    slash(&ctx, &mut sess, &format!("/createitem {TYPE_MAGMATIC_GAS} 1000")).await;
    let gas_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ship_id && i.type_id == TYPE_MAGMATIC_GAS).unwrap().item_id);
    let mut fuel_add = id_msg(1, gas_item as u64);
    encode_varint_field(2, 500, &mut fuel_add);
    encode_bytes_field(3, &id_msg(1, hub_id as u64)[2..], &mut fuel_add);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add, false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_msg(1, hub_id as u64), false).await.status_code, STATUS_OK);

    // 2. Hub Upgrades & Notices
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Vec::new(), false).await.status_code, STATUS_OK);
    slash(&ctx, &mut sess, &format!("/createitem {TYPE_STABILITY_GEN} 1")).await;
    let upg_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ship_id && i.type_id == TYPE_STABILITY_GEN as u32).unwrap().item_id);
    let mut proc_req = id_msg(1, upg_item as u64);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &id_msg(1, TYPE_STABILITY_GEN)[2..], &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc_req);
    encode_bytes_field(3, &id_msg(1, hub_id as u64)[2..], &mut proc_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_req, false).await.status_code, STATUS_OK);
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"));
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest", id_msg(1, hub_id as u64), false).await.status_code, STATUS_OK);
    let mut upg_ident = Vec::new();
    encode_bytes_field(1, &id_msg(1, hub_id as u64)[2..], &mut upg_ident);
    encode_bytes_field(2, &id_msg(1, TYPE_STABILITY_GEN)[2..], &mut upg_ident);
    let mut get_upg = Vec::new();
    encode_bytes_field(1, &upg_ident, &mut get_upg);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.GetRequest", get_upg.clone(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", get_upg, false).await.status_code, STATUS_OK);
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice"));

    // 3. Hub Workforce & Notices
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_msg(1, hub_id as u64), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest", id_msg(1, hub_id as u64), false).await.status_code, STATUS_OK);
    let mut wf_cfg = Vec::new();
    encode_varint_field(1, 1, &mut wf_cfg);
    let mut wf_req = id_msg(1, hub_id as u64);
    encode_bytes_field(2, &wf_cfg, &mut wf_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req, false).await.status_code, STATUS_OK);
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice"));
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_msg(1, hub_id as u64), false).await.status_code, STATUS_OK);

    // 4. Mercenary Den & Planet Resources
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_msg(1, mercden_id as u64), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new(), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new(), false).await.status_code, STATUS_OK);

    // 5. Skyhook Admin, Requests & Notices
    let mut reag = Vec::new();
    encode_bytes_field(1, &id_msg(1, u64::from(TYPE_MAGMATIC_GAS))[2..], &mut reag);
    encode_varint_field(2, 1_200, &mut reag);
    let mut mod_reag = id_msg(1, skyhook_id as u64);
    encode_bytes_field(2, &reag, &mut mod_reag);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_reag, true).await.status_code, STATUS_OK);

    let now = ctx.now_unix();
    let mut win_req = id_msg(1, skyhook_id as u64);
    encode_bytes_field(2, &id_msg(1, now + 3_600)[2..], &mut win_req);
    encode_bytes_field(3, &id_msg(1, now + 7_200)[2..], &mut win_req);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", win_req, true).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.GetRequest", id_msg(1, skyhook_id as u64), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest", id_msg(1, corp_id as u64), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_msg(1, skyhook_id as u64), false).await.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_msg(1, skyhook_id as u64), false).await.status_code, STATUS_OK);
    let sh_notices = drain_notices(&mut rx);
    assert!(sh_notices.iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice"));
    assert!(sh_notices.iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice"));
    assert!(sh_notices.iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.skyhook.api.ActivationNotice"));

    // 6. Space CDN
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let cdn_rsp = quasar_call(&ctx, &mut sess, "eve_public.space.api.cdn.GetUpdateRequest", cdn_req, false).await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    assert_eq!(cdn_rsp.payload.unwrap().normalized_type_name(), "eve_public.space.api.cdn.GetUpdateResponse");
}
