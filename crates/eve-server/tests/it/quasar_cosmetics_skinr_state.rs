//! State-backed integration tests for V24.01 3rd-party SKINR component licenses, custom SKIN
//! designs & licenses, sequencing pricing/durations/jobs, and character title definitions & version
//! notices (`quasar_cosmetics_skinr` and `quasar_cosmetics_skinr_seq`).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_skinr::COMP_CAT_PATTERN;
use std::collections::HashMap;
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

fn admin_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(1);
    s.character_id = Some(cid as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30_000_142);
    s.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    s.corp_role = 0x01;
    s
}

fn player_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(2);
    s.character_id = Some(cid as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30_000_142);
    s.role = 0;
    s.corp_role = 0;
    s
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("skinr-test-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"skinr-it-suite01".to_vec(),
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

fn drain(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p);
        }
    }
    out
}

fn decode(reg: &QuasarDescriptorRegistry, any: &GoogleAny) -> DynamicQuasarMessage {
    reg.decode_dynamic(any.normalized_type_name(), &any.value)
        .unwrap_or_else(|e| panic!("decode {}: {e:?}", any.normalized_type_name()))
}

fn seq_msg(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

fn comp_id_msg(comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &seq_msg(comp_id as u64), &mut b);
    b
}

fn comp_lic_id_msg(char_id: u32, comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &seq_msg(char_id as u64), &mut b);
    encode_bytes_field(2, &comp_id_msg(comp_id, is_pattern), &mut b);
    b
}

fn skin_lic_id_msg(char_id: u32, skin_hex: &str) -> Vec<u8> {
    let mut skin = Vec::new();
    encode_string_field(1, skin_hex, &mut skin);
    let mut b = Vec::new();
    encode_bytes_field(1, &seq_msg(char_id as u64), &mut b);
    encode_bytes_field(2, &skin, &mut b);
    b
}

#[tokio::test]
async fn skinr_component_licenses_and_item_consumption() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let (cid1, cid2) = (90_000_301u32, 90_000_302u32);
    let mut sess1 = admin_sess(50_301, cid1, 98_000_001);
    let mut sess2 = player_sess(50_302, cid2, 98_000_001);
    let mut rx1 = subscribe(&ctx, &mut sess1).await;

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat.components.values().find(|c| !c.is_pattern() && !c.associated_types.is_empty()).expect("coating comp");
    let pattern_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_PATTERN).expect("pattern comp");
    let (coating_item_type_id, coating_uses_per_item) = coating_comp.associated_types[0];

    // 1. Initially character owns no component licenses; GetRequest returns 404
    let owned0 = rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(owned0.status_code, STATUS_OK);
    let owned0_msg = decode(&reg, owned0.payload.as_ref().unwrap());
    assert!(owned0_msg.get(1).is_none() && owned0_msg.get(2).is_none());

    let mut get_fin_req = Vec::new();
    encode_bytes_field(1, &comp_lic_id_msg(cid1, coating_comp.component_id, false), &mut get_fin_req);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_fin_req.clone()).await.status_code, STATUS_NOT_FOUND);

    // 2. Non-admin cannot grant finite or infinite component licenses (403); invalid component returns 400
    let mut grant_fin_req = Vec::new();
    encode_bytes_field(1, &comp_id_msg(coating_comp.component_id, false), &mut grant_fin_req);
    encode_varint_field(2, 5, &mut grant_fin_req);
    assert_eq!(rpc(&ctx, &mut sess2, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_fin_req.clone()).await.status_code, STATUS_FORBIDDEN);

    let mut bad_grant = Vec::new();
    encode_bytes_field(1, &comp_id_msg(999_999_999, false), &mut bad_grant);
    encode_varint_field(2, 5, &mut bad_grant);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", bad_grant).await.status_code, STATUS_BAD_REQUEST);

    // 3. Admin grants finite license (capacity = 5) and infinite pattern license -> GrantedNotice emitted
    let grant_fin_rsp = rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_fin_req).await;
    assert_eq!(grant_fin_rsp.status_code, STATUS_OK);
    let notices = drain(&mut rx1);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice");
    let fin_notice_msg = decode(&reg, &notices[0]);
    let Some(DynamicValue::Message(kind_m)) = fin_notice_msg.get(1) else { panic!("expected Kind") };
    assert_eq!(kind_m.get(3), Some(&DynamicValue::Uint64(5)));

    let get_fin_rsp = rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_fin_req.clone()).await;
    assert_eq!(get_fin_rsp.status_code, STATUS_OK);
    let fin_attrs = decode(&reg, get_fin_rsp.payload.as_ref().unwrap());
    let Some(DynamicValue::Message(lic_attr)) = fin_attrs.get(1) else { panic!("expected finite.Attributes") };
    assert_eq!(lic_attr.get(2), Some(&DynamicValue::Uint64(5)));
    assert_eq!(rpc(&ctx, &mut sess2, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_fin_req.clone()).await.status_code, STATUS_FORBIDDEN);

    let mut grant_inf_req = Vec::new();
    encode_bytes_field(1, &comp_id_msg(pattern_comp.component_id, true), &mut grant_inf_req);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest", grant_inf_req).await.status_code, STATUS_OK);
    assert_eq!(drain(&mut rx1)[0].normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice");

    let mut get_inf_req = Vec::new();
    encode_bytes_field(1, &comp_lic_id_msg(cid1, pattern_comp.component_id, true), &mut get_inf_req);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest", get_inf_req.clone()).await.status_code, STATUS_OK);

    let owned1 = rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest", Vec::new()).await;
    let owned1_msg = decode(&reg, owned1.payload.as_ref().unwrap());
    assert!(matches!(owned1_msg.get(1), Some(DynamicValue::Repeated(v)) if v.len() == 1));
    assert!(matches!(owned1_msg.get(2), Some(DynamicValue::Repeated(v)) if v.len() == 1));

    // 4. ConsumeRequest consumes inventory item stack and grants component license charges
    let item_id = 770_001i64;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().expect("seeded char");
        ch.character_id = cid1 as i64;
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(60_003_760);
        s.characters.insert(cid1 as i64, ch);
        s.items.insert(item_id, InventoryItemRecord {
            item_id, type_id: coating_item_type_id, owner_id: cid1 as i64, location_id: 60_003_760,
            flag: InventoryFlag::Hangar, quantity: 3, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });

    let mut item_outer = Vec::new();
    encode_bytes_field(1, &seq_msg(item_id as u64), &mut item_outer);
    let mut consume_req = Vec::new();
    encode_bytes_field(1, &item_outer, &mut consume_req);
    encode_varint_field(2, 2, &mut consume_req);

    assert_eq!(rpc(&ctx, &mut sess2, "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&item_id).map(|i| i.quantity)), Some(1));
    assert_eq!(drain(&mut rx1)[0].normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice");

    if coating_uses_per_item > 0 {
        let get_after = rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_fin_req.clone()).await;
        let after_msg = decode(&reg, get_after.payload.as_ref().unwrap());
        let Some(DynamicValue::Message(la)) = after_msg.get(1) else { panic!("finite attrs") };
        assert_eq!(la.get(2), Some(&DynamicValue::Uint64(5 + 2 * (coating_uses_per_item as u64))));
    }

    // 5. Revoke finite and infinite component licenses -> subsequent GetRequest returns 404
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest", get_fin_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_fin_req).await.status_code, STATUS_NOT_FOUND);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest", get_inf_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest", get_inf_req).await.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
async fn skinr_custom_skins_licenses_and_sequencing_jobs() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 90_000_310u32;
    let ship_id = 880_001i64;
    let rifter_type_id = 587u32;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().expect("seeded char");
        ch.character_id = cid as i64;
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(1_000_000);
        s.characters.insert(cid as i64, ch);
        let sk_map = s.skills.entry(cid as i64).or_default();
        sk_map.insert(81044, TrainedSkillRecord { type_id: 81044, level: 4, skill_points: 45_255.0 });
        sk_map.insert(81040, TrainedSkillRecord { type_id: 81040, level: 5, skill_points: 256_000.0 });
        s.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: rifter_type_id, owner_id: cid as i64, location_id: 60_003_760,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });

    let mut sess = admin_sess(50_310, cid, 98_000_001);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. Verify SDE-backed tier pricing, durations (with skill discounts), and early completion pricing
    let pricing_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    let pricing_msg = decode(&reg, pricing_rsp.payload.as_ref().unwrap());
    assert!(matches!(pricing_msg.get(1), Some(DynamicValue::Repeated(v)) if !v.is_empty()));
    assert_eq!(pricing_msg.get(2), Some(&DynamicValue::Uint32(8)));

    let dur_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await;
    assert_eq!(dur_rsp.status_code, STATUS_OK);
    assert_eq!(decode(&reg, dur_rsp.payload.as_ref().unwrap()).get(2), Some(&DynamicValue::Uint32(15)));

    let early_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest", Vec::new()).await;
    assert_eq!(early_rsp.status_code, STATUS_OK);
    assert!(matches!(decode(&reg, early_rsp.payload.as_ref().unwrap()).get(1), Some(DynamicValue::Repeated(v)) if v.len() == 3));

    // 2. InitiateRequest without owning required component license fails with 400 and emits FailedNotice
    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coat_id = skinr_cat.components.values().find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1)).expect("coating comp").component_id;

    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &seq_msg(1), &mut slot_msg);
    let mut coat_cfg = Vec::new();
    encode_bytes_field(1, &seq_msg(coat_id as u64), &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);

    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    encode_bytes_field(2, &seq_msg(rifter_type_id as u64), &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Valkyrie Rifter", &mut init_req);
    encode_string_field(2, "Minmatar Elite", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req);

    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req.clone()).await.status_code, STATUS_BAD_REQUEST);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.FailedNotice"));

    // 3. Grant finite coating license (capacity = 5), then InitiateRequest (licenses = 2) succeeds
    let mut grant_coat = Vec::new();
    encode_bytes_field(1, &comp_id_msg(coat_id, false), &mut grant_coat);
    encode_varint_field(2, 5, &mut grant_coat);
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_coat).await.status_code, STATUS_OK);
    let _ = drain(&mut rx);

    let init_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice"));

    // Verify finite component license capacity was decremented from 5 to 3
    let mut get_coat_lic = Vec::new();
    encode_bytes_field(1, &comp_lic_id_msg(cid, coat_id, false), &mut get_coat_lic);
    let coat_after = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_coat_lic).await;
    let Some(DynamicValue::Message(ca)) = decode(&reg, coat_after.payload.as_ref().unwrap()).get(1).cloned() else { panic!("finite attrs") };
    assert_eq!(ca.get(2), Some(&DynamicValue::Uint64(3)));

    // Extract job UUID and skin_hex from InitiateResponse
    let init_msg = decode(&reg, init_rsp.payload.as_ref().unwrap());
    let Some(DynamicValue::Message(job_id_m)) = init_msg.get(1) else { panic!("job id") };
    let Some(DynamicValue::Bytes(job_uuid)) = job_id_m.get(1) else { panic!("job uuid") };
    let Some(DynamicValue::Message(job_attr_m)) = init_msg.get(2) else { panic!("job attrs") };
    let Some(DynamicValue::Message(skin_id_m)) = job_attr_m.get(1) else { panic!("skin id") };
    let Some(DynamicValue::String(skin_hex)) = skin_id_m.get(1).cloned() else { panic!("skin hex") };

    // Verify thirdparty.api.GetRequest returns design and 404 on unknown hex
    let mut get_skin_req = Vec::new();
    let mut skin_id_bytes = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id_bytes);
    encode_bytes_field(1, &skin_id_bytes, &mut get_skin_req);
    let skin_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest", get_skin_req).await;
    assert_eq!(skin_rsp.status_code, STATUS_OK);
    let Some(DynamicValue::Message(sa)) = decode(&reg, skin_rsp.payload.as_ref().unwrap()).get(1).cloned() else { panic!("skin attrs") };
    assert_eq!(sa.get(1), Some(&DynamicValue::String("Valkyrie Rifter".into())));
    assert_eq!(sa.get(6), Some(&DynamicValue::Bool(true)));

    let mut bad_skin_req = Vec::new();
    let mut bad_skin_id = Vec::new();
    encode_string_field(1, "unknown_hex_404", &mut bad_skin_id);
    encode_bytes_field(1, &bad_skin_id, &mut bad_skin_req);
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest", bad_skin_req).await.status_code, STATUS_NOT_FOUND);

    // Verify job.api.GetAllActiveRequest and job.api.GetRequest
    let active_rsp = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    assert_eq!(active_rsp.status_code, STATUS_OK);
    assert!(matches!(decode(&reg, active_rsp.payload.as_ref().unwrap()).get(1), Some(DynamicValue::Repeated(v)) if v.len() == 1));

    let mut job_id_outer = Vec::new();
    let mut job_id_inner = Vec::new();
    encode_bytes_field(1, job_uuid, &mut job_id_inner);
    encode_bytes_field(1, &job_id_inner, &mut job_id_outer);
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest", job_id_outer.clone()).await.status_code, STATUS_OK);

    // 4. CompleteEarlyRequest completes the job, emits CompletedNotice + AddedNotice, and grants 2 unactivated licenses
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", job_id_outer).await.status_code, STATUS_OK);
    let comp_notices = drain(&mut rx);
    assert!(comp_notices.iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"));
    assert!(comp_notices.iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice"));

    // 5. ActivateRequest, ApplyRequest, UnapplyRequest, admin.GrantRequest, and admin.RevokeRequest
    let mut lic_req = Vec::new();
    encode_bytes_field(1, &skin_lic_id_msg(cid, &skin_hex), &mut lic_req);
    let lic_get0 = rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req.clone()).await;
    assert_eq!(lic_get0.status_code, STATUS_OK);
    let Some(DynamicValue::Message(la0)) = decode(&reg, lic_get0.payload.as_ref().unwrap()).get(1).cloned() else { panic!("lic attrs") };
    assert_eq!(la0.get(2), Some(&DynamicValue::Uint64(2)));

    let mut apply_req = lic_req.clone();
    encode_bytes_field(2, &seq_msg(ship_id as u64), &mut apply_req);
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest", apply_req.clone()).await.status_code, STATUS_BAD_REQUEST);

    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest", lic_req.clone()).await.status_code, STATUS_OK);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.license.api.UpdatedNotice"));

    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest", apply_req.clone()).await.status_code, STATUS_OK);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.api.SetNotice"));

    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest", apply_req).await.status_code, STATUS_OK);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.api.SetNotice"));

    let mut admin_grant = Vec::new();
    encode_bytes_field(1, &skin_id_bytes, &mut admin_grant);
    encode_varint_field(2, 3, &mut admin_grant);
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest", admin_grant).await.status_code, STATUS_OK);
    let _ = drain(&mut rx);

    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest", lic_req.clone()).await.status_code, STATUS_OK);
    assert!(drain(&mut rx).iter().any(|n| n.normalized_type_name() == "eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice"));
    assert_eq!(rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req).await.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
async fn character_title_definitions_and_version_notices() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut admin = admin_sess(50_320, 90_000_320, 98_000_001);
    let mut player = player_sess(50_321, 90_000_321, 98_000_001);
    let mut rx = subscribe(&ctx, &mut player).await;

    // 1. GetLatestRequest returns Checkpoint with semantic version (24, 1, 0) and language-specific URL/CRC
    let latest_en = rpc(&ctx, &mut player, "eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new()).await;
    assert_eq!(latest_en.status_code, STATUS_OK);
    let en_msg = decode(&reg, latest_en.payload.as_ref().unwrap());
    let Some(DynamicValue::Message(cp_en)) = en_msg.get(1) else { panic!("checkpoint") };
    assert!(matches!(cp_en.get(1), Some(DynamicValue::String(u)) if u.contains("v24.1.0/en-us.json")));
    let Some(DynamicValue::Message(ver_en)) = cp_en.get(2) else { panic!("version") };
    assert_eq!(ver_en.get(1), Some(&DynamicValue::Uint32(24)));
    assert_eq!(ver_en.get(2), Some(&DynamicValue::Uint32(1)));

    // 2. Non-admin cannot set default version (403); admin setting version (24, 2, 5) broadcasts CheckVersionNotice
    let mut ver_spec = Vec::new();
    encode_varint_field(1, 24, &mut ver_spec);
    encode_varint_field(2, 2, &mut ver_spec);
    encode_varint_field(3, 5, &mut ver_spec);
    let mut set_ver_req = Vec::new();
    encode_bytes_field(2, &ver_spec, &mut set_ver_req);

    assert_eq!(rpc(&ctx, &mut player, "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest", set_ver_req.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut admin, "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest", set_ver_req).await.status_code, STATUS_OK);

    let notices = drain(&mut rx);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].normalized_type_name(), "eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice");
    let notice_msg = decode(&reg, &notices[0]);
    let Some(DynamicValue::Message(nv)) = notice_msg.get(1) else { panic!("version in notice") };
    assert_eq!(nv.get(1), Some(&DynamicValue::Uint32(24)));
    assert_eq!(nv.get(2), Some(&DynamicValue::Uint32(2)));
    assert_eq!(nv.get(3), Some(&DynamicValue::Uint32(5)));

    let latest_after = rpc(&ctx, &mut player, "eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new()).await;
    let after_msg = decode(&reg, latest_after.payload.as_ref().unwrap());
    let Some(DynamicValue::Message(cp_after)) = after_msg.get(1) else { panic!("checkpoint after") };
    assert!(matches!(cp_after.get(1), Some(DynamicValue::String(u)) if u.contains("v24.2.5/en-us.json")));
}
