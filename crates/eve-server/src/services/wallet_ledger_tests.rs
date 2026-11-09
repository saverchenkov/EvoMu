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
    assert!(char_j.is_crowset(), "account.GetTransactions must return CRowset");
    let char_j_rows = char_j.crowset_rows().unwrap();
    assert!(char_j_rows.iter().any(|r| r.dbrow_get("description").and_then(|v| v.as_str()) == Some("test journal")));

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
    assert!(corp_j.is_crowset());
    assert!(corp_j.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("description").and_then(|v| v.as_str()) == Some("corp journal")));

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
    assert!(char_tx.is_crowset());
    assert!(char_tx.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("stationID") == Some(&PyRep::Integer(60003760))));

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
    assert!(corp_tx.is_crowset());
    assert!(corp_tx.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("stationID") == Some(&PyRep::Integer(60003760))));
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

#[test]
fn issue_550_wallet_rbac_trader_accountant_and_pending_stasis() {
    let ctx = make_test_context();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let ceo_id = 90_000_001_i64;
    let mem_id = 90_000_002_i64;
    let corp_id = 98_000_010_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "RBAC Ledger".into(),
            ticker: "RBAC".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(100_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(100_000));
        corp.set_division_balance(2, IskCents::from_isk(50_000));
        corp.members.insert(ceo_id);
        corp.members.insert(mem_id);
        // Prematurely mirrored role that is STILL gated in pending_role_grants
        corp.member_roles
            .insert(mem_id, corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]);
        snap.corporations.insert(corp_id, corp);
        snap.corp_governance.entry(corp_id).or_default().pending_role_grants.insert(
            mem_id,
            (corp_roles::ROLE_ACCOUNT_CAN_TAKE[1], now + 3_600),
        );
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(1_000);
        }
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 9_000_001,
            date_unix: now,
            type_id: 34,
            quantity: 100,
            price: IskCents::from_isk(5),
            buyer_id: corp_id,
            seller_id: ceo_id,
            location_id: 60_003_760,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            journal_ref_id: 1,
        });
    });

    let mut mem_sess = char_session(mem_id, corp_id);
    // Forged session.corp_role MUST NOT bypass DB role checks
    mem_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    // 1. Accountant alone can view journal & market transactions, but CANNOT withdraw from div 1
    let cash_err = handle(
        &ctx,
        &mut mem_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(mem_id), PyRep::Real(100.0), PyRep::Integer(1000)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(cash_err.code, "CrpAccessDenied");

    // 2. Unmatured pending_role_grants Account Take 2 stays blocked despite member_roles bit
    let stasis_err = handle(
        &ctx,
        &mut mem_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(mem_id), PyRep::Real(100.0), PyRep::Integer(1001)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(stasis_err.code, "CrpAccessDenied");

    // 3. ROLE_TRADER alone can call marketProxy.CorpGetTransactions, NOT account.GetTransactions
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.get_mut(&corp_id).unwrap().pending_role_grants.clear();
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, corp_roles::ROLE_TRADER);
    });
    assert!(handle(
        &ctx,
        &mut mem_sess,
        "marketProxy",
        "CorpGetTransactions",
        &[PyRep::Integer(0), PyRep::Integer(1000)],
        &HashMap::new(),
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle(
            &ctx,
            &mut mem_sess,
            "account",
            "GetTransactions",
            &[PyRep::Integer(1000), PyRep::None, PyRep::None, PyRep::Bool(true)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // 4. Advance clock past stasis -> Account Take 2 matures and grants Div 2 withdrawal
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.get_mut(&corp_id).unwrap().pending_role_grants.insert(
            mem_id,
            (corp_roles::ROLE_ACCOUNT_CAN_TAKE[1], now + 3_600),
        );
    });
    ctx.advance_virtual_clock(3_601);
    handle(
        &ctx,
        &mut mem_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(mem_id), PyRep::Real(500.0), PyRep::Integer(1001)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp_id].division_balance(2), IskCents::from_isk(49_500));
        assert_eq!(snap.characters[&mem_id].balance, IskCents::from_isk(1_500));
    });
}

#[test]
fn get_entry_types_covers_all_ref_types_and_matches_accountsvc_schema() {
    use eve_db::ledger::ref_types;

    let ctx = make_test_context();
    let mut sess = char_session(90_000_001, 98_000_001);
    let rep = handle(
        &ctx,
        &mut sess,
        "account",
        "GetEntryTypes",
        &[],
        &HashMap::new(),
    )
    .expect("account.GetEntryTypes handled")
    .expect("account.GetEntryTypes succeeds");

    let wire = eve_proto::marshal::encode(&rep);
    let decoded = eve_proto::marshal::decode(&wire).expect("GetEntryTypes roundtrip");
    let PyRep::ObjectEx { header, list, .. } = decoded else {
        panic!("expected CRowset ObjectEx, got {rep:?}");
    };
    assert!(format!("{header:?}").contains("CRowset"));

    let mut seen_ids = std::collections::HashSet::new();
    for row in &list {
        let PyRep::DBRow { columns, .. } = row else {
            panic!("expected DBRow element, got {row:?}");
        };
        let get = |name: &str| {
            columns
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("missing column {name}"))
        };
        let id = get("entryTypeID").as_i64().expect("entryTypeID integer") as i32;
        let name = match get("entryTypeName") {
            PyRep::String(s) => s,
            other => panic!("expected entryTypeName string, got {other:?}"),
        };
        assert!(!name.is_empty(), "entryTypeName empty for {id}");
        assert_eq!(get("entryTypeNameID"), &PyRep::None);
        match get("description") {
            PyRep::String(s) => assert!(!s.is_empty(), "description empty for {id}"),
            other => panic!("expected description string, got {other:?}"),
        }
        seen_ids.insert(id);
    }

    for required in [
        ref_types::MARKET_TRANSACTION,
        ref_types::PLAYER_DONATION,
        ref_types::CORPORATION_ACCOUNT_WITHDRAWAL,
        ref_types::MARKET_ESCROW,
        ref_types::BROKER_FEE,
        ref_types::TRANSACTION_TAX,
        ref_types::CONTRACT_COLLATERAL,
        ref_types::CONTRACT_REWARD,
        ref_types::CONTRACT_PRICE,
        ref_types::CONTRACT_BROKERS_FEE,
    ] {
        assert!(seen_ids.contains(&required), "missing refType {required}");
    }
}
