use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, ObserverMiningRecord,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_sde::{DogmaAttributeType, InvType, SdeRegistry, TypeListInfo};
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume,
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
        default_attributes,
    }
}

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.attributes.insert(
        3262,
        DogmaAttributeType {
            attribute_id: 3262,
            name: "gasDecompressionBaseEfficiency".into(),
            default_value: 0.80,
            high_is_good: true,
            stackable: true,
            published: true,
        },
    );
    sde.types.insert(
        62452,
        make_inv_type(
            62452,
            255,
            16,
            "Gas Decompression Efficiency",
            0.01,
            HashMap::from([(3260, 1.0)]),
        ),
    );
    sde.types.insert(
        62453,
        make_inv_type(
            62453,
            255,
            16,
            "Shipboard Compression Technology",
            0.01,
            HashMap::from([(3263, 10.0)]),
        ),
    );
    // Athanor (35835) and Tatara (35836)
    sde.types.insert(
        35835,
        make_inv_type(
            35835,
            1406,
            65,
            "Athanor",
            10_000.0,
            HashMap::from([(3261, 0.04), (2722, 2.0), (1547, 2.0)]),
        ),
    );
    sde.types.insert(
        35836,
        make_inv_type(
            35836,
            1406,
            65,
            "Tatara",
            10_000.0,
            HashMap::from([(3261, 0.10), (2722, 5.5), (1547, 3.0)]),
        ),
    );
    // Standup Reprocessing Facility I (35899)
    sde.types.insert(
        35899,
        make_inv_type(
            35899,
            1322,
            66,
            "Standup Reprocessing Facility I",
            100.0,
            HashMap::from([(717, 0.50)]),
        ),
    );
    // Veldspar (1230) -> Compressed Veldspar (62516)
    sde.types
        .insert(1230, make_inv_type(1230, 462, 25, "Veldspar", 0.1, HashMap::new()));
    sde.types.insert(
        62516,
        make_inv_type(62516, 462, 25, "Compressed Veldspar", 0.001, HashMap::new()),
    );
    sde.compressible_types.insert(1230, 62516);
    // Golden Cytoserocin (25268, group 711) -> Compressed Golden Cytoserocin (62377)
    sde.types.insert(
        25268,
        make_inv_type(25268, 711, 4, "Golden Cytoserocin", 10.0, HashMap::new()),
    );
    sde.types.insert(
        62377,
        make_inv_type(
            62377,
            4168,
            4,
            "Compressed Golden Cytoserocin",
            0.1,
            HashMap::new(),
        ),
    );
    sde.compressible_types.insert(25268, 62377);

    // Rorqual (28352, group 883), Industrial Core I (28583, group 515), Asteroid Ore Compressor I (62454, group 4174)
    sde.types
        .insert(28352, make_inv_type(28352, 883, 6, "Rorqual", 1e6, HashMap::new()));
    sde.types.insert(
        28583,
        make_inv_type(
            28583,
            515,
            7,
            "Industrial Core I",
            5.0,
            HashMap::from([(1298, 883.0)]),
        ),
    );
    sde.types.insert(
        62454,
        make_inv_type(
            62454,
            4174,
            7,
            "Capital Asteroid Ore Compressor I",
            5.0,
            HashMap::from([(1298, 883.0), (3265, 1.0), (3255, 500.0), (54, 44_000.0)]),
        ),
    );
    sde.type_lists.insert(
        500,
        TypeListInfo {
            type_list_id: 500,
            name: "Compressible Ore".into(),
            included_type_ids: vec![1230],
            ..Default::default()
        },
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_gas_decompression_efficiency_and_reprocessing_yields() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let struct_id = 1_000_000_000_100;

    assert!((compute_gas_decompression_efficiency(&ctx, char_id, None) - 0.80).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            62452,
            TrainedSkillRecord {
                type_id: 62452,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35836, // Tatara (+0.10)
                owner_corp_id: 1_000_009,
                solar_system_id: 30_000_142,
                name: "Tatara".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                ..Default::default()
            },
        );
    });

    // 0.80 + 0.10 (Tatara) + 0.05 (skill lvl 5) = 0.95
    assert!(
        (compute_gas_decompression_efficiency(&ctx, char_id, Some(struct_id)) - 0.95).abs() < 1e-6
    );
    assert!((compute_structure_reprocessing_base_yield(&ctx.sde, 35835) - 0.51).abs() < 1e-6);
    assert!((compute_structure_reprocessing_base_yield(&ctx.sde, 35836) - 0.5275).abs() < 1e-6);

    let rigged = ctx.db.with_snapshot(|snap| {
        compute_structure_rigged_base_yield(&ctx.sde, snap, struct_id, ctx.sde.types.get(&1230))
    });
    assert!((rigged - 0.5275).abs() < 1e-6);
}

#[tokio::test]
async fn test_in_space_compression_and_structure_compression_and_decompression() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let ship_id = 900_001;
    let struct_id = 1_000_000_000_200;
    let sys_id = 30_000_142;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 28352,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Active Industrial Core (900_002) and Active Compressor (900_003)
        snap.items.insert(
            900_002,
            InventoryItemRecord {
                item_id: 900_002,
                type_id: 28583,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_003,
            InventoryItemRecord {
                item_id: 900_003,
                type_id: 62454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(900_002);
        snap.module_active_states.insert(900_003);
        // Ore stack in SpecializedOreHold (900_004)
        snap.items.insert(
            900_004,
            InventoryItemRecord {
                item_id: 900_004,
                type_id: 1230,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        // Structure + ore stack (900_005) + compressed gas stack (900_006)
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_007,
            InventoryItemRecord {
                item_id: 900_007,
                type_id: 35899,
                owner_id: 1_000_009,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(900_007, true);
        snap.items.insert(
            900_005,
            InventoryItemRecord {
                item_id: 900_005,
                type_id: 1230,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_006,
            InventoryItemRecord {
                item_id: 900_006,
                type_id: 62377,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);

    let comp_space = handle_in_space_compression_mgr(&ctx, &mut session, &[PyRep::Integer(900_004)])
        .await
        .expect("in-space compression should succeed");
    assert_eq!(
        comp_space
            .dict_get("compressedTypeID")
            .and_then(|v| v.as_i64()),
        Some(62516)
    );
    assert_eq!(
        comp_space.dict_get("quantity").and_then(|v| v.as_i64()),
        Some(5_000)
    );

    // Dock in structure for structure compression & gas decompression
    session.structure_id = Some(struct_id);
    let comp_struct = handle_structure_compression_mgr(
        &ctx,
        &mut session,
        "CompressItemInStructure",
        &[PyRep::Integer(900_005)],
    )
    .unwrap()
    .expect("structure compression should succeed");
    assert_eq!(
        comp_struct
            .dict_get("compressedTypeID")
            .and_then(|v| v.as_i64()),
        Some(62516)
    );

    let decomp_gas = handle_structure_compression_mgr(
        &ctx,
        &mut session,
        "DecompressGasInStructure",
        &[PyRep::Integer(900_006)],
    )
    .unwrap()
    .expect("gas decompression should succeed");
    assert_eq!(
        decomp_gas
            .dict_get("decompressedTypeID")
            .and_then(|v| v.as_i64()),
        Some(28694)
    );
    // 100 * 0.84 (base 0.80 + Athanor 0.04) = 84
    assert_eq!(
        decomp_gas
            .dict_get("decompressedQuantity")
            .and_then(|v| v.as_i64()),
        Some(84)
    );

    let eff_rep = handle_structure_compression_mgr(
        &ctx,
        &mut session,
        "GetMyGasDecompressionEfficiency",
        &[PyRep::Integer(struct_id)],
    )
    .unwrap()
    .unwrap();
    assert!((eff_rep.as_f64().unwrap() - 0.84).abs() < 1e-6);
}

#[tokio::test]
async fn test_corp_and_character_mining_ledger_rpcs() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let obs_id = 1_000_000_000_300;
    let now = ctx.now_unix();

    ctx.db
        .record_mining_ledger_entry_with_waste(char_id, 30_000_142, 1230, 1200, 150, now)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Mining Corp".into();
                __corp.ticker = "MINE".into();
                __corp.ceo_id = char_id;
                __corp.tax_rate = 0.05;
                __corp },
        );
        snap.structures.insert(
            obs_id,
            UpwellStructureRecord {
                structure_id: obs_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Observer Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.observer_mining_ledger.insert(
            obs_id,
            vec![ObserverMiningRecord {
                observer_id: obs_id,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 1230,
                quantity: 2_500,
                last_updated_date: "2026-11-05".into(),
                timestamp_unix: now,
            }],
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);

    let obs_list = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserversWithMiningEvents",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return observers");
    assert_ne!(obs_list, PyRep::None);
    let obs_rows = obs_list.crowset_rows().expect("observers CRowset");
    assert_eq!(obs_rows.len(), 1);
    assert_eq!(obs_rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(obs_id)));
    assert_eq!(obs_rows[0].dbrow_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(obs_rows[0].dbrow_get("itemName"), Some(&PyRep::String("Observer Athanor".into())));

    let obs_ledger = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserverLedger",
        &[PyRep::Integer(obs_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return observer ledger");
    assert_ne!(obs_ledger, PyRep::None);
    let ledger_rows = obs_ledger.crowset_rows().expect("observer ledger CRowset");
    assert_eq!(ledger_rows.len(), 1);
    assert_eq!(ledger_rows[0].dbrow_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(ledger_rows[0].dbrow_get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(ledger_rows[0].dbrow_get("typeID"), Some(&PyRep::Integer(1230)));
    assert_eq!(ledger_rows[0].dbrow_get("quantity"), Some(&PyRep::Integer(2_500)));
    assert_eq!(
        ledger_rows[0].dbrow_get("eventDate"),
        Some(&PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(now)))
    );
    assert_eq!(ledger_rows[0].dbrow_get("solarsystemID"), Some(&PyRep::Integer(30_000_142)));

    let char_logs = handle_compression(
        &ctx,
        &mut session,
        "characterMiningLedger",
        "GetCharacterLogs",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return character logs");
    if let PyRep::List(items) = char_logs {
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].dict_get("eventDate").and_then(|v| v.as_i64()),
            Some(crate::sp6_lifecycle::unix_to_filetime(now))
        );
        assert_eq!(
            items[0].dict_get("solarsystemID").and_then(|v| v.as_i64()),
            Some(30_000_142)
        );
        assert_eq!(
            items[0].dict_get("solarSystemID").and_then(|v| v.as_i64()),
            Some(30_000_142)
        );
        assert_eq!(
            items[0].dict_get("quantity").and_then(|v| v.as_i64()),
            Some(1200)
        );
        assert_eq!(
            items[0].dict_get("quantityWasted").and_then(|v| v.as_i64()),
            Some(150)
        );
    } else {
        panic!("expected list of character mining logs");
    }

    assert_eq!(parse_date_string_to_unix_secs("1970-01-01"), Some(0));
    assert_eq!(parse_date_string_to_unix_secs("invalid-date"), None);

    let mut unauth_sess = SessionState::default();
    assert!(
        handle_compression(
            &ctx,
            &mut unauth_sess,
            "characterMiningLedger",
            "GetCharacterLogs",
            &[],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .is_err(),
        "unauthenticated session must be rejected by characterMiningLedger.GetCharacterLogs"
    );
}

#[tokio::test]
async fn corp_mining_ledger_get_observer_ledger_handles_none_and_rejects_bad_types() {
    let ctx = make_ctx();
    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.corporation_id = Some(1_000_009);
    let kw = HashMap::new();

    let expected_empty = eve_proto::DBRowDescriptor::new([
        ("characterID", eve_proto::DbColumnType::I8),
        ("corporationID", eve_proto::DbColumnType::I8),
        ("typeID", eve_proto::DbColumnType::I4),
        ("quantity", eve_proto::DbColumnType::I8),
        ("eventDate", eve_proto::DbColumnType::FileTime),
        ("solarsystemID", eve_proto::DbColumnType::I4),
    ])
    .build_crowset(Vec::new());

    let res_none = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserverLedger",
        &[PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetObserverLedger(None) must return empty CRowset");
    assert!(res_none.is_crowset(), "GetObserverLedger(None) must return CRowset");
    assert_eq!(res_none.crowset_rows().map(<[PyRep]>::len), Some(0));
    assert_eq!(res_none, expected_empty);

    let res_empty = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserverLedger",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetObserverLedger() with missing arg must return empty CRowset");
    assert!(res_empty.is_crowset(), "GetObserverLedger() must return CRowset");
    assert_eq!(res_empty, expected_empty);

    let bad_err = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserverLedger",
        &[PyRep::String("not-an-id".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("non-integer non-None arg must fail with InvalidArgument");
    assert_eq!(bad_err.code, "InvalidArgument");

    let denied_err = handle_compression(
        &ctx,
        &mut session,
        "corpMiningLedger",
        "GetObserverLedger",
        &[PyRep::Integer(9_999_999)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unknown observer ID must fail with CrpAccessDenied");
    assert_eq!(denied_err.code, "CrpAccessDenied");
}

#[test]
fn parse_date_string_to_unix_secs_covers_leap_years_epoch_and_invalid_inputs() {
    for bad in [
        "",
        "2026",
        "2026-11",
        "2026-11-10-01",
        "bad-11-10",
        "2026-ab-10",
        "2026-11-xy",
        "2026-00-10",
        "2026-13-01",
        "2026-01-00",
        "2026-01-32",
        "1969-12-31",
    ] {
        assert_eq!(
            parse_date_string_to_unix_secs(bad),
            None,
            "invalid or pre-epoch date string {bad:?} must return None"
        );
    }

    let known_dates = [
        ("1970-01-01", 0_u64),
        ("1970-01-02", 86_400),
        ("1970-02-28", 58 * 86_400),
        ("1970-03-01", 59 * 86_400),
        ("1972-02-29", 789 * 86_400),
        ("2000-02-29", 11_016 * 86_400),
        ("2000-03-01", 11_017 * 86_400),
        ("2024-01-15", 1_705_276_800),
        ("2024-02-29", 1_709_164_800),
        ("2024-03-01", 1_709_251_200),
        ("2025-03-01", 1_740_787_200),
        ("2026-11-10", 1_794_268_800),
        ("2100-03-01", 4_107_542_400),
    ];
    for (date_str, expected_secs) in known_dates {
        let parsed = parse_date_string_to_unix_secs(date_str)
            .unwrap_or_else(|| panic!("expected Some for {date_str}"));
        assert_eq!(parsed, expected_secs, "date {date_str}");
        assert_eq!(eve_db::unix_secs_to_date_string(parsed), date_str);
    }
}

#[test]
fn caller_has_structure_extraction_role_checks_session_ceo_and_member_roles() {
    use eve_db::corp_roles::{
        ROLE_DIRECTOR, ROLE_STARBASE_CONFIG, ROLE_STATION_MANAGER, ROLE_TRADER,
    };
    use eve_db::WorldStateSnapshot;

    let mut snap = WorldStateSnapshot::default();
    let corp_id = 98_444_001_i64;
    let ceo_id = 91_444_001_i64;
    let config_member = 91_444_002_i64;
    let plain_member = 91_444_003_i64;

    // Missing corp -> permissive fallback
    assert!(caller_has_structure_extraction_role(&snap, plain_member, corp_id, 0));

    // Corp with ceo_id == 0 or empty member_roles -> permissive fallback
    let mut corp = CorporationRecord::default();
    corp.corporation_id = corp_id;
    corp.ceo_id = 0;
    snap.corporations.insert(corp_id, corp.clone());
    assert!(caller_has_structure_extraction_role(&snap, plain_member, corp_id, 0));

    corp.ceo_id = ceo_id;
    snap.corporations.insert(corp_id, corp.clone());
    assert!(caller_has_structure_extraction_role(&snap, plain_member, corp_id, 0));

    // Populate member_roles -> strict role enforcement
    corp.member_roles.insert(config_member, ROLE_STARBASE_CONFIG);
    corp.member_roles.insert(plain_member, ROLE_TRADER);
    snap.corporations.insert(corp_id, corp);

    assert!(caller_has_structure_extraction_role(&snap, ceo_id, corp_id, 0));
    assert!(caller_has_structure_extraction_role(&snap, config_member, corp_id, 0));
    assert!(!caller_has_structure_extraction_role(&snap, plain_member, corp_id, 0));
    // Stale session corp role cannot override explicit DB member_roles (#837)
    assert!(!caller_has_structure_extraction_role(
        &snap,
        plain_member,
        corp_id,
        ROLE_DIRECTOR
    ));
    assert!(!caller_has_structure_extraction_role(
        &snap,
        plain_member,
        corp_id,
        ROLE_STATION_MANAGER
    ));
    let unlisted_member = 90_000_077;
    assert!(caller_has_structure_extraction_role(
        &snap,
        unlisted_member,
        corp_id,
        ROLE_STARBASE_CONFIG
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compression_handlers_cover_space_structure_arg_shapes_and_mining_ledgers() {
    use eve_db::corp_roles::{
        ROLE_ACCOUNTANT, ROLE_STATION_MANAGER, ROLE_TRADER,
    };
    use eve_db::MiningLedgerRecord;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let ctx = make_ctx();
    let corp_id = 98_555_001_i64;
    let other_corp_id = 98_555_002_i64;
    let miner_id = 91_555_001_i64;
    let manager_id = 91_555_002_i64;
    let outsider_id = 91_555_003_i64;
    let rorq_id = 105_555_001_i64;
    let core_mod_id = 105_555_002_i64;
    let comp_mod_id = 105_555_003_i64;
    let ore_ice_hold = 105_555_010_i64;
    let ore_cargo = 105_555_011_i64;
    let ore_zero = 105_555_012_i64;
    let structure_id = 1_066_555_001_i64;
    let empty_structure_id = 1_066_555_002_i64;
    let foreign_structure_id = 1_066_555_003_i64;
    let reproc_mod_id = 105_555_020_i64;
    let struct_ore_id = 105_555_021_i64;
    let struct_gas_id = 105_555_022_i64;
    let sys_id = 30_000_142_u32;

    let now = ctx.now_unix();
    let mk_item = |item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32| {
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    };
    let mk_struct = |sid: i64, owner: i64, name: &str| UpwellStructureRecord {
        structure_id: sid,
        type_id: 35836, // Tatara
        owner_corp_id: owner,
        solar_system_id: sys_id,
        name: name.into(),
        position: [1_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 100_000.0,
        armor_hp: 100_000.0,
        hull_hp: 100_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    };

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (cid, station, ship) in [
            (miner_id, None, rorq_id),
            (manager_id, Some(structure_id), 105_555_099),
            (outsider_id, Some(structure_id), 105_555_098),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = format!("Miner{cid}");
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = station;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship;
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "MiningLedgerCorp".into();
        corp.ticker = "MLC".into();
        corp.ceo_id = 91_555_999;
        corp.members = [miner_id, manager_id, outsider_id, 91_555_999].into_iter().collect();
        corp.member_roles.insert(manager_id, ROLE_STATION_MANAGER);
        corp.member_roles.insert(miner_id, ROLE_ACCOUNTANT);
        corp.member_roles.insert(outsider_id, ROLE_TRADER);
        snap.corporations.insert(corp_id, corp);

        // Rorqual + Industrial Core I + Asteroid Ore Compressor I active
        snap.items.insert(rorq_id, mk_item(rorq_id, 28352, miner_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(core_mod_id, mk_item(core_mod_id, 28583, miner_id, rorq_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(comp_mod_id, mk_item(comp_mod_id, 62454, miner_id, rorq_id, InventoryFlag::HiSlot1, 1));
        snap.module_active_states.insert(core_mod_id);
        snap.module_active_states.insert(comp_mod_id);

        snap.items.insert(ore_ice_hold, mk_item(ore_ice_hold, 1230, miner_id, rorq_id, InventoryFlag::SpecializedIceHold, 250));
        snap.items.insert(ore_cargo, mk_item(ore_cargo, 1230, miner_id, rorq_id, InventoryFlag::Cargo, 100));
        snap.items.insert(ore_zero, mk_item(ore_zero, 1230, miner_id, rorq_id, InventoryFlag::SpecializedOreHold, 0));

        // Structure compression fixtures (1-arg vs 2-arg)
        snap.structures.insert(structure_id, mk_struct(structure_id, corp_id, "Moon Tatara"));
        snap.structures.insert(empty_structure_id, mk_struct(empty_structure_id, corp_id, "Empty Moon"));
        snap.structures.insert(foreign_structure_id, mk_struct(foreign_structure_id, other_corp_id, "Foreign Moon"));
        snap.items.insert(reproc_mod_id, mk_item(reproc_mod_id, 35899, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1));
        snap.module_online_states.insert(reproc_mod_id, true);
        snap.items.insert(struct_ore_id, mk_item(struct_ore_id, 1230, manager_id, structure_id, InventoryFlag::Hangar, 50));
        // Compressed Amber Mykoserocin (62377 -> 28694) in hangar, multiple units so DecompressGasInStructure has 2-arg & 1-arg targets
        snap.items.insert(struct_gas_id, mk_item(struct_gas_id, 62377, manager_id, structure_id, InventoryFlag::Hangar, 10));

        // Observer ledger entries: own non-empty, foreign non-empty (ignored), expired + 2 aggregated + timestamp==0 fallback
        snap.observer_mining_ledger.insert(empty_structure_id, Vec::new());
        snap.observer_mining_ledger.insert(
            foreign_structure_id,
            vec![ObserverMiningRecord {
                observer_id: foreign_structure_id,
                character_id: outsider_id,
                recorded_corporation_id: other_corp_id,
                type_id: 1230,
                quantity: 99,
                last_updated_date: "2026-11-10".into(),
                timestamp_unix: now.saturating_sub(100),
            }],
        );
        let old_ts = now.saturating_sub(91 * 86_400);
        let mid_ts = now.saturating_sub(3_600);
        snap.observer_mining_ledger.insert(
            structure_id,
            vec![
                ObserverMiningRecord {
                    observer_id: structure_id,
                    character_id: miner_id,
                    recorded_corporation_id: corp_id,
                    type_id: 1230,
                    quantity: 500,
                    last_updated_date: "2025-01-01".into(),
                    timestamp_unix: old_ts.max(1),
                },
                ObserverMiningRecord {
                    observer_id: structure_id,
                    character_id: miner_id,
                    recorded_corporation_id: corp_id,
                    type_id: 1230,
                    quantity: 120,
                    last_updated_date: "2026-11-10".into(),
                    timestamp_unix: 0, // fallback to parse_date_string_to_unix_secs
                },
                ObserverMiningRecord {
                    observer_id: structure_id,
                    character_id: manager_id,
                    recorded_corporation_id: corp_id,
                    type_id: 1230,
                    quantity: 100,
                    last_updated_date: "2026-11-10".into(),
                    timestamp_unix: mid_ts.saturating_sub(100),
                },
                ObserverMiningRecord {
                    observer_id: structure_id,
                    character_id: manager_id,
                    recorded_corporation_id: corp_id,
                    type_id: 1230,
                    quantity: 250,
                    last_updated_date: "2026-11-10".into(),
                    timestamp_unix: mid_ts,
                },
            ],
        );

        // Personal character mining ledger entries: old (skipped), timestamp==0 fallback, 2 aggregated with waste
        let push_char_entry = |snap: &mut eve_db::WorldStateSnapshot, rec: MiningLedgerRecord, waste: i64| {
            let idx = snap.mining_ledger.len();
            snap.mining_ledger.push(rec);
            snap.mining_and_ess_state.mining_ledger_waste.insert(idx, waste);
        };
        push_char_entry(
            snap,
            MiningLedgerRecord {
                character_id: miner_id,
                solar_system_id: sys_id,
                type_id: 1230,
                quantity: 999,
                date: "2025-01-01".into(),
                timestamp_unix: old_ts.max(1),
            },
            77,
        );
        push_char_entry(
            snap,
            MiningLedgerRecord {
                character_id: miner_id,
                solar_system_id: sys_id,
                type_id: 1230,
                quantity: 100,
                date: "2026-11-10".into(),
                timestamp_unix: mid_ts.saturating_sub(50),
            },
            7,
        );
        push_char_entry(
            snap,
            MiningLedgerRecord {
                character_id: miner_id,
                solar_system_id: sys_id,
                type_id: 1230,
                quantity: 250,
                date: "2026-11-10".into(),
                timestamp_unix: mid_ts,
            },
            13,
        );
        push_char_entry(
            snap,
            MiningLedgerRecord {
                character_id: miner_id,
                solar_system_id: sys_id,
                type_id: 25268,
                quantity: 40,
                date: "2026-11-10".into(),
                timestamp_unix: 0, // fallback to parse_date_string_to_unix_secs("2026-11-10")
            },
            3,
        );
    });

    // 1. In-space compression checks & success in SpecializedIceHold
    let mut space_sess = SessionState::default();
    space_sess.user_id = Some(1);
    space_sess.character_id = Some(miner_id);
    space_sess.corporation_id = Some(corp_id);
    space_sess.solar_system_id = Some(sys_id);
    space_sess.ship_id = Some(rorq_id);

    assert_eq!(
        handle_in_space_compression_mgr(&ctx, &mut space_sess, &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_in_space_compression_mgr(&ctx, &mut space_sess, &[PyRep::Integer(ore_cargo)])
            .await
            .unwrap_err()
            .code,
        "NotInMiningHold"
    );
    assert_eq!(
        handle_in_space_compression_mgr(&ctx, &mut space_sess, &[PyRep::Integer(ore_zero)])
            .await
            .unwrap_err()
            .code,
        "CompressionFailed"
    );

    let comp_rep =
        handle_in_space_compression_mgr(&ctx, &mut space_sess, &[PyRep::Integer(ore_ice_hold)])
            .await
            .expect("in-space compression in Ice hold succeeds");
    assert_eq!(comp_rep.dict_get("compressedTypeID").and_then(|v| v.as_i64()), Some(62516));
    assert_eq!(comp_rep.dict_get("compressedUnits").and_then(|v| v.as_i64()), Some(250));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ore_ice_hold].type_id, 62516);
        assert_eq!(snap.items[&ore_ice_hold].version, 2);
    });

    // Docked character rejects in-space compression
    let mut docked_sess = space_sess.clone();
    docked_sess.structure_id = Some(structure_id);
    assert_eq!(
        handle_in_space_compression_mgr(&ctx, &mut docked_sess, &[PyRep::Integer(ore_ice_hold)])
            .await
            .unwrap_err()
            .code,
        "MustBeInSpace"
    );

    // 2. `handle_structure_compression_mgr`: wrong structure_id (2-arg), 1-arg compress, 1-arg & 2-arg decompress
    let mut struct_sess = SessionState::default();
    struct_sess.user_id = Some(1);
    struct_sess.character_id = Some(manager_id);
    struct_sess.corporation_id = Some(corp_id);
    struct_sess.structure_id = Some(structure_id);
    struct_sess.solar_system_id = Some(sys_id);

    let kw = HashMap::new();
    let bad_compress = handle_compression(
        &ctx,
        &mut struct_sess,
        "structureCompressionMgr",
        "CompressItemInStructure",
        &[PyRep::Integer(foreign_structure_id), PyRep::Integer(struct_ore_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_compress.code, "InvalidStructure");

    // 1-arg CompressItemInStructure ([itemID])
    let ok_1arg_comp = handle_compression(
        &ctx,
        &mut struct_sess,
        "structureCompressionMgr",
        "CompressItemInStructure",
        &[PyRep::Integer(struct_ore_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("1-arg CompressItemInStructure succeeds");
    assert_eq!(
        ok_1arg_comp.dict_get("compressedTypeID").and_then(|v| v.as_i64()),
        Some(62516)
    );

    let bad_decomp = handle_compression(
        &ctx,
        &mut struct_sess,
        "structureCompressionMgr",
        "DecompressGasInStructure",
        &[PyRep::Integer(foreign_structure_id), PyRep::Integer(struct_gas_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_decomp.code, "InvalidStructure");

    // 1-arg DecompressGasInStructure ([itemID])
    let ok_1arg_decomp = handle_compression(
        &ctx,
        &mut struct_sess,
        "structureCompressionMgr",
        "DecompressGasInStructure",
        &[PyRep::Integer(struct_gas_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("1-arg DecompressGasInStructure succeeds");
    assert_eq!(
        ok_1arg_decomp.dict_get("decompressedTypeID").and_then(|v| v.as_i64()),
        Some(28694)
    );

    // 3. `corpMiningLedger.GetObserversWithMiningEvents` & `GetObserverLedger`
    let mut outsider_sess = SessionState::default();
    outsider_sess.user_id = Some(1);
    outsider_sess.character_id = Some(outsider_id);
    outsider_sess.corporation_id = Some(corp_id);
    assert_eq!(
        handle_compression(
            &ctx,
            &mut outsider_sess,
            "corpMiningLedger",
            "GetObserversWithMiningEvents",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_compression(
            &ctx,
            &mut outsider_sess,
            "corpMiningLedger",
            "GetObserverLedger",
            &[PyRep::Integer(structure_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    let obs_rep = handle_compression(
        &ctx,
        &mut struct_sess,
        "corpMiningLedger",
        "GetObserversWithMiningEvents",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("station manager sees observers");
    let obs_rows = obs_rep.crowset_rows().expect("observers crowset");
    assert_eq!(obs_rows.len(), 1, "only non-empty own-corp observer must be returned");
    assert_eq!(obs_rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(structure_id)));

    // Accountant (miner_id) fetches GetObserverLedger
    let ledger_rep = handle_compression(
        &ctx,
        &mut space_sess,
        "corpMiningLedger",
        "GetObserverLedger",
        &[PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("accountant sees observer ledger");
    let ledger_rows = ledger_rep.crowset_rows().expect("ledger crowset");
    assert_eq!(ledger_rows.len(), 2, "old entry >90d ago must be filtered out; 2 rows remain");

    // Check aggregated manager row (`100 + 250 = 350`, `eventDate = filetime(mid_ts)`)
    // and miner row (`raw_ts == 0 -> filetime(parse_date_string_to_unix_secs("2026-11-10"))`)
    let expected_fallback_ft = crate::sp6_lifecycle::unix_to_filetime(1_794_268_800);
    let expected_mid_ft = crate::sp6_lifecycle::unix_to_filetime(now.saturating_sub(3_600));
    let mut found_miner = false;
    let mut found_manager = false;
    for row in ledger_rows {
        let cid = row.dbrow_get("characterID").and_then(|v| v.as_i64()).unwrap_or(0);
        let qty = row.dbrow_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
        let ft = row.dbrow_get("eventDate").and_then(|v| v.as_i64()).unwrap_or(0);
        if cid == miner_id {
            found_miner = true;
            assert_eq!(qty, 120);
            assert_eq!(ft, expected_fallback_ft);
        } else if cid == manager_id {
            found_manager = true;
            assert_eq!(qty, 350);
            assert_eq!(ft, expected_mid_ft);
        }
    }
    assert!(found_miner && found_manager);

    // 4. `characterMiningLedger.GetCharacterLogs`
    let char_logs = handle_compression(
        &ctx,
        &mut space_sess,
        "characterMiningLedger",
        "GetCharacterLogs",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetCharacterLogs succeeds");
    let PyRep::List(ref log_items) = char_logs else {
        panic!("expected list of character mining logs");
    };
    assert_eq!(log_items.len(), 2, "old entry skipped, remaining aggregated into 2 entries");
    for entry in log_items {
        let tid = entry.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0);
        let qty = entry.dict_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
        let waste = entry.dict_get("quantityWasted").and_then(|v| v.as_i64()).unwrap_or(0);
        let ft = entry.dict_get("eventDate").and_then(|v| v.as_i64()).unwrap_or(0);
        if tid == 1230 {
            assert_eq!(qty, 350);
            assert_eq!(waste, 20);
            assert_eq!(ft, expected_mid_ft);
        } else if tid == 25268 {
            assert_eq!(qty, 40);
            assert_eq!(waste, 3);
            assert_eq!(ft, expected_fallback_ft);
        } else {
            panic!("unexpected typeID {tid}");
        }
    }
}

#[cfg(test)]
#[path = "mining_compression_extra_unit_tests.rs"]
mod extra_unit_tests;
