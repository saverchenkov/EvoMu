//! Integration tests closing parity for LP store offer browsing and purchases:
//! - `rpc:LPStoreMgr.GetAvailableOffersFromCorp`
//! - `rpc:LPStoreMgr.TakeOfferForCharacter`
//! - `rpc:LPStoreMgr.TakeOfferForCorporation`
//! - `feat:lp.lp-store-offers`
//! - `feat:lp.lp-store-buy`

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::lp_store::{
    load_corp_lp_balances, register_lp_store_offer, save_corp_lp_balances, LPOffer,
};
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const CORP_ID: i64 = 98_000_001;
const JITA_CALDARI_STATION: i64 = 60_003_760; // Owner: 1000035 (Caldari Navy)
const POINEN_FEDERATION_STATION: i64 = 60_003_934; // Owner: 1000036 (Federation Navy)

const CALDARI_NAVY_CORP: i64 = 1_000_035;
const FEDERATION_NAVY_CORP: i64 = 1_000_036;

const TRITANIUM_TYPE_ID: u32 = 34;
const CARACAL_TYPE_ID: u32 = 621;
const BALLISTIC_CONTROL_TYPE_ID: u32 = 15_810;
const CONDOR_SHIP_TYPE_ID: u32 = 583;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn setup_docked_pilot(ctx: &ServerContext, char_id: i64, station_id: i64, ship_id: i64) -> SessionState {
    let mut sess = SessionState::new(5501);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("character exists");
        ch.character_id = char_id;
        ch.corporation_id = CORP_ID;
        ch.active_ship_id = ship_id;
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(50_000_000);
        ch.loyalty_points.insert(CALDARI_NAVY_CORP, 100_000);
        ch.loyalty_points.insert(FEDERATION_NAVY_CORP, 50_000);
        ch.standings.insert(CALDARI_NAVY_CORP, 0.0);

        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == TRITANIUM_TYPE_ID));

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: CONDOR_SHIP_TYPE_ID,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    sess
}

#[tokio::test]
async fn test_lp_store_offers_browsing_and_station_corp_resolution() {
    // DATA GAP: requires SDE/live store catalog import (feat:lp.lp-store-offers)
    // DATA GAP: requires server-only offer catalog (rpc:LPStoreMgr.GetAvailableOffersFromCorp)

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let ship_id = 95_000_001;
    let mut sess = setup_docked_pilot(&ctx, CHAR_ID, JITA_CALDARI_STATION, ship_id).await;

    // Seed test offers for Caldari Navy and Federation Navy
    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(snap, LPOffer {
            offer_id: 10_001,
            corp_id: CALDARI_NAVY_CORP,
            type_id: BALLISTIC_CONTROL_TYPE_ID,
            quantity: 1,
            lp_cost: 2_400,
            isk_cost: IskCents::from_isk(2_400_000),
            req_items: vec![(TRITANIUM_TYPE_ID, 100)],
            ak_cost: 0,
            min_standing: Some(1.5),
        });
        register_lp_store_offer(snap, LPOffer {
            offer_id: 10_002,
            corp_id: FEDERATION_NAVY_CORP,
            type_id: CARACAL_TYPE_ID,
            quantity: 1,
            lp_cost: 15_000,
            isk_cost: IskCents::from_isk(15_000_000),
            req_items: vec![],
            ak_cost: 0,
            min_standing: None,
        });
    });

    // 1. Browsing with empty args while docked at Jita (Caldari Navy station) resolves Caldari Navy offers
    let def_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .expect("GetAvailableOffersFromCorp with station default");

    let PyRep::List(ref def_offers) = def_rep else {
        panic!("expected PyRep::List of offers");
    };
    let caldari_offer = def_offers
        .iter()
        .find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(10_001))
        .expect("offer 10001 should be present in Caldari Navy catalog");

    assert_eq!(caldari_offer.dict_get("corpID").and_then(|v| v.as_i64()), Some(CALDARI_NAVY_CORP));
    assert_eq!(caldari_offer.dict_get("typeID").and_then(|v| v.as_i64()), Some(BALLISTIC_CONTROL_TYPE_ID as i64));
    assert_eq!(caldari_offer.dict_get("lpCost").and_then(|v| v.as_i64()), Some(2_400));
    let isk_cost = caldari_offer.dict_get("iskCost").and_then(|v| v.as_f64()).expect("iskCost");
    assert!((isk_cost - 2_400_000.0).abs() < 0.01);
    let min_st = caldari_offer.dict_get("minStanding").and_then(|v| v.as_f64()).expect("minStanding");
    assert!((min_st - 1.5).abs() < 0.01);

    let PyRep::List(ref req_items) = caldari_offer.dict_get("reqItems").expect("reqItems should be list") else {
        panic!("reqItems must be a list");
    };
    assert_eq!(req_items.len(), 1);
    assert_eq!(req_items[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(TRITANIUM_TYPE_ID as i64));
    assert_eq!(req_items[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(100));

    // 2. Querying explicit corp ID 1000036 (Federation Navy) returns Federation Navy offers
    let fed_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(FEDERATION_NAVY_CORP)])
        .await
        .expect("GetAvailableOffersFromCorp explicit corp");

    let PyRep::List(ref fed_offers) = fed_rep else {
        panic!("expected PyRep::List of offers");
    };
    let fed_offer = fed_offers
        .iter()
        .find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(10_002))
        .expect("offer 10002 should be present in Federation Navy catalog");
    assert_eq!(fed_offer.dict_get("corpID").and_then(|v| v.as_i64()), Some(FEDERATION_NAVY_CORP));

    // 3. Docking at Poinen station (Federation Navy station) resolves Federation Navy offers by default
    sess.station_id = Some(POINEN_FEDERATION_STATION);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(POINEN_FEDERATION_STATION);
    });
    let def_fed_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .expect("GetAvailableOffersFromCorp default at Fed Navy station");
    let PyRep::List(ref def_fed_offers) = def_fed_rep else {
        panic!("expected PyRep::List");
    };
    assert!(def_fed_offers.iter().any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(10_002)));

    // 4. Querying an unknown corp returns an empty list
    let empty_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(999_888_777)])
        .await
        .expect("GetAvailableOffersFromCorp unknown corp");
    assert_eq!(empty_rep, PyRep::List(vec![]));
}

#[tokio::test]
async fn test_lp_store_character_purchase_costs_standings_and_cargo_consumption() {
    // parity: rpc:LPStoreMgr.TakeOfferForCharacter
    // parity: feat:lp.lp-store-buy
    eve_testkit::attest(&[
        "rpc:LPStoreMgr.TakeOfferForCharacter",
        "feat:lp.lp-store-buy",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let ship_id = 95_000_002;
    let mut sess = setup_docked_pilot(&ctx, CHAR_ID, JITA_CALDARI_STATION, ship_id).await;

    // Register offers:
    // - 20001: standard offer without trade-in items
    // - 20002: standing-gated offer (requires standing >= 2.0)
    // - 20003: requires trade-in items (50x Tritanium)
    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(snap, LPOffer {
            offer_id: 20_001,
            corp_id: CALDARI_NAVY_CORP,
            type_id: BALLISTIC_CONTROL_TYPE_ID,
            quantity: 1,
            lp_cost: 2_000,
            isk_cost: IskCents::from_isk(1_000_000),
            req_items: vec![],
            ak_cost: 0,
            min_standing: None,
        });
        register_lp_store_offer(snap, LPOffer {
            offer_id: 20_002,
            corp_id: CALDARI_NAVY_CORP,
            type_id: BALLISTIC_CONTROL_TYPE_ID,
            quantity: 1,
            lp_cost: 1_000,
            isk_cost: IskCents::from_isk(500_000),
            req_items: vec![],
            ak_cost: 0,
            min_standing: Some(2.0),
        });
        register_lp_store_offer(snap, LPOffer {
            offer_id: 20_003,
            corp_id: CALDARI_NAVY_CORP,
            type_id: BALLISTIC_CONTROL_TYPE_ID,
            quantity: 1,
            lp_cost: 1_500,
            isk_cost: IskCents::from_isk(800_000),
            req_items: vec![(TRITANIUM_TYPE_ID, 50)],
            ak_cost: 0,
            min_standing: None,
        });
    });

    // 1. Validation: InsufficientLoyaltyPoints
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().loyalty_points.insert(CALDARI_NAVY_CORP, 500); // Need 2000
    });
    let err_lp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_001), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_lp.code, "InsufficientLoyaltyPoints");

    // Restore LP, then validate InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
        ch.balance = IskCents::from_isk(100); // Need 1,000,000 ISK
    });
    let err_fund = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_001), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_fund.code, "InsufficientFunds");

    // Restore ISK balance
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().balance = IskCents::from_isk(50_000_000);
    });

    // 2. Validation: Standing gate (min_standing = 2.0, character has 0.0)
    let err_stand = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_002), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_stand.code, "InsufficientStanding");

    // Boost standing to 3.5 -> offer 20002 succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().standings.insert(CALDARI_NAVY_CORP, 3.5);
    });
    let pass_stand = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_002), PyRep::Integer(1)],
    )
    .await
    .expect("standing gated purchase should succeed");
    assert_eq!(pass_stand.dict_get("offerID").and_then(|v| v.as_i64()), Some(20_002));

    // 3. Validation: MissingRequiredItems
    let err_missing = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_003), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_missing.code, "MissingRequiredItems");

    // 4. Trade-in consumption from ACTIVE SHIP CARGO
    // Put 50x Tritanium in active ship cargo (location_id == ship_id, flag == Cargo)
    let cargo_trit_id = 96_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_trit_id,
            InventoryItemRecord {
                item_id: cargo_trit_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Call TakeOfferForCharacter with 2 args (offer_id, quantity), verifying default station corp resolution
    let buy_cargo_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(20_003), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter taking trade-in from active ship cargo");

    let delivered_id = buy_cargo_rep
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify trade-in item in cargo was consumed
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cargo_trit_id), "trade-in item in ship cargo must be consumed");
        let delivered_item = snap.items.get(&delivered_id).expect("delivered item must exist");
        assert_eq!(delivered_item.owner_id, CHAR_ID);
        assert_eq!(delivered_item.location_id, JITA_CALDARI_STATION);
        assert_eq!(delivered_item.flag, InventoryFlag::Hangar);
        assert_eq!(delivered_item.type_id, BALLISTIC_CONTROL_TYPE_ID as eve_sde::TypeId);
        assert_eq!(delivered_item.quantity, 1);
    });

    // 5. Trade-in consumption from STATION HANGAR
    // Put 100x Tritanium in station hangar (location_id == JITA_CALDARI_STATION, flag == Hangar)
    let hangar_trit_id = 96_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            hangar_trit_id,
            InventoryItemRecord {
                item_id: hangar_trit_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: JITA_CALDARI_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let buy_hangar_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(20_003), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter taking trade-in from station hangar");

    let hangar_delivered_id = buy_hangar_rep
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let remaining_trit = snap.items.get(&hangar_trit_id).expect("hangar trit remaining item");
        assert_eq!(remaining_trit.quantity, 50, "100 - 50 = 50 tritanium remaining in station hangar");

        let delivered = snap.items.get(&hangar_delivered_id).expect("hangar delivered item");
        assert_eq!(delivered.owner_id, CHAR_ID);
        assert_eq!(delivered.location_id, JITA_CALDARI_STATION);
        assert_eq!(delivered.flag, InventoryFlag::Hangar);
    });
}

#[tokio::test]
async fn test_lp_store_corporation_purchase_and_delivery() {
    // parity: rpc:LPStoreMgr.TakeOfferForCorporation
    eve_testkit::attest(&["rpc:LPStoreMgr.TakeOfferForCorporation"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let ship_id = 95_000_003;
    let mut sess = setup_docked_pilot(&ctx, CHAR_ID, JITA_CALDARI_STATION, ship_id).await;

    // Setup player corporation with wallet & LP balance
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::types::CorporationRecord {
            corporation_id: CORP_ID,
            name: "Parity Test Corp".into(),
            ticker: "PTC".into(),
            ceo_id: CHAR_ID + 9999, // Character is not CEO so role check applies
            alliance_id: None,
            tax_rate: 0.0,
            master_wallet: IskCents::from_isk(100_000_000),
            members: [CHAR_ID].into_iter().collect(),
            member_roles: HashMap::new(),
            applications: HashMap::new(),
            wallet_divisions: vec![],
            description: "".into(),
            url: "".into(),
            hq_station_id: Some(JITA_CALDARI_STATION),
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_isk(50_000_000);
        snap.corporations.insert(CORP_ID, corp);

        let mut corp_lp = HashMap::new();
        corp_lp.insert(CALDARI_NAVY_CORP, 40_000);
        save_corp_lp_balances(snap, CORP_ID, &corp_lp);

        register_lp_store_offer(snap, LPOffer {
            offer_id: 30_001,
            corp_id: CALDARI_NAVY_CORP,
            type_id: BALLISTIC_CONTROL_TYPE_ID,
            quantity: 1,
            lp_cost: 5_000,
            isk_cost: IskCents::from_isk(5_000_000),
            req_items: vec![(TRITANIUM_TYPE_ID, 200)],
            ak_cost: 0,
            min_standing: None,
        });

        // Put 200x Tritanium in corp hangar (owner_id == CORP_ID, flag == Hangar)
        snap.items.insert(
            97_000_001,
            InventoryItemRecord {
                item_id: 97_000_001,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: CORP_ID,
                location_id: JITA_CALDARI_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Role verification: pilot lacking Director/Accountant role fails
    sess.corp_role = 0;
    let err_role = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(30_001), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_role.code, "InsufficientCorporationRole");

    // 2. Grant Director role and execute purchase
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let corp_buy_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(30_001), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCorporation purchase");

    assert_eq!(corp_buy_rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(30_001));
    let corp_deliv_id = corp_buy_rep
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // 3. Verify state effects:
    // - Corp LP balance deducted: 40_000 - 5_000 = 35_000
    // - Corp ISK deducted: 50M - 5M = 45M
    // - Corp trade-in items consumed
    // - Delivered item owned by CORP_ID and flagged CorpDeliveries in station
    ctx.db.with_snapshot(|snap| {
        let balances = load_corp_lp_balances(snap, CORP_ID);
        assert_eq!(balances.get(&CALDARI_NAVY_CORP).copied(), Some(35_000));

        let corp = snap.corporations.get(&CORP_ID).unwrap();
        assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(45_000_000));

        assert!(!snap.items.contains_key(&97_000_001), "corp trade-in item must be consumed");

        let delivered = snap.items.get(&corp_deliv_id).expect("delivered item");
        assert_eq!(delivered.owner_id, CORP_ID);
        assert_eq!(delivered.location_id, JITA_CALDARI_STATION);
        assert_eq!(delivered.flag, InventoryFlag::CorpDeliveries);
        assert_eq!(delivered.type_id, BALLISTIC_CONTROL_TYPE_ID as eve_sde::TypeId);
        assert_eq!(delivered.quantity, 1);
    });
}
