//! Deep branch-coverage and mutation-killing unit tests for wallet ledgers & divisions,
//! market transactions, automatic bills, PLEX Vault, New Eden Store (`storeManager`),
//! Fast Checkout, `activityMgr`, player/corp/alliance bounties, and NPC bounty taxes.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx, PyRepTestExt};
use crate::services::combat_crimewatch::{
    crimewatch_bounty::handle_apply_tags_for_sec_status, handle as handle_crimewatch,
};
use crate::services::combat_npc_encounters::flush_npc_bounty_tick;
use crate::services::fast_checkout_and_activity::{handle as handle_fc_act, set_user_fast_checkout_enabled};
use crate::services::market_contracts::PLEX_TYPE_ID;
use crate::services::quasar_payment_store::{
    save_store_catalog, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
};
use crate::services::quasar_payment_store_types::{get_plex_transactions, PaymentTokenRecord};
use crate::services::wallet_ledger::handle as handle_ledger;
use crate::services::wallet_serverops::{
    create_bill, process_due_automatic_bills, CorpAutoPaySettings,
};
use crate::services::wallet_services_ext::handle as handle_ext;
use eve_db::{
    corp_roles, ledger::MarketTransactionRecord, AllianceRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, OwnerNoteRecord, ServerClock, VirtualServerClock,
};
use eve_machonet::{service_roles::ROLE_GML, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

async fn bounty(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_crimewatch(ctx, sess, "bountyProxy", method, args, &HashMap::new())
        .await
        .expect("bountyProxy handled")
}

fn ledger(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_ledger(ctx, sess, svc, method, args, kw).expect("ledger handled")
}

fn ext(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_ext(ctx, sess, svc, method, args, kw).expect("ext handled")
}

fn rowset_len(rep: &PyRep) -> usize {
    if let Some(rows) = rep.crowset_rows() {
        return rows.len();
    }
    match rep {
        PyRep::Rowset { rows, .. } => rows.len(),
        PyRep::Object { args, .. } => args
            .dict_get("lines")
            .and_then(|v| v.as_list())
            .map(|l| l.len())
            .unwrap_or(0),
        _ => 0,
    }
}

fn setup_wallet_world(now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>, i64, i64, i64, i64) {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let (char1, char2, corp1, corp2, station_id) =
        (90_000_001_i64, 90_000_002_i64, 98_600_001_i64, 98_600_002_i64, 60_003_760_i64);

    ctx.db.with_mut_snapshot(|snap| {
        let cur_plex = snap.plex_vault_balance(char1);
        if cur_plex != 0 {
            snap.adjust_plex_vault(char1, -cur_plex);
        }
        if let Some(c1) = snap.characters.get_mut(&char1) {
            c1.corporation_id = corp1;
            c1.station_id = Some(station_id);
            c1.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(c2) = snap.characters.get_mut(&char2) {
            c2.corporation_id = corp2;
            c2.station_id = Some(station_id);
            c2.balance = IskCents::from_isk(1_000_000);
        }
        for &(cid, name, ticker, ceo, tax) in &[
            (corp1, "Alpha Ledger Corp", "ALCP", char1, 0.20),
            (corp2, "Beta Ledger Corp", "BLCP", char2, 0.10),
        ] {
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.name = name.into();
                __corp.ticker = ticker.into();
                __corp.ceo_id = ceo;
                __corp.members = [ceo].into_iter().collect();
                __corp.member_roles = [(ceo, corp_roles::ROLE_DIRECTOR)].into_iter().collect();
                __corp.hq_station_id = Some(station_id);
                __corp.tax_rate = tax;
                __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
                __corp };
            corp.ensure_wallet_divisions();
            for (idx, div) in corp.wallet_divisions.iter_mut().enumerate() {
                div.balance = if idx == 0 {
                    IskCents::from_isk(10_000_000)
                } else {
                    IskCents::from_isk(5_000_000)
                };
            }
            snap.corporations.insert(cid, corp);
        }
    });
    (ctx, clock, char1, char2, corp1, corp2)
}

#[tokio::test]
async fn corp_wallet_divisions_journal_filters_market_txs_and_automatic_bills() {
    // 2024-03-15 12:00:00 UTC = 1_710_504_000
    let (ctx, clock, char1, char2, corp1, corp2) = setup_wallet_world(1_710_504_000);
    let mut sess1 = auth_session(1, char1, corp1, 30_000_142);
    sess1.station_id = Some(60_003_760);
    let mut sess2 = auth_session(2, char2, corp2, 30_000_142);
    let kw = HashMap::new();

    // Unauthorized corp member without Accountant or AccountTake1 denied across ledger & market RPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp1) {
            corp.members.insert(char2);
            corp.member_roles.insert(char2, 0);
        }
        if let Some(ch) = snap.characters.get_mut(&char2) {
            ch.corporation_id = corp1;
        }
    });
    assert_eq!(
        ledger(&ctx, &mut sess2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(char1), PyRep::Real(1000.0), PyRep::Integer(1000)], &kw)
            .unwrap_err().code,
        "CrpAccessDenied"
    );
    assert_eq!(
        ledger(&ctx, &mut sess2, "account", "GetTransactions", &[PyRep::Integer(1000), PyRep::None, PyRep::None, PyRep::Bool(true)], &kw)
            .unwrap_err().code,
        "CrpAccessDenied"
    );
    assert_eq!(
        ledger(&ctx, &mut sess2, "marketProxy", "CorpGetTransactions", &[], &kw).unwrap_err().code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2) {
            ch.corporation_id = corp2;
        }
    });

    // Validation & insufficient balance errors
    assert_eq!(
        ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(char2), PyRep::Real(0.0), PyRep::Integer(1000)], &kw)
            .unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(char2), PyRep::Real(100.0), PyRep::Integer(999)], &kw)
            .unwrap_err().code,
        "InvalidAccountKey"
    );
    assert_eq!(
        ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(char2), PyRep::Real(20_000_000.0), PyRep::Integer(1000)], &kw)
            .unwrap_err().code,
        "NotEnoughMoney"
    );
    assert_eq!(
        ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(corp1), PyRep::Real(100.0), PyRep::Integer(1000)], &kw)
            .unwrap_err().code,
        "InvalidAccountKey"
    );
    assert_eq!(
        ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(88_888_888), PyRep::Real(100.0), PyRep::Integer(1000)], &kw)
            .unwrap_err().code,
        "InvalidRecipient"
    );

    // Internal transfer: div 1 (1000) -> div 3 (1002) by 250_000 ISK
    let mut kw_to = HashMap::new();
    kw_to.insert("toAccountKey".into(), PyRep::Integer(1002));
    kw_to.insert("reason".into(), PyRep::String("SRP Top-Up".into()));
    ledger(&ctx, &mut sess1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(corp1), PyRep::Real(250_000.0), PyRep::Integer(1000)], &kw_to).unwrap();
    ctx.db.with_snapshot(|snap| {
        let c = &snap.corporations[&corp1];
        assert_eq!(c.master_wallet_balance(), IskCents::from_isk(9_750_000));
        assert_eq!(c.wallet_divisions[0].balance, IskCents::from_isk(9_750_000));
        assert_eq!(c.wallet_divisions[2].balance, IskCents::from_isk(5_250_000));
    });

    // External transfer from div 3 (1002) to char2 (100_000 ISK) leaves master_wallet unchanged
    ledger(
        &ctx, &mut sess1, "account", "GiveCashFromCorpAccount",
        &[PyRep::Integer(char2), PyRep::Real(100_000.0), PyRep::Integer(1002), PyRep::None, PyRep::String("Fleet bonus".into())],
        &kw,
    ).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp1].master_wallet_balance(), IskCents::from_isk(9_750_000));
        assert_eq!(snap.corporations[&corp1].wallet_divisions[2].balance, IskCents::from_isk(5_150_000));
        assert_eq!(snap.characters[&char2].balance, IskCents::from_isk(1_100_000));
    });

    // Corp journal filtering by accountKey (1002) and (year=2024, month=3 vs month=4)
    let j_1002 = ledger(&ctx, &mut sess1, "account", "GetTransactions", &[PyRep::Integer(1002), PyRep::Integer(2024), PyRep::Integer(3), PyRep::Bool(true)], &kw).unwrap();
    let j_wrong = ledger(&ctx, &mut sess1, "account", "GetTransactions", &[PyRep::Integer(1002), PyRep::Integer(2024), PyRep::Integer(4), PyRep::Bool(true)], &kw).unwrap();
    assert_eq!(rowset_len(&j_1002), 2);
    assert_eq!(rowset_len(&j_wrong), 0);

    // Market transactions cutoff (exact timestamp included, 1s later cutoff excludes old tx)
    let tx_time = 1_710_504_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 5001,
            type_id: 34,
            quantity: 100,
            price: IskCents::from_isk(5),
            location_id: 60_003_760,
            buyer_id: char1,
            seller_id: char2,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            journal_ref_id: 77,
            date_unix: tx_time,
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 5002,
            type_id: 35,
            quantity: 200,
            price: IskCents::from_isk(10),
            location_id: 60_003_760,
            buyer_id: corp1,
            seller_id: char2,
            buyer_account_key: 1002,
            seller_account_key: 1000,
            journal_ref_id: 78,
            date_unix: tx_time + 10,
        });
    });
    let ft_at = ((tx_time as i64) + 11_644_473_600) * 10_000_000;
    let ft_after = (((tx_time + 1) as i64) + 11_644_473_600) * 10_000_000;
    let m_char_at = ledger(&ctx, &mut sess1, "marketProxy", "CharGetTransactions", &[PyRep::Integer(ft_at)], &kw).unwrap();
    let m_char_after = ledger(&ctx, &mut sess1, "marketProxy", "CharGetTransactions", &[PyRep::Integer(ft_after)], &kw).unwrap();
    assert_eq!(rowset_len(&m_char_at), 1);
    assert_eq!(rowset_len(&m_char_after), 0);
    let m_corp_1002 = ledger(&ctx, &mut sess1, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(ft_at), PyRep::Integer(1002)], &kw).unwrap();
    assert_eq!(rowset_len(&m_corp_1002), 1);

    // Automatic corporation bill payment: due timestamp boundary + division 2 (1001) deduction
    let due_time = ctx.now_unix() + 1;
    let _bill_due = create_bill(&ctx, 2, corp1, corp2, 300_000.0, due_time, 0.0, 60_003_760, 0);
    let _bill_disabled = create_bill(&ctx, 5, corp1, corp2, 100_000.0, due_time, 0.0, 0, 0);
    ctx.db.with_mut_snapshot(|snap| {
        let mut enabled = BTreeMap::new();
        enabled.insert(2, true);
        let ap = CorpAutoPaySettings { division_id: 1001, enabled_bill_types: enabled };
        let nid = -(7_200_000_000_000_i64 + corp1);
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -72,
                item_id: corp1,
                label: "autopay".into(),
                note: serde_json::to_string(&ap).unwrap(),
                timestamp_unix: ctx.now_unix(),
            },
        );
    });
    assert_eq!(process_due_automatic_bills(&ctx), 0, "must not pay bill 1s before due");
    clock.advance_secs(1);
    assert_eq!(process_due_automatic_bills(&ctx), 1, "only enabled bill type 2 paid at due_unix");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp1].wallet_divisions[1].balance, IskCents::from_isk(4_700_000));
        assert_eq!(snap.corporations[&corp1].master_wallet_balance(), IskCents::from_isk(9_750_000));
        assert_eq!(snap.corporations[&corp2].master_wallet_balance(), IskCents::from_isk(10_300_000));
    });
}

#[tokio::test]
async fn plex_vault_and_nes_store_manager_purchases_kill_mutations() {
    let (ctx, _clock, char1, _char2, corp1, _corp2) = setup_wallet_world(1_710_504_000);
    let (station_id, plex_item_id, non_plex_id) = (60_003_760_i64, 90_000_801_i64, 90_000_802_i64);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        for &(id, tid, qty) in &[(plex_item_id, PLEX_TYPE_ID, 100), (non_plex_id, 34, 50)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char1,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = auth_session(1, char1, corp1, 30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(100_000_001);

    // PLEX Vault Deposit guards
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(plex_item_id), PyRep::Integer(0)], &kw).unwrap_err().code, "InvalidArgument");
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id + 1), PyRep::Integer(plex_item_id), PyRep::Integer(10)], &kw).unwrap_err().code, "NotDockedAtLocation");
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(non_plex_id), PyRep::Integer(10)], &kw).unwrap_err().code, "NotPlex");
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(plex_item_id), PyRep::Integer(101)], &kw).unwrap_err().code, "NotEnoughQuantity");

    // Partial deposit (40) then exact remaining deposit (60)
    ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(plex_item_id), PyRep::Integer(40)], &kw).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.plex_vault_balance(char1), 40);
        assert_eq!(snap.items[&plex_item_id].quantity, 60);
    });
    ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(plex_item_id), PyRep::Integer(60)], &kw).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.plex_vault_balance(char1), 100);
        assert!(!snap.items.contains_key(&plex_item_id));
    });

    // PLEX Vault Withdraw validation & stack merge
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(101), PyRep::Integer(station_id)], &kw).unwrap_err().code, "NotEnoughPlex");
    assert_eq!(ledger(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(10), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw).unwrap_err().code, "InvalidFlag");
    let w1 = ledger(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(30), PyRep::Integer(station_id)], &kw).unwrap();
    assert_eq!(w1.dict_get("remainingVaultBalance"), Some(&PyRep::Integer(70)));
    let w2 = ledger(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(25)], &kw).unwrap();
    assert_eq!(w2.dict_get("remainingVaultBalance"), Some(&PyRep::Integer(45)));
    assert_eq!(w2.dict_get("itemID"), w1.dict_get("itemID"), "consecutive withdraws merge into same stack");

    // Re-deposit 54 PLEX so vault = 99 PLEX
    let stack_id = w1.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(stack_id), PyRep::Integer(54)], &kw).unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char1)), 99);

    // Custom NES catalog & storeManager boundary conditions
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory { id: 1, name: "Omega".into(), description: "Omega".into() }],
                products: vec![StoreProduct { id: 501, name: "Extractor".into(), description: "SE".into(), type_id: 40519, quantity: 3 }],
                offers: vec![
                    StoreOffer { id: 2001, name: "Omega + SE".into(), description: "Bundle".into(), category_id: 1, price_plex: 50, product_ids: vec![501], targeted: false, price_cents: None, currency: None },
                    StoreOffer { id: 2002, name: "Targeted Promo".into(), description: "Hidden".into(), category_id: 1, price_plex: 20, product_ids: vec![501], targeted: true, price_cents: None, currency: None },
                ],
            },
        );
    });
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "get_categories", &[], &kw).unwrap().as_list().map(|l| l.len()), Some(2));
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "get_products", &[], &kw).unwrap().as_list().map(|l| l.len()), Some(1));
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "get_offers", &[], &kw).unwrap().as_list().map(|l| l.len()), Some(1));

    // Currency & quantity validation errors
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(2001), PyRep::String("AUR".into())], &kw).unwrap_err().code, "AurumCurrencyRemoved");
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(2001), PyRep::String("ISK".into())], &kw).unwrap_err().code, "InvalidCurrency");
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(2001), PyRep::String("PLX".into()), PyRep::Integer(0)], &kw).unwrap_err().code, "InvalidQuantity");
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(999_999)], &kw).unwrap_err().code, "OfferNotFound");

    // Qty 2 * 50 PLEX = 100 PLEX; with 99 PLEX in vault -> false / InsufficientPlexBalance
    let can_99 = ext(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::String("VEVC-2001".into()), PyRep::String("PLX".into()), PyRep::Integer(2)], &kw).unwrap();
    assert_eq!(can_99, PyRep::Bool(false));
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(2001), PyRep::String("PLX".into()), PyRep::Integer(2)], &kw).unwrap_err().code, "InsufficientPlexBalance");

    // Deposit remaining 1 PLEX so vault == 100 (exact threshold), then purchase qty 2 -> 6 Extractors delivered!
    ledger(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(station_id), PyRep::Integer(stack_id), PyRep::Integer(1)], &kw).unwrap();
    assert_eq!(ext(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(2001), PyRep::String("PLEX".into()), PyRep::Integer(2)], &kw).unwrap(), PyRep::Bool(true));

    let bought = ext(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::String("VEVC-2001".into()), PyRep::String("PLX".into()), PyRep::Integer(2)], &kw).unwrap();
    assert_eq!(bought.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(bought.dict_get("plexSpent"), Some(&PyRep::Integer(100)));
    assert_eq!(bought.dict_get("plexRemaining"), Some(&PyRep::Integer(0)));
    let deliv_id = bought.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&deliv_id].quantity, 6);
        assert_eq!(get_plex_transactions(snap).iter().filter(|t| t.user_id == 1).last().map(|t| t.amount), Some(-100));
    });
}

#[tokio::test]
async fn fast_checkout_activity_mgr_and_bounties_taxes_kill_mutations() {
    let (ctx, _clock, char1, char2, corp1, corp2) = setup_wallet_world(1_710_504_000);
    let ally1 = 99_600_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(ally1, AllianceRecord { alliance_id: ally1, name: "Bounty Test Alliance".into(), short_name: "BTAL".into(), executor_corp_id: corp1, member_corp_ids: [corp1].into_iter().collect() });
        if let Some(c) = snap.corporations.get_mut(&corp1) { c.alliance_id = Some(ally1); }
        if let Some(ch) = snap.characters.get_mut(&char1) { ch.alliance_id = Some(ally1); }
    });

    let mut sess1 = auth_session(1, char1, corp1, 30_000_142);
    sess1.station_id = Some(60_003_760);
    let mut sess2 = auth_session(2, char2, corp2, 30_000_142);
    let kw = HashMap::new();

    // FastCheckout disabled by default -> FastCheckoutDisabled; enable via free_fast_checkout_plex
    assert_eq!(handle_fc_act(&ctx, &sess1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "FastCheckoutDisabled");
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = true;
    set_user_fast_checkout_enabled(&ctx, 1, true);
    let offers_rep = handle_fc_act(&ctx, &sess1, "FastCheckoutService", "GetOffersForUser", &[], &kw).unwrap().unwrap();
    let offer0 = &offers_rep.dict_get("plex").and_then(|v| v.as_list()).unwrap()[0];
    let fc_offer_id = offer0.dict_get("offerID").and_then(|v| v.as_i64()).unwrap();
    let fc_plex_qty = offer0.dict_get("baseQuantity").and_then(|v| v.as_i64()).unwrap();

    // Empty password rejected even in free_fast_checkout_plex? Wait: !free_plex_mode && password.is_empty() -> succeeds in free_plex_mode; now test paid mode password & token gates!
    let fc_buy = handle_fc_act(&ctx, &sess1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(fc_offer_id), PyRep::Integer(2)], &kw).unwrap().unwrap();
    assert_eq!(fc_buy.dict_get("status"), Some(&PyRep::String("COMPLETED".into())));
    assert_eq!(fc_buy.dict_get("plexAmount"), Some(&PyRep::Integer(fc_plex_qty * 2)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char1)), fc_plex_qty * 2);

    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    let paid_uid = ctx.db.provision_player_account("fc_paid_user", "secret123");
    set_user_fast_checkout_enabled(&ctx, paid_uid, true);
    let paid_sess = auth_session(paid_uid, char1, corp1, 30_000_142);
    assert_eq!(handle_fc_act(&ctx, &paid_sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(fc_offer_id), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "InvalidPassword");
    assert_eq!(handle_fc_act(&ctx, &paid_sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(fc_offer_id), PyRep::Integer(1), PyRep::String("secret123".into())], &kw).unwrap().unwrap_err().code, "PaymentTokenRequired");

    ctx.db.with_mut_snapshot(|snap| {
        let tok = PaymentTokenRecord { id: 1, user_id: paid_uid, token_str: "tok".into(), is_quick_pay: true, consumed: false };
        snap.quasar_commerce.payment_tokens = vec![tok];
    });
    let fc_paid = handle_fc_act(&ctx, &paid_sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(fc_offer_id), PyRep::Integer(1), PyRep::String("secret123".into())], &kw).unwrap().unwrap();
    assert_eq!(fc_paid.dict_get("status"), Some(&PyRep::String("CONFIRMED".into())));
    assert_eq!(fc_paid.dict_get("plexAmount"), Some(&PyRep::Integer(fc_plex_qty)));
    assert_eq!(fc_paid.dict_get("newPlexBalance"), Some(&PyRep::Integer(fc_plex_qty * 3)));
    handle_fc_act(&ctx, &sess1, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw).unwrap().unwrap();

    // ActivityMgr validation + seen badge tracking
    assert_eq!(handle_fc_act(&ctx, &sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(0)], &kw).unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_fc_act(&ctx, &sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(999_999_999)], &kw).unwrap().unwrap_err().code, "InvalidActivityID");
    if let Ok(ids) = ctx.sde.load_new_activity_ids() {
        if let Some(&aid) = ids.iter().next() {
            handle_fc_act(&ctx, &sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(aid)], &kw).unwrap().unwrap();
            let badges = handle_fc_act(&ctx, &sess1, "activityMgr", "GetActivityBadgeStatuses", &[], &kw).unwrap().unwrap();
            assert!(badges.as_list().unwrap().contains(&PyRep::Integer(aid)));
        }
    }

    // Player & Corp/Alliance Bounties: exact 100,000 ISK minimum boundary & leaderboards
    assert_eq!(bounty(&ctx, &mut sess1, "AddToBounty", &[PyRep::Integer(char2), PyRep::Real(99_999.99)]).await.unwrap_err().code, "BountyAmountTooLow");
    assert_eq!(bounty(&ctx, &mut sess1, "AddToBounty", &[PyRep::Integer(77_777_777), PyRep::Real(100_000.0)]).await.unwrap_err().code, "TargetNotFound");
    bounty(&ctx, &mut sess1, "AddToBounty", &[PyRep::Integer(char2), PyRep::Real(100_000.0)]).await.unwrap();
    bounty(&ctx, &mut sess1, "AddToBounty", &[PyRep::Integer(corp2), PyRep::Real(250_000.0)]).await.unwrap();
    bounty(&ctx, &mut sess1, "AddToBounty", &[PyRep::Integer(ally1), PyRep::Real(500_000.0)]).await.unwrap();

    let single = bounty(&ctx, &mut sess2, "GetBounty", &[PyRep::Integer(char2)]).await.unwrap();
    assert_eq!(single.dict_get("bountyISK"), Some(&PyRep::Real(100_000.0)));
    let top_corps = bounty(&ctx, &mut sess1, "GetTopCorpBounties", &[]).await.unwrap();
    assert_eq!(top_corps.as_list().unwrap()[0].dict_get("targetID"), Some(&PyRep::Integer(corp2)));
    let top_allies = bounty(&ctx, &mut sess1, "GetTopAllianceBounties", &[]).await.unwrap();
    assert_eq!(top_allies.as_list().unwrap()[0].dict_get("targetID"), Some(&PyRep::Integer(ally1)));
    let search_pilot = bounty(&ctx, &mut sess1, "SearchBounties", &[PyRep::Integer(char2)]).await.unwrap();
    assert_eq!(search_pilot.as_list().map(|l| l.len()), Some(1));

    // Kill rights sell/cancel + GM bounty clear
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.insert(
            7001,
            eve_db::KillRightRecord {
                kill_right_id: 7001,
                owner_id: char2,
                target_id: char1,
                activation_fee: IskCents::from_isk(0),
                is_active_suspect: false,
            },
        );
    });
    bounty(&ctx, &mut sess2, "SellKillRight", &[PyRep::Integer(7001), PyRep::Real(50_000.0)]).await.unwrap();
    bounty(&ctx, &mut sess2, "CancelSellKillRight", &[PyRep::Integer(7001)]).await.unwrap();
    sess1.role |= ROLE_GML;
    let cleared = bounty(&ctx, &mut sess1, "ClearBounty", &[PyRep::Integer(char2)]).await.unwrap();
    assert_eq!(cleared.dict_get("action"), Some(&PyRep::String("clear".into())));

    // NPC Bounty Tick & Corp Tax threshold (< 100,000 ISK tax-free vs == 100,000 ISK taxed at 20%)
    let sys_id = 30_000_142_u32;
    let char1_before = ctx.db.with_snapshot(|s| s.characters[&char1].balance);
    let corp1_before = ctx.db.with_snapshot(|s| s.corporations[&corp1].master_wallet_balance());

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char1), 99_999.0);
    });
    let pilot_sub = flush_npc_bounty_tick(&ctx, sys_id, char1, 0.0).pilot_isk;
    assert!((pilot_sub - 99_999.0).abs() < 1e-6, "< 100k bounty tick must be untaxed");
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp1].master_wallet_balance()), corp1_before);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char1), 100_000.0);
    });
    let pilot_taxed = flush_npc_bounty_tick(&ctx, sys_id, char1, 0.0).pilot_isk;
    assert!((pilot_taxed - 80_000.0).abs() < 1e-6, "== 100k bounty tick taxed at 20%");
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp1].master_wallet_balance().0), corp1_before.0 + IskCents::from_isk(20_000).0);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char1].balance.0), char1_before.0 + IskCents::from_isk(99_999 + 80_000).0);

    // Lowsec CONCORD Security Office Clone Soldier Tag turn-in fee & sec status delta
    if let Some((&ls_st_id, _)) = ctx.sde.stations.iter().find(|(&sid, st)| {
        ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|sys| sys.security_status > 0.0 && sys.security_status < 0.45)
            && crate::services::station_structure::npc_station_offers_service(&ctx.sde, sid, 27)
    }) {
        let mut ls_sess = auth_session(2, char2, corp2, ctx.sde.stations[&ls_st_id].solar_system_id);
        ls_sess.station_id = Some(ls_st_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char2) {
                c.station_id = Some(ls_st_id);
                c.security_status = -1.2;
                c.balance = IskCents::from_isk(20_000_000);
            }
            snap.items.insert(
                90_000_911,
                InventoryItemRecord { item_id: 90_000_911, type_id: 33141, owner_id: char2, location_id: ls_st_id, flag: InventoryFlag::Hangar, quantity: 2, singleton: false, version: 1, ..Default::default() },
            );
        });
        let tags_dict = PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))]);
        let sec_res = handle_apply_tags_for_sec_status(&ctx, &ls_sess, &[PyRep::Real(-0.5), tags_dict]).unwrap().unwrap();
        assert!((sec_res.dict_get("newSecurityStatus").and_then(|v| v.as_f64()).unwrap() - (-0.7)).abs() < 1e-6);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char2].balance), IskCents::from_isk(10_000_000));
    }
}
