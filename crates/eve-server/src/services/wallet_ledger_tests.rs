use super::*;
use crate::router::ServerContext;
use eve_db::ledger::MarketTransactionRecord;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

#[test]
fn give_cash_from_corp_account_validates_roles_balances_and_transfers() {
    let ctx = make_test_context();
    let ceo_id = 90_000_001;
    let member_id = 90_000_002;
    let corp_id = 98_000_001;
    let other_corp_id = 98_000_002;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
        }
        let mut mem = snap.characters[&ceo_id].clone();
        mem.character_id = member_id;
        mem.user_id = 502;
        mem.name = "Member Pilot".into();
        mem.corporation_id = corp_id;
        mem.balance = IskCents::from_f64_isk(1_000.0);
        snap.characters.insert(member_id, mem);

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Source Corp".into(),
            ticker: "SRC".into(),
            ceo_id,
            master_wallet: IskCents::from_f64_isk(100_000.0),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(100_000.0);
        snap.corporations.insert(corp_id, corp);

        let mut corp2 = CorporationRecord {
            corporation_id: other_corp_id,
            name: "Dest Corp".into(),
            ticker: "DST".into(),
            ceo_id: 90_000_099,
            ..Default::default()
        };
        corp2.ensure_wallet_divisions();
        snap.corporations.insert(other_corp_id, corp2);
    });

    let mut ceo_sess = char_session(ceo_id, corp_id);
    assert!(handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(1),
            PyRep::Integer(90_000_002),
            PyRep::Real(100.0)
        ],
        &HashMap::new(),
    )
    .is_none());

    assert_eq!(
        handle(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(member_id),
                PyRep::Real(0.0),
                PyRep::Integer(1000)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(member_id),
                PyRep::Real(100.0),
                PyRep::Integer(999)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidAccountKey"
    );

    let mut mem_sess = char_session(member_id, corp_id);
    assert_eq!(
        handle(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(ceo_id),
                PyRep::Real(100.0),
                PyRep::Integer(1000)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(member_id),
                PyRep::Real(500_000.0),
                PyRep::Integer(1000)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );

    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(corp_id),
            PyRep::Real(20_000.0),
            PyRep::Integer(1000),
            PyRep::Integer(1001),
            PyRep::String("Div transfer".into()),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(member_id),
            PyRep::Real(5_000.0),
            PyRep::Integer(1001),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(other_corp_id),
            PyRep::Real(3_000.0),
            PyRep::Integer(1001),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp_id].wallet_divisions[0].balance,
            IskCents::from_f64_isk(80_000.0)
        );
        assert_eq!(
            snap.corporations[&corp_id].wallet_divisions[1].balance,
            IskCents::from_f64_isk(12_000.0)
        );
        assert_eq!(
            snap.characters[&member_id].balance,
            IskCents::from_f64_isk(6_000.0)
        );
        assert_eq!(
            snap.corporations[&other_corp_id].master_wallet,
            IskCents::from_f64_isk(3_000.0)
        );
    });
}

#[test]
fn journal_and_market_transactions_filter_and_enforce_corp_roles() {
    let ctx = make_test_context();
    let ceo_id = 90_000_001;
    let member_id = 90_000_002;
    let corp_id = 98_000_001;

    let now = ctx.now_unix();
    let (yr, mo) = year_month(now);
    let ft_now = filetime(now);
    assert_eq!(unix_from_filetime(ft_now), now);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
        }
        let mut mem = snap.characters[&ceo_id].clone();
        mem.character_id = member_id;
        mem.user_id = 502;
        mem.name = "Member".into();
        mem.corporation_id = corp_id;
        mem.balance = IskCents(0);
        snap.characters.insert(member_id, mem);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TST".into(),
                ceo_id,
                ..Default::default()
            },
        );
        snap.append_journal_entry(JournalEntryInput {
            owner_id: ceo_id,
            entry_type: "PlayerDonation",
            amount: IskCents(100_00),
            balance_after: IskCents(500_00),
            reason: "test journal".into(),
            meta: JournalMeta::new(10, ceo_id, member_id, now).with_account_key(1000),
        });
        snap.append_journal_entry(JournalEntryInput {
            owner_id: corp_id,
            entry_type: "CorporationAccountWithdrawal",
            amount: IskCents(-50_00),
            balance_after: IskCents(950_00),
            reason: "corp journal".into(),
            meta: JournalMeta::new(37, corp_id, ceo_id, now).with_account_key(1000),
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 1,
            date_unix: now,
            type_id: 34,
            quantity: 100,
            price: IskCents(5_00),
            location_id: 60_003_760,
            buyer_id: ceo_id,
            seller_id: corp_id,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            journal_ref_id: 1,
        });
    });

    let mut ceo_sess = char_session(ceo_id, corp_id);
    let mut mem_sess = char_session(member_id, corp_id);

    let char_j = handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GetTransactions",
        &[
            PyRep::Integer(1000),
            PyRep::Integer(yr),
            PyRep::Integer(mo),
            PyRep::Bool(false),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{char_j:?}").contains("test journal"));

    assert_eq!(
        handle(
            &ctx,
            &mut mem_sess,
            "account",
            "GetTransactions",
            &[
                PyRep::Integer(1000),
                PyRep::Integer(yr),
                PyRep::Integer(mo),
                PyRep::Bool(true)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    let corp_j = handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GetTransactions",
        &[
            PyRep::Integer(1000),
            PyRep::Integer(yr),
            PyRep::Integer(mo),
            PyRep::Bool(true),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{corp_j:?}").contains("corp journal"));

    let char_tx = handle(
        &ctx,
        &mut ceo_sess,
        "marketProxy",
        "CharGetTransactions",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{char_tx:?}").contains("60003760"));

    assert_eq!(
        handle(
            &ctx,
            &mut mem_sess,
            "marketProxy",
            "CorpGetTransactions",
            &[PyRep::Integer(0), PyRep::Integer(1000)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    let corp_tx = handle(
        &ctx,
        &mut ceo_sess,
        "marketProxy",
        "CorpGetTransactions",
        &[PyRep::Integer(0), PyRep::Integer(1000)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{corp_tx:?}").contains("60003760"));
}

#[test]
fn deposit_and_withdraw_plex_vault_lifecycle() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);
    session.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70_001,
            InventoryItemRecord {
                item_id: 70_001,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            70_002,
            InventoryItemRecord {
                item_id: 70_002,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        let cur = snap.plex_vault_balance(char_id);
        snap.adjust_plex_vault(char_id, -cur);
    });

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(station_id),
                PyRep::Integer(70_002),
                PyRep::Integer(10)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotPlex"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(station_id),
                PyRep::Integer(70_001),
                PyRep::Integer(200)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughQuantity"
    );

    handle(
        &ctx,
        &mut session,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(station_id),
            PyRep::Integer(70_001),
            PyRep::Integer(60),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&70_001].quantity, 40);
        assert_eq!(snap.plex_vault_balance(char_id), 60);
    });

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(100), PyRep::Integer(station_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughPlex"
    );

    let wd = handle(
        &ctx,
        &mut session,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(25), PyRep::Integer(station_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(wd.dict_get("quantityWithdrawn").unwrap().as_i64(), Some(25));
    assert_eq!(
        wd.dict_get("remainingVaultBalance").unwrap().as_i64(),
        Some(35)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&70_001].quantity, 65);
        assert_eq!(snap.plex_vault_balance(char_id), 35);
    });
}
