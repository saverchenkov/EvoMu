//! Wallet ledger integration tests: corporation wallet transfers, the wallet journal with parties and
//! context, market transactions recorded at fill time, the account-wide PLEX Vault and deposits, NPC
//! seed orders, and persistence of the ledger tables across a SQLite restart.



use crate::common;
use eve_db::ledger::{ref_types, JournalContext, JournalEntryInput, JournalMeta};
use eve_db::{corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA: i64 = 60_003_760;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const PLEX: TypeId = 44_992;
const TRITANIUM: TypeId = 34;

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, cid: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("select");
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, m: &str, a: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, a, &HashMap::new(), None).await
}

async fn call_kw(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    m: &str,
    a: &[PyRep],
    kw: &[(&str, PyRep)],
) -> Result<PyRep, BridgeRpcError> {
    let kw: HashMap<String, PyRep> = kw.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    ctx.dispatch_rpc(s, svc, None, m, a, &kw, None).await
}

/// Rowset -> Vec of column-name maps.
fn rows(rep: &PyRep) -> Vec<HashMap<String, PyRep>> {
    let PyRep::Rowset { columns, rows } = rep else { panic!("expected Rowset, got {rep:?}") };
    rows.iter().map(|r| columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
}

fn int(r: &HashMap<String, PyRep>, k: &str) -> i64 {
    r.get(k).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("{k} in {r:?}"))
}

fn real(r: &HashMap<String, PyRep>, k: &str) -> f64 {
    r.get(k).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("{k} in {r:?}"))
}

fn year_month_now(ctx: &ServerContext) -> (i64, i64) {
    let days = (ctx.now_unix() / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m)
}

async fn journal(ctx: &ServerContext, s: &mut SessionState, key: i64, corp: bool) -> Vec<HashMap<String, PyRep>> {
    let (y, m) = year_month_now(ctx);
    let rep = call(
        ctx,
        s,
        "account",
        "GetTransactions",
        &[PyRep::Integer(key), PyRep::Integer(y), PyRep::Integer(m), PyRep::Bool(corp)],
    )
    .await
    .expect("account.GetTransactions");
    rows(&rep)
}

fn setup_corp(ctx: &ServerContext) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        let cid = 98_123_456;
        let mut corp = CorporationRecord {
            corporation_id: cid,
            name: "Ledger Test Corp".into(),
            ticker: "LDGR".into(),
            ceo_id: P1,
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in corp.wallet_divisions.iter_mut() {
            d.balance = IskCents::from_isk(1_000_000);
        }
        corp.master_wallet = IskCents::from_isk(1_000_000);
        corp.members.insert(P1);
        corp.members.insert(P2);
        snap.corporations.insert(cid, corp);
        for c in [P1, P2] {
            snap.characters.get_mut(&c).unwrap().corporation_id = cid;
        }
        cid
    })
}

fn set_role(ctx: &ServerContext, corp: i64, cid: i64, role: u64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp).unwrap().member_roles.insert(cid, role);
    });
}

fn div_balance(ctx: &ServerContext, corp: i64, div: u8) -> IskCents {
    ctx.db.with_snapshot(|s| s.corporations[&corp].wallet_divisions.iter().find(|d| d.division_id == div).unwrap().balance)
}

#[tokio::test]
async fn test_give_cash_from_corp_account_roles_and_journal() {
    // parity: feat:wallet.give-cash-corp, rpc:account.GiveCashFromCorpAccount
    eve_testkit::attest(&["feat:wallet.give-cash-corp", "rpc:account.GiveCashFromCorpAccount"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = pilot(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = pilot(&ctx, 802, "other", "other-secret", P2).await;
    let corp = setup_corp(&ctx);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&P1).unwrap().balance = IskCents::from_isk(10));

    // No role: denied for both withdrawal and corp journal / corp market transactions.
    set_role(&ctx, corp, P2, 0);
    let denied = call(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P1), PyRep::Real(100.0), PyRep::Integer(1002)])
        .await
        .unwrap_err();
    assert_eq!(denied.code, "CrpAccessDenied");
    let (y, m) = year_month_now(&ctx);
    let j_denied = call(&ctx, &mut s2, "account", "GetTransactions", &[PyRep::Integer(1002), PyRep::Integer(y), PyRep::Integer(m), PyRep::Bool(true)])
        .await
        .unwrap_err();
    assert_eq!(j_denied.code, "CrpAccessDenied");
    let tx_denied = call(&ctx, &mut s2, "marketProxy", "CorpGetTransactions", &[PyRep::None, PyRep::Integer(1000)])
        .await
        .unwrap_err();
    assert_eq!(tx_denied.code, "CrpAccessDenied");
    // Junior Accountant may view the corp journal but not withdraw.
    set_role(&ctx, corp, P2, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
    let junior = call(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P1), PyRep::Real(100.0), PyRep::Integer(1002)])
        .await
        .unwrap_err();
    assert_eq!(junior.code, "CrpAccessDenied");
    assert!(journal(&ctx, &mut s2, 1002, true).await.is_empty(), "junior accountant can read the (empty) division journal");

    // Account Take 3 (division 3 = accountKey 1002) allows withdrawing from division 3 only.
    set_role(&ctx, corp, P2, corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]);
    call_kw(
        &ctx,
        &mut s2,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(P1), PyRep::Real(2_500.0), PyRep::Integer(1002)],
        &[("reason", PyRep::String("ship replacement".into()))],
    )
    .await
    .expect("withdraw from division 3 with Account Take 3");
    assert_eq!(div_balance(&ctx, corp, 3), IskCents::from_isk(997_500));
    assert_eq!(ctx.db.get_character(P1).unwrap().balance, IskCents::from_isk(2_510));
    let other_div = call(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P1), PyRep::Real(1.0), PyRep::Integer(1001)])
        .await
        .unwrap_err();
    assert_eq!(other_div.code, "CrpAccessDenied", "Account Take 3 does not cover division 2");
    let broke = call(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P1), PyRep::Real(5_000_000.0), PyRep::Integer(1002)])
        .await
        .unwrap_err();
    assert_eq!(broke.code, "NotEnoughMoney");

    // Accountant alone cannot withdraw from a corp division; Account Take 1 (or Director) is required.
    set_role(&ctx, corp, P2, corp_roles::ROLE_ACCOUNTANT);
    let acct_denied = call_kw(
        &ctx,
        &mut s2,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(corp), PyRep::Real(40_000.0), PyRep::Integer(1000)],
        &[("toAccountKey", PyRep::Integer(1006))],
    )
    .await
    .unwrap_err();
    assert_eq!(acct_denied.code, "CrpAccessDenied", "Accountant alone cannot withdraw from division 1");

    // Division 1 take role allows division-to-division transfer inside the corp via toAccountKey.
    set_role(&ctx, corp, P2, corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
    call_kw(
        &ctx,
        &mut s2,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(corp), PyRep::Real(40_000.0), PyRep::Integer(1000)],
        &[("toAccountKey", PyRep::Integer(1006))],
    )
    .await
    .expect("division 1 -> division 7");
    assert_eq!(div_balance(&ctx, corp, 1), IskCents::from_isk(960_000));
    assert_eq!(div_balance(&ctx, corp, 7), IskCents::from_isk(1_040_000));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp].master_wallet), IskCents::from_isk(960_000));

    // Journal: corp side (division 3) and recipient side, with parties, ref type and reason.
    let corp_rows = journal(&ctx, &mut s2, 1002, true).await;
    assert_eq!(corp_rows.len(), 1);
    let w = &corp_rows[0];
    assert_eq!(int(w, "entryTypeID"), ref_types::CORPORATION_ACCOUNT_WITHDRAWAL as i64);
    assert_eq!((int(w, "ownerID1"), int(w, "ownerID2")), (corp, P1));
    assert_eq!(real(w, "amount"), -2_500.0);
    assert_eq!(real(w, "balance"), 997_500.0);
    assert_eq!(int(w, "referenceID"), P2, "context: the character who made the transfer");
    assert_eq!(w.get("description"), Some(&PyRep::String("ship replacement".into())));
    let p1_rows = journal(&ctx, &mut s1, 1000, false).await;
    let credit = p1_rows.iter().find(|r| int(r, "entryTypeID") == 37).expect("recipient journal");
    assert_eq!((real(credit, "amount"), real(credit, "balance")), (2_500.0, 2_510.0));
    assert_eq!((int(credit, "ownerID1"), int(credit, "ownerID2")), (corp, P1));
    let internal: Vec<_> = journal(&ctx, &mut s2, 1006, true).await;
    assert_eq!(internal.len(), 1);
    assert_eq!((real(&internal[0], "amount"), int(&internal[0], "ownerID2")), (40_000.0, corp));
}

#[tokio::test]
async fn test_journal_carries_ref_type_amounts_balance_parties_reason_and_context() {
    // parity: feat:wallet.journal, feat:wallet.transactions, rpc:account.GetTransactions
    eve_testkit::attest(&["feat:wallet.journal", "feat:wallet.transactions", "rpc:account.GetTransactions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = pilot(&ctx, 811, "demo", "demo-secret", P1).await;
    let mut s2 = pilot(&ctx, 812, "other", "other-secret", P2).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&P1).unwrap().balance = IskCents::from_isk(100_000);
        s.characters.get_mut(&P2).unwrap().balance = IskCents::from_isk(100_000);
        s.skills.entry(P2).or_default().remove(&16_597);
        s.market_orders.retain(|_, o| o.type_id != TRITANIUM);
    });
    // Player donation.
    call(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(1_000.0), PyRep::String("rent".into())])
        .await
        .expect("GiveCash");
    // P1 lists 10 Tritanium @ 50; P2 buys them (fill at 50).
    let sell = call(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA)),
                ("typeID", PyRep::Integer(TRITANIUM as i64)),
                ("price", PyRep::Real(50.0)),
                ("quantity", PyRep::Integer(10)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("sell");
    let PyRep::KeyVal(sell_kv) = &sell else { panic!() };
    let sell_oid = sell_kv["orderID"].as_i64().unwrap();
    call(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA),
            PyRep::Integer(TRITANIUM as i64),
            PyRep::Real(50.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("instant buy");
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&sell_oid].vol_remaining), 0);

    let p1 = journal(&ctx, &mut s1, 1000, false).await;
    let p2 = journal(&ctx, &mut s2, 1000, false).await;
    let by = |rs: &[HashMap<String, PyRep>], t: i32| rs.iter().filter(|r| int(r, "entryTypeID") == t as i64).cloned().collect::<Vec<_>>();
    // Donation: both sides, parties P1 -> P2, reason preserved, signed amounts and running balances.
    let d1 = &by(&p1, ref_types::PLAYER_DONATION)[0];
    let d2 = &by(&p2, ref_types::PLAYER_DONATION)[0];
    assert_eq!((int(d1, "ownerID1"), int(d1, "ownerID2")), (P1, P2));
    assert_eq!((int(d2, "ownerID1"), int(d2, "ownerID2")), (P1, P2));
    assert_eq!((real(d1, "amount"), real(d2, "amount")), (-1_000.0, 1_000.0));
    assert_eq!((real(d1, "balance"), real(d2, "balance")), (99_000.0, 101_000.0));
    assert!(matches!(d1.get("description"), Some(PyRep::String(s)) if s.contains("rent")));
    // Broker fee for listing: debit, ref 46, P1 -> station owner corp.
    let jita_owner = ctx.sde.stations[&JITA].owner_corporation_id;
    let fee = &by(&p1, ref_types::BROKER_FEE)[0];
    assert!(real(fee, "amount") < 0.0);
    assert_eq!((int(fee, "ownerID1"), int(fee, "ownerID2")), (P1, jita_owner));
    // Market transaction: buyer debit and seller credit share the transaction as context (referenceID).
    let buy = &by(&p2, ref_types::MARKET_TRANSACTION)[0];
    let sold = &by(&p1, ref_types::MARKET_TRANSACTION)[0];
    assert_eq!(real(buy, "amount"), -500.0);
    assert_eq!((int(buy, "ownerID1"), int(buy, "ownerID2")), (P2, P1));
    assert_eq!(int(buy, "referenceID"), int(sold, "referenceID"));
    let tax = &by(&p1, ref_types::TRANSACTION_TAX)[0];
    let scc = ctx.sde.npc_corporations.values().find(|c| c.name == "Secure Commerce Commission").unwrap().corporation_id;
    assert_eq!((int(tax, "ownerID1"), int(tax, "ownerID2")), (P1, scc));
    assert_eq!(int(tax, "referenceID"), int(buy, "referenceID"));
    assert!((real(sold, "amount") + real(tax, "amount") - 500.0).abs() < 0.01 + -real(tax, "amount") * 2.0);
    // Every row has a timestamp in the requested month and the entry's balance.
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    for r in p1.iter().chain(p2.iter()) {
        assert!(int(r, "transactionDate") <= now_ft && int(r, "transactionDate") > now_ft - 3_600 * 10_000_000);
    }
    assert_eq!(real(&p2[0], "balance"), ctx.db.get_character(P2).unwrap().balance.to_wire_f64(), "newest first");
    // Other month -> empty.
    let other = call(&ctx, &mut s1, "account", "GetTransactions", &[PyRep::Integer(1000), PyRep::Integer(2001), PyRep::Integer(1), PyRep::Bool(false)])
        .await
        .unwrap();
    assert!(rows(&other).is_empty());

    // Market transactions (marketProxy.CharGetTransactions): client, type, quantity, price, location.
    let txs_p2 = rows(&call(&ctx, &mut s2, "marketProxy", "CharGetTransactions", &[PyRep::None]).await.expect("CharGetTransactions"));
    let txs_p1 = rows(&call(&ctx, &mut s1, "marketProxy", "CharGetTransactions", &[PyRep::None]).await.unwrap());
    assert_eq!(txs_p2.len(), 1);
    let t = &txs_p2[0];
    assert_eq!((int(t, "buyerID"), int(t, "sellerID")), (P2, P1));
    assert_eq!((int(t, "typeID"), int(t, "quantity"), int(t, "stationID")), (TRITANIUM as i64, 10, JITA));
    assert_eq!(real(t, "price"), 50.0);
    assert_eq!(int(t, "accountID"), int(t, "buyerAccountID"), "personal, not corp, transaction");
    assert_eq!(int(t, "transactionID"), int(&txs_p1[0], "transactionID"));
    assert_eq!(int(t, "transactionID"), int(buy, "referenceID"));
    assert_eq!(int(t, "journalRefID"), int(buy, "transactionID"));
    let later = (ctx.now_unix() as i64 + 11_644_473_600 + 60) * 10_000_000;
    assert!(rows(&call(&ctx, &mut s2, "marketProxy", "CharGetTransactions", &[PyRep::Integer(later)]).await.unwrap()).is_empty());
}

#[tokio::test]
async fn test_plex_vault_is_account_wide_and_deposit_moves_plex_items() {
    // parity: feat:wallet.plex-vault, rpc:vaultManager.get_account_balance, rpc:invbroker.DepositPlexToVault
    eve_testkit::attest(&["feat:wallet.plex-vault", "rpc:vaultManager.get_account_balance", "rpc:invbroker.DepositPlexToVault"]);
    let ctx = ServerContext::new();
    let mut s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    // A second character on the same account sees the same vault.
    let (alt, uid) = ctx.db.with_mut_snapshot(|s| {
        let mut c = s.characters[&P1].clone();
        let uid = c.user_id;
        c.character_id = 90_000_777;
        c.name = "Vault Alt".into();
        s.characters.insert(c.character_id, c);
        let cur = s.plex_vault_balance(P1);
        s.adjust_plex_vault(P1, 250 - cur).unwrap();
        (90_000_777_i64, uid)
    });
    let b1 = call(&ctx, &mut s1, "vaultManager", "get_account_balance", &[PyRep::String("PLX".into())]).await.unwrap();
    assert_eq!(b1, PyRep::Integer(250));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(alt)), 250, "alt shares the account vault");
    assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&uid).copied()), Some(250));

    // Deposit 30 of a 50-PLEX hangar stack.
    let item_id = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: PLEX,
                owner_id: P1,
                location_id: JITA,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });
    let invoice = PyRep::Integer(1);
    let wrong_loc = call(&ctx, &mut s1, "invbroker", "DepositPlexToVault", &[PyRep::Integer(60_008_494), PyRep::Integer(item_id), PyRep::Integer(30), invoice.clone()])
        .await
        .unwrap_err();
    assert_eq!(wrong_loc.code, "NotDockedAtLocation");
    let too_many = call(&ctx, &mut s1, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA), PyRep::Integer(item_id), PyRep::Integer(51), invoice.clone()])
        .await
        .unwrap_err();
    assert_eq!(too_many.code, "NotEnoughQuantity");
    let reference = call(&ctx, &mut s1, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA), PyRep::Integer(item_id), PyRep::Integer(30), invoice.clone()])
        .await
        .expect("DepositPlexToVault");
    assert!(reference.as_i64().is_some());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&item_id].quantity), 20);
    assert_eq!(
        call(&ctx, &mut s1, "vaultManager", "get_account_balance", &[PyRep::String("PLX".into())]).await.unwrap(),
        PyRep::Integer(280)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(alt)), 280);
    call(&ctx, &mut s1, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA), PyRep::Integer(item_id), PyRep::Integer(20), invoice.clone()])
        .await
        .expect("deposit rest");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&item_id)), "emptied stack removed");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(P1)), 300);
    // Non-PLEX items are rejected.
    let trit = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.owner_id == P1 && i.type_id == TRITANIUM && i.location_id == JITA).map(|i| i.item_id).unwrap()
    });
    let not_plex = call(&ctx, &mut s1, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA), PyRep::Integer(trit), PyRep::Integer(1), invoice])
        .await
        .unwrap_err();
    assert_eq!(not_plex.code, "NotPlex");
}

#[tokio::test]
async fn test_npc_seed_orders_skillbooks_blueprints_faction_items() {
    // parity: feat:market.npc-seed-orders
    eve_testkit::attest(&["feat:market.npc-seed-orders"]);
    let ctx = ServerContext::new();
    let mut s1 = pilot(&ctx, 831, "demo", "demo-secret", P1).await;
    let jita_faction = ctx.sde.stations[&JITA].owner_faction_id;
    // Every published, market-listed skillbook has an NPC sell order in Jita.
    let books: Vec<TypeId> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 16 && t.published && t.market_group_id.is_some() && t.base_price > 0.0)
        .map(|t| t.type_id)
        .collect();
    assert!(books.len() > 100);
    let (npc_books, npc_bps, faction_items) = ctx.db.with_snapshot(|s| {
        let npc: Vec<_> = s.market_orders.values().filter(|o| o.is_npc && !o.is_bid && o.station_id == JITA).collect();
        let cat = |o: &&eve_db::MarketOrderRecord| ctx.sde.types.get(&o.type_id).map(|t| t.category_id);
        let books: std::collections::HashSet<TypeId> = npc.iter().filter(|o| cat(o) == Some(16)).map(|o| o.type_id).collect();
        let bps = npc.iter().filter(|o| cat(o) == Some(9)).count();
        let fac: Vec<(i64, TypeId, i32, IskCents)> = npc
            .iter()
            .filter(|o| {
                ctx.sde
                    .types
                    .get(&o.type_id)
                    .is_some_and(|t| t.category_id != 16 && t.category_id != 9 && t.meta_group_id == Some(4))
            })
            .map(|o| (o.order_id, o.type_id, o.vol_remaining, o.price))
            .collect();
        (books, bps, fac)
    });
    assert!(books.iter().all(|b| npc_books.contains(b)), "all skillbooks seeded");
    assert!(npc_bps > 0, "NPC blueprint orders exist");
    assert!(!faction_items.is_empty(), "faction items seeded in Jita");
    for (_, tid, _, _) in &faction_items {
        assert_eq!(ctx.sde.types[tid].faction_id.map(i64::from), jita_faction, "Jita sells its own faction's items only");
    }
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&P1).unwrap().balance = IskCents::from_isk(10_000_000_000));
    let buy = |tid: TypeId, price: f64, qty: i64| {
        vec![
            PyRep::Integer(JITA),
            PyRep::Integer(tid as i64),
            PyRep::Real(price),
            PyRep::Integer(qty),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ]
    };
    // Skillbook stock never depletes.
    let (book_oid, book_tid, book_price) = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| o.is_npc && o.station_id == JITA && ctx.sde.types.get(&o.type_id).map(|t| t.category_id) == Some(16))
            .min_by_key(|o| (o.price.0, o.order_id))
            .map(|o| (o.order_id, o.type_id, o.price.to_wire_f64()))
            .unwrap()
    });
    call(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &buy(book_tid, book_price, 3)).await.expect("buy books");
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&book_oid].vol_remaining), 1_000);
    // Faction stock depletes by exactly the quantity bought.
    let &(f_oid, f_tid, f_vol, f_price) = faction_items.iter().min_by_key(|f| (f.3, f.0)).unwrap();
    call(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &buy(f_tid, f_price.to_wire_f64(), 3)).await.expect("buy faction item");
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&f_oid].vol_remaining), f_vol - 3);
    let delivered = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == P1 && i.type_id == f_tid && i.location_id == JITA).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(delivered, 3);
}

#[tokio::test]
async fn test_ledger_tables_persist_across_sqlite_restart_and_migrate_legacy_plex() {
    // parity: feat:wallet.plex-vault, feat:wallet.journal, feat:serverops.persistence
    eve_testkit::attest(&["feat:wallet.plex-vault", "feat:wallet.journal", "feat:serverops.persistence"]);
    let path = std::env::temp_dir().join(format!("evomu_ledger_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    {
        let repo = eve_db::WorldRepository::open_sqlite(&path).expect("open");
        repo.with_mut_snapshot(|s| {
            *s = demo.clone();
            // Legacy per-character PLEX (pre-vault schema) must fold into the account vault on load.
            s.account_plex_vault.clear();
            s.characters.get_mut(&P2).unwrap().plex_balance = 77;
            s.append_journal_entry(JournalEntryInput {
                owner_id: P1,
                entry_type: "PlayerDonation",
                amount: IskCents::from_isk(-5),
                balance_after: IskCents::from_isk(95),
                reason: "persisted".into(),
                meta: JournalMeta::new(ref_types::PLAYER_DONATION, P1, P2, 1_700_000_000)
                    .with_context(JournalContext::Character, P1),
            });
            s.record_market_transaction(1_700_000_001, TRITANIUM, 5, IskCents::from_isk(7), JITA, P2, P1, 0);
        });
    }
    let (p2_vault, meta_ok, tx_ok, legacy_zeroed) = {
        let repo = eve_db::WorldRepository::open_sqlite(&path).expect("reopen");
        let out = repo.with_snapshot(|s| {
            let entry = s.wallet_journal.iter().rev().find(|j| j.reason == "persisted").expect("journal row");
            let m = &s.wallet_journal_meta[&entry.entry_id];
            (
                s.plex_vault_balance(P2),
                m.first_party_id == P1 && m.second_party_id == P2 && m.context_id == P1 && m.date_unix == 1_700_000_000,
                s.market_transactions.iter().any(|t| t.buyer_id == P2 && t.quantity == 5 && t.price == IskCents::from_isk(7)),
                s.characters[&P2].plex_balance == 0,
            )
        });
        // A write after the migration persists the folded vault; no double counting on next load.
        repo.with_mut_snapshot(|s| {
            s.adjust_plex_vault(P2, 3).unwrap();
        });
        out
    };
    assert_eq!((p2_vault, meta_ok, tx_ok, legacy_zeroed), (77, true, true, true));
    let repo = eve_db::WorldRepository::open_sqlite(&path).expect("reopen 2");
    assert_eq!(repo.with_snapshot(|s| s.plex_vault_balance(P2)), 80);
    drop(repo);
    let _ = std::fs::remove_file(&path);
}


pub(crate) mod wallet_bills_trade_raffle {
//! Bills, Aurum removal, direct station trade window, HyperNet Relay, POS removal, and bill/contract
//! division isolation integration tests (part 2 of `wallet_serverops_complete`).

use crate::common;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::wallet_serverops::{
    compute_required_hypercores, create_bill, process_due_automatic_bills,
    run_daily_downtime_and_jobs, CorpBillRecord, HYPERCORE_TYPE_ID,
};
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const TRITANIUM: TypeId = 34;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn balance(ctx: &ServerContext, char_id: i64) -> IskCents {
    ctx.db.get_character(char_id).unwrap().balance
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    let PyRep::KeyVal(kv) = rep else { panic!("expected KeyVal, got {rep:?}") };
    kv.get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key}"))
}

#[tokio::test]
async fn test_corporation_and_personal_bills_and_autopay() {
    // parity: feat:wallet.bills
    eve_testkit::attest(&["feat:wallet.bills"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 881, "demo", "demo-secret", P1).await;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    let landlord_corp = 1_000_035_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.master_wallet = IskCents::from_isk(10_000_000);
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(1_000_000);
        }
    });

    let p_bill = create_bill(&ctx, 1, P1, landlord_corp, 2_500.0, now + 3600, 0.0, 0, 0);
    let c_bill = create_bill(
        &ctx,
        2,
        corp_id,
        landlord_corp,
        10_000.0,
        now + 3600,
        0.0,
        JITA_STATION_ID,
        0,
    );

    let bills_rep = rpc(&ctx, &mut s1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
    let PyRep::Rowset { rows, .. } = bills_rep else { panic!("expected Rowset"); };
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill))));
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill))));

    let p1_before = balance(&ctx, P1);
    let pay_p = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[PyRep::Integer(p_bill)]).await.expect("CharPayBill");
    assert_eq!(pay_p, PyRep::Bool(true));
    assert_eq!(p1_before.0 - balance(&ctx, P1).0, 250_000);

    let div3_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    let pay_c = rpc(&ctx, &mut s1, "billMgr", "PayCorporationBill", &[PyRep::Integer(c_bill), PyRep::Integer(1002)]).await.expect("PayCorporationBill");
    assert_eq!(pay_c, PyRep::Bool(true));
    let div3_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    assert_eq!(div3_before.0 - div3_after.0, 1_000_000);

    let auto_dict = PyRep::Dict(vec![
        (PyRep::Integer(2), PyRep::Bool(true)),
        (PyRep::String("divisionID".into()), PyRep::Integer(1004)),
    ]);
    let set_ap = rpc(&ctx, &mut s1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(corp_id), auto_dict)])]).await.expect("SendAutomaticPaySettings");
    assert_eq!(set_ap, PyRep::Bool(true));

    let get_ap = rpc(&ctx, &mut s1, "billMgr", "GetAutomaticPaySettings", &[]).await.expect("GetAutomaticPaySettings");
    let PyRep::Dict(ap_entries) = get_ap else { panic!("expected Dict"); };
    assert!(!ap_entries.is_empty());

    let due_bill = create_bill(&ctx, 2, corp_id, landlord_corp, 5_000.0, now - 10, 0.0, 0, 0);
    let paid_auto = process_due_automatic_bills(&ctx);
    assert!(paid_auto >= 1);
    let is_due_paid = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| serde_json::from_str::<CorpBillRecord>(&n.note).ok())
            .find(|b| b.bill_id == due_bill)
            .map(|b| b.paid)
    });
    assert_eq!(is_due_paid, Some(true));
}

#[tokio::test]
async fn test_new_eden_store_and_aurum_removal() {
    // parity: feat:removed.aurum-currency
    eve_testkit::attest(&["feat:removed.aurum-currency"]);
    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut s1 = setup_pilot(&ctx, 882, "demo", "demo-secret", P1).await;

    let cats = rpc(&ctx, &mut s1, "storeManager", "get_categories", &[]).await.expect("get_categories");
    let PyRep::List(cat_list) = cats else { panic!("expected List"); };
    assert!(!cat_list.is_empty());

    let offers = rpc(&ctx, &mut s1, "storeManager", "get_offers", &[]).await.expect("get_offers");
    let PyRep::List(offer_list) = offers else { panic!("expected List"); };
    assert!(!offer_list.is_empty());

    let can_p = rpc(&ctx, &mut s1, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)]).await.expect("can_purchase_offer");
    assert_eq!(can_p, PyRep::Bool(true));

    let aur_err = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    ctx.db.with_mut_snapshot(|s| {
        s.adjust_plex_vault(P1, 500).unwrap();
    });
    let vault_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    let buy_res = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.expect("buy_offer");
    let PyRep::KeyVal(buy_kv) = buy_res else { panic!("expected KeyVal"); };
    assert_eq!(buy_kv.get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(buy_kv.get("offerID"), Some(&PyRep::Integer(1002)));
    let vault_after = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    assert_eq!(vault_before - vault_after, 110, "Offer 1002 costs 110 PLEX");

    let has_skin = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(has_skin, "Purchased SKIN delivered to station hangar");
}

#[tokio::test]
async fn test_station_direct_trade_window() {
    // parity: feat:wallet.trade-window
    eve_testkit::attest(&["feat:wallet.trade-window"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 883, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 884, "other", "other-secret", P2).await;

    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    let p1_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });
    let p1_singleton = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: 587,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before = balance(&ctx, P1);
    let p2_bal_before = balance(&ctx, P2);

    let init_rep = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade");
    let trade_id = kv_i64(&init_rep, "tradeContainerID");

    let overflow_qty = (1i64 << 32) - 1000;
    let err_overflow = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(overflow_qty)]).await.unwrap_err();
    assert_eq!(err_overflow.code, "InvalidQuantity");

    let err_negative = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(-100)]).await.unwrap_err();
    assert_eq!(err_negative.code, "InvalidQuantity");

    let err_singleton = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_singleton), PyRep::Integer(2)]).await.unwrap_err();
    assert_eq!(err_singleton.code, "CannotSplitSingleton");

    let off_item1 = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(60)]).await.expect("AddItem 60");
    assert_eq!(kv_i64(&off_item1, "tradeContainerID"), trade_id);

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(30)]).await.expect("AddItem 30");

    let err_exceed = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(20)]).await.unwrap_err();
    assert_eq!(err_exceed.code, "InsufficientQuantity");

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(10)]).await.expect("AddItem 10");

    let off_money = rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(50_000.0)]).await.expect("OfferMoney");
    assert_eq!(kv_i64(&off_money, "tradeContainerID"), trade_id);

    let acc1 = rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s1");
    let PyRep::KeyVal(kv1) = acc1 else { panic!("expected KeyVal"); };
    assert_eq!(kv1.get("completed"), Some(&PyRep::Bool(false)));

    let acc2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s2");
    let PyRep::KeyVal(kv2) = acc2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2.get("completed"), Some(&PyRep::Bool(true)));

    let p2_has_trit = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_item).map(|i| i.owner_id == P2 && i.quantity == 100 && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p2_has_trit, "Item ownership transferred to P2 in hangar");
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before.0, IskCents::from_isk(50_000).0);
    assert_eq!(p2_bal_before.0 - balance(&ctx, P2).0, IskCents::from_isk(50_000).0);

    let p1_partial_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before_partial = balance(&ctx, P1);
    let p2_bal_before_partial = balance(&ctx, P2);
    let total_isk_before = p1_bal_before_partial.0 + p2_bal_before_partial.0;

    let init_rep2 = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade 2");
    let trade_id2 = kv_i64(&init_rep2, "tradeContainerID");

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id2), PyRep::Integer(p1_partial_item), PyRep::Integer(30)]).await.expect("AddItem partial");
    rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id2), PyRep::Real(15_000.0)]).await.expect("OfferMoney partial");
    rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s1");
    let acc2_2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s2");
    let PyRep::KeyVal(kv2_2) = acc2_2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2_2.get("completed"), Some(&PyRep::Bool(true)));

    let p1_retains_70 = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_partial_item).map(|i| i.owner_id == P1 && i.quantity == 70 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p1_retains_70, "Seller A retains exactly 70 units of the item in hangar");

    let p2_received_30 = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P2 && i.type_id == TRITANIUM && i.quantity == 30 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(p2_received_30, "Buyer B receives 30 units of the item in hangar");

    let total_isk_after = balance(&ctx, P1).0 + balance(&ctx, P2).0;
    assert_eq!(total_isk_before, total_isk_after, "Total ISK is strictly conserved");
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before_partial.0, IskCents::from_isk(15_000).0);
    assert_eq!(p2_bal_before_partial.0 - balance(&ctx, P2).0, IskCents::from_isk(15_000).0);

    let total_item_units: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| (i.item_id == p1_partial_item || (i.owner_id == P2 && i.quantity == 30)) && i.type_id == TRITANIUM).map(|i| i.quantity).sum()
    });
    assert_eq!(total_item_units, 100, "Item units strictly conserved (70 retained + 30 transferred = 100)");
}

#[tokio::test]
async fn test_hypernet_relay_and_hypercore_scaling_and_expiry() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 885, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 886, "other", "other-secret", P2).await;
    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    assert!(compute_required_hypercores(10_000_000.0, 10).is_err());
    let cores_8 = compute_required_hypercores(50_000_000.0, 8).unwrap();
    let cores_512 = compute_required_hypercores(50_000_000.0, 512).unwrap();
    assert!(cores_512 >= cores_8, "More nodes require more HyperCores");

    let (raffle_item, _) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let it_id = s.next_entity_id;
        s.items.insert(it_id, InventoryItemRecord {
            item_id: it_id,
            type_id: 587,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.next_entity_id += 1;
        let hc_id = s.next_entity_id;
        s.items.insert(hc_id, InventoryItemRecord {
            item_id: hc_id,
            type_id: HYPERCORE_TYPE_ID,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        (it_id, hc_id)
    });

    let c_raf = rpc(&ctx, &mut s1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(raffle_item)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(16)),
        (PyRep::String("ticketPrice".into()), PyRep::Real(10_000.0)),
    ])]).await.expect("CreateRaffle");
    let raffle_id = kv_i64(&c_raf, "raffle_id");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), -52_568);

    let p2_before = balance(&ctx, P2);
    let buy_t = rpc(&ctx, &mut s2, "raffleProxy", "BuyTicket", &[PyRep::Integer(raffle_id), PyRep::Integer(0)]).await.expect("BuyTicket");
    assert_eq!(kv_i64(&buy_t, "raffle_id"), raffle_id);
    assert_eq!(kv_i64(&buy_t, "tickets_remaining"), 15);
    assert_eq!(p2_before.0 - balance(&ctx, P2).0, IskCents::from_isk(10_000).0);

    ctx.advance_virtual_clock(72 * 3_600 + 1);
    let raf_info = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.expect("GetRaffle sweeps expired");
    let PyRep::KeyVal(r_kv) = raf_info else { panic!("expected KeyVal"); };
    assert_eq!(r_kv.get("is_expired"), Some(&PyRep::Bool(true)));

    assert_eq!(balance(&ctx, P2), p2_before, "Buyer refunded upon raffle expiry");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].flag), InventoryFlag::Hangar);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), P1);
}

#[tokio::test]
async fn test_pos_towers_and_starbases_removed() {
    // parity: feat:removed.pos-towers
    eve_testkit::attest(&["feat:removed.pos-towers"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 887, "demo", "demo-secret", P1).await;

    let towers = rpc(&ctx, &mut s1, "posMgr", "GetControlTowers", &[]).await.expect("GetControlTowers");
    assert_eq!(towers, PyRep::List(Vec::new()));

    let moon = rpc(&ctx, &mut s1, "posMgr", "GetMoonForTower", &[PyRep::Integer(1)]).await.expect("GetMoonForTower");
    assert_eq!(moon, PyRep::None);

    let anchor_err = rpc(&ctx, &mut s1, "posMgr", "AnchorOrbital", &[PyRep::Integer(1), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(anchor_err.code, "StarbaseRemoved");
}

#[tokio::test]
async fn test_downtime_contract_expiry_settlement_and_corp_bill_division_isolation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 888, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 889, "other", "other-secret", P2).await;
    let now = ctx.now_unix();

    let exch_item_id = 995_001_i64;
    let auc_item_id = 995_002_i64;
    let exch_cid = 88_001_i64;
    let auc_cid = 88_002_i64;
    let p1_init = balance(&ctx, P1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            exch_item_id,
            InventoryItemRecord {
                item_id: exch_item_id,
                type_id: 34,
                owner_id: P1,
                location_id: exch_cid,
                flag: InventoryFlag::Locked,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            exch_cid,
            eve_db::ContractRecord {
                contract_id: exch_cid,
                issuer_id: P1,
                issuer_corp_id: 98_000_001,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                reward: IskCents::from_isk(25_000),
                expiration_unix: now - 10,
                item_ids: vec![exch_item_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            auc_item_id,
            InventoryItemRecord {
                item_id: auc_item_id,
                type_id: 587,
                owner_id: P1,
                location_id: auc_cid,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            auc_cid,
            eve_db::ContractRecord {
                contract_id: auc_cid,
                issuer_id: P1,
                issuer_corp_id: 98_000_001,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                current_bid: IskCents::from_isk(100_000),
                current_bidder_id: Some(P2),
                expiration_unix: now - 10,
                item_ids: vec![auc_item_id],
                ..Default::default()
            },
        );
    });

    let report = run_daily_downtime_and_jobs(&ctx);
    assert_eq!(report.get("expiredContracts"), Some(&2));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&exch_cid].status, "Expired");
        assert_eq!(snap.items[&exch_item_id].owner_id, P1);
        assert_eq!(snap.items[&exch_item_id].location_id, JITA_STATION_ID);
        assert_eq!(snap.items[&exch_item_id].flag, InventoryFlag::Hangar);

        assert_eq!(snap.contracts[&auc_cid].status, "Completed");
        assert_eq!(snap.contracts[&auc_cid].acceptor_id, Some(P2));
        assert_eq!(snap.items[&auc_item_id].owner_id, P2);
        assert_eq!(snap.items[&auc_item_id].location_id, JITA_STATION_ID);
        assert_eq!(snap.items[&auc_item_id].flag, InventoryFlag::Hangar);
    });
    assert_eq!(
        balance(&ctx, P1).0 - p1_init.0,
        IskCents::from_isk(125_000).0,
        "Issuer must receive 25k reward refund + 100k winning auction bid"
    );

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&P2) {
            ch2.corporation_id = corp_id;
        }
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(50_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.member_roles.insert(P2, 0);
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_isk(50_000);
        for d in &mut corp.wallet_divisions {
            if d.division_id == 1 {
                d.balance = IskCents::from_isk(50_000);
            } else if d.division_id == 2 {
                d.balance = IskCents::from_isk(200_000);
            }
        }
    });
    s2.corporation_id = Some(corp_id);
    s2.corp_role = 0;

    let unauth_ap = rpc(
        &ctx,
        &mut s2,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![(
            PyRep::String("divisionID".into()),
            PyRep::Integer(1001),
        )])],
    )
    .await
    .expect_err("Non-accountant must not configure automatic bill payment");
    assert_eq!(unauth_ap.code, "CorpRoleRequired");

    let bill_id = create_bill(&ctx, 2, corp_id, 1_000_035, 120_000.0, now + 86_400, 0.0, JITA_STATION_ID, 0);
    rpc(
        &ctx,
        &mut s1,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(bill_id), PyRep::Integer(1001)],
    )
    .await
    .expect("PayCorporationBill from division 2 (1001) succeeds even though division 1 only has 50k");

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        let div1 = corp.wallet_divisions.iter().find(|d| d.division_id == 1).unwrap().balance;
        let div2 = corp.wallet_divisions.iter().find(|d| d.division_id == 2).unwrap().balance;
        assert_eq!(corp.master_wallet, IskCents::from_isk(50_000), "master_wallet untouched");
        assert_eq!(div1, IskCents::from_isk(50_000), "Division 1 untouched");
        assert_eq!(div2, IskCents::from_isk(80_000), "Division 2 debited by 120k");
    });
}

#[tokio::test]
async fn test_wallet_transactions_crowset_and_entry_types_live_client_compat() {
    use eve_db::ledger::{ref_types, JournalContext, JournalEntryInput, JournalMeta};
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 991, "demo", "demo-secret", P1).await;
    let kw = HashMap::new();

    // Seed a donation journal entry so GetTransactions has a live row to convert.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let meta = JournalMeta::new(ref_types::PLAYER_DONATION, P1, P2, now)
            .with_context(JournalContext::Character, P1)
            .with_account_key(1000);
        snap.append_journal_entry(JournalEntryInput {
            owner_id: P1,
            entry_type: "player_donation",
            amount: IskCents::from_isk(-250),
            balance_after: IskCents::from_isk(99_750),
            reason: "compat test".into(),
            meta,
        });
    });

    // 1. account.GetEntryTypes returns CRowset with entryTypeID/entryTypeName/entryTypeNameID/description
    let entry_types = ctx
        .dispatch_rpc(
            &mut s1,
            "account",
            None,
            "GetEntryTypes",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("account.GetEntryTypes");
    let PyRep::ObjectEx { header, list, .. } = &entry_types else {
        panic!("expected CRowset ObjectEx, got {entry_types:?}");
    };
    assert!(
        format!("{header:?}").contains("CRowset"),
        "expected CRowset header, got {header:?}"
    );
    assert!(
        list.iter().any(|r| {
            matches!(r, PyRep::DBRow { columns, .. } if columns.iter().any(|(k, v)| k == "entryTypeID" && *v == PyRep::Integer(i64::from(ref_types::PLAYER_DONATION))))
        }),
        "GetEntryTypes must include PLAYER_DONATION"
    );

    // 2. account.GetTransactions under machoNet:26000 returns CRowset (blue.DBRow elements for KeyVal)
    let macho_journal = ctx
        .dispatch_rpc(
            &mut s1,
            "account",
            None,
            "GetTransactions",
            &[PyRep::Integer(1000), PyRep::None, PyRep::None, PyRep::Bool(false)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("machoNet account.GetTransactions");
    let PyRep::ObjectEx { header: j_hdr, list: j_rows, .. } = &macho_journal else {
        panic!("expected CRowset ObjectEx for machoNet GetTransactions, got {macho_journal:?}");
    };
    assert!(format!("{j_hdr:?}").contains("CRowset"));
    assert_eq!(j_rows.len(), 1);
    let wire = eve_proto::marshal::encode(&macho_journal);
    let roundtrip = eve_proto::marshal::decode(&wire).expect("decode GetTransactions CRowset");
    let PyRep::ObjectEx { list: rt_rows, .. } = roundtrip else {
        panic!("decoded CRowset expected");
    };
    let PyRep::DBRow { columns, .. } = &rt_rows[0] else {
        panic!("expected decoded DBRow");
    };
    let amt = columns
        .iter()
        .find(|(k, _)| k == "amount")
        .and_then(|(_, v)| v.as_f64())
        .unwrap();
    assert!((amt + 250.0).abs() < 1e-6);

    // 3. marketProxy.CharGetTransactions under machoNet:26000 returns CRowset too
    let macho_mkt = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "CharGetTransactions",
            &[PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("machoNet marketProxy.CharGetTransactions");
    let PyRep::ObjectEx { header: m_hdr, .. } = &macho_mkt else {
        panic!("expected CRowset ObjectEx for machoNet CharGetTransactions, got {macho_mkt:?}");
    };
    assert!(format!("{m_hdr:?}").contains("CRowset"));
}

}
