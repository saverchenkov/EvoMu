//! Focused unit tests for uncovered branches in `wallet_ledger`
//! (`DepositPlex`, `WithdrawPlex` positional/kwargs/ship cargo/overflow,
//! `GiveCashFromCorpAccount` errors, and `MarketTransactions`).

use super::*;
use crate::router::ServerContext;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn make_ctx() -> ServerContext {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    let sde = Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)));
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
}

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
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
fn plex_vault_deposit_withdraw_cargo_and_error_paths() {
    let ctx = make_ctx();
    let char_id = 90_000_001i64;
    let other_id = 90_000_002i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_810_500i64;
    let plex_hangar_id = 96_810_100i64;
    let plex_cargo_id = 96_810_101i64;
    let other_hangar_id = 96_810_102i64;

    let start_vault = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        let init_b = snap.adjust_plex_vault(char_id, 25).expect("seed vault");

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // PLEX at a DIFFERENT station (60_003_761)
        snap.items.insert(
            other_hangar_id,
            InventoryItemRecord {
                item_id: other_hangar_id,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: 60_003_761,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // PLEX in active ship cargo at current station
        snap.items.insert(
            plex_cargo_id,
            InventoryItemRecord {
                item_id: plex_cargo_id,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 40,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // PLEX in current station hangar
        snap.items.insert(
            plex_hangar_id,
            InventoryItemRecord {
                item_id: plex_hangar_id,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 60,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        init_b
    });

    let mut sess = make_session(char_id, 1_000_009);
    sess.ship_id = Some(ship_id);

    // 1. Undocked DepositPlexToVault rejected
    let mut undocked = make_session(char_id, 1_000_009);
    undocked.station_id = None;
    let err = handle(
        &ctx,
        &mut undocked,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(station_id), PyRep::Integer(plex_hangar_id), PyRep::Integer(10)],
        &HashMap::new(),
    )
    .expect("handled")
    .expect_err("must be docked");
    assert_eq!(err.code, "NotDockedAtLocation");

    // 2. Invalid/non-positive quantity DepositPlexToVault & WithdrawPlexFromVault rejected
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(station_id), PyRep::Integer(plex_hangar_id), PyRep::Integer(0)],
            &HashMap::new()
        )
        .expect("handled")
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(-5)], &HashMap::new())
            .expect("handled")
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // 3. Deposit 120 fails (hangar item only has 60 -> NotEnoughQuantity)
    let err = handle(
        &ctx,
        &mut sess,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(station_id), PyRep::Integer(plex_hangar_id), PyRep::Integer(120)],
        &HashMap::new(),
    )
    .expect("handled")
    .expect_err("insufficient stack quantity");
    assert_eq!(err.code, "NotEnoughQuantity");

    // 4. Deposit all 60 from hangar stack -> removes hangar stack, returns tx entity_id, and adds 60 to vault
    let deposit_eid = handle(
        &ctx,
        &mut sess,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(station_id), PyRep::Integer(plex_hangar_id), PyRep::Integer(60)],
        &HashMap::new(),
    )
    .expect("handled")
    .expect("deposit 60 ok")
    .as_i64()
    .unwrap();
    assert!(deposit_eid > 0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&plex_hangar_id));
        assert_eq!(snap.plex_vault_balance(char_id), start_vault + 60);
    });

    // 5. WithdrawPlexFromVault via kwargs into Hangar (flag 4) & into Ship Cargo (flag 5) & invalid target owner
    let kw_hangar = HashMap::from([
        ("quantity".to_string(), PyRep::Integer(30)),
        ("locationID".to_string(), PyRep::Integer(station_id)),
        ("flag".to_string(), PyRep::Integer(4)),
    ]);
    let w1_rep = handle(&ctx, &mut sess, "invbroker", "WithdrawPlexFromVault", &[], &kw_hangar)
        .expect("handled")
        .expect("withdraw to hangar ok");
    assert!(matches!(w1_rep, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id)), start_vault + 30);

    let w2_rep = handle(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(25), PyRep::Integer(ship_id), PyRep::Integer(5)],
        &HashMap::new(),
    )
    .expect("handled")
    .expect("withdraw to ship cargo merges stack");
    assert!(matches!(w2_rep, PyRep::KeyVal(_)));
    let rem_vault = ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&plex_cargo_id].quantity, 65);
        snap.plex_vault_balance(char_id)
    });
    assert_eq!(rem_vault, start_vault + 5);

    // 6. Withdraw more than vault balance fails
    let err = handle(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(rem_vault + 1)],
        &HashMap::new(),
    )
    .expect("handled")
    .expect_err("insufficient vault plex");
    assert_eq!(err.code, "NotEnoughPlex");

    // 7. Withdraw into another character's item fails
    let mut other_sess = make_session(other_id, 1_000_009);
    let err = handle(
        &ctx,
        &mut other_sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(1), PyRep::Integer(ship_id), PyRep::Integer(5)],
        &HashMap::new(),
    )
    .expect("handled");
    assert!(err.is_err());
    let _ = CorporationRecord::default();
    let _ = IskCents(0);
    let _ = ServerContext::require_character;
}

#[test]
fn insurance_journal_legacy_non_type_context_falls_back_to_ship_class_type() {
    let ctx = make_ctx();
    let char_id = 90_000_001i64;
    let corp_id = 98_000_001i64;
    let now = ctx.now_unix();
    let (yr, mo) = year_month(now);

    ctx.db.with_mut_snapshot(|snap| {
        snap.append_journal_entry(JournalEntryInput {
            owner_id: char_id,
            entry_type: "InsurancePayout",
            amount: IskCents(1_000_00),
            balance_after: IskCents(5_000_00),
            reason: "legacy-insurance-char-ctx".into(),
            meta: JournalMeta::new(ref_types::INSURANCE, 10_000_132, char_id, now)
                .with_context(JournalContext::Character, 90_000_001)
                .with_account_key(ACCOUNTING_KEY_CASH),
        });
        snap.append_journal_entry(JournalEntryInput {
            owner_id: char_id,
            entry_type: "InsurancePayout",
            amount: IskCents(2_000_00),
            balance_after: IskCents(7_000_00),
            reason: "modern-insurance-type-ctx".into(),
            meta: JournalMeta::new(ref_types::INSURANCE, 10_000_132, char_id, now)
                .with_context(JournalContext::Type, 587)
                .with_account_key(ACCOUNTING_KEY_CASH),
        });
        snap.append_journal_entry(JournalEntryInput {
            owner_id: char_id,
            entry_type: "PlayerDonation",
            amount: IskCents(3_000_00),
            balance_after: IskCents(10_000_00),
            reason: "non-insurance-char-ctx".into(),
            meta: JournalMeta::new(ref_types::PLAYER_DONATION, 90_000_002, char_id, now)
                .with_context(JournalContext::Character, 90_000_002)
                .with_account_key(ACCOUNTING_KEY_CASH),
        });
    });

    let mut sess = make_session(char_id, corp_id);
    let rep = handle(
        &ctx,
        &mut sess,
        "account",
        "GetTransactions",
        &[
            PyRep::Integer(ACCOUNTING_KEY_CASH as i64),
            PyRep::Integer(yr),
            PyRep::Integer(mo),
            PyRep::Bool(false),
        ],
        &HashMap::new(),
    )
    .expect("handled")
    .expect("GetTransactions ok");
    let rows = rep.crowset_rows().expect("crowset rows");
    let ref_for = |desc: &str| {
        rows.iter()
            .find(|r| r.dbrow_get("description").and_then(|v| v.as_str()) == Some(desc))
            .unwrap_or_else(|| panic!("missing journal row {desc}"))
            .dbrow_get("referenceID")
            .and_then(|v| v.as_i64())
    };
    assert_eq!(ref_for("legacy-insurance-char-ctx"), Some(670));
    assert_eq!(ref_for("modern-insurance-type-ctx"), Some(587));
    assert_eq!(ref_for("non-insurance-char-ctx"), Some(90_000_002));
}
