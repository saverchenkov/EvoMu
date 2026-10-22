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

    // Accountant: division-to-division transfer inside the corp via toAccountKey.
    set_role(&ctx, corp, P2, corp_roles::ROLE_ACCOUNTANT);
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
