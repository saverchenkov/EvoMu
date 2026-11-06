//! Deep branch-coverage and mutation-killing unit tests for `quasar_market` (`Q-6`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use crate::services::quasar_market::{publish_market_new_entry_notice, register_market_handlers};
use eve_db::snapshot_ext::CorpMemberRoleAssignment;
use eve_db::{corp_roles, CorporationRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_market_router() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| snap.market_orders.clear());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = Arc::new(PublicGatewayRouter::new(
        Arc::clone(&descriptors),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    register_market_handlers(&router, Arc::clone(&db), Arc::clone(&sde), descriptors);
    (router, db)
}

fn invoke_market(
    router: &PublicGatewayRouter,
    req_type: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router.handlers.read().unwrap().get(req_type).cloned().unwrap();
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
        value: Vec::new(),
    };
    handler(ctx, &any, msg)
}

fn make_ctx_with_corp(
    char_id: Option<u32>,
    corp_id: Option<u32>,
    identity_role_bits: u64,
) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        corp_id,
        identity_role_bits,
        "tranquility".to_string(),
        b"gw-test".to_vec(),
        1_760_000_000,
    )
}

fn sub_id(val: DynamicValue) -> DynamicValue {
    let mut sub = DynamicQuasarMessage::new("Identifier");
    sub.set(1, val);
    DynamicValue::Message(Box::new(sub))
}

fn decode_sub_varint(bytes: &[u8], outer_field: u32, inner_field: u32) -> Option<u64> {
    let wm = WireMessage::decode(bytes).ok()?;
    let f = wm.fields.iter().find(|f| f.field_number == outer_field)?;
    let sub = WireMessage::decode(f.value.as_bytes()?).ok()?;
    sub.fields.iter().find(|sf| sf.field_number == inner_field)?.value.as_varint()
}

fn make_order(
    order_id: i64,
    owner_id: i64,
    station_id: i64,
    region_id: u32,
    type_id: u32,
    is_bid: bool,
    price_cents: i64,
    vol_entered: i32,
    vol_remaining: i32,
    escrow_cents: i64,
) -> MarketOrderRecord {
    MarketOrderRecord {
        order_id,
        owner_id,
        station_id,
        region_id,
        type_id,
        is_bid,
        price: IskCents(price_cents),
        vol_entered,
        vol_remaining,
        min_volume: 1,
        is_npc: false,
        escrow: IskCents(escrow_cents),
    }
}

#[test]
fn get_orders_and_public_open_filters_and_wire_encoding_branches() {
    let (router, db) = setup_market_router();
    let cid = 211_300_001i64;
    let ctx = make_ctx_with_corp(Some(cid as u32), None, 0);

    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(1001, make_order(1001, cid, 60_003_760, 10_000_002, 34, false, 25_000, 100, 80, 99_900));
        snap.market_orders.insert(1002, make_order(1002, cid, 60_003_760, 10_000_002, 35, true, -500, 50, 40, 50_000));
        snap.market_orders.insert(1003, make_order(1003, cid, 60_003_760, 10_000_002, 34, false, 30_000, 10, 0, 0));
        snap.market_orders.insert(1004, make_order(1004, cid, 60_008_494, 10_000_043, 34, true, 40_000, 20, 20, 80_000));
    });

    // 1. GetOrdersRequest: test nested DynamicValue variants (Uint32, Int64, Int32, String fallback->0)
    let mut req = DynamicQuasarMessage::new("GetOrdersRequest");
    req.set(1, sub_id(DynamicValue::Uint32(10_000_002)));
    req.set(2, sub_id(DynamicValue::Int64(60_003_760)));
    req.set(3, sub_id(DynamicValue::String("ignored".into())));
    let resp = invoke_market(&router, "eve.market.orders.GetOrdersRequest", &ctx, &req).unwrap();
    let wm = WireMessage::decode(&resp.value).unwrap();
    let sell_fields: Vec<_> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    let buy_fields: Vec<_> = wm.fields.iter().filter(|f| f.field_number == 2).collect();
    assert_eq!(sell_fields.len(), 1);
    assert_eq!(buy_fields.len(), 1);

    // Sell order attributes: no field 1 (issuer_character), no field 15 (escrow), price = 250 ISK
    let sell_entry = sell_fields[0].value.as_bytes().unwrap();
    assert_eq!(decode_sub_varint(sell_entry, 1, 1), Some(1001));
    let sell_attrs_bytes = WireMessage::decode(sell_entry)
        .unwrap()
        .fields
        .into_iter()
        .find(|f| f.field_number == 2)
        .unwrap()
        .value
        .as_bytes()
        .unwrap()
        .to_vec();
    let sell_attrs = WireMessage::decode(&sell_attrs_bytes).unwrap();
    assert!(sell_attrs.fields.iter().all(|f| f.field_number != 1));
    assert_eq!(decode_sub_varint(&sell_attrs_bytes, 3, 1), Some(34));
    assert_eq!(decode_sub_varint(&sell_attrs_bytes, 5, 1), Some(250));
    assert_eq!(decode_sub_varint(&sell_attrs_bytes, 12, 1), Some(60_003_760));
    assert_eq!(decode_sub_varint(&sell_attrs_bytes, 13, 1), Some(10_000_002));
    assert!(sell_attrs.fields.iter().all(|f| f.field_number != 15));

    // Buy order attributes: no field 1 or field 15 (escrow) leaked in public feed, negative price clamped to 0
    let buy_entry = buy_fields[0].value.as_bytes().unwrap();
    assert_eq!(decode_sub_varint(buy_entry, 1, 1), Some(1002));
    let buy_attrs_bytes = WireMessage::decode(buy_entry)
        .unwrap()
        .fields
        .into_iter()
        .find(|f| f.field_number == 2)
        .unwrap()
        .value
        .as_bytes()
        .unwrap()
        .to_vec();
    let buy_attrs = WireMessage::decode(&buy_attrs_bytes).unwrap();
    assert!(buy_attrs.fields.iter().all(|f| f.field_number != 1));
    assert_eq!(decode_sub_varint(&buy_attrs_bytes, 5, 1), Some(0));
    assert!(buy_attrs.fields.iter().all(|f| f.field_number != 15));

    // Filter GetOrdersRequest by specific type 34 (using Int32 inside submessage) and wrong station
    req.set(3, sub_id(DynamicValue::Int32(34)));
    let resp_t34 = invoke_market(&router, "eve.market.orders.GetOrdersRequest", &ctx, &req).unwrap();
    assert_eq!(WireMessage::decode(&resp_t34.value).unwrap().fields.len(), 1);
    req.set(2, DynamicValue::Int32(60_009_999));
    assert!(invoke_market(&router, "eve.market.orders.GetOrdersRequest", &ctx, &req).unwrap().value.is_empty());
}

#[test]
fn public_open_buy_sell_region_station_structure_and_no_type_filter_branches() {
    let (router, db) = setup_market_router();
    let cid = 211_300_002i64;
    let ctx = make_ctx_with_corp(Some(cid as u32), None, 0);

    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(2001, make_order(2001, cid, 60_003_760, 10_000_002, 34, false, 10_000, 10, 10, 0));
        snap.market_orders.insert(2002, make_order(2002, cid, 1_040_000_000_001, 10_000_002, 35, true, 20_000, 10, 10, 200_000));
    });

    // GetPublicOpenInRegionRequest: field 2 = Bool(false) vs Bool(true) (no_type_filter)
    let mut req = DynamicQuasarMessage::new("GetPublicOpenInRegionRequest");
    req.set(1, DynamicValue::Uint64(10_000_002));
    req.set(2, DynamicValue::Bool(false));
    req.set(3, DynamicValue::Uint32(34));
    let r_filtered = invoke_market(&router, "eve.market.orders.GetPublicOpenInRegionRequest", &ctx, &req).unwrap();
    assert_eq!(WireMessage::decode(&r_filtered.value).unwrap().fields.len(), 1);

    req.set(2, DynamicValue::Bool(true));
    let r_all = invoke_market(&router, "eve.market.orders.GetPublicOpenInRegionRequest", &ctx, &req).unwrap();
    assert_eq!(WireMessage::decode(&r_all.value).unwrap().fields.len(), 2);

    // buyorder & sellorder GetAllPublicOpenInRegionRequest with no_type_filter false vs true
    req.set(2, DynamicValue::Bool(false));
    assert!(invoke_market(&router, "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest", &ctx, &req).unwrap().value.is_empty());
    req.set(2, DynamicValue::Bool(true));
    let r_buy_all = invoke_market(&router, "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest", &ctx, &req).unwrap();
    assert_eq!(WireMessage::decode(&r_buy_all.value).unwrap().fields.len(), 1);

    req.set(2, DynamicValue::Bool(false));
    req.set(3, DynamicValue::Int64(35));
    assert!(invoke_market(&router, "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest", &ctx, &req).unwrap().value.is_empty());
    req.set(2, DynamicValue::Bool(true));
    let r_sell_all = invoke_market(&router, "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest", &ctx, &req).unwrap();
    assert_eq!(WireMessage::decode(&r_sell_all.value).unwrap().fields.len(), 1);

    // GetOpenInStationRequest, GetOpenInStructureRequest & GetTypesWithActiveOrdersInRegionRequest
    let mut st_req = DynamicQuasarMessage::new("StationReq");
    st_req.set(1, DynamicValue::Int64(60_003_760));
    let r_st = invoke_market(&router, "eve.market.orders.GetOpenInStationRequest", &ctx, &st_req).unwrap();
    assert_eq!(WireMessage::decode(&r_st.value).unwrap().fields.len(), 1);

    st_req.set(1, DynamicValue::Int64(1_040_000_000_001));
    let r_struct = invoke_market(&router, "eve.market.orders.GetOpenInStructureRequest", &ctx, &st_req).unwrap();
    assert_eq!(WireMessage::decode(&r_struct.value).unwrap().fields.len(), 1);

    let mut types_req = DynamicQuasarMessage::new("TypesReq");
    types_req.set(1, DynamicValue::Uint32(10_000_002));
    let r_types = invoke_market(&router, "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest", &ctx, &types_req).unwrap();
    assert_eq!(WireMessage::decode(&r_types.value).unwrap().fields.len(), 2);
}

#[test]
fn price_history_averages_and_character_corp_authorization_matrix() {
    let (router, db) = setup_market_router();
    let ceo_id = 211_300_010i64;
    let member_no_role = 211_300_011i64;
    let member_trader = 211_300_012i64;
    let member_via_ra = 211_300_013i64;
    let npc_char_with_ctx_corp = 211_300_014i64;
    let outsider_id = 211_300_015i64;
    let corp_id = 98_000_510i64;

    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, c_corp) in [
            (ceo_id, corp_id),
            (member_no_role, corp_id),
            (member_trader, corp_id),
            (member_via_ra, corp_id),
            (npc_char_with_ctx_corp, 1_000_009),
            (outsider_id, 98_000_999),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = c_corp;
            snap.characters.insert(cid, ch);
        }

        let mut member_roles = HashMap::new();
        member_roles.insert(member_trader, corp_roles::ROLE_TRADER);
        member_roles.insert(member_no_role, 1u64 << 40);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "AuthMatrixCorp".into(),
                ticker: "AMXC".into(),
                ceo_id,
                hq_station_id: Some(60_003_760),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, member_no_role, member_trader, member_via_ra, npc_char_with_ctx_corp]),
                member_roles,
                ..Default::default()
            },
        );
        snap.corp_role_assignments.insert(
            (corp_id, member_via_ra),
            CorpMemberRoleAssignment {
                character_id: member_via_ra,
                roles: corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                ..Default::default()
            },
        );

        snap.market_orders.insert(3001, make_order(3001, ceo_id, 60_003_760, 10_000_002, 34, false, 20_000, 10, 10, 0));
        snap.market_orders.insert(3002, make_order(3002, corp_id, 60_003_760, 10_000_002, 34, false, 40_000, 10, 0, 0));
    });

    let ceo_ctx = make_ctx_with_corp(Some(ceo_id as u32), Some(corp_id as u32), 0);

    // 1. Price history average verification (200 + 400)/2 = 300 ISK, count = 2
    let mut ph_req = DynamicQuasarMessage::new("PH");
    ph_req.set(1, DynamicValue::Uint32(10_000_002));
    ph_req.set(2, DynamicValue::Uint32(34));
    ph_req.set(3, DynamicValue::Uint32(34));
    let ph_resp = invoke_market(&router, "eve.market.orders.GetPriceHistoryRequest", &ceo_ctx, &ph_req).unwrap();
    let entry_bytes = WireMessage::decode(&ph_resp.value).unwrap().fields[0].value.as_bytes().unwrap().to_vec();
    assert_eq!(decode_sub_varint(&entry_bytes, 2, 1), Some(300));
    assert_eq!(
        WireMessage::decode(&entry_bytes).unwrap().fields.iter().find(|f| f.field_number == 5).and_then(|f| f.value.as_varint()),
        Some(2)
    );

    // 2. Unauthenticated context -> STATUS_UNAUTHORIZED
    let anon_ctx = make_ctx_with_corp(None, None, 0);
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for rpc in [
        "eve.market.orders.GetOpenForCharacterRequest",
        "eve.market.orders.GetHistoryForCharacterRequest",
        "eve.market.orders.GetOpenForCorporationRequest",
        "eve.market.orders.GetHistoryForCorporationRequest",
        "eve.market.character.sellorder.GetRequest",
    ] {
        assert_eq!(invoke_market(&router, rpc, &anon_ctx, &empty_msg).unwrap_err().0, STATUS_UNAUTHORIZED);
    }

    // 3. Character open vs history & mismatched req_cid -> STATUS_FORBIDDEN
    let mut char_req = DynamicQuasarMessage::new("CharReq");
    char_req.set(1, DynamicValue::Int64(outsider_id));
    assert_eq!(invoke_market(&router, "eve.market.orders.GetOpenForCharacterRequest", &ceo_ctx, &char_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(invoke_market(&router, "eve.market.orders.GetHistoryForCharacterRequest", &ceo_ctx, &char_req).unwrap_err().0, STATUS_FORBIDDEN);

    char_req.set(1, DynamicValue::Int64(0));
    let open_char = invoke_market(&router, "eve.market.orders.GetOpenForCharacterRequest", &ceo_ctx, &char_req).unwrap();
    assert_eq!(WireMessage::decode(&open_char.value).unwrap().fields.len(), 1);

    // 4. Corporation authorization matrix
    let orphan_ctx = make_ctx_with_corp(Some(999_888_777), None, 0);
    assert_eq!(invoke_market(&router, "eve.market.orders.GetOpenForCorporationRequest", &orphan_ctx, &empty_msg).unwrap_err().0, STATUS_FORBIDDEN);

    let outsider_ctx = make_ctx_with_corp(Some(outsider_id as u32), Some(98_000_999), corp_roles::ROLE_DIRECTOR);
    let mut corp_req = DynamicQuasarMessage::new("CorpReq");
    corp_req.set(1, DynamicValue::Int64(corp_id));
    assert_eq!(invoke_market(&router, "eve.market.orders.GetOpenForCorporationRequest", &outsider_ctx, &corp_req).unwrap_err().0, STATUS_FORBIDDEN);

    let no_role_ctx = make_ctx_with_corp(Some(member_no_role as u32), Some(corp_id as u32), 0);
    assert_eq!(invoke_market(&router, "eve.market.orders.GetOpenForCorporationRequest", &no_role_ctx, &corp_req).unwrap_err().0, STATUS_FORBIDDEN);

    let trader_ctx = make_ctx_with_corp(Some(member_trader as u32), Some(corp_id as u32), 0);
    assert!(invoke_market(&router, "eve.market.orders.GetOpenForCorporationRequest", &trader_ctx, &corp_req).unwrap().value.is_empty());
    let hist_corp = invoke_market(&router, "eve.market.orders.GetHistoryForCorporationRequest", &trader_ctx, &corp_req).unwrap();
    assert_eq!(WireMessage::decode(&hist_corp.value).unwrap().fields.len(), 1);

    let ra_ctx = make_ctx_with_corp(Some(member_via_ra as u32), Some(corp_id as u32), 0);
    assert!(invoke_market(&router, "eve.market.orders.GetHistoryForCorporationRequest", &ra_ctx, &corp_req).is_ok());

    let npc_override_ctx = make_ctx_with_corp(Some(npc_char_with_ctx_corp as u32), Some(corp_id as u32), corp_roles::ROLE_ACCOUNTANT);
    assert!(invoke_market(&router, "eve.market.orders.GetHistoryForCorporationRequest", &npc_override_ctx, &empty_msg).is_ok());

    // 5. character.sellorder.GetRequest & corporation.sellorder.GetRequest
    let mut ord_req = DynamicQuasarMessage::new("OrdReq");
    ord_req.set(1, DynamicValue::Int64(999_999));
    assert_eq!(invoke_market(&router, "eve.market.character.sellorder.GetRequest", &ceo_ctx, &ord_req).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(invoke_market(&router, "eve.market.corporation.sellorder.GetRequest", &ceo_ctx, &ord_req).unwrap_err().0, STATUS_NOT_FOUND);

    ord_req.set(1, DynamicValue::Int64(3001));
    assert_eq!(invoke_market(&router, "eve.market.character.sellorder.GetRequest", &outsider_ctx, &ord_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(invoke_market(&router, "eve.market.character.sellorder.GetRequest", &ceo_ctx, &ord_req).is_ok());

    ord_req.set(1, DynamicValue::Int64(3002));
    assert!(invoke_market(&router, "eve.market.corporation.sellorder.GetRequest", &trader_ctx, &ord_req).is_ok());
    assert_eq!(publish_market_new_entry_notice(&router, 30_000_142, 1_760_000_000).unwrap(), 0);
}
