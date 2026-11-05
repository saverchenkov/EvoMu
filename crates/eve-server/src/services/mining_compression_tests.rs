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
            CorporationRecord {
                corporation_id: corp_id,
                name: "Mining Corp".into(),
                ticker: "MINE".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
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
}
