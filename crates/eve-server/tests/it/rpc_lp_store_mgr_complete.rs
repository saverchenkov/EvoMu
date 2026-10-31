//! Integration tests for LPStoreMgr RPCs:
//! - `rpc:LPStoreMgr.GetAvailableOffersFromCorp`
//! - `rpc:LPStoreMgr.TakeOfferForCharacter`
//! - `rpc:LPStoreMgr.TakeOfferForCorporation`
//! - `feat:lp.lp-store-offers`
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
const NO_LP_STATION_ID: i64 = 60_004_723;

const CALDARI_NAVY_CORP: i64 = 1_000_035;
const FEDERATION_NAVY_CORP: i64 = 1_000_120;
const AMARR_NAVY_CORP: i64 = 1_000_084;
const REPUBLIC_FLEET_CORP: i64 = 1_000_051;
const CONCORD_CORP: i64 = 1_000_125;
const PARAGON_CORP: i64 = 1_000_419;
const STATE_PROTECTORATE_CORP: i64 = 1_000_180;
const EMPTY_CUSTODIANS_CORP: i64 = 1_000_438;

// Real Tranquility Caldari Navy offers (`data/esi_lp_store_offers.json`):
// - 3414: type_id 9943, qty 1, lp_cost 5250, isk_cost 5_250_000, req_items []
// - 3565: type_id 27361, qty 5000, lp_cost 2400, isk_cost 2_400_000, req_items [(210, 5000)]
// - 3415: type_id 10222, qty 1, lp_cost 65000, isk_cost 65_000_000, req_items []
const OFFER_3414: i64 = 3_414;
const REWARD_3414_TYPE_ID: u32 = 9_943;
const OFFER_3565: i64 = 3_565;
const REWARD_3565_TYPE_ID: u32 = 27_361;
const OFFER_3415: i64 = 3_415;
const FED_NAVY_ONLY_OFFER_3616: i64 = 3_616;
const IRON_CHARGE_S_TYPE_ID: u32 = 210;
const PARAGON_RIFTER_EMBLEM_OFFER: i64 = 17_208; // Tranquility Paragon Rifter emblem offer (type 72_964, 5,000 LP, 0 ISK)

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
    // parity: feat:lp.lp-store-offers
    // parity: rpc:LPStoreMgr.GetAvailableOffersFromCorp
    eve_testkit::attest(&[
        "feat:lp.lp-store-offers",
        "rpc:LPStoreMgr.GetAvailableOffersFromCorp",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Negative 1: unauthenticated session rejected
    let mut anon = SessionState::new(4200);
    let anon_err = rpc(&ctx, &mut anon, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .unwrap_err();
    assert_eq!(anon_err.code, "Unauthorized");

    let mut sess = setup_docked_session(&ctx).await;

    // Negative 2: station without SERVICE_LOYALTY_POINT_STORE rejected
    sess.station_id = Some(NO_LP_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(NO_LP_STATION_ID);
    });
    let svc_err = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .unwrap_err();
    assert_eq!(svc_err.code, "StationServiceUnavailable");
    sess.station_id = Some(STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(STATION_ID);
    });

    // 0. Empty LP-table NPC corp and non-NPC corp IDs return empty list
    for empty_corp in [EMPTY_CUSTODIANS_CORP, 999_888_777, 999_999] {
        let empty_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(empty_corp)],
        )
        .await
        .expect("GetAvailableOffersFromCorp empty/unknown");
        assert_eq!(empty_rep, PyRep::List(vec![]));
    }

    // 1. Query unseeded Tranquility offers for Caldari Navy with 1 arg (corp_id)
    let rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(CALDARI_NAVY_CORP)],
    )
    .await
    .expect("GetAvailableOffersFromCorp Caldari Navy");
    let PyRep::List(ref offers) = rep else {
        panic!("Expected PyRep::List of offers");
    };
    assert_eq!(offers.len(), 310, "Caldari Navy should expose 310 Tranquility LP offers");

    let o3414 = offers
        .iter()
        .find(|o| k_i64(o, "offerID") == Some(OFFER_3414))
        .expect("Offer 3414");
    assert_eq!(k_i64(o3414, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(o3414, "typeID"), Some(REWARD_3414_TYPE_ID as i64));
    assert_eq!(k_i64(o3414, "quantity"), Some(1));
    assert_eq!(k_i64(o3414, "lpCost"), Some(5_250));
    let isk_cost = o3414.dict_get("iskCost").and_then(|v| v.as_f64()).expect("iskCost");
    assert!((isk_cost - 5_250_000.0).abs() < 0.01);
    let PyRep::List(ref req_items) = o3414.dict_get("reqItems").expect("reqItems") else {
        panic!("reqItems list");
    };
    assert!(req_items.is_empty(), "Offer 3414 has no reqItems");

    let o3565 = offers
        .iter()
        .find(|o| k_i64(o, "offerID") == Some(OFFER_3565))
        .expect("Offer 3565");
    let PyRep::List(ref req_3565) = o3565.dict_get("reqItems").expect("reqItems") else {
        panic!("reqItems list");
    };
    assert_eq!(req_3565.len(), 1);
    assert_eq!(k_i64(&req_3565[0], "typeID"), Some(IRON_CHARGE_S_TYPE_ID as i64));
    assert_eq!(k_i64(&req_3565[0], "quantity"), Some(5_000));

    // 2. Query major Empire navies, CONCORD, Paragon (V24 SDE EverMarks), and FW militia corps unseeded
    for &corp in &[
        FEDERATION_NAVY_CORP,
        AMARR_NAVY_CORP,
        REPUBLIC_FLEET_CORP,
        CONCORD_CORP,
        PARAGON_CORP,
        STATE_PROTECTORATE_CORP,
    ] {
        let corp_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(corp)],
        )
        .await
        .unwrap_or_else(|e| panic!("corp {corp}: {e:?}"));
        let PyRep::List(ref c_offers) = corp_rep else {
            panic!("list expected");
        };
        assert!(!c_offers.is_empty(), "Offers for corp {corp} should not be empty");
        assert!(
            c_offers.iter().any(|o| k_i64(o, "lpCost").unwrap_or(-1) > 0),
            "Corp {corp} should have positive-LP offers"
        );
        for o in c_offers {
            assert_eq!(k_i64(o, "corpID"), Some(corp));
            assert!(k_i64(o, "offerID").unwrap() > 0);
            assert!(k_i64(o, "typeID").unwrap() > 0);
            assert!(k_i64(o, "quantity").unwrap() > 0);
            assert!(k_i64(o, "lpCost").unwrap() >= 0);
        }
    }

    // 3. Default no args at Jita 4-4 returns Caldari Navy offers
    let def = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
        .await
        .expect("GetAvailableOffersFromCorp default");
    let PyRep::List(ref def_offers) = def else {
        panic!("list expected");
    };
    assert_eq!(def_offers.len(), 310);
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

    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == CHAR_ID && it.type_id == IRON_CHARGE_S_TYPE_ID));
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.balance = IskCents::from_isk(50_000_000);
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
        }
    });

    // 1. Validation: OfferNotFound (non-existent offer ID and Federation-Navy-only offer on Caldari Navy)
    let err1 = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(999_999_999), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err1.code, "OfferNotFound");
    let err2 = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(FED_NAVY_ONLY_OFFER_3616),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err2.code, "OfferNotFound");

    // 2. Validation: InvalidQuantity
    for bad_qty in [0, -1, -50] {
        let eq = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(bad_qty)],
        )
        .await
        .unwrap_err();
        assert_eq!(eq.code, "InvalidQuantity");
    }

    // 3. Validation: InsufficientLoyaltyPoints
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().loyalty_points.insert(CALDARI_NAVY_CORP, 1_000);
    });
    let elp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(elp.code, "InsufficientLoyaltyPoints");

    // 4. Validation: InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
        ch.balance = IskCents::from_isk(100_000);
    });
    let efund = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(efund.code, "InsufficientFunds");

    // 5. Validation: MissingRequiredItems (Offer 3565 needs 5,000x Iron Charge S)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&CHAR_ID).unwrap().balance = IskCents::from_isk(50_000_000);
    });
    let emiss = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3565), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
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
    let edock = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(edock.message, "MustBeDocked");
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
        ch.station_id = saved_st;
        ch.structure_id = saved_str;
    });
    sess.station_id = saved_st;
    sess.structure_id = saved_str;

    // 7. Successful purchase without trade-in items (Offer 3414, 2x): 10,500 LP, 10.5M ISK
    let res_3414 = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter 3414 2x");

    assert_eq!(k_i64(&res_3414, "offerID"), Some(OFFER_3414));
    assert_eq!(k_i64(&res_3414, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(&res_3414, "quantity"), Some(2));
    assert_eq!(k_i64(&res_3414, "remainingLP"), Some(50_000 - 10_500));
    let delivered_id = k_i64(&res_3414, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&delivered_id).expect("Delivered item exists");
        assert_eq!(it.type_id, REWARD_3414_TYPE_ID);
        assert_eq!(it.owner_id, CHAR_ID);
        assert_eq!(it.location_id, STATION_ID);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(it.quantity, 2);

        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.loyalty_points.get(&CALDARI_NAVY_CORP).copied(), Some(39_500));
        assert_eq!(ch.balance, IskCents::from_isk(50_000_000 - 10_500_000));
        assert!(snap.wallet_journal.iter().any(|j| {
            j.character_id == CHAR_ID
                && j.entry_type == "LPStorePurchase"
                && j.amount == IskCents::from_isk(10_500_000)
        }));
        let logs = snap.char_lp_logs.get(&CHAR_ID).expect("LP logs");
        assert!(logs.iter().any(|l| l.corp_id == CALDARI_NAVY_CORP && l.amount == -10_500));
    });

    // Verify OnLPChange notification signature: (change_type, issuer_corp_id, lp_before, lp_after)
    let notifs = ctx.drain_notifications_for_char(CHAR_ID);
    let lp_notif = notifs
        .iter()
        .find(|n| n.event == "OnLPChange")
        .expect("OnLPChange notification");
    assert_eq!(
        lp_notif.args,
        vec![
            PyRep::String("take_offer".into()),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(50_000),
            PyRep::Integer(39_500),
        ]
    );

    // 8. Successful purchase WITH trade-in items (Offer 3565, 2x): requires 10,000x Iron Charge S (210)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            995_000_001,
            eve_db::InventoryItemRecord {
                item_id: 995_000_001,
                type_id: IRON_CHARGE_S_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 12_500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let res_3565 = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3565), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter 3565 2x");
    assert_eq!(k_i64(&res_3565, "remainingLP"), Some(39_500 - 4_800));

    ctx.db.with_snapshot(|snap| {
        let remaining_ammo: i32 = snap
            .items
            .values()
            .filter(|it| it.owner_id == CHAR_ID && it.type_id == IRON_CHARGE_S_TYPE_ID)
            .map(|it| it.quantity)
            .sum();
        assert_eq!(remaining_ammo, 2_500, "10,000 Iron Charge S consumed (12,500 - 10,000 = 2,500)");
    });
}

#[tokio::test]
async fn test_lp_store_mgr_take_offer_for_corporation() {
    // parity: rpc:LPStoreMgr.TakeOfferForCorporation
    eve_testkit::attest(&["rpc:LPStoreMgr.TakeOfferForCorporation"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_docked_session(&ctx).await;

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
            ceo_id: CHAR_ID + 999,
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
        corp_lp.insert(PARAGON_CORP, 7_500);
        eve_server::services::lp_store::save_corp_lp_balances(snap, CORP_ID, &corp_lp);
        snap.items
            .retain(|_, it| !(it.owner_id == CORP_ID && it.type_id == IRON_CHARGE_S_TYPE_ID));
    });

    // 1. Validation: InsufficientCorporationRole for regular member
    sess.corp_role = 0;
    let err_role = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_role.code, "InsufficientCorporationRole");
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 2. Validation: InsufficientLoyaltyPoints for corp (Offer 3415 costs 65,000 LP, corp has 60,000)
    let err_clp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3415), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_clp.code, "InsufficientLoyaltyPoints");

    // 3. Validation: InsufficientFunds for corp
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&CORP_ID).unwrap();
        c.wallet_divisions[0].balance = IskCents::from_isk(1_000);
        c.master_wallet = c.wallet_divisions[0].balance;
    });
    let err_cfund = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3414), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cfund.code, "InsufficientFunds");
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&CORP_ID).unwrap();
        c.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
        c.master_wallet = c.wallet_divisions[0].balance;
    });

    // 4. Validation: MissingRequiredItems in corp hangar (Offer 3565 needs 5,000x Iron Charge S)
    let err_citems = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3565), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_citems.code, "MissingRequiredItems");

    // Seed corp hangar (CorpSAG1) with 20,000x Iron Charge S
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            996_000_001,
            eve_db::InventoryItemRecord {
                item_id: 996_000_001,
                type_id: IRON_CHARGE_S_TYPE_ID,
                owner_id: CORP_ID,
                location_id: STATION_ID,
                flag: InventoryFlag::CorpSAG1,
                quantity: 20_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 5. Successful corporate purchase with trade-in items (Offer 3565, 3x):
    // 3 * 5,000 = 15,000 Iron Charge S, 3 * 2,400 = 7,200 LP, 3 * 2.4M = 7.2M ISK
    let char_bal_before = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().balance);
    let char_lp_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().loyalty_points[&CALDARI_NAVY_CORP]);

    let res_corp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(OFFER_3565), PyRep::Integer(3)],
    )
    .await
    .expect("TakeOfferForCorporation 3565 3x");

    assert_eq!(k_i64(&res_corp, "offerID"), Some(OFFER_3565));
    assert_eq!(k_i64(&res_corp, "corpID"), Some(CALDARI_NAVY_CORP));
    assert_eq!(k_i64(&res_corp, "quantity"), Some(15_000));
    assert_eq!(k_i64(&res_corp, "remainingLP"), Some(60_000 - 7_200));
    let corp_delivered_id = k_i64(&res_corp, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let corp_lp = eve_server::services::lp_store::load_corp_lp_balances(snap, CORP_ID);
        assert_eq!(corp_lp.get(&CALDARI_NAVY_CORP).copied(), Some(52_800));

        let corp = snap.corporations.get(&CORP_ID).unwrap();
        assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(100_000_000 - 7_200_000));
        assert_eq!(corp.master_wallet, IskCents::from_isk(100_000_000 - 7_200_000));
        assert!(snap.wallet_journal.iter().any(|j| {
            j.character_id == CORP_ID
                && j.entry_type == "LPStorePurchase"
                && j.amount == IskCents::from_isk(7_200_000)
        }));

        let rem_ammo: i32 = snap
            .items
            .values()
            .filter(|it| it.owner_id == CORP_ID && it.type_id == IRON_CHARGE_S_TYPE_ID)
            .map(|it| it.quantity)
            .sum();
        assert_eq!(rem_ammo, 5_000);

        let delivered = snap.items.get(&corp_delivered_id).expect("Delivered corp item");
        assert_eq!(delivered.owner_id, CORP_ID);
        assert_eq!(delivered.type_id, REWARD_3565_TYPE_ID);
        assert_eq!(delivered.quantity, 15_000);
        assert_eq!(delivered.location_id, STATION_ID);
        assert_eq!(delivered.flag, InventoryFlag::CorpDeliveries);

        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.balance, char_bal_before);
        assert_eq!(ch.loyalty_points[&CALDARI_NAVY_CORP], char_lp_before);
    });

    // Verify OnCorpLPChange notification signature: (reason, issuer_corp_id)
    let corp_notifs = ctx.drain_notifications_for_char(CHAR_ID);
    let clp_notif = corp_notifs
        .iter()
        .find(|n| n.event == "OnCorpLPChange")
        .expect("OnCorpLPChange notification");
    assert_eq!(
        clp_notif.args,
        vec![
            PyRep::String("take_offer".into()),
            PyRep::Integer(CALDARI_NAVY_CORP),
        ]
    );

    // 6. Zero-ISK Tranquility Paragon EverMarks offer for corp at Jita VI - Paragon Fulfillment Center (60_015_169)
    sess.station_id = Some(60_015_169);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = Some(60_015_169);
        }
    });
    let res_paragon = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(PARAGON_CORP),
            PyRep::Integer(PARAGON_RIFTER_EMBLEM_OFFER),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCorporation Paragon 17208");
    assert_eq!(k_i64(&res_paragon, "remainingLP"), Some(7_500 - 5_000));
    let paragon_delivered_id = k_i64(&res_paragon, "deliveredItemID").expect("deliveredItemID");

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&paragon_delivered_id).expect("Paragon item delivered");
        assert_eq!(it.owner_id, CORP_ID);
        assert_eq!(it.type_id, 72_964);
        assert_eq!(it.flag, InventoryFlag::CorpDeliveries);
        let corp_lp = eve_server::services::lp_store::load_corp_lp_balances(snap, CORP_ID);
        assert_eq!(corp_lp.get(&PARAGON_CORP).copied(), Some(2_500));
    });
}
