//! End-to-end integration tests for Quasar Batch 3 parity closure:
//! - Paragon Hub Cosmetic Market (SDE-seeded component listings, skill-reduced tax, SKIN listings, notices)
//! - SKINR 3rd-Party Sequencing (skill discounts, PLEX debit, natural time-elapsed & early completion, notices)
//! - Sovereignty Hub Workforce Transit Relay Network (multi-hop Transit relay, Configure/GetState/Notices)
//! - Sovereignty Orbital Skyhook (planet-derived Workforce, reagent simulation, theft vulnerability & notices)

#![forbid(unsafe_code)]

use crate::common;
use eve_db::corp_roles::ROLE_DIRECTOR;
use eve_db::{IskCents, ServerClock, TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::{BUYER_FEES_VERSION, PARAGON_CORP_ID};
use eve_server::services::quasar_sov_hub::planet_yield;
use eve_server::services::quasar_sov_workforce::systems_within_jumps;
use eve_server::services::sov_gameplay::SKYHOOK_HOURLY_REAGENT_YIELD;
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn make_sess(sid: u64, cid: u32, corp_id: u32, sys_id: u32, is_admin: bool) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(if is_admin { 1 } else { 2 });
    s.character_id = Some(cid as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(sys_id);
    s.role = if is_admin {
        eve_machonet::service_roles::ROLE_GM_DEFAULT
    } else {
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
    };
    s.corp_role = ROLE_DIRECTOR;
    s
}

async fn qrpc(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("quasar-b3-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"quasar-b3-suite1".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

async fn subscribe(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream receiver");
    while rx.try_recv().is_ok() {}
    rx
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

fn decode_dyn(reg: &QuasarDescriptorRegistry, any: &GoogleAny) -> DynamicQuasarMessage {
    reg.decode_dynamic(any.normalized_type_name(), &any.value)
        .unwrap_or_else(|e| panic!("decode {}: {e:?}", any.normalized_type_name()))
}

fn id_msg(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

fn req_with_id(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &id_msg(id), &mut b);
    b
}

fn pick_slot_and_coating(ctx: &ServerContext, ship_type_id: u32) -> (u32, u32) {
    let cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    for &slot_id in cat.allowed_slots_for_ship(ship_type_id) {
        let allowed_cats = cat.slot_allowed_categories.get(&slot_id);
        if let Some(c) = cat.components.values().find(|c| {
            !c.is_pattern()
                && !c.disallowed_slots.contains(&slot_id)
                && allowed_cats.map_or(true, |ac| ac.is_empty() || ac.contains(&c.category))
        }) {
            return (slot_id, c.component_id);
        }
    }
    panic!("no valid slot/coating for ship {ship_type_id}");
}

fn build_skin_init_req(ctx: &ServerContext, name: &str, qty: u64) -> Vec<u8> {
    let (slot_id, coat_id) = pick_slot_and_coating(ctx, 587);
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &id_msg(slot_id as u64), &mut slot_msg);
    let mut coat_cfg = Vec::new();
    encode_bytes_field(1, &id_msg(coat_id as u64), &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    encode_bytes_field(2, &id_msg(587), &mut layout);
    encode_varint_field(3, 1, &mut layout);
    let mut req = Vec::new();
    encode_string_field(1, name, &mut req);
    encode_string_field(2, "Paragon", &mut req);
    encode_bytes_field(3, &layout, &mut req);
    encode_varint_field(5, qty, &mut req);
    req
}

fn uuid_msg(uuid: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    b
}

fn req_with_uuid(uuid: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &uuid_msg(uuid), &mut b);
    b
}

fn extract_uuid_from_entry(entry: &DynamicQuasarMessage) -> Vec<u8> {
    match entry.get(1) {
        Some(DynamicValue::Message(id_m)) => match id_m.get(1) {
            Some(DynamicValue::Bytes(b)) => b.clone(),
            other => panic!("expected uuid bytes, got {other:?}"),
        },
        other => panic!("expected id message, got {other:?}"),
    }
}

#[tokio::test]
async fn test_quasar_cosmetics_market_sde_and_skill_fees() {
    // parity: quasar:eveProto/generated/eve/cosmetic/market/component/listing/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/search/search_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/cosmetic/market/component/listing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/search/search_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let _reg = &ctx.quasar_gateway.descriptors;

    let seller_cid = 90_000_001u32;
    let buyer_cid = 90_000_002u32;
    ctx.db.with_mut_snapshot(|s| {
        for cid in [seller_cid, buyer_cid] {
            if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
                ch.balance = IskCents(100_000_000 * 100);
                ch.plex_balance = 50_000;
                ch.loyalty_points.insert(PARAGON_CORP_ID as i64, 100_000);
            }
        }
    });

    let mut admin = make_sess(81_001, seller_cid, 98_000_001, 30_000_142, true);
    let mut seller = make_sess(81_002, seller_cid, 98_000_001, 30_000_142, false);
    let mut buyer = make_sess(81_003, buyer_cid, 98_000_002, 30_000_142, false);
    let mut buyer_rx = subscribe(&ctx, &mut buyer).await;

    // 1. Component listings are seeded from SDE on startup without test helpers.
    let comp_all = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(comp_all.status_code, STATUS_OK);
    let dyn_comp = decode_dyn(_reg, comp_all.payload.as_ref().unwrap());
    let first_comp_uuid = match dyn_comp.get(1) {
        Some(DynamicValue::Repeated(items)) => {
            assert_eq!(items.len(), 4, "expected 4 SDE-seeded component listings");
            match &items[0] {
                DynamicValue::Message(entry) => extract_uuid_from_entry(entry),
                other => panic!("expected listing entry, got {other:?}"),
            }
        }
        other => panic!("expected repeated component listings, got {other:?}"),
    };

    let mut buy_comp_req = Vec::new();
    encode_bytes_field(1, &uuid_msg(&first_comp_uuid), &mut buy_comp_req);
    encode_varint_field(2, 1, &mut buy_comp_req);
    let buy_comp_rsp = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest", buy_comp_req).await;
    assert_eq!(buy_comp_rsp.status_code, STATUS_OK);
    assert!(drain_notices(&mut buyer_rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"));

    // 2. Tax rate reflects Paragon Relations (84217) & Executive Paragon Partnership (84220) skills.
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut tax_req = Vec::new();
    encode_bytes_field(1, &pub_target, &mut tax_req);
    let tax_before = qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", tax_req.clone()).await;
    assert_eq!(tax_before.status_code, STATUS_OK);
    let rate_before = match decode_dyn(_reg, tax_before.payload.as_ref().unwrap()).get(1) {
        Some(DynamicValue::Message(m)) => m.get(1).cloned(),
        _ => None,
    };
    assert_eq!(rate_before, Some(DynamicValue::Sint64(20)));

    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(seller_cid as i64).or_default();
        sk.insert(84_217, TrainedSkillRecord { type_id: 84_217, level: 5, skill_points: 256_000.0 });
        sk.insert(84_220, TrainedSkillRecord { type_id: 84_220, level: 5, skill_points: 256_000.0 });
    });
    let tax_after = qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", tax_req).await;
    assert_eq!(tax_after.status_code, STATUS_OK);
    let rate_after = match decode_dyn(_reg, tax_after.payload.as_ref().unwrap()).get(1) {
        Some(DynamicValue::Message(m)) => m.get(1).cloned(),
        _ => None,
    };
    assert_eq!(rate_after, Some(DynamicValue::Sint64(10)));
    assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest", Vec::new()).await.status_code, STATUS_OK);

    // 3. Grant finite coating license, sequence a 3rd-party SKIN (and complete early), list it on Paragon Hub, and purchase it.
    let (_, coat_id) = pick_slot_and_coating(&ctx, 587);
    let mut comp_id_m = Vec::new();
    encode_bytes_field(1, &id_msg(coat_id as u64), &mut comp_id_m);
    let mut grant_coat = Vec::new();
    encode_bytes_field(1, &comp_id_m, &mut grant_coat);
    encode_varint_field(2, 5, &mut grant_coat);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_coat).await.status_code, STATUS_OK);

    let seq_req = build_skin_init_req(&ctx, "Batch3 Market SKIN", 1);
    let seq_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", seq_req).await;
    assert_eq!(seq_rsp.status_code, STATUS_OK, "seq_rsp: {}", seq_rsp.status_message);
    let dyn_seq = decode_dyn(_reg, seq_rsp.payload.as_ref().unwrap());
    let seq_job_uuid = extract_uuid_from_entry(&dyn_seq);
    let skin_hex = match dyn_seq.get(2) {
        Some(DynamicValue::Message(attr)) => match attr.get(1) {
            Some(DynamicValue::Message(sid)) => match sid.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                other => panic!("expected skin_hex string, got {other:?}"),
            },
            other => panic!("expected skin id, got {other:?}"),
        },
        other => panic!("expected job attributes, got {other:?}"),
    };
    assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", req_with_uuid(&seq_job_uuid)).await.status_code, STATUS_OK);
    let mut skin_id_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id_msg);

    let mut buyer_fee_req = Vec::new();
    encode_bytes_field(1, &pub_target, &mut buyer_fee_req);
    encode_bytes_field(2, &skin_id_msg, &mut buyer_fee_req);
    assert_eq!(qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBuyerFeeRequest", buyer_fee_req).await.status_code, STATUS_OK);

    let mut isk_msg = Vec::new();
    encode_varint_field(1, 2_000_000u64 << 1, &mut isk_msg);
    encode_varint_field(2, 0, &mut isk_msg);
    let mut dur_msg = Vec::new();
    encode_varint_field(1, 172_800, &mut dur_msg);
    let mut lic_id = Vec::new();
    encode_bytes_field(1, &id_msg(seller_cid as u64), &mut lic_id);
    encode_bytes_field(2, &skin_id_msg, &mut lic_id);
    let mut create_req = Vec::new();
    encode_bytes_field(1, &isk_msg, &mut create_req);
    encode_bytes_field(3, &lic_id, &mut create_req);
    encode_bytes_field(4, &dur_msg, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);
    encode_bytes_field(6, &pub_target, &mut create_req);
    assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", create_req).await.status_code, STATUS_OK);

    let owned_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest", Vec::new()).await;
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    let skin_listing_uuid = match decode_dyn(_reg, owned_rsp.payload.as_ref().unwrap()).get(1) {
        Some(DynamicValue::Repeated(items)) => {
            assert_eq!(items.len(), 1);
            match &items[0] {
                DynamicValue::Message(entry) => extract_uuid_from_entry(entry),
                other => panic!("expected listing entry, got {other:?}"),
            }
        }
        other => panic!("expected 1 owned listing, got {other:?}"),
    };

    let mut buy_skin_req = Vec::new();
    encode_bytes_field(1, &uuid_msg(&skin_listing_uuid), &mut buy_skin_req);
    encode_varint_field(2, 1, &mut buy_skin_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_skin_req);
    assert_eq!(qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", buy_skin_req).await.status_code, STATUS_OK);
    assert!(drain_notices(&mut buyer_rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"));
}

#[tokio::test]
async fn test_quasar_skinr_sequencing_natural_and_early_completion() {

    let ctx = ServerContext::new();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(common::TEST_CLOCK_BASE_UNIX));
    ctx.set_clock(clock.clone());
    let _reg = &ctx.quasar_gateway.descriptors;

    let cid = 90_000_001u32;
    let mut admin = make_sess(82_001, cid, 98_000_001, 30_000_142, true);
    let mut player = make_sess(82_002, cid, 98_000_001, 30_000_142, false);
    let mut rx = subscribe(&ctx, &mut player).await;

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest", Vec::new()).await.status_code, STATUS_OK);

    let (_, coat_id) = pick_slot_and_coating(&ctx, 587);
    let mut comp_id_m = Vec::new();
    encode_bytes_field(1, &id_msg(coat_id as u64), &mut comp_id_m);
    let mut grant_coat = Vec::new();
    encode_bytes_field(1, &comp_id_m, &mut grant_coat);
    encode_varint_field(2, 5, &mut grant_coat);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_coat).await.status_code, STATUS_OK);
    let _ = drain_notices(&mut rx);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.plex_balance = 10_000;
        }
    });
    let init_req = build_skin_init_req(&ctx, "Batch3 Custom SKIN", 1);
    let ok_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req.clone()).await;
    assert_eq!(ok_rsp.status_code, STATUS_OK, "ok_rsp: {}", ok_rsp.status_message);
    let job1_uuid = extract_uuid_from_entry(&decode_dyn(_reg, ok_rsp.payload.as_ref().unwrap()));
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice"));
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await.status_code, STATUS_OK);

    clock.advance_secs(86_400);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest", req_with_uuid(&job1_uuid)).await.status_code, STATUS_OK);
    let _ = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"));

    let ok_rsp2 = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req.clone()).await;
    assert_eq!(ok_rsp2.status_code, STATUS_OK);
    let job2_uuid = extract_uuid_from_entry(&decode_dyn(_reg, ok_rsp2.payload.as_ref().unwrap()));
    let _ = drain_notices(&mut rx);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", req_with_uuid(&job2_uuid)).await.status_code, STATUS_OK);
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"));

    // Exceeding finite component license capacity emits FailedNotice.
    let fail_req = build_skin_init_req(&ctx, "Batch3 Fail SKIN", 10);
    let fail_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", fail_req).await;
    assert_eq!(fail_rsp.status_code, STATUS_BAD_REQUEST);
    let _ = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    assert!(drain_notices(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.FailedNotice"));
}

#[tokio::test]
async fn test_quasar_sov_hub_workforce_transit_relay_network() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let _reg = &ctx.quasar_gateway.descriptors;

    let null_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.solar_system_id < 31_000_000
                && ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.type_id == 11)
        })
        .map(|s| s.solar_system_id)
        .take(120)
        .collect();
    let mut found = None;
    for &a in &null_systems {
        let da = systems_within_jumps(&ctx.sde, a, 6);
        for (&b, &dab) in &da {
            if dab == 0 || dab > 5 {
                continue;
            }
            let db = systems_within_jumps(&ctx.sde, b, 5);
            for (&c, &dbc) in &db {
                if dbc > 0 && c != a && da.get(&c).copied().unwrap_or(99) > 5 {
                    found = Some((a, b, c));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (sys_a, sys_b, sys_c) = found.expect("3-hop nullsec relay chain");

    let cid = 90_000_001u32;
    let corp_id = 98_000_001u32;
    let alliance_id = 99_000_001i64;
    let (hub_a, hub_b, hub_c) = (1_000_000_201i64, 1_000_000_202i64, 1_000_000_203i64);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.corporation_id = corp_id as i64;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = sys_b;
        }
        if let Some(ch2) = s.characters.get_mut(&90_000_002) {
            ch2.corporation_id = corp_id as i64;
            ch2.alliance_id = Some(alliance_id);
            ch2.solar_system_id = sys_a;
        }
        if let Some(corp) = s.corporations.get_mut(&(corp_id as i64)) {
            corp.alliance_id = Some(alliance_id);
        }
        for (hid, sys) in [(hub_a, sys_a), (hub_b, sys_b), (hub_c, sys_c)] {
            s.system_states.entry(sys).or_default().sov_alliance_id = Some(alliance_id);
            let mut st = UpwellStructureRecord::default();
            st.structure_id = hid;
            st.type_id = TYPE_SOVEREIGNTY_HUB;
            st.owner_corp_id = corp_id as i64;
            st.solar_system_id = sys;
            st.state = "shield_vulnerable".into();
            s.structures.insert(hid, st);
            s.sov_runtime.sov_hubs.entry(hid).or_default();
        }
        let temp_a = ctx.sde.celestials_in_system(sys_a).into_iter().find(|c| c.type_id == 11).unwrap();
        let mut sh_a = UpwellStructureRecord::default();
        sh_a.structure_id = 1_000_000_204;
        sh_a.type_id = TYPE_ORBITAL_SKYHOOK;
        sh_a.owner_corp_id = corp_id as i64;
        sh_a.solar_system_id = sys_a;
        sh_a.state = "shield_vulnerable".into();
        s.structures.insert(1_000_000_204, sh_a);
        s.sov_runtime.skyhook_planets.insert(1_000_000_204, temp_a.celestial_id);
        s.sov_runtime.skyhook_active.insert(1_000_000_204, true);
    });

    let mut sess = make_sess(83_001, cid, corp_id, sys_b, false);
    sess.alliance_id = Some(alliance_id);
    let mut sess_a = make_sess(83_002, 90_000_002, corp_id, sys_a, false);
    sess_a.alliance_id = Some(alliance_id);
    let mut rx_a = subscribe(&ctx, &mut sess_a).await;

    // 1. Configure hub_b -> Transit (field 4)
    let mut cfg_b = Vec::new();
    encode_varint_field(4, 1, &mut cfg_b);
    let mut req_b = Vec::new();
    encode_bytes_field(1, &id_msg(hub_b as u64), &mut req_b);
    encode_bytes_field(2, &cfg_b, &mut req_b);
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", req_b).await.status_code, STATUS_OK);

    // 2. Configure hub_a -> Export(sys_c, 500) & hub_c -> Import([sys_a])
    let mut exp_cfg = Vec::new();
    encode_bytes_field(3, &id_msg(sys_c as u64), &mut exp_cfg);
    encode_varint_field(2, 500, &mut exp_cfg);
    let mut cfg_a = Vec::new();
    encode_bytes_field(3, &exp_cfg, &mut cfg_a);
    let mut req_a = Vec::new();
    encode_bytes_field(1, &id_msg(hub_a as u64), &mut req_a);
    encode_bytes_field(2, &cfg_a, &mut req_a);
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", req_a).await.status_code, STATUS_OK);

    let mut imp_src = Vec::new();
    encode_bytes_field(1, &id_msg(sys_a as u64), &mut imp_src);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(1, &imp_src, &mut imp_cfg);
    let mut cfg_c = Vec::new();
    encode_bytes_field(2, &imp_cfg, &mut cfg_c);
    let mut req_c = Vec::new();
    encode_bytes_field(1, &id_msg(hub_c as u64), &mut req_c);
    encode_bytes_field(2, &cfg_c, &mut req_c);
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", req_c).await.status_code, STATUS_OK);

    let st_a_conn = qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", req_with_id(hub_a as u64)).await;
    assert_eq!(st_a_conn.status_code, STATUS_OK);
    let dyn_st_a = decode_dyn(_reg, st_a_conn.payload.as_ref().unwrap());
    let st_m = match dyn_st_a.get(1) {
        Some(DynamicValue::Message(m)) => m,
        other => panic!("expected state message, got {other:?}"),
    };
    let exp_m = match st_m.get(3) {
        Some(DynamicValue::Message(m)) => m,
        other => panic!("expected export message, got {other:?}"),
    };
    assert!(exp_m.get(5).is_some(), "expected connected export via Transit relay, got {exp_m:?}");

    // 3. Sever Transit relay (hub_b -> Inactive) and verify StateChangedNotice + disconnected export.
    let _ = drain_notices(&mut rx_a);
    let mut cfg_b_off = Vec::new();
    encode_varint_field(1, 1, &mut cfg_b_off);
    let mut req_b_off = Vec::new();
    encode_bytes_field(1, &id_msg(hub_b as u64), &mut req_b_off);
    encode_bytes_field(2, &cfg_b_off, &mut req_b_off);
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", req_b_off).await.status_code, STATUS_OK);
    let st_a_disc = qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", req_with_id(hub_a as u64)).await;
    let dyn_st_a_disc = decode_dyn(_reg, st_a_disc.payload.as_ref().unwrap());
    let exp_m_disc = dyn_st_a_disc.get(1).and_then(|v| match v { DynamicValue::Message(m) => m.get(3), _ => None }).and_then(|v| match v { DynamicValue::Message(m) => Some(m), _ => None }).expect("export disc");
    assert!(exp_m_disc.get(6).is_some(), "expected disconnected export after Transit relay severed, got {exp_m_disc:?}");
    let got_notices: Vec<String> = drain_notices(&mut rx_a).into_iter().map(|n| n.normalized_type_name().to_string()).collect();
    assert!(got_notices.iter().any(|n| n == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice"), "got notices: {got_notices:?}");
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest", req_with_id(hub_b as u64)).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut sess, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", req_with_id(hub_b as u64)).await.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_sov_skyhook_workforce_and_admin_lifecycle() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(common::TEST_CLOCK_BASE_UNIX));
    ctx.set_clock(clock.clone());
    let _reg = &ctx.quasar_gateway.descriptors;

    let (sys_id, planet) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.type_id == 11)
                .map(|c| (s.solar_system_id, c))
        })
        .expect("nullsec system with Temperate planet");

    let cid = 90_000_001u32;
    let corp_id = 98_000_001u32;
    let skyhook_id = 1_000_000_301i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.corporation_id = corp_id as i64;
            ch.solar_system_id = sys_id;
        }
        let mut st = UpwellStructureRecord::default();
        st.structure_id = skyhook_id;
        st.type_id = TYPE_ORBITAL_SKYHOOK;
        st.owner_corp_id = corp_id as i64;
        st.solar_system_id = sys_id;
        st.state = "shield_vulnerable".into();
        s.structures.insert(skyhook_id, st);
        s.sov_runtime.skyhook_planets.insert(skyhook_id, planet.celestial_id);
        s.sov_runtime.skyhook_reagents.insert(skyhook_id, (81_143, 500));
        s.sov_runtime.skyhook_active.insert(skyhook_id, true);
    });

    let mut admin = make_sess(84_001, cid, corp_id, sys_id, true);
    let mut rx = subscribe(&ctx, &mut admin).await;

    let get_rsp = qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.GetRequest", req_with_id(skyhook_id as u64)).await;
    assert_eq!(get_rsp.status_code, STATUS_OK);
    let wm = WireMessage::decode(&get_rsp.payload.as_ref().unwrap().value).expect("decode GetResponse wire");
    let (_, expected_wf) = planet_yield(11);
    assert!(expected_wf > 0);
    let wf_bytes = wm.fields.iter().find(|f| f.field_number == 7).and_then(|f| f.value.as_bytes()).expect("workforce field 7");
    let wf_wm = WireMessage::decode(wf_bytes).expect("decode workforce");
    assert_eq!(wf_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(expected_wf));
    let rdef_bytes = wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_bytes()).expect("reagent def field 4");
    let rdef_wm = WireMessage::decode(rdef_bytes).expect("decode reagent def");
    let amt_bytes = rdef_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()).expect("amt field 2");
    let amt_wm = WireMessage::decode(amt_bytes).expect("decode amt");
    assert_eq!(amt_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(SKYHOOK_HOURLY_REAGENT_YIELD));

    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.GetAllLocalRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest", req_with_id(corp_id as u64)).await.status_code, STATUS_OK);

    let mut mod_req = Vec::new();
    encode_bytes_field(1, &id_msg(skyhook_id as u64), &mut mod_req);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req).await.status_code, STATUS_OK);

    let now = ctx.now_unix();
    let mut ts_start = Vec::new();
    encode_varint_field(1, now + 60, &mut ts_start);
    let mut ts_end = Vec::new();
    encode_varint_field(1, now + 3_660, &mut ts_end);
    let mut sched_req = Vec::new();
    encode_bytes_field(1, &id_msg(skyhook_id as u64), &mut sched_req);
    encode_bytes_field(2, &ts_start, &mut sched_req);
    encode_bytes_field(3, &ts_end, &mut sched_req);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", sched_req).await.status_code, STATUS_OK);

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice"));
    assert!(notices.iter().any(|n| n.normalized_type_name() == "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice"));

    clock.advance_secs(120);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest", req_with_id(sys_id as u64)).await.status_code, STATUS_OK);
}
