use crate::router::ServerContext;
use crate::services::mining_advanced::{tick_all_metenox_moon_drills, tick_all_moon_extractions};
use eve_db::{
    AutoMoonMinerRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MoonExtractionRecord, NeuralAttributes, ServerClock, UpwellStructureRecord, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{CategoryId, EffectId, GroupId, InvType, SdeRegistry, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

fn mk_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    volume: f64,
    capacity: f64,
    default_effect_id: Option<EffectId>,
    attrs: &[(u16, f64)],
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000.0,
        volume,
        capacity,
        radius: 100.0,
        base_price: 100.0,
        published: true,
        portion_size: 100,
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
        default_effect_id,
        description: String::new(),
        effect_ids: default_effect_id.into_iter().collect(),
        default_attributes: attrs.iter().copied().collect(),
    }
}

fn build_moon_test_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types
        .insert(35835, mk_type(35835, 1406, 65, "Athanor", 500_000.0, 100_000.0, None, &[]));
    sde.types
        .insert(35894, mk_type(35894, 1415, 66, "Standup Moon Drill I", 4_000.0, 0.0, None, &[]));
    sde.types.insert(
        37178,
        mk_type(37178, 1912, 66, "Standup L Moon Drill Stability I", 500.0, 0.0, None, &[(2707, 100.0)]),
    );
    sde.types.insert(
        81826,
        mk_type(81826, 4744, 65, "Metenox Moon Drill", 500_000.0, 480_000.0, None, &[]),
    );
    sde.types
        .insert(4051, mk_type(4051, 1136, 4, "Nitrogen Fuel Block", 5.0, 0.0, None, &[]));
    sde.types
        .insert(81143, mk_type(81143, 711, 2, "Magmatic Gas", 0.5, 0.0, None, &[]));
    for (tid, name) in [
        (45490, "Sylvite"),
        (45491, "Bitumens"),
        (45492, "Zeolites"),
        (45493, "Coesite"),
    ] {
        sde.types.insert(tid, mk_type(tid, 1884, 25, name, 10.0, 0.0, None, &[]));
    }
    sde.types
        .insert(16634, mk_type(16634, 427, 4, "Atmospheric Gases", 0.05, 0.0, None, &[]));
    Arc::new(sde)
}

#[tokio::test]
async fn test_moon_extraction_validation_fracture_decay_and_auto_fracture() {
    let sde = build_moon_test_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(10_000));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(clock.clone());
    let char_id = 90_005_001i64;
    let corp_id = 98_005_001i64;
    let struct_id = 1_050_000_001i64;
    let sys_id = 30_002_537u32;
    let moon = sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("Amamake moon in canonical SDE");
    let moon_pos = [moon.position.x + 1_000.0, moon.position.y, moon.position.z];

    db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: 5,
                name: "MoonManager".into(),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: Some(struct_id),
                solar_system_id: sys_id,
                constellation_id: 20_000_001,
                region_id: 10_000_001,
                active_ship_id: 1_000_001,
                balance: IskCents::default(),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 5.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes {
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    charisma: 19,
                    bonus_remaps: 0,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
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
                position: moon_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(5);
    sess.user_id = Some(5);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let too_short = crate::services::mining_advanced::handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id), PyRep::Integer(10_000 + 518_399)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(too_short.code, "InvalidExtractionDuration");

    let started = crate::services::mining_advanced::handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id), PyRep::Integer(10_000 + 518_400)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("start extraction");
    assert_eq!(started.dict_get("status").and_then(|v| v.as_str()), Some("Started"));

    let early_frac = crate::services::mining_advanced::handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(early_frac.code, "ChunkNotReadyToFracture");

    clock.advance_secs(518_400);
    let fractured = crate::services::mining_advanced::handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("fracture ok");
    assert_eq!(fractured.dict_get("status").and_then(|v| v.as_str()), Some("Fractured"));
    assert!(db.with_snapshot(|s| !s.moon_extractions[&struct_id].spawned_asteroid_ids.is_empty()));

    clock.advance_secs(172_800);
    tick_all_moon_extractions(&ctx).await;
    db.with_snapshot(|s| {
        let ext = &s.moon_extractions[&struct_id];
        assert_eq!(ext.status, "Decayed");
        assert!(ext.spawned_asteroid_ids.is_empty());
    });

    // Rig stability modifier (2707 = +100% => 21_600s auto-fracture delay)
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            99_887_766,
            InventoryItemRecord {
                item_id: 99_887_766,
                type_id: 37178,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let now = 10_000 + 518_400 + 172_800;
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: moon.celestial_id,
                extraction_start_unix: now - 518_400,
                chunk_arrival_unix: now,
                natural_decay_unix: now + 172_800,
                status: "Ready".into(),
                auto_fracture: false,
                event_id: 500,
                moon_ores: vec![(45492, 1.0)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
    });
    clock.advance_secs(10_800);
    tick_all_moon_extractions(&ctx).await;
    db.with_snapshot(|s| assert_eq!(s.moon_extractions[&struct_id].status, "Ready"));

    clock.advance_secs(10_800);
    tick_all_moon_extractions(&ctx).await;
    db.with_snapshot(|s| {
        let ext = &s.moon_extractions[&struct_id];
        assert_eq!(ext.status, "Fractured");
        assert!(ext.auto_fracture);
    });
}

#[test]
fn test_metenox_moon_drill_active_out_of_gas_bay_full_and_out_of_fuel() {
    let sde = build_moon_test_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(20_000));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(clock);
    let drill_id = 1_060_000_001i64;
    let corp_id = 98_006_001i64;
    let sys_id = 30_002_537u32;
    let moon = sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("Amamake moon in canonical SDE");
    let drill_pos = [moon.position.x + 1_000.0, moon.position.y, moon.position.z];

    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            drill_id,
            UpwellStructureRecord {
                structure_id: drill_id,
                type_id: 81826,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Metenox-1".into(),
                position: drill_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 81143,
                owner_id: corp_id,
                location_id: drill_id,
                flag: InventoryFlag::Cargo,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.auto_moon_miners.insert(
            drill_id,
            AutoMoonMinerRecord {
                structure_id: drill_id,
                fuel_units_remaining: 500,
                magmatic_gas_remaining: 1_000,
                efficiency: 0.40,
                cycle_duration_seconds: 3_600,
                last_cycle_unix: 20_000,
                ore_type_id: 45492,
                hourly_yield: 0,
                status: "Active".into(),
            },
        );
    });

    tick_all_metenox_moon_drills(&ctx, 20_000, 23_600);
    db.with_snapshot(|snap| {
        let am = &snap.auto_moon_miners[&drill_id];
        assert_eq!(am.status, "Active");
        assert!(am.fuel_units_remaining < 500);
        assert!(am.magmatic_gas_remaining < 1_000);
    });

    db.with_mut_snapshot(|snap| snap.items.get_mut(&700_001).unwrap().quantity = 1);
    tick_all_metenox_moon_drills(&ctx, 23_600, 27_200);
    db.with_snapshot(|snap| assert_eq!(snap.auto_moon_miners[&drill_id].status, "OutOfMagmaticGas"));

    db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&700_001).unwrap().quantity = 500;
        snap.items.insert(
            700_002,
            InventoryItemRecord {
                item_id: 700_002,
                type_id: 45492,
                owner_id: corp_id,
                location_id: drill_id,
                flag: InventoryFlag::SpecializedMineralHold,
                quantity: 48_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    tick_all_metenox_moon_drills(&ctx, 27_200, 30_800);
    db.with_snapshot(|snap| assert_eq!(snap.auto_moon_miners[&drill_id].status, "OutputBayFull"));

    db.with_mut_snapshot(|snap| {
        snap.items.remove(&700_002);
        snap.structures.get_mut(&drill_id).unwrap().fuel_blocks = 0;
    });
    tick_all_metenox_moon_drills(&ctx, 30_800, 34_400);
    db.with_snapshot(|snap| assert_eq!(snap.auto_moon_miners[&drill_id].status, "OutOfFuel"));
}
