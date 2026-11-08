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
}
