//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_market`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_db::{
    corp_roles, CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketOrderMeta, MarketOrderRecord, UpwellStructureRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
    WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(QuasarDescriptorRegistry::open_default_or_empty))
}

fn setup() -> (Arc<WorldRepository>, PublicGatewayRouter) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let desc = shared_descriptors();
    let router = PublicGatewayRouter::new(Arc::clone(&desc), Arc::clone(&db), Arc::clone(&sde));
    register_market_handlers(&router, Arc::clone(&db), sde, desc);
    (db, router)
}

fn sess(uid: Option<u64>, cid: Option<i64>, corp: Option<i64>, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = corp;
    s.corp_role = corp_role;
    s
}

fn call(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![1; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                value,
            }),
            ..Default::default()
        },
        None,
        false,
        now,
    )
}

fn auth_ctx(cid: Option<i64>, corp: Option<i64>, roles: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        cid.map(|c| c as i32),
        cid.map(|c| c as u32),
        corp.map(|c| c as u32),
        roles,
        "tranquility",
        vec![],
        1_760_000_500,
    )
}

fn extract_varints(wire: &WireMessage, field: u32) -> Vec<u64> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_varint())
        .collect()
}

fn extract_messages(wire: &WireMessage, field: u32) -> Vec<WireMessage> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_bytes().and_then(|b| WireMessage::decode(b).ok()))
        .collect()
}

fn seed_upwell_with_market_service(
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    owner_corp_id: i64,
    state: &str,
    quantum_core: bool,
    fuel_blocks: u32,
    service_online: bool,
) {
    snap.structures.insert(
        structure_id,
        UpwellStructureRecord {
            structure_id,
            type_id: 35833, // Fortizar (supported market structure)
            owner_corp_id,
            solar_system_id: 30000142,
            name: "Market Fortizar".to_string(),
            position: [0.0, 0.0, 0.0],
            state: state.to_string(),
            quantum_core_installed: quantum_core,
            fuel_blocks,
            shield_hp: 1.0,
            armor_hp: 1.0,
            hull_hp: 1.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );
    let svc_item_id = structure_id + 10_000;
    snap.items.insert(
        svc_item_id,
        InventoryItemRecord {
            item_id: svc_item_id,
            type_id: 35_892, // Standup Market Hub I
            owner_id: owner_corp_id,
            location_id: structure_id,
            flag: InventoryFlag::ServiceSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    snap.module_online_states.insert(svc_item_id, service_online);
}

#[test]
fn encode_order_attributes_defaults_custom_meta_and_private_fields() {
    let mut snap = WorldStateSnapshot::default();
    let order = MarketOrderRecord {
        order_id: 9001,
        owner_id: 2110_001,
        station_id: 6000_3760,
        region_id: 1000_0002,
        type_id: 34,
        is_bid: true,
        price: IskCents(123_45),
        vol_entered: 500,
        vol_remaining: 250,
        min_volume: 10,
        is_npc: false,
        escrow: IskCents(99_999),
    };

    // Public (include_private_details = false) with missing meta -> default duration=90, issued=1_760_000_000, no field 1 or 15
    let pub_bytes = encode_order_attributes(&snap, &order, true, false);
    let pub_wire = WireMessage::decode(&pub_bytes).expect("decode public attrs");
    assert!(
        !pub_wire.fields.iter().any(|f| f.field_number == 1),
        "owner_id omitted on public attributes"
    );
    assert!(
        !pub_wire.fields.iter().any(|f| f.field_number == 15),
        "escrow omitted on public attributes"
    );
    assert_eq!(extract_varints(&pub_wire, 6), vec![90]);
    let pub_ts: Vec<u64> = extract_messages(&pub_wire, 8)
        .into_iter()
        .flat_map(|m| extract_varints(&m, 1))
        .collect();
    assert_eq!(pub_ts, vec![1_760_000_000]);
    assert_eq!(extract_varints(&pub_wire, 9), vec![10]);
    assert_eq!(extract_varints(&pub_wire, 10), vec![250]);
    assert_eq!(extract_varints(&pub_wire, 11), vec![500]);

    // Private with custom meta + is_buy=true -> includes owner_id (field 1) and escrow (field 15) and custom meta
    snap.market_order_meta.insert(
        9001,
        MarketOrderMeta {
            issued_unix: 1_765_000_123,
            duration_days: 30,
            range: "region".to_string(),
            issued_by: Some(2110_001),
            wallet_division: Some(1000),
            is_corporation: false,
            state: None,
        },
    );
    let priv_bytes = encode_order_attributes(&snap, &order, true, true);
    let priv_wire = WireMessage::decode(&priv_bytes).expect("decode private attrs");
    assert!(!extract_messages(&priv_wire, 1).is_empty(), "owner_id included");
    assert!(!extract_messages(&priv_wire, 15).is_empty(), "escrow included for private buy");
    assert_eq!(extract_varints(&priv_wire, 6), vec![30]);
    let priv_ts: Vec<u64> = extract_messages(&priv_wire, 8)
        .into_iter()
        .flat_map(|m| extract_varints(&m, 1))
        .collect();
    assert_eq!(priv_ts, vec![1_765_000_123]);

    // Private sell order (is_buy = false) -> includes owner_id, omits escrow
    let sell_bytes = encode_order_attributes(&snap, &order, false, true);
    let sell_wire = WireMessage::decode(&sell_bytes).expect("decode sell attrs");
    assert!(!extract_messages(&sell_wire, 1).is_empty());
    assert!(
        !sell_wire.fields.iter().any(|f| f.field_number == 15),
        "sell orders must not encode escrow"
    );
}

#[test]
fn can_caller_view_market_location_enforces_structure_online_and_npc_station_fallback() {
    let mut snap = WorldStateSnapshot::default();
    // Non-structure location (NPC station) -> always visible
    assert!(can_caller_view_market_location(&snap, 0, 6000_3760));
    assert!(can_caller_view_market_location(&snap, 9999, 6000_3760));

    // Offline structure (no fuel) -> hidden
    seed_upwell_with_market_service(&mut snap, 1_000_000_001, 98_000_001, "ShieldVulnerable", true, 0, true);
    assert!(!can_caller_view_market_location(&snap, 9999, 1_000_000_001));

    // Missing quantum core -> hidden
    seed_upwell_with_market_service(&mut snap, 1_000_000_002, 98_000_001, "ShieldVulnerable", false, 500, true);
    assert!(!can_caller_view_market_location(&snap, 9999, 1_000_000_002));

    // HullReinforced -> hidden even with core & fuel
    seed_upwell_with_market_service(&mut snap, 1_000_000_003, 98_000_001, "HullReinforced", true, 500, true);
    assert!(!can_caller_view_market_location(&snap, 9999, 1_000_000_003));

    // Market hub service module offline -> hidden
    seed_upwell_with_market_service(&mut snap, 1_000_000_004, 98_000_001, "ShieldVulnerable", true, 500, false);
    assert!(!can_caller_view_market_location(&snap, 9999, 1_000_000_004));
}

#[test]
fn resolve_caller_corp_id_prefers_character_record_unless_default_npc_corp() {
    let mut snap = WorldRepository::create_demo_snapshot();
    let tmpl = snap.characters.get(&90_000_001).unwrap().clone();

    let mut ch_real = tmpl.clone();
    ch_real.character_id = 5001;
    ch_real.corporation_id = 98_000_777;
    snap.characters.insert(5001, ch_real);

    let mut ch_npc = tmpl;
    ch_npc.character_id = 5002;
    ch_npc.corporation_id = 1_000_009;
    snap.characters.insert(5002, ch_npc);

    // Non-1000009 char corp overrides ctx.effective_corporation_id()
    let ctx_real = auth_ctx(Some(5001), Some(98_000_111), 0);
    assert_eq!(resolve_caller_corp_id(&ctx_real, &snap, 5001), 98_000_777);

    // 1000009 char corp prefers ctx.effective_corporation_id()
    let ctx_npc = auth_ctx(Some(5002), Some(98_000_222), 0);
    assert_eq!(resolve_caller_corp_id(&ctx_npc, &snap, 5002), 98_000_222);

    // Missing char falls back to ctx corp or 0
    let ctx_missing = auth_ctx(Some(5999), Some(98_000_333), 0);
    assert_eq!(resolve_caller_corp_id(&ctx_missing, &snap, 5999), 98_000_333);
    let ctx_none = auth_ctx(Some(5999), None, 0);
    assert_eq!(resolve_caller_corp_id(&ctx_none, &snap, 5999), 0);
}

#[test]
fn authorize_corp_financial_access_checks_auth_membership_and_role_isolation() {
    let mut snap = WorldRepository::create_demo_snapshot();
    let target_corp = 98_100_001_i64;
    let other_corp = 98_100_002_i64;

    let mut ch = snap.characters.get(&90_000_001).unwrap().clone();
    ch.character_id = 7001;
    ch.corporation_id = other_corp;
    snap.characters.insert(7001, ch);

    let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = target_corp; __corp.name = "Test Corp".to_string(); __corp.ticker = "TSTC".to_string(); __corp.ceo_id = 7099; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = Default::default(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = Vec::new(); __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = None; let __mw = IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp };
    corp.members.insert(7001); // member of target_corp, but without role
    snap.corporations.insert(target_corp, corp);

    // 1. Unauthenticated -> 401
    let unauth = auth_ctx(None, None, corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        authorize_corp_financial_access(&unauth, &snap, target_corp).map_err(|(c, _)| c),
        Err(STATUS_UNAUTHORIZED)
    );

    // 2. target_corp <= 0 -> 403
    let no_corp = auth_ctx(Some(7999), None, corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        authorize_corp_financial_access(&no_corp, &snap, 0).map_err(|(c, _)| c),
        Err(STATUS_FORBIDDEN)
    );

    // 3. ctx.identity.roles must NOT grant access to a DIFFERENT corp even if char is listed in target_corp.members
    let valid_ctx = auth_ctx(Some(7001), Some(other_corp), corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        authorize_corp_financial_access(&valid_ctx, &snap, target_corp).map_err(|(c, _)| c),
        Err(STATUS_FORBIDDEN)
    );

    // 4. Member with no role -> 403; adding ROLE_TRADER in corp.member_roles -> Ok(target_corp)
    snap.corporations.get_mut(&target_corp).unwrap().member_roles.insert(7001, corp_roles::ROLE_TRADER);
    assert_eq!(authorize_corp_financial_access(&valid_ctx, &snap, target_corp), Ok(target_corp));

    // 5. Remove member_roles, grant via snap.corp_role_assignments -> Ok(target_corp)
    snap.corporations.get_mut(&target_corp).unwrap().member_roles.clear();
    snap.corp_role_assignments.insert(
        (target_corp, 7001),
        CorpMemberRoleAssignment {
            character_id: 7001,
            roles: corp_roles::ROLE_JUNIOR_ACCOUNTANT,
            ..Default::default()
        },
    );
    assert_eq!(authorize_corp_financial_access(&valid_ctx, &snap, target_corp), Ok(target_corp));

    // 6. Non-member outsider -> 403
    let outsider = auth_ctx(Some(7002), Some(other_corp), corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        authorize_corp_financial_access(&outsider, &snap, target_corp).map_err(|(c, _)| c),
        Err(STATUS_FORBIDDEN)
    );

    // 7. CEO always allowed
    let ceo = auth_ctx(Some(7099), Some(target_corp), 0);
    assert_eq!(authorize_corp_financial_access(&ceo, &snap, target_corp), Ok(target_corp));
}

#[test]
fn market_get_orders_and_for_all_types_respect_zero_vol_and_200_cap_and_filters() {
    let (db, router) = setup();
    let s = sess(Some(10), Some(2110_010), Some(1_000_009), 0);

    db.with_mut_snapshot(|snap| {
        // Insert 205 open sell orders for region 1000_0002 + 1 exhausted order (vol_remaining = 0) + 1 other region
        for i in 0..205 {
            snap.market_orders.insert(
                50_000 + i,
                MarketOrderRecord {
                    order_id: 50_000 + i,
                    owner_id: 2110_010,
                    station_id: 6000_3760,
                    region_id: 1000_0002,
                    type_id: 34,
                    is_bid: false,
                    price: IskCents(500 + i),
                    vol_entered: 100,
                    vol_remaining: 50,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents(0),
                },
            );
        }
        snap.market_orders.insert(
            60_001,
            MarketOrderRecord {
                order_id: 60_001,
                owner_id: 2110_010,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 35,
                is_bid: false,
                price: IskCents(600),
                vol_entered: 10,
                vol_remaining: 0, // exhausted -> must be excluded from active endpoints
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            60_002,
            MarketOrderRecord {
                order_id: 60_002,
                owner_id: 2110_010,
                station_id: 6000_3760,
                region_id: 1000_0043,
                type_id: 36,
                is_bid: true,
                price: IskCents(700),
                vol_entered: 20,
                vol_remaining: 20,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(14_000),
            },
        );
    });

    // GetForAllTypesRequest caps at 200
    let req_all = pb::message("eve.market.price.GetForAllTypesRequest")
        .set("head", true)
        .must_encode();
    let resp_all = call(&router, &s, "eve.market.price.GetForAllTypesRequest", req_all, 1_760_000_100);
    assert_eq!(resp_all.status_code, STATUS_OK);
    let w_all = WireMessage::decode(&resp_all.payload.unwrap().value).unwrap();
    assert_eq!(extract_messages(&w_all, 1).len(), 200, "GetForAllTypesRequest must cap at 200");

    // GetOrdersRequest with empty type_ids returns empty
    let req_empty = pb::message("eve.market.orders.GetOrdersRequest")
        .set("station_id.sequential", 1000_0002u64)
        .must_encode();
    let resp_empty = call(&router, &s, "eve.market.orders.GetOrdersRequest", req_empty, 1_760_000_100);
    assert_eq!(resp_empty.status_code, STATUS_OK);
    let w_empty = WireMessage::decode(&resp_empty.payload.unwrap().value).unwrap();
    assert!(extract_messages(&w_empty, 1).is_empty());

    // GetTypesWithActiveOrdersInRegionRequest excludes exhausted type 35
    let req_types = pb::message("eve.market.orders.GetTypesWithActiveOrdersInRegionRequest")
        .set("region.sequential", 1000_0002u64)
        .must_encode();
    let resp_types = call(
        &router,
        &s,
        "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest",
        req_types,
        1_760_000_100,
    );
    assert_eq!(resp_types.status_code, STATUS_OK);
    let w_types = WireMessage::decode(&resp_types.payload.unwrap().value).unwrap();
    let returned_types: Vec<u64> = extract_messages(&w_types, 1)
        .into_iter()
        .flat_map(|m| extract_varints(&m, 1))
        .collect();
    assert!(returned_types.contains(&34));
    assert!(!returned_types.contains(&35), "vol_remaining==0 type must not be reported active");
    assert!(!returned_types.contains(&36), "other region must not be included");
}

#[test]
fn public_buy_sell_region_endpoints_respect_no_type_filter_flag_and_item_type() {
    let (db, router) = setup();
    let s = sess(Some(11), Some(2110_011), Some(1_000_009), 0);

    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            70_001,
            MarketOrderRecord {
                order_id: 70_001,
                owner_id: 2110_011,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 34,
                is_bid: true,
                price: IskCents(400),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(40_000),
            },
        );
        snap.market_orders.insert(
            70_002,
            MarketOrderRecord {
                order_id: 70_002,
                owner_id: 2110_011,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 35,
                is_bid: true,
                price: IskCents(500),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(50_000),
            },
        );
        snap.market_orders.insert(
            70_003,
            MarketOrderRecord {
                order_id: 70_003,
                owner_id: 2110_011,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 35,
                is_bid: false,
                price: IskCents(600),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    // no_type_filter = true (field 2), even with field 3 = 34 -> returns both type 34 and 35 buy orders
    let region_bytes = pb::message("eve.region.Identifier")
        .set("sequential", 1000_0002u64)
        .must_encode();
    let type34_bytes = pb::message("eve.inventory.genericitemtype.Identifier")
        .set("sequential", 34u64)
        .must_encode();
    let req_all_buy = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::LengthDelimited(region_bytes.clone()) },
            WireField { field_number: 3, value: WireValue::LengthDelimited(type34_bytes) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();
    let resp_buy_all = call(
        &router,
        &s,
        "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest",
        req_all_buy,
        1_760_000_100,
    );
    assert_eq!(resp_buy_all.status_code, STATUS_OK);
    let w_buy_all = WireMessage::decode(&resp_buy_all.payload.unwrap().value).unwrap();
    let buy_all_ids: Vec<u64> = extract_messages(&w_buy_all, 1)
        .into_iter()
        .flat_map(|o| extract_messages(&o, 1))
        .flat_map(|id| extract_varints(&id, 1))
        .collect();
    assert!(buy_all_ids.contains(&70_001));
    assert!(buy_all_ids.contains(&70_002));
    assert!(!buy_all_ids.contains(&70_003));

    // sellorder with item_type=35 and no_type_filter=false -> returns only sell order 70_003
    let type35_bytes = pb::message("eve.inventory.genericitemtype.Identifier")
        .set("sequential", 35u64)
        .must_encode();
    let req_sell = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::LengthDelimited(region_bytes) },
            WireField { field_number: 2, value: WireValue::Varint(0) },
            WireField { field_number: 3, value: WireValue::LengthDelimited(type35_bytes) },
        ],
    }
    .encode();
    let resp_sell = call(
        &router,
        &s,
        "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest",
        req_sell,
        1_760_000_100,
    );
    assert_eq!(resp_sell.status_code, STATUS_OK);
    let w_sell = WireMessage::decode(&resp_sell.payload.unwrap().value).unwrap();
    let sell_ids: Vec<u64> = extract_messages(&w_sell, 1)
        .into_iter()
        .flat_map(|o| extract_messages(&o, 1))
        .flat_map(|id| extract_varints(&id, 1))
        .collect();
    assert_eq!(sell_ids, vec![70_003]);
}

#[test]
fn character_and_corporation_open_vs_history_and_station_structure_filters() {
    let (db, router) = setup();
    let unauth = sess(Some(1), None, None, 0);
    assert_eq!(
        call(&router, &unauth, "eve.market.orders.GetOpenForCharacterRequest", Vec::new(), 1_760_000_100)
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &unauth, "eve.market.orders.GetHistoryForCharacterRequest", Vec::new(), 1_760_000_100)
            .status_code,
        STATUS_FORBIDDEN
    );

    let corp_id = 98_200_001_i64;
    let ceo_id = 2110_050_i64;
    let outsider_id = 2110_051_i64;

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Market Corp".to_string(); __corp.ticker = "MRKT".to_string(); __corp.ceo_id = ceo_id; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = [ceo_id].into_iter().collect(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = Vec::new(); __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = None; let __mw = IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        // Active character order in station 6000_3760
        snap.market_orders.insert(
            80_001,
            MarketOrderRecord {
                order_id: 80_001,
                owner_id: ceo_id,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Exhausted character order (vol_remaining == 0) -> only in history!
        snap.market_orders.insert(
            80_002,
            MarketOrderRecord {
                order_id: 80_002,
                owner_id: ceo_id,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: 34,
                is_bid: true,
                price: IskCents(450),
                vol_entered: 10,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Corp order (issued_by corp or owner_id = corp_id) in structure 1_000_000_099
        snap.market_orders.insert(
            80_003,
            MarketOrderRecord {
                order_id: 80_003,
                owner_id: corp_id,
                station_id: 1_000_000_099,
                region_id: 1000_0002,
                type_id: 35,
                is_bid: false,
                price: IskCents(900),
                vol_entered: 20,
                vol_remaining: 20,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            80_004,
            MarketOrderRecord {
                order_id: 80_004,
                owner_id: corp_id,
                station_id: 1_000_000_099,
                region_id: 1000_0002,
                type_id: 35,
                is_bid: false,
                price: IskCents(900),
                vol_entered: 20,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let ceo_s = sess(Some(50), Some(ceo_id), Some(corp_id), 0);
    let out_s = sess(Some(51), Some(outsider_id), Some(1_000_009), 0);

    let extract_composite_ids = |bytes: &[u8]| -> Vec<u64> {
        extract_messages(&WireMessage::decode(bytes).unwrap(), 1)
            .into_iter()
            .flat_map(|o| {
                extract_messages(&o, 1)
                    .into_iter()
                    .chain(extract_messages(&o, 2))
            })
            .flat_map(|entry| extract_messages(&entry, 1))
            .flat_map(|id| extract_varints(&id, 1))
            .collect()
    };

    // Character open vs history
    let ch_open = call(&router, &ceo_s, "eve.market.orders.GetOpenForCharacterRequest", Vec::new(), 1_760_000_100);
    let ch_open_ids = extract_composite_ids(&ch_open.payload.unwrap().value);
    assert_eq!(ch_open_ids, vec![80_001]);

    let ch_hist = call(&router, &ceo_s, "eve.market.orders.GetHistoryForCharacterRequest", Vec::new(), 1_760_000_100);
    let ch_hist_ids = extract_composite_ids(&ch_hist.payload.unwrap().value);
    assert!(ch_hist_ids.contains(&80_001) && ch_hist_ids.contains(&80_002));

    // Outsider cannot query corp open or history
    assert_eq!(
        call(&router, &out_s, "eve.market.orders.GetOpenForCorporationRequest", Vec::new(), 1_760_000_100).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &out_s, "eve.market.orders.GetHistoryForCorporationRequest", Vec::new(), 1_760_000_100).status_code,
        STATUS_FORBIDDEN
    );

    // CEO gets corp open (only 80_003) and history (80_003 + 80_004)
    let corp_open = call(&router, &ceo_s, "eve.market.orders.GetOpenForCorporationRequest", Vec::new(), 1_760_000_100);
    assert_eq!(corp_open.status_code, STATUS_OK);
    let corp_open_ids = extract_composite_ids(&corp_open.payload.unwrap().value);
    assert_eq!(corp_open_ids, vec![80_003]);

    let corp_hist = call(&router, &ceo_s, "eve.market.orders.GetHistoryForCorporationRequest", Vec::new(), 1_760_000_100);
    assert_eq!(corp_hist.status_code, STATUS_OK);
    let corp_hist_ids = extract_composite_ids(&corp_hist.payload.unwrap().value);
    assert!(corp_hist_ids.contains(&80_003) && corp_hist_ids.contains(&80_004));

    // Station & Structure filters exclude vol_remaining==0
    let req_st = pb::message("eve.market.orders.GetOpenInStationRequest")
        .set("station.sequential", 6000_3760u64)
        .must_encode();
    let resp_st = call(&router, &ceo_s, "eve.market.orders.GetOpenInStationRequest", req_st, 1_760_000_100);
    let st_ids = extract_composite_ids(&resp_st.payload.unwrap().value);
    assert!(st_ids.contains(&80_001) && !st_ids.contains(&80_002) && !st_ids.contains(&80_003));

    let req_str = pb::message("eve.market.orders.GetOpenInStructureRequest")
        .set("structure.sequential", 1_000_000_099u64)
        .must_encode();
    let resp_str = call(&router, &ceo_s, "eve.market.orders.GetOpenInStructureRequest", req_str, 1_760_000_100);
    let str_ids = extract_composite_ids(&resp_str.payload.unwrap().value);
    assert_eq!(str_ids, vec![80_003]);
}

#[test]
fn can_caller_view_market_location_register_and_authorize_corp_financial_access_edges() {
    let (db, router) = setup();
    let corp_id = 98_777_001_i64;
    let other_corp_id = 98_777_002_i64;
    let cid_db_only = 90_777_001_i64;
    let cid_sess_only = 90_777_002_i64;
    let cid_both_roles = 90_777_003_i64;
    let cid_wrong_corp_with_sess_role = 90_777_004_i64;
    let cid_outsider = 90_777_005_i64;
    let destroyed_st_id = 1_000_000_777_001_i64;
    let open_station_id = 60_003_760_i64;

    db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Fin Corp".into();
        corp.ticker = "FIN".into();
        corp.ceo_id = 90_777_099;
        corp.members.insert(cid_db_only);
        corp.members.insert(cid_sess_only);
        corp.members.insert(cid_both_roles);
        corp.members.insert(cid_wrong_corp_with_sess_role);
        corp.member_roles.insert(cid_db_only, corp_roles::ROLE_ACCOUNTANT);
        corp.member_roles.insert(cid_both_roles, corp_roles::ROLE_ACCOUNTANT);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        let tpl = snap.characters.values().next().unwrap().clone();
        for (cid, c_corp) in [
            (cid_db_only, corp_id),
            (cid_sess_only, corp_id),
            (cid_both_roles, corp_id),
            (cid_wrong_corp_with_sess_role, other_corp_id),
            (cid_outsider, other_corp_id),
        ] {
            let mut ch = tpl.clone();
            ch.character_id = cid;
            ch.name = format!("FinChar {cid}");
            ch.corporation_id = c_corp;
            ch.alliance_id = None;
            snap.characters.insert(cid, ch);
        }

        seed_upwell_with_market_service(
            snap,
            destroyed_st_id,
            corp_id,
            "Destroyed",
            true,
            100,
            true,
        );
    });

    // 1. `can_caller_view_market_location`: NPC station returns true, destroyed structure returns false
    db.with_snapshot(|snap| {
        assert!(can_caller_view_market_location(snap, cid_outsider, open_station_id));
        assert!(!can_caller_view_market_location(snap, cid_db_only, destroyed_st_id));
    });

    // 2. `register_market_handlers` handles registered request (not unimplemented)
    let ping_s = sess(Some(77), Some(cid_db_only), Some(corp_id), 0);
    let ping_resp = call(
        &router,
        &ping_s,
        "eve.market.price.GetForAllTypesRequest",
        Vec::new(),
        1_760_000_500,
    );
    assert_eq!(ping_resp.status_code, STATUS_OK);

    // 3. `authorize_corp_financial_access`:
    //    - Returns exact `Ok(corp_id)` (> 1) when DB-only, session-only, or both roles present on same corp
    //    - Rejects cross-corp member carrying session role (`caller_corp == target_corp` vs `!=`)
    //    - XOR (`| -> ^`) is killed when BOTH DB role and session role are set simultaneously!
    db.with_snapshot(|snap| {
        let a_db = auth_ctx(Some(cid_db_only), Some(corp_id), 0);
        assert_eq!(authorize_corp_financial_access(&a_db, snap, corp_id), Ok(corp_id));

        let a_sess = auth_ctx(Some(cid_sess_only), Some(corp_id), corp_roles::ROLE_ACCOUNTANT);
        assert_eq!(authorize_corp_financial_access(&a_sess, snap, corp_id), Ok(corp_id));

        let a_both = auth_ctx(Some(cid_both_roles), Some(corp_id), corp_roles::ROLE_ACCOUNTANT);
        assert_eq!(authorize_corp_financial_access(&a_both, snap, corp_id), Ok(corp_id));

        let a_wrong_corp = auth_ctx(
            Some(cid_wrong_corp_with_sess_role),
            Some(other_corp_id),
            corp_roles::ROLE_ACCOUNTANT,
        );
        assert_eq!(
            authorize_corp_financial_access(&a_wrong_corp, snap, corp_id).unwrap_err().0,
            STATUS_FORBIDDEN
        );

        let a_out = auth_ctx(Some(cid_outsider), Some(other_corp_id), corp_roles::ROLE_DIRECTOR);
        assert_eq!(
            authorize_corp_financial_access(&a_out, snap, corp_id).unwrap_err().0,
            STATUS_FORBIDDEN
        );
    });
}
