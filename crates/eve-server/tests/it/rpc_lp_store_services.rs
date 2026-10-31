//! Integration tests for Loyalty Point Store (`LPStoreMgr`), LP Services (`LPSvc`), and FW LP (`feat:fw.fw-lp`).

use crate::common;
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const CALDARI_NAVY_CORP: i64 = 1_000_035;
const CONCORD_CORP: i64 = 1_000_125;
const STATE_PROTECTORATE_CORP: i64 = 1_000_180; // Caldari militia corp
const OTHER_PLAYER_CORP: i64 = 98_000_001;

fn get_dict_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if let PyRep::String(s) | PyRep::Token(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn setup_pilot_session(ctx: &ServerContext) -> SessionState {
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
    sess.station_id = Some(60_003_760);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    common::seed_test_lp_store_offers(ctx);
    sess
}

#[tokio::test]
async fn test_lp_store_offers_and_purchases() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // 1. Get available offers from Caldari Navy (1000035)
    let offers_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(CALDARI_NAVY_CORP)],
    )
    .await
    .expect("GetAvailableOffersFromCorp Caldari Navy");

    let PyRep::List(ref offers) = offers_rep else {
        panic!("Expected List of offers from GetAvailableOffersFromCorp");
    };
    assert!(!offers.is_empty(), "Caldari Navy offers should not be empty");
    let offer_1001 = offers
        .iter()
        .find(|o| get_dict_entry(o, "offerID").and_then(|v| v.as_i64()) == Some(1001))
        .expect("Offer 1001 must exist");
    assert_eq!(
        get_dict_entry(offer_1001, "corpID").and_then(|v| v.as_i64()),
        Some(CALDARI_NAVY_CORP)
    );
    assert_eq!(
        get_dict_entry(offer_1001, "typeID").and_then(|v| v.as_i64()),
        Some(15810) // Caldari Navy Ballistic Control System
    );
    assert_eq!(
        get_dict_entry(offer_1001, "lpCost").and_then(|v| v.as_i64()),
        Some(2_400)
    );

    // 2. Militia LP Store query (State Protectorate 1000180, `feat:fw.fw-lp`)
    let fw_offers_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(STATE_PROTECTORATE_CORP)],
    )
    .await
    .expect("GetAvailableOffersFromCorp militia");
    let PyRep::List(ref fw_offers) = fw_offers_rep else {
        panic!("Expected List from militia LP store");
    };
    assert!(
        fw_offers
            .iter()
            .any(|o| get_dict_entry(o, "offerID").and_then(|v| v.as_i64()) == Some(7002)),
        "Militia offer 7002 must be available in State Protectorate LP store"
    );

    // 3. Seed pilot LP and ISK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.balance = IskCents::from_isk(100_000_000);
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 50_000);
            ch.loyalty_points.insert(STATE_PROTECTORATE_CORP, 20_000);
        }
    });

    // 4. Take offer for character (offer 1001, 1x)
    let take_char_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1001),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter offer 1001");

    assert_eq!(
        get_dict_entry(&take_char_rep, "offerID").and_then(|v| v.as_i64()),
        Some(1001)
    );
    assert_eq!(
        get_dict_entry(&take_char_rep, "remainingLP").and_then(|v| v.as_i64()),
        Some(50_000 - 2_400)
    );
    let delivered_item_id = get_dict_entry(&take_char_rep, "deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify delivered item in station hangar
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&delivered_item_id).expect("Delivered item exists");
        assert_eq!(it.type_id, 15810);
        assert_eq!(it.owner_id, CHAR_ID);
        assert_eq!(it.quantity, 1);
        assert_eq!(it.flag, InventoryFlag::Hangar);
    });

    // 5. Spend FW LP in militia LP store (`feat:fw.fw-lp`: spent in militia LP stores at State Protectorate station 60_015_067)
    sess.station_id = Some(60_015_067);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = Some(60_015_067);
        }
    });
    let take_fw_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(STATE_PROTECTORATE_CORP),
            PyRep::Integer(7002),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter militia offer 7002");
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = Some(60_003_760);
        }
    });
    assert_eq!(
        get_dict_entry(&take_fw_rep, "remainingLP").and_then(|v| v.as_i64()),
        Some(20_000 - 10_000)
    );

    // 6. Take offer for corporation
    // Seed corporation LP balance and ISK balance
    let player_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().corporation_id);
    let initial_char_balance = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().balance);
    
    ctx.db.with_mut_snapshot(|snap| {
        let mut bals = HashMap::new();
        bals.insert(CALDARI_NAVY_CORP, 30_000);
        eve_server::services::lp_store::save_corp_lp_balances(snap, player_corp_id, &bals);
        if let Some(corp) = snap.corporations.get_mut(&player_corp_id) {
            corp.master_wallet = IskCents::from_isk(100_000_000);
        } else {
            let corp = eve_db::types::CorporationRecord {
                corporation_id: player_corp_id,
                name: "Test Corp".into(),
                ticker: "TEST".into(),
                ceo_id: CHAR_ID + 1, // ensure the caller is not CEO so role check applies
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: IskCents::from_isk(100_000_000),
                members: [CHAR_ID].into_iter().collect(),
                member_roles: std::collections::HashMap::new(),
                applications: std::collections::HashMap::new(),
                wallet_divisions: vec![],
                description: "".into(),
                url: "".into(),
                hq_station_id: None,
            };
            snap.corporations.insert(player_corp_id, corp);
        }
    });

    // Verify Junior Accountant cannot take offer
    sess.corp_role = eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT;
    let err_jr = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1001),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Junior Accountant should fail");
    assert_eq!(err_jr.code, "InsufficientCorporationRole");

    // Verify grantable roles are not sufficient
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert(
            (player_corp_id, CHAR_ID),
            eve_db::snapshot_ext::CorpMemberRoleAssignment {
                character_id: CHAR_ID,
                roles: 0,
                grantable_roles: eve_db::corp_roles::ROLE_DIRECTOR,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );
    });
    sess.corp_role = 0;
    let err_grant = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1001),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Grantable roles should fail");
    assert_eq!(err_grant.code, "InsufficientCorporationRole");

    // Restore Director role for successful purchase
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let take_corp_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1001),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCorporation");
    assert_eq!(
        get_dict_entry(&take_corp_rep, "remainingLP").and_then(|v| v.as_i64()),
        Some(30_000 - 2_400)
    );
    let corp_delivered_item_id = get_dict_entry(&take_corp_rep, "deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID for corp");

    // Verify wallet debit is from corporation wallet, not character wallet, and item delivered to corp
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.balance, initial_char_balance, "Character balance should be untouched");
        
        let corp = snap.corporations.get(&player_corp_id).unwrap();
        assert_eq!(
            corp.master_wallet,
            IskCents::from_isk(100_000_000).checked_sub(IskCents::from_isk(2_400_000)).unwrap(),
            "Corporation wallet should be debited"
        );

        let it = snap.items.get(&corp_delivered_item_id).expect("Delivered corp item exists");
        assert_eq!(it.type_id, 15810);
        assert_eq!(it.owner_id, player_corp_id, "Item should be owned by the corporation");
        assert_eq!(it.quantity, 1);
        assert_eq!(it.flag, eve_db::types::InventoryFlag::CorpDeliveries, "Item should be in corp deliveries hangar");
    });

    // 7. Error cases
    // Offer not found
    let err_not_found = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(999_999)],
    )
    .await
    .expect_err("Unknown offer should fail");
    assert_eq!(err_not_found.code, "OfferNotFound");

    // Missing required items (offer 1004 requires 100x Tritanium type 34; clear default Tritanium first)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == CHAR_ID && it.type_id == 34));
    });
    let err_missing_items = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Missing required items should fail");
    assert_eq!(err_missing_items.code, "MissingRequiredItems");

    // Fulfill trade-in requirements and successfully take offer 1004
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_000_100,
            eve_db::InventoryItemRecord {
                item_id: 990_000_100,
                type_id: 34,
                owner_id: CHAR_ID,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let take_tradein_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter with trade-in should succeed");
    assert_eq!(
        get_dict_entry(&take_tradein_rep, "deliveredTypeID").and_then(|v| v.as_i64()),
        Some(15810)
    );
    // Verify trade-in items were consumed
    let remaining_tradein_qty: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.owner_id == CHAR_ID && it.type_id == 34)
            .map(|it| it.quantity)
            .sum()
    });
    assert_eq!(remaining_tradein_qty, 0, "Trade-in minerals should be fully consumed");
}

#[tokio::test]
async fn test_lp_wallet_balances_and_transfers() {
    // parity: rpc:LPSvc.GetAllMyCharacterWalletLPBalances
    // parity: rpc:LPSvc.GetAllMyCorporationWalletLPBalances
    // parity: rpc:LPSvc.TransferLPFromMyWalletToOtherCorp
    // parity: rpc:LPSvc.TransferLPFromMyCorpWalletToOtherCorp
    // parity: rpc:LPSvc.ExchangeConcordLP
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // Seed pilot with LP: 40,000 Caldari Navy + 15,000 CONCORD
    let player_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).unwrap().corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.loyalty_points.insert(CALDARI_NAVY_CORP, 40_000);
            ch.loyalty_points.insert(CONCORD_CORP, 15_000);
        }
        let mut bals = HashMap::new();
        bals.insert(CALDARI_NAVY_CORP, 50_000);
        eve_server::services::lp_store::save_corp_lp_balances(snap, player_corp_id, &bals);
    });

    // 1. GetAllMyCharacterWalletLPBalances
    let char_bals_rep = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "GetAllMyCharacterWalletLPBalances",
        &[],
    )
    .await
    .expect("GetAllMyCharacterWalletLPBalances");

    let PyRep::List(ref entries) = char_bals_rep else {
        panic!("Expected List from GetAllMyCharacterWalletLPBalances");
    };
    assert!(!entries.is_empty(), "Character LP balances not empty");
    let caldari_bal = entries
        .iter()
        .find(|e| get_dict_entry(e, "corpID").and_then(|v| v.as_i64()) == Some(CALDARI_NAVY_CORP))
        .expect("Caldari Navy LP entry");
    assert_eq!(
        get_dict_entry(caldari_bal, "loyaltyPoints").and_then(|v| v.as_i64()),
        Some(40_000)
    );

    // 2. GetAllMyCorporationWalletLPBalances
    let corp_bals_rep = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "GetAllMyCorporationWalletLPBalances",
        &[PyRep::Integer(player_corp_id)],
    )
    .await
    .expect("GetAllMyCorporationWalletLPBalances");
    let PyRep::List(ref corp_entries) = corp_bals_rep else {
        panic!("Expected List from GetAllMyCorporationWalletLPBalances");
    };
    let corp_caldari = corp_entries
        .iter()
        .find(|e| get_dict_entry(e, "corpID").and_then(|v| v.as_i64()) == Some(CALDARI_NAVY_CORP))
        .expect("Corp Caldari entry");
    assert_eq!(
        get_dict_entry(corp_caldari, "loyaltyPoints").and_then(|v| v.as_i64()),
        Some(50_000)
    );

    // 3. TransferLPFromMyWalletToOtherCorp (transfer 5,000 Caldari Navy LP to other player corp)
    let transfer_rep = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[
            PyRep::Integer(OTHER_PLAYER_CORP),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(5_000),
        ],
    )
    .await
    .expect("TransferLPFromMyWalletToOtherCorp");
    assert_eq!(
        get_dict_entry(&transfer_rep, "transferredLP").and_then(|v| v.as_i64()),
        Some(5_000)
    );
    assert_eq!(
        get_dict_entry(&transfer_rep, "senderRemainingLP").and_then(|v| v.as_i64()),
        Some(35_000)
    );
    assert_eq!(
        get_dict_entry(&transfer_rep, "targetCorpLPBalance").and_then(|v| v.as_i64()),
        Some(5_000)
    );

    // 4. TransferLPFromMyCorpWalletToOtherCorp (transfer 10,000 LP from my corp to other corp)
    let corp_transfer_rep = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "TransferLPFromMyCorpWalletToOtherCorp",
        &[
            PyRep::Integer(OTHER_PLAYER_CORP),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(10_000),
        ],
    )
    .await
    .expect("TransferLPFromMyCorpWalletToOtherCorp");
    assert_eq!(
        get_dict_entry(&corp_transfer_rep, "transferredLP").and_then(|v| v.as_i64()),
        Some(10_000)
    );
    assert_eq!(
        get_dict_entry(&corp_transfer_rep, "senderRemainingLP").and_then(|v| v.as_i64()),
        Some(40_000)
    );
    assert_eq!(
        get_dict_entry(&corp_transfer_rep, "targetCorpLPBalance").and_then(|v| v.as_i64()),
        Some(15_000) // 5k previous + 10k
    );

    // 5. ExchangeConcordLP (10,000 CONCORD LP -> Caldari Navy at 0.8 rate = 8,000 LP;
    //    and 2,500 CONCORD LP -> Sisters of EVE 1_000_130 at 0.4 rate = 1,000 LP)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.loyalty_points.insert(1_000_130, 100); // Sisters of EVE (non-pirate, non-empire)
        }
    });
    let exchange_rep = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "ExchangeConcordLP",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(10_000),
        ],
    )
    .await
    .expect("ExchangeConcordLP");
    assert_eq!(
        get_dict_entry(&exchange_rep, "concordLPSpent").and_then(|v| v.as_i64()),
        Some(10_000)
    );
    assert_eq!(
        get_dict_entry(&exchange_rep, "convertedLP").and_then(|v| v.as_i64()),
        Some(8_000)
    );
    assert_eq!(
        get_dict_entry(&exchange_rep, "remainingConcordLP").and_then(|v| v.as_i64()),
        Some(5_000) // 15k - 10k
    );
    assert_eq!(
        get_dict_entry(&exchange_rep, "newTargetCorpLP").and_then(|v| v.as_i64()),
        Some(35_000 + 8_000) // 35k remaining + 8k converted
    );

    // 5b. Non-empire, non-pirate 0.4 tier (Sisters of EVE 1_000_130: 2,500 CONCORD LP -> 1,000 LP)
    let exchange_soe = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "ExchangeConcordLP",
        &[PyRep::Integer(1_000_130), PyRep::Integer(2_500)],
    )
    .await
    .expect("ExchangeConcordLP Sisters of EVE 0.4 tier");
    assert_eq!(get_dict_entry(&exchange_soe, "convertedLP").and_then(|v| v.as_i64()), Some(1_000));
    assert_eq!(get_dict_entry(&exchange_soe, "newTargetCorpLP").and_then(|v| v.as_i64()), Some(1_100));

    // 5c. Preconditions: requires existing LP (> 0) with target corp and docked in NPC station
    let err_no_existing_lp = rpc(&ctx, &mut sess, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(1_000_002), PyRep::Integer(1_000)])
        .await
        .expect_err("ExchangeConcordLP without existing LP with target corp must fail");
    assert_eq!(err_no_existing_lp.code, "NoExistingTargetCorpLP");

    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
        }
    });
    let err_undocked = rpc(&ctx, &mut sess, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(1_000)])
        .await
        .expect_err("ExchangeConcordLP in space must fail");
    assert_eq!(err_undocked.code, "MustBeDockedInStation");
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = Some(60_003_760);
        }
    });

    // 6. Error handling
    // Transfer non-positive amount
    let err_neg = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[
            PyRep::Integer(OTHER_PLAYER_CORP),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect_err("Transfer 0 LP should fail");
    assert_eq!(err_neg.code, "InvalidLPAmount");

    // Foreign corporation LP wallet query must be denied
    let err_foreign_corp_lp = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "GetAllMyCorporationWalletLPBalances",
        &[PyRep::Integer(OTHER_PLAYER_CORP)],
    )
    .await
    .expect_err("Querying a foreign corporation's LP wallet must fail");
    assert_eq!(err_foreign_corp_lp.code, "CrpAccessDenied");

    // Cannot exchange CONCORD to CONCORD, militia corp, Food Relief (1000139), Sanctuary (1000159), Mordu's Legion (1000128), pirate corp, or non-existent corp
    for invalid_target in [CONCORD_CORP, STATE_PROTECTORATE_CORP, 1_000_139, 1_000_159, 1_000_128, 1_000_127, 999_999_999] {
        let err_invalid_target = rpc(
            &ctx,
            &mut sess,
            "LPSvc",
            "ExchangeConcordLP",
            &[PyRep::Integer(invalid_target), PyRep::Integer(1_000)],
        )
        .await
        .expect_err("Exchange CONCORD to ineligible or unknown corp should fail");
        assert_eq!(err_invalid_target.code, "InvalidTargetCorp");
    }

    // 7. Unprivileged caller (corp_role == 0) is rejected by GetAllMyCorporationWalletLPBalances,
    // TakeOfferForCorporation, and TransferLPFromMyCorpWalletToOtherCorp.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.remove(&player_corp_id);
        snap.corp_role_assignments.remove(&(player_corp_id, CHAR_ID));
    });
    sess.corp_role = 0;
    let err_unpriv_view = rpc(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[PyRep::Integer(player_corp_id)])
        .await
        .expect_err("Unprivileged caller must be rejected by GetAllMyCorporationWalletLPBalances");
    assert_eq!(err_unpriv_view.code, "CrpAccessDenied");
    let err_unpriv_take = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1001),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Unprivileged caller must be rejected by TakeOfferForCorporation");
    assert_eq!(err_unpriv_take.code, "InsufficientCorporationRole");

    let err_unpriv_transfer = rpc(
        &ctx,
        &mut sess,
        "LPSvc",
        "TransferLPFromMyCorpWalletToOtherCorp",
        &[
            PyRep::Integer(OTHER_PLAYER_CORP),
            PyRep::Integer(CALDARI_NAVY_CORP),
            PyRep::Integer(1_000),
        ],
    )
    .await
    .expect_err("Unprivileged caller must be rejected by TransferLPFromMyCorpWalletToOtherCorp");
    assert_eq!(err_unpriv_transfer.code, "InsufficientCorporationRole");
}
