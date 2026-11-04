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

pub(crate) mod lp_store_offers_browsing_station {
    // Integration tests closing parity for LP store offer browsing and purchases:
    // - `rpc:LPStoreMgr.GetAvailableOffersFromCorp`
    // - `rpc:LPStoreMgr.TakeOfferForCharacter`
    // - `rpc:LPStoreMgr.TakeOfferForCorporation`
    // - `feat:lp.lp-store-offers`
    // - `feat:lp.lp-store-buy`

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
    const ALGOGILLE_FED_NAVY_STATION: i64 = 60_011_734; // Owner: 1000120 (Federation Navy)
    const DAMAGED_BUREAU_NO_LP_STATION: i64 = 60_004_723; // Lacks SERVICE_LOYALTY_POINT_STORE

    const CALDARI_NAVY_CORP: i64 = 1_000_035;
    const FEDERATION_NAVY_CORP: i64 = 1_000_120;
    const EMPTY_CUSTODIANS_CORP: i64 = 1_000_438;

    // Real Tranquility Caldari Navy offers in `data/esi_lp_store_offers.json`:
    // - Offer 3414: type_id 9943, qty 1, lp_cost 5250, isk_cost 5_250_000, req_items []
    // - Offer 3565: type_id 27361 (Caldari Navy Iron Charge S), qty 5000, lp_cost 2400,
    //               isk_cost 2_400_000, req_items [(210, 5000)] (210 = Iron Charge S)
    const CALDARI_NO_REQ_OFFER_ID: i64 = 3_414;
    const CALDARI_NO_REQ_REWARD_TYPE_ID: u32 = 9_943;
    const CALDARI_TRADEIN_OFFER_ID: i64 = 3_565;
    const CALDARI_TRADEIN_REWARD_TYPE_ID: u32 = 27_361;
    const IRON_CHARGE_S_TYPE_ID: u32 = 210;
    const FED_NAVY_EXCLUSIVE_OFFER_ID: i64 = 3_616;
    const CONDOR_SHIP_TYPE_ID: u32 = 583;

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

    async fn setup_docked_pilot(
        ctx: &ServerContext,
        char_id: i64,
        station_id: i64,
        ship_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(5501);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
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
            ch.loyalty_points.insert(EMPTY_CUSTODIANS_CORP, 10_000);
            ch.standings.insert(EMPTY_CUSTODIANS_CORP, 0.0);

            snap.items
                .retain(|_, it| !(it.owner_id == char_id && it.type_id == IRON_CHARGE_S_TYPE_ID));

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
        // parity: feat:lp.lp-store-offers
        // parity: rpc:LPStoreMgr.GetAvailableOffersFromCorp
        eve_testkit::attest(&[
            "feat:lp.lp-store-offers",
            "rpc:LPStoreMgr.GetAvailableOffersFromCorp",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        // Negative path 1: unauthenticated session rejected
        let mut anon = SessionState::new(5500);
        let anon_err = rpc(&ctx, &mut anon, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
            .await
            .expect_err("unauthenticated GetAvailableOffersFromCorp must fail");
        assert_eq!(anon_err.code, "Unauthorized");

        let ship_id = 95_000_001;
        let mut sess = setup_docked_pilot(&ctx, CHAR_ID, JITA_CALDARI_STATION, ship_id).await;

        // Negative path 2: station lacking SERVICE_LOYALTY_POINT_STORE rejects default browsing
        sess.station_id = Some(DAMAGED_BUREAU_NO_LP_STATION);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(DAMAGED_BUREAU_NO_LP_STATION);
        });
        let no_svc_err = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
            .await
            .expect_err("station without LP store service must reject GetAvailableOffersFromCorp");
        assert_eq!(no_svc_err.code, "StationServiceUnavailable");

        // Restore docking at Jita 4-4 (Caldari Navy station)
        sess.station_id = Some(JITA_CALDARI_STATION);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(JITA_CALDARI_STATION);
        });

        // 1. Browsing with empty args at Jita 4-4 resolves unseeded Tranquility Caldari Navy offers (310 offers)
        let def_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
            .await
            .expect("GetAvailableOffersFromCorp with station default");
        let PyRep::List(ref def_offers) = def_rep else {
            panic!("expected PyRep::List of offers");
        };
        assert_eq!(def_offers.len(), 310, "Caldari Navy has 310 Tranquility LP offers");

        let caldari_offer = def_offers
            .iter()
            .find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(CALDARI_TRADEIN_OFFER_ID))
            .expect("Tranquility offer 3565 should be present in Caldari Navy catalog");
        assert_eq!(caldari_offer.dict_get("corpID").and_then(|v| v.as_i64()), Some(CALDARI_NAVY_CORP));
        assert_eq!(
            caldari_offer.dict_get("typeID").and_then(|v| v.as_i64()),
            Some(CALDARI_TRADEIN_REWARD_TYPE_ID as i64)
        );
        assert_eq!(caldari_offer.dict_get("quantity").and_then(|v| v.as_i64()), Some(5_000));
        assert_eq!(caldari_offer.dict_get("lpCost").and_then(|v| v.as_i64()), Some(2_400));
        let isk_cost = caldari_offer.dict_get("iskCost").and_then(|v| v.as_f64()).expect("iskCost");
        assert!((isk_cost - 2_400_000.0).abs() < 0.01);

        let PyRep::List(ref req_items) = caldari_offer.dict_get("reqItems").expect("reqItems list") else {
            panic!("reqItems must be a list");
        };
        assert_eq!(req_items.len(), 1);
        assert_eq!(req_items[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(IRON_CHARGE_S_TYPE_ID as i64));
        assert_eq!(req_items[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(5_000));

        // 2. Querying explicit corp ID 1000120 (Federation Navy) returns Federation Navy offers (including 3616)
        let fed_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(FEDERATION_NAVY_CORP)],
        )
        .await
        .expect("GetAvailableOffersFromCorp explicit Federation Navy");
        let PyRep::List(ref fed_offers) = fed_rep else {
            panic!("expected PyRep::List of offers");
        };
        assert_eq!(fed_offers.len(), 319);
        let fed_offer = fed_offers
            .iter()
            .find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(FED_NAVY_EXCLUSIVE_OFFER_ID))
            .expect("Federation Navy exclusive offer 3616 present");
        assert_eq!(fed_offer.dict_get("corpID").and_then(|v| v.as_i64()), Some(FEDERATION_NAVY_CORP));

        // 3. Docking at Algogille Federation Navy station resolves Federation Navy offers by default
        sess.station_id = Some(ALGOGILLE_FED_NAVY_STATION);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(ALGOGILLE_FED_NAVY_STATION);
        });
        let def_fed_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[])
            .await
            .expect("GetAvailableOffersFromCorp default at Federation Navy station");
        let PyRep::List(ref def_fed_offers) = def_fed_rep else {
            panic!("expected PyRep::List");
        };
        assert!(def_fed_offers
            .iter()
            .any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(FED_NAVY_EXCLUSIVE_OFFER_ID)));

        // 4. Empire FW, Pirate Insurgency (Angel/Guristas), and CONCORD catalogs are populated unseeded
        for &(corp_id, min_len) in &[
            (1_000_125_i64, 200usize),
            (1_000_179, 350),
            (1_000_180, 350),
            (1_000_181, 350),
            (1_000_182, 350),
            (1_000_138, 250),
            (1_000_436, 350),
            (1_000_127, 300),
            (1_000_437, 350),
        ] {
            let rep = rpc(
                &ctx,
                &mut sess,
                "LPStoreMgr",
                "GetAvailableOffersFromCorp",
                &[PyRep::Integer(corp_id)],
            )
            .await
            .expect("GetAvailableOffersFromCorp faction corp");
            let PyRep::List(list) = rep else { panic!("expected list") };
            assert!(list.len() >= min_len, "corp {corp_id} expected >= {min_len} offers, got {}", list.len());
        }

        // 5. Empty LP-table NPC corp and unknown corp ID return empty list
        for empty_id in [EMPTY_CUSTODIANS_CORP, 999_888_777] {
            let empty_rep = rpc(
                &ctx,
                &mut sess,
                "LPStoreMgr",
                "GetAvailableOffersFromCorp",
                &[PyRep::Integer(empty_id)],
            )
            .await
            .expect("GetAvailableOffersFromCorp empty/unknown corp");
            assert_eq!(empty_rep, PyRep::List(vec![]));
        }
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

        // 1. Validation against real Tranquility offer 3414 (5,250 LP, 5,250,000 ISK, no req_items)
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&CHAR_ID).unwrap().loyalty_points.insert(CALDARI_NAVY_CORP, 500);
        });
        let err_lp = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_NO_REQ_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(err_lp.code, "InsufficientLoyaltyPoints");

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
            ch.balance = IskCents::from_isk(100);
        });
        let err_fund = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_NO_REQ_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(err_fund.code, "InsufficientFunds");

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&CHAR_ID).unwrap().balance = IskCents::from_isk(50_000_000);
        });

        // Purchase real Tranquility offer 3414 (no trade-in items)
        let pass_3414 = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_NO_REQ_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("Tranquility offer 3414 purchase should succeed");
        assert_eq!(pass_3414.dict_get("offerID").and_then(|v| v.as_i64()), Some(CALDARI_NO_REQ_OFFER_ID));
        assert_eq!(
            pass_3414.dict_get("deliveredTypeID").and_then(|v| v.as_i64()),
            Some(CALDARI_NO_REQ_REWARD_TYPE_ID as i64)
        );

        // 2. Validation: MissingRequiredItems on real Tranquility offer 3565 (needs 5,000x Iron Charge S)
        let err_missing = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_TRADEIN_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(err_missing.code, "MissingRequiredItems");

        // 3. Trade-in consumption from ACTIVE SHIP CARGO using 2-arg TakeOfferForCharacter (station default corp)
        let cargo_ammo_id = 96_000_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                cargo_ammo_id,
                InventoryItemRecord {
                    item_id: cargo_ammo_id,
                    type_id: IRON_CHARGE_S_TYPE_ID,
                    owner_id: CHAR_ID,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 5_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let buy_cargo_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(CALDARI_TRADEIN_OFFER_ID), PyRep::Integer(1)],
        )
        .await
        .expect("TakeOfferForCharacter taking trade-in from active ship cargo");
        let delivered_id = buy_cargo_rep
            .dict_get("deliveredItemID")
            .and_then(|v| v.as_i64())
            .expect("deliveredItemID");

        ctx.db.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&cargo_ammo_id), "trade-in item in ship cargo must be consumed");
            let delivered_item = snap.items.get(&delivered_id).expect("delivered item must exist");
            assert_eq!(delivered_item.owner_id, CHAR_ID);
            assert_eq!(delivered_item.location_id, JITA_CALDARI_STATION);
            assert_eq!(delivered_item.flag, InventoryFlag::Hangar);
            assert_eq!(delivered_item.type_id, CALDARI_TRADEIN_REWARD_TYPE_ID);
            assert_eq!(delivered_item.quantity, 5_000);
        });

        // 4. Trade-in consumption from STATION HANGAR (10,000 - 5,000 = 5,000 remaining)
        let hangar_ammo_id = 96_000_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                hangar_ammo_id,
                InventoryItemRecord {
                    item_id: hangar_ammo_id,
                    type_id: IRON_CHARGE_S_TYPE_ID,
                    owner_id: CHAR_ID,
                    location_id: JITA_CALDARI_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 10_000,
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
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_TRADEIN_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("TakeOfferForCharacter taking trade-in from station hangar");
        let hangar_delivered_id = buy_hangar_rep
            .dict_get("deliveredItemID")
            .and_then(|v| v.as_i64())
            .expect("deliveredItemID");

        ctx.db.with_snapshot(|snap| {
            let remaining_ammo = snap.items.get(&hangar_ammo_id).expect("hangar ammo remaining item");
            assert_eq!(remaining_ammo.quantity, 5_000);
            let delivered = snap.items.get(&hangar_delivered_id).expect("hangar delivered item");
            assert_eq!(delivered.owner_id, CHAR_ID);
            assert_eq!(delivered.location_id, JITA_CALDARI_STATION);
            assert_eq!(delivered.flag, InventoryFlag::Hangar);
            assert_eq!(delivered.type_id, CALDARI_TRADEIN_REWARD_TYPE_ID);
        });

        // 5. Cross-station, Upwell structure, standing gate, and 2-arg offer_id collision checks
        const FULCRUM_STATION: i64 = 60_015_187;
        ctx.db.with_mut_snapshot(|snap| {
            for (oid, lp, isk, min_st) in [
                (920_002, 1_000, 500_000, Some(2.0)),
                (CALDARI_NAVY_CORP, 500, 100_000, None), // collides numerically with NPC corp ID
            ] {
                register_lp_store_offer(
                    snap,
                    LPOffer {
                        offer_id: oid,
                        corp_id: EMPTY_CUSTODIANS_CORP,
                        type_id: CALDARI_NO_REQ_REWARD_TYPE_ID,
                        quantity: 1,
                        lp_cost: lp,
                        isk_cost: IskCents::from_isk(isk),
                        req_items: vec![],
                        ak_cost: 0,
                        min_standing: min_st,
                    },
                );
            }
        });

        // Cross-station NPC LP purchase while docked at Jita 4-4 (Caldari Navy 1_000_035) must be rejected
        let err_cross_fed = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(FEDERATION_NAVY_CORP), PyRep::Integer(FED_NAVY_EXCLUSIVE_OFFER_ID), PyRep::Integer(1)]).await.unwrap_err();
        assert_eq!(err_cross_fed.code, "InvalidStationCorpForLPStore");
        let err_cross_cust = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(EMPTY_CUSTODIANS_CORP), PyRep::Integer(920_002), PyRep::Integer(1)]).await.unwrap_err();
        assert_eq!(err_cross_cust.code, "InvalidStationCorpForLPStore");
        let err_two_arg_foreign = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(920_002), PyRep::Integer(1)]).await.unwrap_err();
        assert_eq!(err_two_arg_foreign.code, "OfferNotFound");

        // Player-owned Upwell structure (>= 1_000_000_000_000) must reject NPC LP store redemptions
        sess.station_id = None;
        sess.structure_id = Some(1_000_000_777_001);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_777_001);
        });
        let err_upwell = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(CALDARI_NO_REQ_OFFER_ID), PyRep::Integer(1)]).await.unwrap_err();
        assert_eq!(err_upwell.code, "StationServiceUnavailable");

        // Dock at The Fulcrum (60_015_187, owned by Deathless Custodians 1_000_438 with service 25)
        sess.structure_id = None;
        sess.station_id = Some(FULCRUM_STATION);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&CHAR_ID).unwrap();
            ch.station_id = Some(FULCRUM_STATION);
            ch.structure_id = None;
            if let Some(sh) = snap.items.get_mut(&ship_id) { sh.location_id = FULCRUM_STATION; }
        });

        let err_stand = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(EMPTY_CUSTODIANS_CORP), PyRep::Integer(920_002), PyRep::Integer(1)]).await.unwrap_err();
        assert_eq!(err_stand.code, "InsufficientStanding");

        ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&CHAR_ID).unwrap().standings.insert(EMPTY_CUSTODIANS_CORP, 3.5); });
        let pass_stand = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(EMPTY_CUSTODIANS_CORP), PyRep::Integer(920_002), PyRep::Integer(1)]).await.expect("standing-gated purchase should succeed");
        assert_eq!(pass_stand.dict_get("offerID").and_then(|v| v.as_i64()), Some(920_002));

        let pass_two_arg_high_oid = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(920_002), PyRep::Integer(2)]).await.expect("2-arg offer_id > 100_000 should succeed");
        assert_eq!(pass_two_arg_high_oid.dict_get("offerID").and_then(|v| v.as_i64()), Some(920_002));
        assert_eq!(pass_two_arg_high_oid.dict_get("quantity").and_then(|v| v.as_i64()), Some(2));

        let pass_two_arg_corp_collision = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(2)]).await.expect("2-arg offer_id == corp_id must parse as (offer_id, qty)");
        assert_eq!(pass_two_arg_corp_collision.dict_get("corpID").and_then(|v| v.as_i64()), Some(EMPTY_CUSTODIANS_CORP));
        assert_eq!(pass_two_arg_corp_collision.dict_get("offerID").and_then(|v| v.as_i64()), Some(CALDARI_NAVY_CORP));
        assert_eq!(pass_two_arg_corp_collision.dict_get("quantity").and_then(|v| v.as_i64()), Some(2));
    }

    #[tokio::test]
    async fn test_lp_store_corporation_purchase_and_delivery() {
        // parity: rpc:LPStoreMgr.TakeOfferForCorporation
        eve_testkit::attest(&["rpc:LPStoreMgr.TakeOfferForCorporation"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let ship_id = 95_000_003;
        let mut sess = setup_docked_pilot(&ctx, CHAR_ID, JITA_CALDARI_STATION, ship_id).await;

        // Setup player corporation with wallet, LP balance, and trade-in ammo for real Tranquility offer 3565
        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = eve_db::types::CorporationRecord {
                corporation_id: CORP_ID,
                name: "Parity Test Corp".into(),
                ticker: "PTC".into(),
                ceo_id: CHAR_ID + 9999,
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: IskCents::from_isk(50_000_000),
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

            snap.items.insert(
                97_000_001,
                InventoryItemRecord {
                    item_id: 97_000_001,
                    type_id: IRON_CHARGE_S_TYPE_ID,
                    owner_id: CORP_ID,
                    location_id: JITA_CALDARI_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 5_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // 1. Negative path 1: pilot lacking Director/Accountant role fails
        sess.corp_role = 0;
        let err_role = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_TRADEIN_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(err_role.code, "InsufficientCorporationRole");

        // 2. Negative path 2: OfferNotFound on non-existent offer ID
        sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
        let err_not_found = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(999_999_999), PyRep::Integer(1)],
        )
        .await
        .unwrap_err();
        assert_eq!(err_not_found.code, "OfferNotFound");

        // 3. Execute corporate purchase of real Tranquility offer 3565 (2,400 LP, 2,400,000 ISK, 5,000x type 210)
        let corp_buy_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[
                PyRep::Integer(CALDARI_NAVY_CORP),
                PyRep::Integer(CALDARI_TRADEIN_OFFER_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("TakeOfferForCorporation purchase of Tranquility offer 3565");

        assert_eq!(
            corp_buy_rep.dict_get("offerID").and_then(|v| v.as_i64()),
            Some(CALDARI_TRADEIN_OFFER_ID)
        );
        let corp_deliv_id = corp_buy_rep
            .dict_get("deliveredItemID")
            .and_then(|v| v.as_i64())
            .expect("deliveredItemID");

        ctx.db.with_snapshot(|snap| {
            let balances = load_corp_lp_balances(snap, CORP_ID);
            assert_eq!(balances.get(&CALDARI_NAVY_CORP).copied(), Some(37_600));

            let corp = snap.corporations.get(&CORP_ID).unwrap();
            assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(47_600_000));
            assert!(!snap.items.contains_key(&97_000_001), "corp trade-in item must be consumed");

            let delivered = snap.items.get(&corp_deliv_id).expect("delivered item");
            assert_eq!(delivered.owner_id, CORP_ID);
            assert_eq!(delivered.location_id, JITA_CALDARI_STATION);
            assert_eq!(delivered.flag, InventoryFlag::CorpDeliveries);
            assert_eq!(delivered.type_id, CALDARI_TRADEIN_REWARD_TYPE_ID);
            assert_eq!(delivered.quantity, 5_000);
        });
    }
}

pub(crate) mod lp_store_mgr_gameplay {
    // Integration tests for LP Store mechanics, NPC-NPC Standings dispatch, and Shipcaster Travel mechanics.
    //
    // Verifies:
    // - `TakeOfferForCharacter` and `TakeOfferForCorporation` LP/ISK deduction, item consumption, and delivery.
    // - `standingMgr.GetNPCNPCStandings` Rowset shape and standing values dispatch.
    // - `starterShipcasterTravelMgr.CanCharacterJump` weapons timer and capital ship restrictions.
    // - `CmdJumpThroughStarterShipcaster` 5km proximity boundary checks (4.9km success, 5.1km rejection), session timer, and ball transfer.
    // - `CmdJumpThroughShipcaster` ball transfer and destination placement.
    //
    // DATA GAPs (server-only data not present in V24 client FSD):
    // - LP store corporation offer catalog is server-only.
    // - Static NPC-NPC cross-standing matrix is server-internal.
    // - Starter and interstellar shipcaster placements and linkages are server-side structures.

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::lp_store::{
        load_corp_lp_balances, save_corp_lp_balances, save_lp_store_offers, LPOffer,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    async fn rpc(
        ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    fn k_i64(r: &PyRep, k: &str) -> Option<i64> { r.dict_get(k).and_then(|v| v.as_i64()) }
    fn kv_i64(rep: &PyRep, key: &str) -> i64 {
        match rep { PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()).unwrap_or(0), _ => 0 }
    }
    fn kv_bool(rep: &PyRep, key: &str) -> bool {
        match rep { PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()).unwrap_or(false), _ => false }
    }

    async fn spawn_test_ship(
        ctx: &ServerContext,
        handle: &eve_server::system_actor::SystemHandle,
        char_id: i64,
        ship_id: i64,
        type_id: u32,
        pos: DVec3,
    ) {
        let _ = handle.spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id, position: pos, velocity: DVec3::ZERO,
                max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, type_id),
        ).await;
    }

    #[tokio::test]
    async fn test_lp_store_mgr_gameplay_closure() {
        // DATA GAP: LP store corporation offer catalog is server-only and not present in V24 client FSD.
        // Tests authentic LP/ISK deduction, required item trade-in consumption, and hangar delivery mechanics.
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let char_id = 90_000_001;
        let corp_id = 98_000_001;
        let station_id = 60_003_760;
        let ship_id = 100_000_001;

        let mut sess = SessionState::new(4321);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        sess.station_id = Some(station_id);
        sess.ship_id = Some(ship_id);
        sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

        // Seed LP store test offers for corp 1_000_035
        let test_offers = vec![
            LPOffer {
                offer_id: 1001, corp_id: 1_000_035, type_id: 15810, quantity: 1,
                lp_cost: 2_400, isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![], ak_cost: 0, min_standing: None,
            },
            LPOffer {
                offer_id: 1004, corp_id: 1_000_035, type_id: 621, quantity: 1,
                lp_cost: 5_000, isk_cost: IskCents::from_isk(5_000_000),
                req_items: vec![(34, 100)], ak_cost: 0, min_standing: None,
            },
        ];
        ctx.db.with_mut_snapshot(|s| {
            save_lp_store_offers(s, &test_offers);
        });

        // 1. GetAvailableOffersFromCorp returns seeded offers for corporation
        let rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(1_000_035)],
        )
        .await
        .expect("GetAvailableOffersFromCorp");
        let PyRep::List(ref offers) = rep else {
            panic!("Expected List for 1_000_035");
        };
        assert_eq!(offers.len(), 2, "Seeded corp must return 2 offers");
        for o in offers {
            assert_eq!(k_i64(o, "corpID"), Some(1_000_035));
            assert!(k_i64(o, "offerID").unwrap_or(0) > 0);
            assert!(k_i64(o, "typeID").unwrap_or(0) > 0);
            assert!(k_i64(o, "quantity").unwrap_or(0) > 0);
            assert!(k_i64(o, "lpCost").unwrap_or(0) >= 0);
            assert!(
                o.dict_get("iskCost")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(-1.0)
                    >= 0.0
            );
        }

        // Unknown or unseeded corp returns empty list
        let empty_rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(999_888_777)],
        )
        .await
        .expect("GetAvailableOffersFromCorp unknown");
        assert_eq!(empty_rep, PyRep::List(vec![]));

        // 2. TakeOfferForCharacter with seeded offer (offer 1001: Ballistic Control System, 2400 LP, 2.4M ISK)
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.corporation_id = corp_id;
            ch.loyalty_points.insert(1_000_035, 50_000);
            ch.balance = IskCents::from_isk(100_000_000);
            ch.station_id = Some(station_id);

            let mut corp = eve_db::types::CorporationRecord {
                corporation_id: corp_id,
                name: "LP Test Corp".into(),
                ticker: "LPTC".into(),
                ceo_id: char_id + 999,
                master_wallet: IskCents::from_isk(100_000_000),
                members: [char_id].into_iter().collect(),
                hq_station_id: Some(station_id),
                ..Default::default()
            };
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
            s.corporations.insert(corp_id, corp);
        });
        sess.corporation_id = Some(corp_id);

        let res_char = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(1_000_035), PyRep::Integer(1001), PyRep::Integer(2)],
        )
        .await
        .expect("TakeOfferForCharacter 1001 x2");
        assert_eq!(kv_i64(&res_char, "offerID"), 1001);
        assert_eq!(kv_i64(&res_char, "corpID"), 1_000_035);
        assert_eq!(kv_i64(&res_char, "quantity"), 2);
        assert_eq!(kv_i64(&res_char, "remainingLP"), 50_000 - 4_800);
        assert!(kv_i64(&res_char, "deliveredItemID") > 0);

        ctx.db.with_snapshot(|s| {
            let ch = s.characters.get(&char_id).unwrap();
            assert_eq!(
                ch.loyalty_points.get(&1_000_035).copied(),
                Some(50_000 - 4_800)
            );
            assert_eq!(ch.balance, IskCents::from_isk(100_000_000 - 4_800_000));
            let delivered = s.items.values().find(|it| {
                it.owner_id == char_id
                    && it.location_id == station_id
                    && it.type_id == 15810
                    && it.flag == InventoryFlag::Hangar
            });
            assert!(
                delivered.is_some(),
                "Delivered item must be in character hangar"
            );
        });

        // Take offer 1004 which requires trade-in items (100x Tritanium type 34)
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                77_000_101,
                InventoryItemRecord {
                    item_id: 77_000_101,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let trit_qty_before: i64 = ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.type_id == 34
                        && it.location_id == station_id
                })
                .map(|it| it.quantity as i64)
                .sum()
        });

        let res_char_1004 = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(1_000_035), PyRep::Integer(1004), PyRep::Integer(2)],
        )
        .await
        .expect("TakeOfferForCharacter 1004 x2 with trade-in");
        assert_eq!(kv_i64(&res_char_1004, "offerID"), 1004);
        assert_eq!(kv_i64(&res_char_1004, "corpID"), 1_000_035);
        assert_eq!(kv_i64(&res_char_1004, "quantity"), 2);
        assert!(kv_i64(&res_char_1004, "deliveredItemID") > 0);

        // Verify trade-in items consumed (2x 100 = 200 consumed)
        ctx.db.with_snapshot(|s| {
            let trit_qty_after: i64 = s
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.type_id == 34
                        && it.location_id == station_id
                })
                .map(|it| it.quantity as i64)
                .sum();
            assert_eq!(
                trit_qty_before - trit_qty_after,
                200,
                "Trade-in Tritanium must be consumed"
            );
        });

        // 3. TakeOfferForCorporation
        let mut corp_bals = HashMap::new();
        corp_bals.insert(1_000_035, 100_000);
        ctx.db.with_mut_snapshot(|s| {
            save_corp_lp_balances(s, corp_id, &corp_bals);
            if let Some(c) = s.corporations.get_mut(&corp_id) {
                c.ensure_wallet_divisions();
                c.wallet_divisions[0].balance = IskCents::from_isk(50_000_000);
            }
        });

        let res_corp = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[PyRep::Integer(1_000_035), PyRep::Integer(1001), PyRep::Integer(1)],
        )
        .await
        .expect("TakeOfferForCorporation 1001");
        assert_eq!(kv_i64(&res_corp, "offerID"), 1001);
        assert_eq!(kv_i64(&res_corp, "corpID"), 1_000_035);
        assert_eq!(kv_i64(&res_corp, "quantity"), 1);
        assert!(kv_i64(&res_corp, "deliveredItemID") > 0);

        ctx.db.with_snapshot(|s| {
            let b = load_corp_lp_balances(s, corp_id);
            assert_eq!(b.get(&1_000_035).copied(), Some(100_000 - 2_400));
            let c = s.corporations.get(&corp_id).unwrap();
            assert_eq!(
                c.wallet_divisions[0].balance,
                IskCents::from_isk(50_000_000 - 2_400_000)
            );
            let corp_item = s.items.values().find(|it| {
                it.owner_id == corp_id
                    && it.location_id == station_id
                    && it.type_id == 15810
            });
            assert!(corp_item.is_some(), "Delivered item must be owned by corp");
        });
    }

    #[tokio::test]
    async fn test_standing_mgr_npc_npc_standings_closure() {
        // DATA GAP: V24 client SDE does not ship the full server-side inter-NPC standings matrix;
        // stands open until static data provenance is available, while GetNPCNPCStandings returns derived faction pairs.
        let ctx = ServerContext::new();
        let mut sess = SessionState::new(4322);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let char_id = 90_000_001;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");

        let rep = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
            .await
            .expect("GetNPCNPCStandings");

        let PyRep::Rowset { columns, rows, .. } = rep else {
            panic!("GetNPCNPCStandings must return a Rowset");
        };

        assert_eq!(columns, vec!["fromID", "toID", "standing"]);
        assert!(!rows.is_empty(), "GetNPCNPCStandings must return derived inter-faction pairs");

        let mut rel_map = HashMap::new();
        for row in &rows {
            assert_eq!(row.len(), 3);
            let from_id = row[0].as_i64().expect("fromID");
            let to_id = row[1].as_i64().expect("toID");
            let standing = match &row[2] {
                PyRep::Real(v) => *v,
                PyRep::Integer(v) => *v as f64,
                other => panic!("expected real standing, got {other:?}"),
            };
            assert!((-10.0..=10.0).contains(&standing), "Standing must be in [-10, 10]");
            rel_map.insert((from_id, to_id), standing);
        }
        for &(f, t, exp) in &[
            (500_003, 500_002, -5.0),
            (500_015, 500_002, 3.0),
            (500_026, 500_027, -10.0),
            (500_027, 500_026, -10.0),
        ] {
            assert_eq!(rel_map.get(&(f, t)).copied(), Some(exp), "mismatch for ({f}, {t})");
        }
    }

    #[tokio::test]
    async fn test_shipcaster_and_starter_travel_closure() {
        // DATA GAP: Starter and interstellar shipcaster structures and linkages are server-side.
        // Tests CanCharacterJump weapons timer and capital ship restrictions, proximity check, and ball transfer.
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut sess = SessionState::new(4323);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let char_id = 90_000_001;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");

        let ship_id = 100_000_001;
        let starter_sys: u32 = 30000141; // Kisogo (State War Academy starter school)
        let staging_sys: u32 = 30045324; // Onnamon (Caldari FW HQ)
        let starter_launcher_id: i64 = 105_000_117;

        // Deploy & build 2 Caldari FW shipcaster beacons in lowsec warzone systems (30002811, 30002813)
        for (idx, war_sys) in [30002811_u32, 30002813_u32].into_iter().enumerate() {
            let pos = DVec3::new(100_000.0 + (idx as f64) * 5_000.0, 0.0, 0.0);
            sess.station_id = None;
            sess.ship_id = Some(ship_id);
            sess.solar_system_id = Some(war_sys);
            spawn_test_ship(&ctx, &ctx.systems.get_or_spawn(war_sys), char_id, ship_id, 601, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            let cid = ctx.db.with_mut_snapshot(|s| {
                if let Some(ch) = s.characters.get_mut(&char_id) {
                    ch.war_faction_id = Some(500_001);
                    ch.solar_system_id = war_sys;
                    ch.station_id = None;
                }
                if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = war_sys as i64; }
                s.next_entity_id += 1;
                let cid = s.next_entity_id;
                s.items.insert(cid, InventoryItemRecord {
                    item_id: cid, type_id: 75933, owner_id: char_id, location_id: ship_id,
                    flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
                });
                cid
            });
            let dropped = rpc(&ctx, &mut sess, "ship", "Drop", &[PyRep::List(vec![PyRep::Integer(cid)]), PyRep::Integer(char_id), PyRep::Bool(false)]).await.expect("Drop");
            let pad_id = match dropped {
                PyRep::Dict(v) => v.into_iter().find_map(|(k, val)| match (k, val) {
                    (PyRep::Integer(ik), PyRep::List(ids)) if ik == cid => ids.first().and_then(|x| x.as_i64()),
                    _ => None,
                }).unwrap_or(0),
                _ => 0,
            };
            let reqs = eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
                st.landing_pads.iter().find(|p| p.item_id == pad_id).map(|p| p.required_materials.clone()).unwrap_or_default()
            });
            let mat_dict: Vec<(PyRep, PyRep)> = ctx.db.with_mut_snapshot(|s| {
                reqs.into_iter().map(|(tid, qty)| {
                    s.next_entity_id += 1;
                    let iid = s.next_entity_id;
                    s.items.insert(iid, InventoryItemRecord {
                        item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                        flag: InventoryFlag::Cargo, quantity: qty as i32, ..Default::default()
                    });
                    (PyRep::Integer(i64::from(tid)), PyRep::Integer(i64::from(qty)))
                }).collect()
            });
            rpc(&ctx, &mut sess, "beyonce", "CallComponentFromClient", &[PyRep::Integer(pad_id), PyRep::String("underConstruction".into()), PyRep::String("DepositItemsFromShip".into()), PyRep::Dict(mat_dict)]).await.expect("DepositItemsFromShip");
        }

        // 1. GetAllLandingPads
        let all_pads_rep = rpc(
            &ctx,
            &mut sess,
            "shipCasterLandingPadMgr",
            "GetAllLandingPads",
            &[],
        )
        .await
        .expect("GetAllLandingPads");
        let PyRep::List(all_pads) = all_pads_rep else {
            panic!("Expected List");
        };
        assert_eq!(all_pads.len(), 2);
        for pad in &all_pads {
            assert!(kv_i64(pad, "itemID") > 0);
            assert!(kv_i64(pad, "solarSystemID") > 0);
            assert!(matches!(kv_i64(pad, "factionID"), 500001..=500004));
            assert!(kv_bool(pad, "isBuilt") && kv_bool(pad, "canBeLinked"));
        }

        // 2. GetFactionLandingPads
        let fpad_rep = rpc(
            &ctx,
            &mut sess,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(500001)],
        )
        .await
        .expect("GetFactionLandingPads");
        let PyRep::List(fpads) = fpad_rep else {
            panic!("Expected List");
        };
        assert_eq!(fpads.len(), 2);
        assert_eq!(kv_i64(&fpads[0], "factionID"), 500001);
        let linked: Vec<&PyRep> = fpads.iter().filter(|p| kv_bool(p, "isLinked")).collect();
        assert_eq!(linked.len(), 2);
        let caldari_active_pad_id = kv_i64(linked[0], "itemID");
        let caldari_active_warzone_sys = kv_i64(linked[0], "solarSystemID") as u32;
        let empty_pads = rpc(
            &ctx,
            &mut sess,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(999_999)],
        )
        .await
        .expect("GetFactionLandingPads unknown");
        assert_eq!(empty_pads, PyRep::List(vec![]));

        // 3. GetFactionsWithShipcaster
        let fws_rep = rpc(
            &ctx,
            &mut sess,
            "shipCasterLauncherMgr",
            "GetFactionsWithShipcaster",
            &[],
        )
        .await
        .expect("GetFactionsWithShipcaster");
        let PyRep::List(fws) = fws_rep else {
            panic!("Expected List");
        };
        assert_eq!(
            fws,
            vec![
                PyRep::Integer(500001),
                PyRep::Integer(500002),
                PyRep::Integer(500003),
                PyRep::Integer(500004)
            ]
        );

        // 4. CanCharacterJump
        // Undocked in frigate: true
        sess.station_id = None;
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(starter_sys);
        let now_rookie = ctx.now_unix().saturating_sub(86_400);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.solar_system_id = starter_sys;
                ch.station_id = None;
                ch.create_date_unix = now_rookie;
            }
            if let Some(ship) = s.items.get_mut(&ship_id) {
                ship.type_id = 601; // Ibis frigate
                ship.location_id = starter_sys as i64;
            }
        });

        let can_jump = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CanCharacterJump",
            &[],
        )
        .await
        .expect("CanCharacterJump");
        assert_eq!(can_jump, PyRep::Bool(true));

        // Weapons timer active -> false
        let now = ctx.now_unix();
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            cw.get_or_create(char_id).weapon_timer_until_unix = now + 60;
        }
        let timer_rep = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CanCharacterJump",
            &[],
        )
        .await
        .expect("CanCharacterJump with timer");
        assert_eq!(timer_rep, PyRep::Bool(false));
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            cw.get_or_create(char_id).weapon_timer_until_unix = 0;
        }

        // Capital ship -> false
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ship) = s.items.get_mut(&ship_id) {
                ship.type_id = 19720; // Revelation (Dreadnought, group 485)
            }
        });
        let cap_rep = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CanCharacterJump",
            &[],
        )
        .await
        .expect("CanCharacterJump with dreadnought");
        assert_eq!(cap_rep, PyRep::Bool(false));
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ship) = s.items.get_mut(&ship_id) {
                ship.type_id = 601; // Reset to frigate
            }
        });

        // 5. CmdJumpThroughStarterShipcaster
        // Negative test (a): Out of range boundary check (5.1km > SHIPCASTER_MAX_RANGE_M 5.0km from launcher)
        let starter_pos = eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
            st.starter_launchers.iter().find(|l| l.solar_system_id == starter_sys).map(|l| l.position).unwrap()
        });
        let handle_starter = ctx.systems.get_or_spawn(starter_sys);
        spawn_test_ship(
            &ctx,
            &handle_starter,
            char_id,
            ship_id,
            601,
            starter_pos + DVec3::new(0.0, 5_100.0, 0.0),
        )
        .await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
        let oor_err = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(starter_launcher_id)],
        )
        .await
        .unwrap_err();
        assert_eq!(oor_err.code, "OutsideOperationalRange");

        // Negative test (b): Session-change timer active at valid in-range boundary (4.9km <= 5.0km)
        assert!(
            handle_starter
                .set_ball_position(ship_id, starter_pos + DVec3::new(0.0, 4_900.0, 0.0))
                .await
        );
        let now_unix = ctx.now_unix();
        eve_server::services::space_destiny::record_session_change_timer(
            &ctx,
            char_id,
            now_unix,
            starter_sys,
            0,
            "test_session_lock",
        );
        let timer_err = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(starter_launcher_id)],
        )
        .await
        .unwrap_err();
        assert_eq!(timer_err.code, "SessionChangeInProgress");

        // Success path: clear session timer and perform jump at 4.9km boundary
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
        let jump_starter = rpc(
            &ctx,
            &mut sess,
            "starterShipcasterTravelMgr",
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(starter_launcher_id)],
        )
        .await
        .expect("CmdJumpThroughStarterShipcaster");
        assert_eq!(jump_starter, PyRep::None);
        assert_eq!(sess.solar_system_id, Some(staging_sys));

        ctx.db.with_snapshot(|s| {
            assert_eq!(
                s.characters.get(&char_id).unwrap().solar_system_id,
                staging_sys
            );
            assert_eq!(
                s.items.get(&ship_id).unwrap().location_id,
                staging_sys as i64
            );
        });

        // 6. CmdJumpThroughShipcaster (Interstellar FW shipcaster)
        let (origin_launcher_id, origin_pos) = eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
            st.launchers.iter().find(|l| l.faction_id == 500001).map(|l| (l.item_id, l.position)).unwrap()
        });
        let handle_staging = ctx.systems.get_or_spawn(staging_sys);
        spawn_test_ship(
            &ctx,
            &handle_staging,
            char_id,
            ship_id,
            601,
            origin_pos + DVec3::new(1_000.0, 0.0, 0.0),
        )
        .await;

        // Enroll in Caldari militia
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.war_faction_id = Some(500001);
            }
        });
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
        let jump_fw = rpc(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(origin_launcher_id),
                PyRep::Integer(i64::from(caldari_active_warzone_sys)),
                PyRep::Integer(caldari_active_pad_id),
            ],
        )
        .await
        .expect("CmdJumpThroughShipcaster");
        assert_eq!(jump_fw, PyRep::None);
        assert_eq!(sess.solar_system_id, Some(caldari_active_warzone_sys));

        ctx.db.with_snapshot(|s| {
            assert_eq!(
                s.characters.get(&char_id).unwrap().solar_system_id,
                caldari_active_warzone_sys
            );
            assert_eq!(
                s.items.get(&ship_id).unwrap().location_id,
                caldari_active_warzone_sys as i64
            );
        });
    }
}
