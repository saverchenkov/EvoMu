//! Unit and mutation-killing tests for `quasar_market` (`Q-6`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{CorporationRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db)
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

fn encode_id_req(type_name: &str, id: u64) -> GoogleAny {
    let mut id_msg = Vec::new();
    encode_varint_field(1, id, &mut id_msg);
    let mut req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut req);
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
}

#[test]
fn regional_station_structure_price_history_and_new_entry_notice() {
    // parity: feat:quasar.market
    let mut dm = DynamicQuasarMessage::new("test.Msg");
    dm.set(1, DynamicValue::Uint64(50));
    assert_eq!(extract_u64_from_field(&dm, 1), 50);
    dm.set(2, DynamicValue::Uint32(51));
    assert_eq!(extract_u64_from_field(&dm, 2), 51);
    dm.set(3, DynamicValue::Int64(52));
    assert_eq!(extract_u64_from_field(&dm, 3), 52);
    dm.set(4, DynamicValue::Int32(53));
    assert_eq!(extract_u64_from_field(&dm, 4), 53);

    let (router, db) = make_router_and_db();
    let cid = 211200400i64;
    let region_id = 10000002u32;
    let station_id = 60003760i64;

    // Price history before any orders exist (uses fallback 100_000 cents, count 0)
    let user = make_ctx(Some(cid as u32), 0);
    for rpc in [
        "eve.market.orders.GetPriceHistoryRequest",
        "eve.market.price.history.GetForRegionAndTypeRequest",
        "eve.market.region.GetPriceHistoryForTypeRequest",
    ] {
        let resp = router.dispatch(&user, &encode_id_req(rpc, region_id as u64)).expect(rpc);
        assert!(!resp.value.is_empty());
    }

    // Seed 1 sell order and 1 buy order
    db.try_with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            701,
            MarketOrderRecord {
                order_id: 701,
                owner_id: cid,
                station_id,
                region_id,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(500),
                vol_entered: 1000,
                vol_remaining: 800,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            702,
            MarketOrderRecord {
                order_id: 702,
                owner_id: cid,
                station_id,
                region_id,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(450),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 10,
                is_npc: false,
                escrow: IskCents::from_isk(225_000),
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    for rpc in [
        "eve.market.orders.GetOrdersRequest",
        "eve.market.orders.GetPublicOpenInRegionRequest",
        "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest",
        "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest",
        "eve.market.orders.GetTypesWithActiveOrdersInRegionRequest",
        "eve.market.orders.GetPriceHistoryRequest",
        "eve.market.price.history.GetForRegionAndTypeRequest",
        "eve.market.region.GetPriceHistoryForTypeRequest",
        "eve.market.price.GetForAllTypesRequest",
    ] {
        let resp = router.dispatch(&user, &encode_id_req(rpc, region_id as u64)).expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }

    // Station & structure open orders
    for rpc in [
        "eve.market.orders.GetOpenInStationRequest",
        "eve.market.orders.GetOpenInStructureRequest",
    ] {
        let resp = router.dispatch(&user, &encode_id_req(rpc, station_id as u64)).expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }

    // Publish NewEntryNotice
    let _delivered = publish_market_new_entry_notice(&router, 30000142, 1_760_000_000).expect("publish");
    let entry = router.routing_cache.read().unwrap().entries.back().unwrap().clone();
    assert!(entry.notice.payload.unwrap().type_url.ends_with("NewEntryNotice"));
}

#[test]
fn character_and_corporation_market_order_authorization_and_queries() {
    // parity: feat:quasar.market
    let (router, db) = make_router_and_db();
    let cid = 211200410i64;
    let other_cid = 211200411i64;
    let member_cid = 211200412i64;
    let corp_id = 98000410i64;

    db.try_with_mut_snapshot(|snap| {
        for (id, c_id) in [(cid, corp_id), (other_cid, 98000999), (member_cid, corp_id)] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = id;
            ch.corporation_id = c_id;
            snap.characters.insert(id, ch);
        }
        let corp = CorporationRecord {
            corporation_id: corp_id,
            name: "MarketCorp".to_string(),
            ticker: "MCRP".to_string(),
            ceo_id: cid,
            hq_station_id: Some(60003760),
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents(0),
            members: HashSet::from([cid, member_cid]),
            member_roles: HashMap::new(),
            applications: HashMap::new(),
            wallet_divisions: Vec::new(),
            description: String::new(),
            url: String::new(),
        };
        snap.corporations.insert(corp_id, corp);

        snap.market_orders.insert(
            801,
            MarketOrderRecord {
                order_id: 801,
                owner_id: cid,
                station_id: 60003760,
                region_id: 10000002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(500),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            802,
            MarketOrderRecord {
                order_id: 802,
                owner_id: corp_id,
                station_id: 60003760,
                region_id: 10000002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(600),
                vol_entered: 200,
                vol_remaining: 200,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let anon = make_ctx(None, 0);
    let ceo_ctx = make_ctx(Some(cid as u32), 0);
    let other_ctx = make_ctx(Some(other_cid as u32), 0);
    let mem_ctx = make_ctx(Some(member_cid as u32), 0);

    // 7. GetOpenForCharacterRequest & 8. GetHistoryForCharacterRequest
    for rpc in [
        "eve.market.orders.GetOpenForCharacterRequest",
        "eve.market.orders.GetHistoryForCharacterRequest",
    ] {
        let req_own = encode_id_req(rpc, cid as u64);
        assert_eq!(router.dispatch(&anon, &req_own).unwrap_err().0, STATUS_UNAUTHORIZED);
        assert_eq!(router.dispatch(&other_ctx, &req_own).unwrap_err().0, STATUS_FORBIDDEN);
        assert!(!router.dispatch(&ceo_ctx, &req_own).unwrap().value.is_empty());
    }

    // 9. GetOpenForCorporationRequest & 10. GetHistoryForCorporationRequest
    for rpc in [
        "eve.market.orders.GetOpenForCorporationRequest",
        "eve.market.orders.GetHistoryForCorporationRequest",
    ] {
        let req_corp = encode_id_req(rpc, corp_id as u64);
        assert_eq!(router.dispatch(&anon, &req_corp).unwrap_err().0, STATUS_UNAUTHORIZED);
        assert_eq!(router.dispatch(&other_ctx, &req_corp).unwrap_err().0, STATUS_FORBIDDEN);
        assert_eq!(router.dispatch(&mem_ctx, &req_corp).unwrap_err().0, STATUS_FORBIDDEN);
        assert!(!router.dispatch(&ceo_ctx, &req_corp).unwrap().value.is_empty());
    }

    // 16. character.sellorder.GetRequest
    let char_ord = encode_id_req("eve.market.character.sellorder.GetRequest", 801);
    assert_eq!(router.dispatch(&anon, &char_ord).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(
        router
            .dispatch(&ceo_ctx, &encode_id_req("eve.market.character.sellorder.GetRequest", 999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(router.dispatch(&other_ctx, &char_ord).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(!router.dispatch(&ceo_ctx, &char_ord).unwrap().value.is_empty());

    // 17. corporation.sellorder.GetRequest
    let corp_ord = encode_id_req("eve.market.corporation.sellorder.GetRequest", 802);
    assert_eq!(router.dispatch(&anon, &corp_ord).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(
        router
            .dispatch(&ceo_ctx, &encode_id_req("eve.market.corporation.sellorder.GetRequest", 999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(router.dispatch(&other_ctx, &corp_ord).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(router.dispatch(&mem_ctx, &corp_ord).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(!router.dispatch(&ceo_ctx, &corp_ord).unwrap().value.is_empty());
}
