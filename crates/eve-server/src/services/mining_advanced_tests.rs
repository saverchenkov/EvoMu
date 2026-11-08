use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    portion_size: u64,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume,
        capacity,
        radius: 10.0,
        base_price: 100.0,
        published: true,
        portion_size,
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
    }
}

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Refinery hulls: Athanor (35835), Metenox Moon Drill (81826, group 4744)
    sde.types
        .entry(35835)
        .or_insert_with(|| make_inv_type(35835, 1406, 65, "Athanor", 10_000.0, 0.0, 1));
    sde.types.insert(
        81826,
        make_inv_type(81826, 4744, 65, "Metenox Moon Drill", 10_000.0, 480_000.0, 1),
    );
    sde.types
        .entry(35894)
        .or_insert_with(|| make_inv_type(35894, 1322, 66, "Standup Moon Drill I", 100.0, 0.0, 1));

    // Moon ores (45490, 45491, 45492, 45494) + improved & jackpot variants
    for (tid, gid, name) in [
        (45490, 1884, "Sylvite"),
        (46282, 1884, "Brimful Sylvite"),
        (46283, 1884, "Glistening Sylvite"),
        (45491, 1884, "Bitumens"),
        (45492, 1920, "Cobaltite"),
        (45494, 1921, "Vanadinite"),
    ] {
        sde.types
            .insert(tid, make_inv_type(tid, gid, 25, name, 10.0, 0.0, 100));
        sde.type_materials.insert(
            tid,
            vec![TypeMaterialEntry {
                material_type_id: 16634,
                quantity: 65,
            }],
        );
    }
    // Moon material (16634 Atmospheric Gases, group 427)
    sde.types
        .insert(16634, make_inv_type(16634, 427, 4, "Atmospheric Gases", 0.05, 0.0, 1));
    // Fuel Block (4247, group 1136) and Magmatic Gas (81143)
    sde.types
        .insert(4247, make_inv_type(4247, 1136, 4, "Helium Fuel Block", 5.0, 0.0, 1));
    sde.types
        .insert(81143, make_inv_type(81143, 427, 4, "Magmatic Gas", 0.5, 0.0, 1));

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_moon_helpers_and_variants() {
    let ctx = make_ctx();
    let sys_id = 30_002_537;
    let moons = ctx.sde.celestials_in_system(sys_id);
    let moon = moons.iter().find(|c| c.group_id == 8).expect("Amamake has moons");
    let pos = [moon.position.x + 1_000.0, moon.position.y, moon.position.z];
    assert_eq!(
        resolve_structure_moon_id(&ctx.sde, sys_id, pos),
        Some(moon.celestial_id)
    );
    let ores = derive_moon_ores_from_sde(&ctx.sde, sys_id, moon.celestial_id);
    assert_eq!(ores.len(), 3);
    let sum: f64 = ores.iter().map(|(_, f)| *f).sum();
    assert!((sum - 1.0).abs() < 1e-6);

    assert_eq!(find_improved_variant(&ctx.sde, 45490), 46282);
    assert_eq!(find_jackpot_variant(&ctx.sde, 45490), 46283);
}

#[tokio::test]
async fn test_moon_extraction_lifecycle_and_tick() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let struct_id = 1_000_000_000_400;
    let sys_id = 30_002_537;
    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Moon Corp".into(),
                ticker: "MOON".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Moon Athanor".into(),
                state: "ShieldVulnerable".into(),
                position: [moon.position.x + 500.0, moon.position.y, moon.position.z],
                quantum_core_installed: true,
                fuel_blocks: 1000,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.structure_id = Some(struct_id);

    // Invalid duration (< 6 days)
    let bad_dur = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id), PyRep::Integer(3600)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_dur.code, "InvalidExtractionDuration");

    // Start valid 6-day extraction
    let started = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id), PyRep::Integer(518_400)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should start moon extraction");
    assert_eq!(
        started.dict_get("status").and_then(|v| v.as_str()),
        Some("Started")
    );

    // Query resources and corp extractions
    let res_list = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "GetMoonResourcesForStructure",
        &[PyRep::Integer(struct_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(res_list, PyRep::List(v) if v.len() == 3));

    let corp_exts = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "GetExtractionsForCorp",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(corp_exts, PyRep::List(v) if v.len() == 1));

    // Fast-forward chunk_arrival_unix so FractureChunkWithStructure succeeds
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ext) = snap.moon_extractions.get_mut(&struct_id) {
            ext.extraction_start_unix = now - 600_000;
            ext.chunk_arrival_unix = now - 10;
            ext.natural_decay_unix = now + 172_800;
        }
    });

    let fractured = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should fracture arrived chunk");
    assert_eq!(
        fractured.dict_get("status").and_then(|v| v.as_str()),
        Some("Fractured")
    );
    assert!(fractured.dict_get("asteroidCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 3);

    // Advance decay timestamp and run tick_all_moon_extractions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ext) = snap.moon_extractions.get_mut(&struct_id) {
            ext.natural_decay_unix = now - 1;
        }
    });
    tick_all_moon_extractions(&ctx).await;
    let status_after = ctx
        .db
        .with_snapshot(|snap| snap.moon_extractions[&struct_id].status.clone());
    assert_eq!(status_after, "Decayed");
}

#[tokio::test]
async fn test_auto_moon_mining_details_output_and_hourly_tick() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let metenox_id = 1_000_000_000_500;
    let sys_id = 30_002_537;
    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Moon Corp".into(),
                ticker: "MOON".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
        );
        snap.structures.insert(
            metenox_id,
            UpwellStructureRecord {
                structure_id: metenox_id,
                type_id: 81826,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Metenox Drill".into(),
                state: "ShieldVulnerable".into(),
                position: [moon.position.x + 200.0, moon.position.y, moon.position.z],
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            950_001,
            InventoryItemRecord {
                item_id: 950_001,
                type_id: 81143, // Magmatic Gas
                owner_id: corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);

    let details = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return mining details");
    assert_eq!(
        details.dict_get("status").and_then(|v| v.as_str()),
        Some("Active")
    );

    let cycle_out = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(metenox_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return cycle output");
    assert!(matches!(cycle_out, PyRep::List(v) if !v.is_empty()));

    let now = ctx.now_unix();
    tick_all_metenox_moon_drills(&ctx, now - 7_200, now);
    let harvested = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert!(harvested > 0);
}

#[tokio::test]
async fn test_metenox_fuel_no_double_count_disposal_sync_and_reinforce_pause() {
    let mut ctx = make_ctx();
    Arc::make_mut(&mut ctx.sde).types.insert(
        4051,
        make_inv_type(4051, 1136, 4, "Nitrogen Fuel Block", 5.0, 0.0, 1),
    );
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let metenox_id = 1_000_000_000_599;
    let sys_id = 30_002_537;
    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    let fuel_item_id = 950_101i64;
    let gas_item_id = 950_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Moon Corp".into(),
                ticker: "MOON".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
        );
        // Both st.fuel_blocks = 10 and item_fuel = 10 (synced bay): effective total must be 10, NOT 20.
        snap.structures.insert(
            metenox_id,
            UpwellStructureRecord {
                structure_id: metenox_id,
                type_id: 81826,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Metenox Sync Test".into(),
                state: "ShieldVulnerable".into(),
                position: [moon.position.x + 200.0, moon.position.y, moon.position.z],
                quantum_core_installed: true,
                fuel_blocks: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_item_id,
            InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: 4051,
                owner_id: corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            gas_item_id,
            InventoryItemRecord {
                item_id: gas_item_id,
                type_id: 81143,
                owner_id: corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        ..Default::default()
    };
    let details = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("mining details");
    assert_eq!(
        details.dict_get("fuelRemaining").and_then(|v| v.as_i64()),
        Some(10),
        "inspect_metenox_drill must not double-count st.fuel_blocks + item_fuel"
    );

    // Run 1 hourly cycle (consumes 5 fuel blocks) -> item_fuel becomes 5 and st.fuel_blocks must sync to 5.
    let now = ctx.now_unix();
    tick_all_metenox_moon_drills(&ctx, now - 3_600, now);
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.items.get(&fuel_item_id).map(|i| i.quantity), Some(5));
        assert_eq!(snap.structures.get(&metenox_id).map(|s| s.fuel_blocks), Some(5));
        assert_eq!(
            snap.auto_moon_miners.get(&metenox_id).map(|r| r.fuel_units_remaining),
            Some(5),
            "auto_moon_miners must reflect 5 remaining (not 15)"
        );
        // Materializing structure fuel for disposal must NOT mint extra unbacked blocks.
        let before_count = snap
            .items
            .values()
            .filter(|i| i.location_id == metenox_id && i.type_id == 4051)
            .map(|i| i.quantity)
            .sum::<i32>();
        crate::services::structure_acl_defense_rules::materialize_structure_fuel_for_disposal(
            &ctx,
            snap,
            metenox_id,
        );
        let after_count = snap
            .items
            .values()
            .filter(|i| i.location_id == metenox_id && i.type_id == 4051)
            .map(|i| i.quantity)
            .sum::<i32>();
        assert_eq!(before_count, 5);
        assert_eq!(after_count, 5, "stale st.fuel_blocks must not mint unbacked fuel on disposal");
    });

    // Put Metenox into ArmorReinforce / ArmorReinforced / HullReinforce / HullReinforced: drilling must pause.
    let harvest_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    for rf_state in ["ArmorReinforce", "ArmorReinforced", "HullReinforce", "HullReinforced"] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&metenox_id).unwrap().state = rf_state.into();
        });
        tick_all_metenox_moon_drills(&ctx, now, now + 7_200);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items.get(&fuel_item_id).map(|i| i.quantity), Some(5), "no fuel burned in {rf_state}");
            let harvest_now = snap
                .items
                .values()
                .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
                .map(|i| i.quantity)
                .sum::<i32>();
            assert_eq!(harvest_now, harvest_before, "no harvest in {rf_state}");
        });
    }
}
