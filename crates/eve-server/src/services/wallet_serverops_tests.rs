use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState,
    UpwellStructureRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
#[path = "wallet_serverops_extra_unit_tests.rs"]
mod wallet_serverops_extra_unit_tests;

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
fn give_cash_and_cspa_contact_cost_work_and_validate() {
    let ctx = make_test_context();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let corp_id = 98_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            ca.balance = IskCents::from_f64_isk(50_000.0);
        }
        let mut cb = snap.characters[&char_a].clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "Recipient B".into();
        cb.corporation_id = corp_id;
        cb.balance = IskCents::from_f64_isk(10_000.0);
        snap.characters.insert(char_b, cb);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Test Corp".into();
                __corp.ticker = "TST".into();
                __corp.ceo_id = char_a;
                __corp },
        );
    });

    let mut sess = char_session(char_a, corp_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_a), PyRep::Real(100.0)],
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
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(-10.0)],
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
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(999_999_999), PyRep::Real(100.0)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidRecipient"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(999_999.0)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );

    let new_bal = handle(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(char_b),
            PyRep::Real(15_000.0),
            PyRep::String("gift".into()),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(new_bal.as_f64(), Some(35_000.0));

    let new_bal2 = handle(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(5_000.0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(new_bal2.as_f64(), Some(30_000.0));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GetDefaultContactCost",
            &[],
            &HashMap::new()
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "SetContactCost",
            &[PyRep::Real(-1.0)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    handle(
        &ctx,
        &mut sess,
        "account",
        "SetContactCost",
        &[PyRep::Real(2_950.0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GetDefaultContactCost",
            &[],
            &HashMap::new()
        )
        .unwrap()
        .unwrap()
        .as_f64(),
        Some(2_950.0)
    );
    handle(
        &ctx,
        &mut sess,
        "account",
        "SetContactCost",
        &[PyRep::None],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GetDefaultContactCost",
            &[],
            &HashMap::new()
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
}

#[test]
fn bill_mgr_personal_corporate_and_autopay_lifecycle() {
    let ctx = make_test_context();
    let ceo_id = 90_000_001;
    let corp_id = 98_000_001;
    let cred_corp_id = 1_000_125;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_f64_isk(100_000.0);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Debtor Corp".into();
            __corp.ticker = "DBT".into();
            __corp.ceo_id = ceo_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(200_000.0));
            __corp };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(200_000.0);
        snap.corporations.insert(corp_id, corp);
        snap.corporations.insert(
            cred_corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = cred_corp_id;
                __corp.name = "CONCORD".into();
                __corp.ticker = "CND".into();
                __corp },
        );
    });

    let char_bill_id = create_bill(&ctx, 2, ceo_id, cred_corp_id, 10_000.0, 1000, 0.0, 1, 2);
    let corp_bill_id = create_bill(&ctx, 2, corp_id, cred_corp_id, 25_000.0, 1000, 0.0, 1, 2);
    let auto_bill_id = create_bill(&ctx, 6, corp_id, cred_corp_id, 30_000.0, 1000, 0.0, 1, 2);

    let mut sess = char_session(ceo_id, corp_id);
    let bills = handle(
        &ctx,
        &mut sess,
        "billMgr",
        "GetCorporationBills",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{bills:?}").contains(&corp_bill_id.to_string()));

    handle(
        &ctx,
        &mut sess,
        "billMgr",
        "CharPayBill",
        &[PyRep::Integer(char_bill_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "billMgr",
            "CharPayBill",
            &[PyRep::Integer(char_bill_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BillAlreadyPaid"
    );

    handle(
        &ctx,
        &mut sess,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(corp_bill_id), PyRep::Integer(1000)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    let ap_default = handle(
        &ctx,
        &mut sess,
        "billMgr",
        "GetAutomaticPaySettings",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(default_outer) = &ap_default else {
        panic!("expected Dict from GetAutomaticPaySettings");
    };
    let (_, default_inner) = default_outer.first().expect("corp entry");
    let PyRep::Dict(default_pairs) = default_inner else {
        panic!("expected inner Dict");
    };
    for bt in 1..=7 {
        assert!(
            default_pairs.iter().any(|(k, v)| *k == PyRep::Integer(bt) && *v == PyRep::Bool(false)),
            "missing default billTypeID {bt}"
        );
    }
    assert!(default_pairs.iter().any(|(k, v)| k.as_str() == Some("divisionID") && *v == PyRep::Integer(1000)));

    handle(
        &ctx,
        &mut sess,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![
            (PyRep::Integer(6), PyRep::Bool(true)),
            (PyRep::String("divisionID".into()), PyRep::Integer(1000)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    let ap_rep = handle(
        &ctx,
        &mut sess,
        "billMgr",
        "GetAutomaticPaySettings",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(format!("{ap_rep:?}").contains("1000"));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "billMgr",
            "PayCorporationBill",
            &[PyRep::Integer(auto_bill_id), PyRep::Integer(1000)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BillAlreadyPaid"
    );
}

#[test]
fn pos_mgr_and_cluster_telemetry_handlers_work() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        16634,
        InvType {
            type_id: 16634,
            group_id: 427,
            category_id: 4,
            name: "Atmospheric Gases".into(),
            mass: 1.0,
            volume: 0.05,
            capacity: 0.0,
            radius: 10.0,
            base_price: 100.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let ally_id = 99_000_001;
    let tower_id = 1_000_000_500;
    let silo_id = 1_000_000_501;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "POS Corp".into();
                __corp.ticker = "POS".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(ally_id);
                __corp },
        );
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                sov_alliance_id: Some(ally_id),
                ..Default::default()
            },
        );
        snap.structures.insert(
            tower_id,
            UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 12235,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Tower".into(),
                state: "Online".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            silo_id,
            InventoryItemRecord {
                item_id: silo_id,
                type_id: 14343,
                owner_id: corp_id,
                location_id: tower_id,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, corp_id);
    sess.solar_system_id = Some(30_000_142);

    let towers = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "GetControlTowers",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(towers, PyRep::List(ref v) if v.len() == 1));

    let factor = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "GetConsumptionFactor",
        &[PyRep::Integer(tower_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(factor.as_f64(), Some(0.75));

    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "AssumeStructureControl",
        &[PyRep::Integer(tower_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "RelinquishStructureControl",
        &[PyRep::Integer(tower_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "SetStarbasePermissions",
        &[
            PyRep::Integer(tower_id),
            PyRep::String("alliance".into()),
            PyRep::String("starbase".into()),
            PyRep::String("corp".into()),
            PyRep::String("alliance".into()),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let perms = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "GetStarbasePermissions",
        &[PyRep::Integer(tower_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(perms, PyRep::Tuple(ref v) if v.len() == 2));

    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "ChangeStructureProvisionType",
        &[
            PyRep::Integer(tower_id),
            PyRep::Integer(silo_id),
            PyRep::Integer(16634),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "posMgr",
            "GetMoonProcessInfoForTower",
            &[PyRep::Integer(silo_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(16634)
    );

    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "LinkResourceForTower",
        &[
            PyRep::Integer(tower_id),
            PyRep::List(vec![PyRep::Integer(silo_id)]),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    assert!(
        handle(&ctx, &mut sess, "pingService", "Ping", &[], &HashMap::new())
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(matches!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "GetClusterGameStatisticsForClient",
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap(),
        PyRep::Tuple(_)
    ));
    handle(
        &ctx,
        &mut sess,
        "clientStatLogger",
        "LogString",
        &[PyRep::String("metric".into())],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    // AnchorOrbital delegates to real orbital gantry validation rather than returning a static stub.
    let anchor_missing = handle(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[], &HashMap::new())
        .unwrap()
        .unwrap_err();
    assert_eq!(anchor_missing.code, "MissingArgument");
    let anchor_non_gantry = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(tower_id), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(anchor_non_gantry.code, "StarbaseRemoved");

    // #862: GiveCash to a corporation emits OnAccountChange for both sender and recipient corp wallet
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut uid = 1;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(50_000.0);
            uid = ch.user_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
        }
        uid
    });
    ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, 1_700_000_000).unwrap();
    ctx.drain_notifications_for_char(char_id);
    handle(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(1_250.0), PyRep::String("Donation".into())],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        notes.iter().any(|n| n.event == "OnAccountChange"
            && n.args.get(1) == Some(&PyRep::Integer(corp_id))
            && n.args.get(2).and_then(|v| v.as_f64()).is_some_and(|b| b > 0.0)),
        "expected OnAccountChange notification for recipient corporation wallet (#862)"
    );
}

#[test]
fn give_cash_rejects_subcent_amounts_and_oversized_reason_memos() {
    let ctx = make_test_context();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let corp_id = 98_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            // Zero-ISK sender checks that sub-cent rounding cannot exploit zero-cost journal spam.
            ca.balance = IskCents(0);
        }
        let mut cb = snap.characters[&char_a].clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "Recipient B".into();
        cb.corporation_id = corp_id;
        cb.balance = IskCents::from_isk(100);
        snap.characters.insert(char_b, cb);
    });

    let mut sess = char_session(char_a, corp_id);
    let kw_empty = HashMap::new();
    let commits_before = ctx.db.tx_commit_count();
    let (wj_before, wjm_before, wjt_before) = ctx.db.with_snapshot(|s| {
        (
            s.wallet_journal.len(),
            s.wallet_journal_meta.len(),
            s.wallet_journal_timestamps.len(),
        )
    });

    for subcent in [0.001, 0.004, 0.0049] {
        let err = handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(subcent), PyRep::String("memo".into())],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    let huge_reason = "X".repeat(10_000);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(1.0), PyRep::String(huge_reason.clone())],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(1.0)],
            &HashMap::from([("reason".to_string(), PyRep::String(huge_reason))]),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    // Multi-byte UTF-8 > 512 bytes and ASCII > 256 chars both reject before opening a write tx.
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(1.0), PyRep::String("🚀".repeat(129))],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(char_b), PyRep::Real(1.0), PyRep::String("a".repeat(257))],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.wallet_journal.len(), wj_before);
        assert_eq!(s.wallet_journal_meta.len(), wjm_before);
        assert_eq!(s.wallet_journal_timestamps.len(), wjt_before);
        assert_eq!(s.characters[&char_a].balance, IskCents(0));
        assert_eq!(s.characters[&char_b].balance, IskCents::from_isk(100));
    });

    // Valid 0.01 ISK transfer with a 64-char memo succeeds and records both journals.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_a).unwrap().balance = IskCents::from_isk(10);
    });
    let memo_64 = "m".repeat(64);
    let bal_after = handle(
        &ctx,
        &mut sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(char_b), PyRep::Real(0.01), PyRep::String(memo_64.clone())],
        &kw_empty,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bal_after.as_f64(), Some(9.99));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_a].balance, IskCents(999));
        assert_eq!(s.characters[&char_b].balance, IskCents(10_001));
        assert_eq!(s.wallet_journal.len(), wj_before + 2);
        assert!(s.wallet_journal.iter().any(|e| e.character_id == char_a && e.reason.contains(&memo_64)));
        assert!(s.wallet_journal.iter().any(|e| e.character_id == char_b && e.reason.contains(&memo_64)));
    });
}

#[test]
fn set_starbase_permissions_rejects_oversized_rep_flags_without_mutation() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let tower_id = 1_000_000_590;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            {
                let mut corp = CorporationRecord::default();
                corp.corporation_id = corp_id;
                corp.name = "Perm Guard Corp".into();
                corp.ticker = "PGC".into();
                corp.ceo_id = char_id;
                corp
            },
        );
        snap.structures.insert(
            tower_id,
            UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 12235,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Perm Tower".into(),
                state: "Online".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, corp_id);
    sess.solar_system_id = Some(30_000_142);

    // Seed baseline permissions first so we verify failed calls leave `starbase_settings` intact.
    handle(
        &ctx,
        &mut sess,
        "posMgr",
        "SetStarbasePermissions",
        &[
            PyRep::Integer(tower_id),
            PyRep::String("alliance".into()),
            PyRep::String("corp".into()),
            PyRep::String("starbase".into()),
            PyRep::String("corp".into()),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let before = ctx.db.with_snapshot(|s| s.starbase_settings[&tower_id].clone());
    let commits_before = ctx.db.tx_commit_count();

    let oversize_str = "x".repeat(65);
    let invalid_payloads = [
        // `util.Row` container
        PyRep::Object {
            type_name: "util.Row".into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::List(vec![
                    PyRep::String("anchor".into()),
                    PyRep::String("unanchor".into()),
                ]),
                PyRep::List(vec![
                    PyRep::String(oversize_str.clone()),
                    PyRep::String("corp".into()),
                ]),
            ])),
        },
        // `KeyVal` container
        PyRep::KeyVal(HashMap::from([
            ("anchor".to_string(), PyRep::String("alliance".into())),
            ("online".to_string(), PyRep::String(oversize_str.clone())),
        ])),
        // `Dict` container
        PyRep::Dict(vec![
            (PyRep::String("anchor".into()), PyRep::String("corp".into())),
            (
                PyRep::String("offline".into()),
                PyRep::Bytes(oversize_str.as_bytes().to_vec()),
            ),
        ]),
    ];

    for payload in invalid_payloads {
        let err = handle(
            &ctx,
            &mut sess,
            "posMgr",
            "SetStarbasePermissions",
            &[PyRep::Integer(tower_id), payload],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.starbase_settings[&tower_id].anchor, before.anchor);
        assert_eq!(s.starbase_settings[&tower_id].unanchor, before.unanchor);
        assert_eq!(s.starbase_settings[&tower_id].online, before.online);
        assert_eq!(s.starbase_settings[&tower_id].offline, before.offline);
    });
}

#[tokio::test]
async fn pos_mgr_enforces_corp_roles_and_rejects_cloaked_structure_control() {
    let ctx = make_test_context();
    let sys_id = 30_000_142_u32;
    let corp_a = 98_900_001_i64;
    let corp_b = 98_900_002_i64;
    let director_id = 90_900_001_i64;
    let roleless_id = 90_900_002_i64;
    let hangar_query_id = 90_900_003_i64;
    let config_only_id = 90_900_005_i64;
    let caretaker_id = 90_900_006_i64;
    let tactician_id = 90_900_007_i64;
    let outsider_id = 90_900_004_i64;
    let tower_id = 888_900_001_i64;
    let ship_id = 777_900_001_i64;
    let role_hangar_q5 = eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[4];

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            tower_id,
            UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 12_235,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "Test Tower".into(),
                state: "Online".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.corporations.insert(corp_a, {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = corp_a;
            corp.name = "Corp A".into();
            corp.ticker = "CA".into();
            corp.ceo_id = director_id;
            corp.member_roles = HashMap::from([
                (director_id, eve_db::corp_roles::ROLE_DIRECTOR),
                (roleless_id, 0),
                (hangar_query_id, role_hangar_q5),
                (config_only_id, eve_db::corp_roles::ROLE_STARBASE_CONFIG),
                (caretaker_id, eve_db::corp_roles::ROLE_STARBASE_CARETAKER),
                (tactician_id, eve_db::corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN),
            ]);
            corp
        });
        for (cid, corp) in [
            (director_id, corp_a),
            (roleless_id, corp_a),
            (hangar_query_id, corp_a),
            (config_only_id, corp_a),
            (caretaker_id, corp_a),
            (tactician_id, corp_a),
            (outsider_id, corp_b),
        ] {
            let mut c = eve_db::CharacterRecord::default();
            c.character_id = cid;
            c.name = format!("Pilot {cid}");
            c.corporation_id = corp;
            c.solar_system_id = sys_id;
            snap.characters.insert(cid, c);
        }
    });

    let mut dir_sess = char_session(director_id, corp_a);
    dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let mut roleless_sess = char_session(roleless_id, corp_a);
    roleless_sess.corp_role = 0;
    let mut hangar_sess = char_session(hangar_query_id, corp_a);
    hangar_sess.corp_role = role_hangar_q5;
    let mut config_sess = char_session(config_only_id, corp_a);
    config_sess.corp_role = eve_db::corp_roles::ROLE_STARBASE_CONFIG;
    let mut caretaker_sess = char_session(caretaker_id, corp_a);
    caretaker_sess.corp_role = eve_db::corp_roles::ROLE_STARBASE_CARETAKER;
    let mut tactician_sess = char_session(tactician_id, corp_a);
    tactician_sess.corp_role = eve_db::corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN;
    let mut outsider_sess = char_session(outsider_id, corp_b);
    outsider_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    // Outsider director sees only corp_b (no corp_a towers); roleless and read-only hangar-query corp members are rejected.
    assert_eq!(
        handle(&ctx, &mut outsider_sess, "posMgr", "GetControlTowers", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );

    // Outsider, roleless, & ROLE_HANGAR_CAN_QUERY[4] (1<<24) corp members are rejected across all posMgr endpoints.
    for (label, s, expected_err) in [
        ("outsider", &mut outsider_sess, "NotStarbaseOwner"),
        ("roleless", &mut roleless_sess, "RoleNotAuthorized"),
        ("hangar_query_1_24", &mut hangar_sess, "RoleNotAuthorized"),
    ] {
        for method in [
            "GetStarbasePermissions",
            "GetTowerNotificationSettings",
            "GetTowerSentrySettings",
            "GetConsumptionFactor",
            "GetSiloCapacityByItemID",
            "AssumeStructureControl",
            "RelinquishStructureControl",
        ] {
            assert_eq!(
                handle(&ctx, s, "posMgr", method, &[PyRep::Integer(tower_id)], &kw)
                    .unwrap()
                    .unwrap_err()
                    .code,
                expected_err,
                "{label} {method}"
            );
        }
    }

    // ROLE_STARBASE_CARETAKER authorizes fuel/silo queries, NOT config or gunnery control.
    assert!(handle(&ctx, &mut caretaker_sess, "posMgr", "GetConsumptionFactor", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    assert!(handle(&ctx, &mut caretaker_sess, "posMgr", "GetSiloCapacityByItemID", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    for method in ["GetStarbasePermissions", "AssumeStructureControl", "RelinquishStructureControl"] {
        assert_eq!(
            handle(&ctx, &mut caretaker_sess, "posMgr", method, &[PyRep::Integer(tower_id)], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "RoleNotAuthorized",
            "caretaker {method}"
        );
    }

    // ROLE_STARBASE_CONFIG (1<<53) authorizes config/query endpoints, NOT gunnery control.
    assert!(handle(&ctx, &mut config_sess, "posMgr", "GetControlTowers", &[], &kw).unwrap().is_ok());
    assert!(handle(&ctx, &mut config_sess, "posMgr", "GetStarbasePermissions", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    assert!(handle(&ctx, &mut config_sess, "posMgr", "SetStarbasePermissions", &[PyRep::Integer(tower_id), PyRep::String("corp".into())], &kw).unwrap().is_ok());
    assert_eq!(
        handle(&ctx, &mut config_sess, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "RoleNotAuthorized"
    );
    assert_eq!(
        handle(&ctx, &mut config_sess, "posMgr", "RelinquishStructureControl", &[PyRep::Integer(tower_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "RoleNotAuthorized"
    );

    // ROLE_INFRASTRUCTURE_TACTICIAN (1<<57) authorizes AssumeStructureControl and RelinquishStructureControl.
    assert!(handle(&ctx, &mut tactician_sess, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    assert!(handle(&ctx, &mut tactician_sess, "posMgr", "RelinquishStructureControl", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());

    // Cloaked director cannot Assume/Relinquish/RunMoonProcessCycleforTower.
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: eve_destiny::DVec3::ZERO,
                velocity: eve_destiny::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState {
                is_cloaked: true,
                ..Default::default()
            },
            eve_destiny::SlimItemMeta {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    dir_sess.station_id = None;
    dir_sess.solar_system_id = Some(sys_id);
    dir_sess.ship_id = Some(ship_id);
    let commits_before = ctx.db.tx_commit_count();
    let blocked_cloaked = "ActionBlockedWhileCloaked";
    for method in [
        "AssumeStructureControl",
        "RelinquishStructureControl",
        "RunMoonProcessCycleforTower",
    ] {
        assert_eq!(
            handle(&ctx, &mut dir_sess, "posMgr", method, &[PyRep::Integer(tower_id)], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            blocked_cloaked,
            "cloaked {method}"
        );
    }
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
}
