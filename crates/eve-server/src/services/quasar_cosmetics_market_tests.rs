//! Unit tests for `quasar_cosmetics_market` Paragon Hub cosmetic market helpers,
//! broker fees, skill reductions, tax quotes, and buyer fee quotes.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_cosmetics_skinr::{
    SkinDesignRecord, SkinrCosmeticsData, SkinrCosmeticsState, SKINR_BLOB_KEY,
};
use eve_db::{IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "quasar_cosmetics_market_component_tests.rs"]
mod quasar_cosmetics_market_component_tests;

#[cfg(test)]
#[path = "quasar_cosmetics_market_skin_tests.rs"]
mod quasar_cosmetics_market_skin_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn new_market_harness() -> (
    PublicGatewayRouter,
    Arc<WorldRepository>,
    Arc<SkinrCosmeticsState>,
    Arc<CosmeticsMarketState>,
) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let state = CosmeticsMarketState::load_with_sink(
        Arc::clone(&db),
        Arc::clone(&sde),
        Arc::clone(&skinr),
        Arc::clone(&router.macho_notify_sink),
    );
    super::register_with_state(&router, Arc::clone(&state));
    (router, db, skinr, state)
}

pub(super) fn make_char(
    db: &WorldRepository,
    cid: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    isk_cents: i64,
    plex: i64,
    evermarks: i64,
) {
    db.with_mut_snapshot(|s| {
        if let Some(aid) = alliance_id {
            let tmpl_corp = s.corporations.values().next().expect("demo corp").clone();
            let corp = s.corporations.entry(corp_id).or_insert_with(|| {
                let mut c = tmpl_corp;
                c.corporation_id = corp_id;
                c
            });
            corp.alliance_id = Some(aid);
        }
        let mut ch = s.characters.values().next().expect("demo char").clone();
        ch.character_id = cid;
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.balance = IskCents(isk_cents);
        ch.plex_balance = plex;
        ch.home_station_id = 60_003_760;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.loyalty_points.clear();
        if evermarks > 0 {
            ch.loyalty_points
                .insert(PARAGON_CORP_ID as i64, evermarks);
        }
        s.characters.insert(cid, ch);
    });
}

pub(super) fn make_sess(cid: i64, corp_id: i64, alliance_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(cid as u64);
    s.user_id = Some(cid as u64);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s
}

pub(super) fn call(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![7; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: None,
        trace_context: None,
    };
    router.handle_public_request(sess, &req, None, false, now)
}

pub(super) fn wm_sub(wm: &WireMessage, field: u32) -> WireMessage {
    let b = wm
        .fields
        .iter()
        .find(|f| f.field_number == field)
        .and_then(|f| f.value.as_bytes())
        .expect("submessage bytes");
    WireMessage::decode(b).expect("decode submessage")
}

pub(super) fn wm_bytes(wm: &WireMessage, field: u32) -> Vec<u8> {
    wm.fields
        .iter()
        .find(|f| f.field_number == field)
        .and_then(|f| f.value.as_bytes())
        .expect("bytes field")
        .to_vec()
}

pub(super) fn wm_varint(wm: &WireMessage, field: u32) -> u64 {
    wm.fields
        .iter()
        .find(|f| f.field_number == field)
        .and_then(|f| f.value.as_varint())
        .expect("varint field")
}

pub(super) fn encode_isk_msg(field: u32, units: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, encode_zigzag64(units as i64), &mut b);
    encode_varint_field(2, 0, &mut b);
    encode_bytes_field(field, &b, out);
}

#[test]
fn pure_helpers_seed_fees_skills_taxes_and_value_extractors() {
    let mut empty_sde = SdeRegistry::default();
    let mut data = CosmeticsMarketData::default();
    assert!(seed_component_listings_if_empty(&mut data, &empty_sde));
    assert_eq!(data.component_listings.len(), 4);
    assert_eq!(data.next_seq, 10);
    assert!(!seed_component_listings_if_empty(&mut data, &empty_sde));

    let base_type = shared_sde().types.values().next().expect("sde type").clone();
    for tid in [82_400u32, 82_401, 82_402, 82_403] {
        let mut t = base_type.clone();
        t.type_id = tid;
        t.group_id = GROUP_SHIP_SKIN_DESIGN_ELEMENT;
        t.name = format!("Elem {tid}");
        t.published = true;
        empty_sde.types.insert(tid, t);
    }
    let mut sde_data = CosmeticsMarketData::default();
    assert!(seed_component_listings_if_empty(&mut sde_data, &empty_sde));
    let tids: Vec<u32> = sde_data
        .component_listings
        .values()
        .map(|l| l.component_item_type_id)
        .collect();
    assert_eq!(tids, vec![82_400, 82_401, 82_402, 82_403]);

    assert_eq!(broker_fee_for(86_400, &ListingTargetRecord::Public), 100_000);
    assert_eq!(broker_fee_for(86_400, &ListingTargetRecord::Corporation(99)), 50_000);
    assert_eq!(broker_fee_for(7_776_000, &ListingTargetRecord::Alliance(88)), 1_250_000);
    assert_eq!(broker_fee_for(432_000, &ListingTargetRecord::Public), 250_000);
    assert_eq!(broker_fee_for(432_000, &ListingTargetRecord::Character(77)), 125_000);
    assert_eq!(broker_fee_for(1_000, &ListingTargetRecord::Public), 100_000);

    assert_eq!(
        checked_isk_cents(MAX_LISTING_PRICE_UNITS),
        Some(IskCents((MAX_LISTING_PRICE_UNITS * 100) as i64))
    );
    assert_eq!(checked_isk_cents(MAX_LISTING_PRICE_UNITS + 1), None);

    assert_eq!(as_u64(&DynamicValue::Uint64(42)), Some(42));
    assert_eq!(as_u64(&DynamicValue::Uint32(7)), Some(7));
    assert_eq!(as_u64(&DynamicValue::Int64(9)), Some(9));
    assert_eq!(as_u64(&DynamicValue::Int64(-1)), None);
    assert_eq!(as_u64(&DynamicValue::Sint32(5)), Some(5));
    assert_eq!(as_u64(&DynamicValue::Sint32(-2)), None);
    assert_eq!(as_u64(&DynamicValue::Bool(true)), None);

    assert_eq!(as_bool(&DynamicValue::Bool(true)), Some(true));
    assert_eq!(as_bool(&DynamicValue::Uint64(0)), Some(false));
    assert_eq!(as_bool(&DynamicValue::Uint32(2)), Some(true));
    assert_eq!(as_bool(&DynamicValue::Int64(0)), Some(false));
    assert_eq!(as_bool(&DynamicValue::Int32(3)), Some(true));
    assert_eq!(as_bool(&DynamicValue::String("x".into())), None);

    assert_eq!(as_str(&DynamicValue::String("abc".into())), Some("abc"));
    assert_eq!(as_str(&DynamicValue::Bytes(b"xyz".to_vec())), Some("xyz"));
    assert_eq!(as_str(&DynamicValue::Bytes(vec![0xff, 0xfe])), None);
    assert_eq!(as_str(&DynamicValue::Uint64(1)), None);

    let mut msg = DynamicQuasarMessage::new("Test");
    let mut sub_bytes = DynamicQuasarMessage::new("Uuid");
    sub_bytes.set(1, DynamicValue::Bytes(vec![0xAB; 16]));
    msg.set(1, DynamicValue::Message(Box::new(sub_bytes)));
    assert_eq!(uuid_of(&msg, 1), Some([0xAB; 16]));

    let mut sub_str = DynamicQuasarMessage::new("Uuid");
    sub_str.set(1, DynamicValue::String("0123456789abcdef".into()));
    msg.set(2, DynamicValue::Message(Box::new(sub_str)));
    assert_eq!(uuid_of(&msg, 2), Some(*b"0123456789abcdef"));

    let mut sub_bad = DynamicQuasarMessage::new("Uuid");
    sub_bad.set(1, DynamicValue::Bytes(vec![1, 2, 3]));
    msg.set(3, DynamicValue::Message(Box::new(sub_bad)));
    assert_eq!(uuid_of(&msg, 3), None);
}

#[test]
fn fee_tax_and_buyer_fee_rpcs_cover_skills_targets_and_db_skin_fallback() {
    let (router, db, skinr, state) = new_market_harness();
    let now = 1_700_000_000u64;
    let cid = 2_112_000_101i64;
    let corp = 98_000_201i64;
    make_char(&db, cid, corp, Some(99_000_301), 10_000_000_00, 0, 0);

    let anon = SessionState::new(999);
    let r_unauth = call(
        &router,
        &anon,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_unauth.status_code, STATUS_UNAUTHORIZED);

    let mut no_char = SessionState::new(999);
    no_char.user_id = Some(999);
    let r_no_char = call(
        &router,
        &no_char,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_no_char.status_code, STATUS_FORBIDDEN);

    let sess = make_sess(cid, corp, Some(99_000_301), 0);
    let r_pub = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_pub.status_code, STATUS_OK);
    let wm_pub = WireMessage::decode(&r_pub.payload.unwrap().value).unwrap();
    assert_eq!(wm_pub.fields.iter().filter(|f| f.field_number == 1).count(), 6);

    let mut ally_t = Vec::new();
    put_seq(3, 99_000_301, &mut ally_t);
    let mut req_ally = Vec::new();
    encode_bytes_field(1, &ally_t, &mut req_ally);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
            req_ally.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    let mut char_t = Vec::new();
    put_seq(4, cid as u64, &mut char_t);
    let mut req_char = Vec::new();
    encode_bytes_field(1, &char_t, &mut req_char);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
            req_char.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.api.GetTaxRateRequest",
            req_char,
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Character(1)), 30);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Alliance(1)), 10);
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Public), 20);

    db.with_mut_snapshot(|s| {
        let tmpl_sk = s.skills.values().next().and_then(|m| m.values().next()).expect("demo skill").clone();
        let sks = s.skills.entry(cid).or_default();
        for &(sid, lvl) in &[(84217u32, 5u8), (84218, 5), (84220, 5), (81368, 4), (81369, 2)] {
            let mut sk = tmpl_sk.clone();
            sk.type_id = sid;
            sk.level = lvl;
            sks.insert(sid, sk);
        }
    });
    assert_eq!(paragon_hub_skill_reductions(&state, cid as u32), (15, 5 + 4 + 10));
    assert_eq!(paragon_hub_tax_pct(&state, cid as u32, &ListingTargetRecord::Public), 5);

    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    let mut req_bf = req_ally;
    let mut sid_buf = Vec::new();
    encode_string_field(1, "deadbeef01", &mut sid_buf);
    encode_bytes_field(2, &sid_buf, &mut req_bf);
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            req_bf.clone(),
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    skinr.data.write().unwrap().skins.insert(
        "deadbeef01".into(),
        SkinDesignRecord {
            skin_hex: "deadbeef01".into(),
            name: "FallbackSkin".into(),
            line_name: "Line".into(),
            creator_id: cid as u32,
            ship_type_id: 587,
            pattern_blend_mode: 0,
            slots: Vec::new(),
            tier_level: 0,
            first_sequenced_unix: None,
        },
    );
    let mut blob_skinr = SkinrCosmeticsData::default();
    blob_skinr.skins.insert(
        "deadbeef01".into(),
        SkinDesignRecord {
            skin_hex: "deadbeef01".into(),
            name: "FallbackSkin".into(),
            line_name: "Line".into(),
            creator_id: cid as u32,
            ship_type_id: 587,
            pattern_blend_mode: 0,
            slots: Vec::new(),
            tier_level: 3,
            first_sequenced_unix: None,
        },
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            SKINR_BLOB_KEY.into(),
            serde_json::to_string(&blob_skinr).unwrap(),
        );
    });
    let r_bf_ok = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
        req_bf,
        now,
    );
    assert_eq!(r_bf_ok.status_code, STATUS_OK);
    let wm_bf = WireMessage::decode(&r_bf_ok.payload.unwrap().value).unwrap();
    assert_eq!(wm_varint(&wm_sub(&wm_sub(&wm_bf, 1), 1), 2), 7_500);

    let mut req_pub_bf = Vec::new();
    encode_bytes_field(2, &sid_buf, &mut req_pub_bf);
    let r_bf_pub = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
        req_pub_bf,
        now,
    );
    assert_eq!(
        wm_varint(
            &wm_sub(&wm_sub(&WireMessage::decode(&r_bf_pub.payload.unwrap().value).unwrap(), 1), 1),
            2
        ),
        3_000
    );

    let r_defs = call(
        &router,
        &sess,
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_defs.status_code, STATUS_OK);
}
