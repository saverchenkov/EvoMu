//! Integration tests for LPStoreMgr RPCs:
//! - `rpc:LPStoreMgr.TakeOfferForCharacter`
//! - `rpc:LPStoreMgr.TakeOfferForCorporation`
//! - `feat:lp.lp-store-buy`

use crate::common;
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const CORP_ID: i64 = 98_000_001;
const STATION_ID: i64 = 60_003_760;

const CALDARI_NAVY_CORP: i64 = 1_000_035;
const FEDERATION_NAVY_CORP: i64 = 1_000_036;
const AMARR_NAVY_CORP: i64 = 1_000_037;
const REPUBLIC_FLEET_CORP: i64 = 1_000_044;
const CONCORD_CORP: i64 = 1_000_125;
const PARAGON_CORP: i64 = 1_000_419;
const STATE_PROTECTORATE_CORP: i64 = 1_000_180;

const TRITANIUM_TYPE_ID: u32 = 34;
const BALLISTIC_CONTROL_TYPE_ID: u32 = 15810;

fn k_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}

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

async fn setup_docked_session(ctx: &ServerContext) -> SessionState {
    let mut sess = SessionState::new(4201);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = Some(STATION_ID);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess
}

#[tokio::test]
async fn test_lp_store_mgr_get_available_offers() {
    // DATA GAP: requires SDE/live store catalog import (feat:lp.lp-store-offers)
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_docked_session(&ctx).await;

    // 0. Before LP store offers are registered in WorldStateSnapshot, GetAvailableOffersFromCorp returns an empty list
    let empty_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(CALDARI_NAVY_CORP)])
        .await.expect("GetAvailableOffersFromCorp unseeded");
    assert_eq!(empty_rep, PyRep::List(vec![]));

    common::seed_test_lp_store_offers(&ctx);

    // 1. Query offers for Caldari Navy with 1 arg (corp_id)
    let rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(CALDARI_NAVY_CORP)])
        .await.expect("GetAvailableOffersFromCorp Caldari Navy");
    let PyRep::List(ref offers) = rep else { panic!("Expected PyRep::List of offers"); };
    assert!(offers.len() >= 4, "Caldari Navy should have >= 4 catalog offers");

    // Verify structure and fields of offer 1001 (Ballistic Control System)
    let o1001 = offers.iter().find(|o| k_i64(o, "offerID") == Some(1001)).expect("Offer 1001");
    assert_eq!(k_i64(o1001, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(o1001, "typeID"), Some(BALLISTIC_CONTROL_TYPE_ID as i64));
    assert_eq!(k_i64(o1001, "quantity"), Some(1));
    assert_eq!(k_i64(o1001, "lpCost"), Some(2400));
    let isk_cost = o1001.dict_get("iskCost").and_then(|v| v.as_f64()).expect("iskCost");
    assert!((isk_cost - 2_400_000.0).abs() < 0.01);
    let PyRep::List(ref req_items) = o1001.dict_get("reqItems").expect("reqItems") else { panic!("reqItems list"); };
    assert!(req_items.is_empty(), "Offer 1001 has no reqItems");

    // Verify offer 1004 has reqItems (trade-in requirement of 100x Tritanium)
    let o1004 = offers.iter().find(|o| k_i64(o, "offerID") == Some(1004)).expect("Offer 1004");
    let PyRep::List(ref req_1004) = o1004.dict_get("reqItems").expect("reqItems") else { panic!("reqItems list"); };
    assert_eq!(req_1004.len(), 1, "Offer 1004 has 1 trade-in item requirement");
    assert_eq!(k_i64(&req_1004[0], "typeID"), Some(TRITANIUM_TYPE_ID as i64));
    assert_eq!(k_i64(&req_1004[0], "quantity"), Some(100));

    // 2. Query other seeded corps
    for &corp in &[FEDERATION_NAVY_CORP, AMARR_NAVY_CORP, REPUBLIC_FLEET_CORP, CONCORD_CORP, PARAGON_CORP, STATE_PROTECTORATE_CORP] {
        let corp_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(corp)])
            .await.unwrap_or_else(|e| panic!("corp {corp}: {e:?}"));
        let PyRep::List(ref c_offers) = corp_rep else { panic!("list expected"); };
        assert!(!c_offers.is_empty(), "Offers for corp {corp} should not be empty");
        for o in c_offers {
            assert_eq!(k_i64(o, "corpID"), Some(corp));
            assert!(k_i64(o, "offerID").unwrap() > 0);
            assert!(k_i64(o, "typeID").unwrap() > 0);
            assert!(k_i64(o, "quantity").unwrap() > 0);
            assert!(k_i64(o, "lpCost").unwrap() > 0);
        }
    }

    // 3. Query unknown corp returns empty list
    let unk = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(999_999)])
        .await.expect("GetAvailableOffersFromCorp unknown corp");
    let PyRep::List(ref unk_offers) = unk else { panic!("list expected"); };
    assert!(unk_offers.is_empty());

    // 4. Default no args returns Caldari Navy offers
    let def = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await.expect("GetAvailableOffersFromCorp default");
    let PyRep::List(ref def_offers) = def else { panic!("list expected"); };
    assert!(!def_offers.is_empty());
    assert_eq!(k_i64(&def_offers[0], "corpID"), Some(CALDARI_NAVY_CORP));
}

#[tokio::test]
async fn test_lp_store_mgr_take_offer_for_character() {
    // parity: rpc:LPStoreMgr.TakeOfferForCharacter
    // parity: feat:lp.lp-store-buy
    eve_testkit::attest(&[
        "rpc:LPStoreMgr.TakeOfferForCharacter",
        "feat:lp.lp-store-buy",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_docked_session(&ctx).await;
    common::seed_test_lp_store_offers(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == CHAR_ID && it.type_id == TRITANIUM_TYPE_ID));
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.balance = IskCents::from_isk(50_000_000);
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
        }
    });

    // 1. Validation: OfferNotFound
    let err1 = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(999_999), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err1.code, "OfferNotFound");
    let err2 = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(FEDERATION_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err2.code, "OfferNotFound");

    // 2. Validation: InvalidQuantity
    for bad_qty in [0, -1, -50] {
        let eq = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(bad_qty)]).await.unwrap_err();
        assert_eq!(eq.code, "InvalidQuantity");
    }

    // 3. Validation: InsufficientLoyaltyPoints
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().loyalty_points.insert(CALDARI_NAVY_CORP, 1_000);
    });
    let elp = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(elp.code, "InsufficientLoyaltyPoints");

    // 4. Validation: InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
        ch.balance = IskCents::from_isk(100_000);
    });
    let efund = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(efund.code, "InsufficientFunds");

    // 5. Validation: MissingRequiredItems (Offer 1004 needs 100x Tritanium)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().balance = IskCents::from_isk(50_000_000);
    });
    let emiss = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(emiss.code, "MissingRequiredItems");

    // 6. Validation: MustBeDocked
    let (saved_st, saved_str) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        let old = (ch.station_id, ch.structure_id);
        ch.station_id = None;
        ch.structure_id = None;
        old
    });
    sess.station_id = None;
    sess.structure_id = None;
    let edock = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(edock.message, "MustBeDocked");
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        ch.station_id = saved_st;
        ch.structure_id = saved_str;
    });
    sess.station_id = saved_st;
    sess.structure_id = saved_str;

    // 7. Successful purchase without trade-in items (Offer 1001, 2x): 4,800 LP, 4.8M ISK
    let res_1001 = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(2)])
        .await.expect("TakeOfferForCharacter 1001 2x");

    assert_eq!(k_i64(&res_1001, "offerID"), Some(1001));
    assert_eq!(k_i64(&res_1001, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(&res_1001, "quantity"), Some(2));
    assert_eq!(k_i64(&res_1001, "remainingLP"), Some(50_000 - 4_800));
    let delivered_id = k_i64(&res_1001, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&delivered_id).expect("Delivered item exists");
        assert_eq!(it.type_id, BALLISTIC_CONTROL_TYPE_ID);
        assert_eq!(it.owner_id, CHAR_ID);
        assert_eq!(it.location_id, STATION_ID);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(it.quantity, 2);

        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.loyalty_points.get(&CALDARI_NAVY_CORP).copied(), Some(45_200));
        assert_eq!(ch.balance, IskCents::from_isk(50_000_000 - 4_800_000));
        assert!(snap.wallet_journal.iter().any(|j| j.character_id == CHAR_ID && j.entry_type == "LPStorePurchase" && j.amount == IskCents::from_isk(4_800_000)));
        let logs = snap.char_lp_logs.get(&CHAR_ID).expect("LP logs");
        assert!(logs.iter().any(|l| l.corp_id == CALDARI_NAVY_CORP && l.amount == -4_800));
    });

    // Verify OnLPChange notification signature: (change_type, issuer_corp_id, lp_before, lp_after)
    let notifs = ctx.drain_notifications_for_char(CHAR_ID);
    let lp_notif = notifs.iter().find(|n| n.event == "OnLPChange").expect("OnLPChange notification");
    assert_eq!(
        lp_notif.args,
        vec![
            PyRep::String("take_offer".into()),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(50_000),
            PyRep::Integer(45_200),
        ]
    );

    // 8. Successful purchase WITH trade-in items (Offer 1004, 2x): requires 200 Tritanium
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(995_000_001, eve_db::InventoryItemRecord {
            item_id: 995_000_001, type_id: TRITANIUM_TYPE_ID, owner_id: CHAR_ID,
            location_id: STATION_ID, flag: InventoryFlag::Hangar, quantity: 250,
            singleton: false, version: 1, ..Default::default()
        });
    });

    let res_1004 = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1004), PyRep::Integer(2)])
        .await.expect("TakeOfferForCharacter 1004 2x");
    assert_eq!(k_i64(&res_1004, "remainingLP"), Some(45_200 - 3_000));

    ctx.db.with_snapshot(|snap| {
        let remaining_trit: i32 = snap.items.values()
            .filter(|it| it.owner_id == CHAR_ID && it.type_id == TRITANIUM_TYPE_ID)
            .map(|it| it.quantity).sum();
        assert_eq!(remaining_trit, 50, "200 Tritanium consumed from hangar (250 - 200 = 50)");
    });
}

#[tokio::test]
async fn test_lp_store_mgr_take_offer_for_corporation() {
    // parity: rpc:LPStoreMgr.TakeOfferForCorporation
    eve_testkit::attest(&["rpc:LPStoreMgr.TakeOfferForCorporation"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_docked_session(&ctx).await;
    common::seed_test_lp_store_offers(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents::from_isk(10_000_000);
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 5_000);
        }
        let mut corp = eve_db::types::CorporationRecord {
            corporation_id: CORP_ID,
            name: "LP Test Corp".into(),
            ticker: "LPTC".into(),
            ceo_id: CHAR_ID + 999, // Character is not CEO so role check applies
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(100_000_000),
            members: [CHAR_ID].into_iter().collect(),
            member_roles: HashMap::new(),
            applications: HashMap::new(),
            wallet_divisions: vec![],
            description: "".into(),
            url: "".into(),
            hq_station_id: Some(STATION_ID),
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
        snap.corporations.insert(CORP_ID, corp);

        let mut corp_lp = HashMap::new();
        corp_lp.insert(CALDARI_NAVY_CORP, 60_000);
        corp_lp.insert(PARAGON_CORP, 5_000);
        eve_server::services::lp_store::save_corp_lp_balances(snap, CORP_ID, &corp_lp);
        snap.items.retain(|_, it| !(it.owner_id == CORP_ID && it.type_id == TRITANIUM_TYPE_ID));
    });

    // 1. Validation: InsufficientCorporationRole for regular member
    sess.corp_role = 0;
    let err_role = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_role.code, "InsufficientCorporationRole");
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 2. Validation: InsufficientLoyaltyPoints for corp (10x offer 1003 = 100k LP, corp has 60k)
    let err_clp = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1003), PyRep::Integer(10)]).await.unwrap_err();
    assert_eq!(err_clp.code, "InsufficientLoyaltyPoints");

    // 3. Validation: InsufficientFunds for corp
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&CORP_ID).unwrap();
        c.wallet_divisions[0].balance = IskCents::from_isk(1_000);
        c.master_wallet = c.wallet_divisions[0].balance;
    });
    let err_cfund = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_cfund.code, "InsufficientFunds");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&CORP_ID).unwrap();
        c.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
        c.master_wallet = c.wallet_divisions[0].balance;
    });

    // 4. Validation: MissingRequiredItems in corp hangar
    let err_citems = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_citems.code, "MissingRequiredItems");

    // Seed corp hangar (CorpSAG1) with 400x Tritanium
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(996_000_001, eve_db::InventoryItemRecord {
            item_id: 996_000_001, type_id: TRITANIUM_TYPE_ID, owner_id: CORP_ID,
            location_id: STATION_ID, flag: InventoryFlag::CorpSAG1, quantity: 400,
            singleton: false, version: 1, ..Default::default()
        });
    });

    // 5. Successful corporate purchase with trade-in items (Offer 1004, 3x)
    // 3x100 = 300 Tritanium, 3x1500 = 4500 LP, 3x1.5M = 4.5M ISK
    let char_bal_before = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().balance);
    let char_lp_before = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().loyalty_points.get(&CALDARI_NAVY_CORP).copied().unwrap());

    let res_corp = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1004), PyRep::Integer(3)])
        .await.expect("TakeOfferForCorporation 1004 3x");

    assert_eq!(k_i64(&res_corp, "offerID"), Some(1004));
    assert_eq!(k_i64(&res_corp, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(&res_corp, "quantity"), Some(3));
    assert_eq!(k_i64(&res_corp, "remainingLP"), Some(60_000 - 4_500));
    let corp_delivered_id = k_i64(&res_corp, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let corp_lp = eve_server::services::lp_store::load_corp_lp_balances(snap, CORP_ID);
        assert_eq!(corp_lp.get(&CALDARI_NAVY_CORP).copied(), Some(55_500));

        let corp = snap.corporations.get(&CORP_ID).unwrap();
        assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(100_000_000 - 4_500_000));
        assert_eq!(corp.master_wallet, IskCents::from_isk(100_000_000 - 4_500_000));
        assert!(snap.wallet_journal.iter().any(|j| j.character_id == CORP_ID && j.entry_type == "LPStorePurchase" && j.amount == IskCents::from_isk(4_500_000)));

        let rem_trit: i32 = snap.items.values()
            .filter(|it| it.owner_id == CORP_ID && it.type_id == TRITANIUM_TYPE_ID)
            .map(|it| it.quantity).sum();
        assert_eq!(rem_trit, 100);

        let delivered = snap.items.get(&corp_delivered_id).expect("Delivered corp item");
        assert_eq!(delivered.owner_id, CORP_ID);
        assert_eq!(delivered.type_id, BALLISTIC_CONTROL_TYPE_ID);
        assert_eq!(delivered.quantity, 3);
        assert_eq!(delivered.location_id, STATION_ID);
        assert_eq!(delivered.flag, InventoryFlag::CorpDeliveries);

        // Character personal wallet & LP untouched
        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.balance, char_bal_before);
        assert_eq!(ch.loyalty_points.get(&CALDARI_NAVY_CORP).copied().unwrap(), char_lp_before);
    });

    // Verify OnCorpLPChange notification signature: (reason, issuer_corp_id)
    let corp_notifs = ctx.drain_notifications_for_char(CHAR_ID);
    let clp_notif = corp_notifs.iter().find(|n| n.event == "OnCorpLPChange").expect("OnCorpLPChange notification");
    assert_eq!(
        clp_notif.args,
        vec![
            PyRep::String("take_offer".into()),
            PyRep::Integer(CALDARI_NAVY_CORP),
        ]
    );

    // 6. Zero-ISK offer for corp (Paragon emblem offer 6001: 750 LP, 0 ISK)
    let res_paragon = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(PARAGON_CORP), PyRep::Integer(6001), PyRep::Integer(1)])
        .await.expect("TakeOfferForCorporation Paragon 6001");
    assert_eq!(k_i64(&res_paragon, "remainingLP"), Some(5_000 - 750));
    let paragon_delivered_id = k_i64(&res_paragon, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&paragon_delivered_id).expect("Paragon item delivered");
        assert_eq!(it.owner_id, CORP_ID);
        assert_eq!(it.flag, InventoryFlag::CorpDeliveries);
        let corp_lp = eve_server::services::lp_store::load_corp_lp_balances(snap, CORP_ID);
        assert_eq!(corp_lp.get(&PARAGON_CORP).copied(), Some(4_250));
    });
}
