//! Unit and mutation-killing tests for `sqlite_loader_skins`.

use super::*;

fn sample_inv_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    published: bool,
    market_group_id: Option<u32>,
    meta_group_id: Option<u32>,
    attrs: &[(AttributeId, f64)],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type{type_id}"),
        mass: 1.0,
        volume: 1.0,
        capacity: 0.0,
        radius: 1.0,
        base_price: 0.0,
        published,
        portion_size: 1,
        market_group_id,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

#[test]
fn skin_tables_and_on_demand_sqlite_celestial_queries() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sde_skin_licenses (license_type_id INTEGER, skin_id INTEGER, duration INTEGER);
         INSERT INTO sde_skin_licenses VALUES (40001, 10, 30);
         CREATE TABLE sde_skins (
             skin_id INTEGER, internal_name TEXT, skin_material_id INTEGER, types_json TEXT,
             allow_ccp_devs INTEGER, visible_serenity INTEGER, visible_tranquility INTEGER,
             is_structure_skin INTEGER, skin_description TEXT
         );
         INSERT INTO sde_skins VALUES (10, 'TestSkin', 5, '[587, 588]', 1, 0, 1, 0, 'Desc');",
    )
    .unwrap();

    // Without sde_skin_materials table
    let (lic, mat_empty, skins) = load_skin_tables(&conn).unwrap();
    assert_eq!(lic[&40001].skin_id, 10);
    assert_eq!(lic[&40001].duration, 30);
    assert!(mat_empty.is_empty());
    assert_eq!(skins[&10].types, vec![587, 588]);
    assert!(skins[&10].allow_ccp_devs);
    assert!(!skins[&10].visible_serenity);
    assert!(skins[&10].visible_tranquility);
    assert!(!skins[&10].is_structure_skin);
    assert_eq!(skins[&10].skin_description.as_deref(), Some("Desc"));

    // With sde_skin_materials table
    conn.execute_batch(
        "CREATE TABLE sde_skin_materials (skin_material_id INTEGER, display_name_id INTEGER, material_set_id INTEGER);
         INSERT INTO sde_skin_materials VALUES (5, 1001, 2002);
         INSERT INTO sde_skin_materials VALUES (6, NULL, NULL);",
    )
    .unwrap();
    let (_, mats, _) = load_skin_tables(&conn).unwrap();
    assert_eq!(mats.len(), 2);
    assert_eq!(mats[&5].display_name_id, Some(1001));
    assert_eq!(mats[&5].material_set_id, Some(2002));
    assert_eq!(mats[&6].display_name_id, None);

    // Temp SQLite file for celestials_in_system, all_stars, location_solar_system, factions_owning_solar_systems
    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_test_skins_celestials_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp_path);
    {
        let disk_conn = Connection::open(&tmp_path).unwrap();
        disk_conn
            .execute_batch(
                "CREATE TABLE sde_celestials (
                    celestial_id INTEGER, system_id INTEGER, type_id INTEGER, group_id INTEGER,
                    name TEXT, radius REAL, x REAL, y REAL, z REAL,
                    orbit_id INTEGER, orbit_index INTEGER, celestial_index INTEGER
                );
                INSERT INTO sde_celestials VALUES (40000001, 30000142, 3802, 6, 'Jita - Star', 1000.0, 0.0, 0.0, 0.0, NULL, NULL, NULL);
                INSERT INTO sde_celestials VALUES (40000002, 30000142, 11, 7, 'Jita I', 500.0, 10.0, 20.0, 30.0, 40000001, 1, 1);
                CREATE TABLE sde_solar_systems (solar_system_id INTEGER, faction_id INTEGER);
                INSERT INTO sde_solar_systems VALUES (30000142, 500001);
                INSERT INTO sde_solar_systems VALUES (30002187, 500003);
                INSERT INTO sde_solar_systems VALUES (31000001, NULL);",
            )
            .unwrap();
    }

    let mut sde = SdeRegistry::default();
    assert!(sde.celestials_in_system(30000142).is_empty());
    assert!(sde.all_stars().is_empty());
    assert!(sde.factions_owning_solar_systems().is_empty());
    assert_eq!(sde.location_solar_system(40000001), None);

    sde.stations.insert(
        60003760,
        StationInfo {
            station_id: 60003760,
            solar_system_id: 30000142,
            constellation_id: 20000020,
            region_id: 10000002,
            station_type_id: 1529,
            name: "Jita IV - Moon 4".to_string(),
            position: DVec3::ZERO,
            owner_corporation_id: 1000035,
            owner_faction_id: None,
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    assert_eq!(sde.location_solar_system(60003760), Some(30000142));

    sde.db_path = Some(tmp_path.clone());
    let cels = sde.celestials_in_system(30000142);
    assert_eq!(cels.len(), 2);
    assert_eq!(cels[0].celestial_id, 40000001);
    assert_eq!(cels[1].position, DVec3::new(10.0, 20.0, 30.0));
    assert_eq!(sde.all_stars(), vec![(40000001, 3802)]);
    assert_eq!(sde.location_solar_system(40000002), Some(30000142));
    assert_eq!(sde.location_solar_system(99999999), None);

    let loc_map = sde.location_solar_system_map();
    assert_eq!(loc_map.get(&60003760), Some(&30000142));
    assert_eq!(loc_map.get(&40000002), Some(&30000142));
    assert_eq!(sde.factions_owning_solar_systems(), vec![500001, 500003]);

    // Test cdn_resource_path
    assert!(sde.cdn_resource_path("res:/unindexed").is_err());
    sde.resources.insert(
        "res:/missing".to_string(),
        SdeResourceInfo {
            res_path: "res:/missing".to_string(),
            cdn_relative_path: "/nonexistent/evomu_missing_file.static".to_string(),
            md5: String::new(),
            byte_size: 0,
            format: String::new(),
            record_count: 0,
            spot: String::new(),
        },
    );
    assert!(sde.cdn_resource_path("res:/missing").is_err());
    sde.resources.insert(
        "res:/temp_sqlite".to_string(),
        SdeResourceInfo {
            res_path: "res:/temp_sqlite".to_string(),
            cdn_relative_path: tmp_path.display().to_string(),
            md5: String::new(),
            byte_size: 0,
            format: String::new(),
            record_count: 0,
            spot: String::new(),
        },
    );
    assert_eq!(sde.cdn_resource_path("res:/temp_sqlite").unwrap(), tmp_path);

    let _ = std::fs::remove_file(&tmp_path);
}

#[test]
fn dungeon_ores_blueprints_skills_index_and_fighter_abilities() {
    let mut sde = SdeRegistry::default();
    // Asteroid ore (cat 25, group 462) and Ice (cat 25, group 465)
    sde.types
        .insert(1230, sample_inv_type(1230, 462, 25, true, Some(20), Some(1), &[]));
    sde.types
        .insert(16262, sample_inv_type(16262, 465, 25, true, Some(20), None, &[]));
    sde.dungeon_asteroids.insert(
        100,
        vec![
            DungeonAsteroidEntry {
                room_id: 1,
                type_id: 1230,
                asteroid_count: 10,
            },
            DungeonAsteroidEntry {
                room_id: 2,
                type_id: 1230,
                asteroid_count: 5,
            },
            DungeonAsteroidEntry {
                room_id: 1,
                type_id: 16262,
                asteroid_count: 3,
            },
        ],
    );
    sde.dungeon_asteroids.insert(
        101,
        vec![DungeonAsteroidEntry {
            room_id: 1,
            type_id: 16262,
            asteroid_count: 2,
        }],
    );

    assert_eq!(sde.dungeon_ore_types(999), None);
    assert_eq!(sde.dungeon_ore_types(100), Some(vec![1230]));
    assert_eq!(sde.dungeon_ore_types(101), None);
    assert_eq!(sde.dungeon_ice_types(999), None);
    assert_eq!(sde.dungeon_ice_types(100), Some(vec![16262]));

    // Market group hierarchy: 20 -> 10 -> None
    sde.market_groups.insert(
        10,
        MarketGroupInfo {
            market_group_id: 10,
            parent_group_id: None,
            name: "Root".to_string(),
            has_types: false,
            icon_id: None,
        },
    );
    sde.market_groups.insert(
        20,
        MarketGroupInfo {
            market_group_id: 20,
            parent_group_id: Some(10),
            name: "Child".to_string(),
            has_types: true,
            icon_id: None,
        },
    );
    assert_eq!(sde.root_market_group_id(20), 10);

    // Ship with 6 required skill pairs + blueprint type (category 9, no market_group_id -> root_mg = 2)
    sde.types.insert(
        587,
        sample_inv_type(
            587,
            25,
            6,
            true,
            Some(20),
            Some(2),
            &[
                (182, 3330.0),
                (277, 1.0),
                (183, 3331.0),
                (278, 2.0),
                (184, 3332.0),
                (279, 3.0),
                (1285, 3333.0),
                (1286, 4.0),
                (1289, 3334.0),
                (1287, 5.0),
                (1290, 3335.0),
                (1288, 1.0),
            ],
        ),
    );
    sde.types
        .insert(688, sample_inv_type(688, 105, 9, true, None, None, &[]));
    sde.types
        .insert(689, sample_inv_type(689, 25, 6, false, Some(20), None, &[(182, 3330.0), (277, 1.0)]));

    let reqs = sde.required_skills_for_type(587);
    assert_eq!(reqs.len(), 6);
    assert_eq!(reqs[&3333], 4);
    assert!(sde.required_skills_for_type(99999).is_empty());

    sde.blueprints.insert(
        688,
        BlueprintInfo {
            blueprint_type_id: 688,
            product_type_id: 587,
            product_quantity: 1,
            manufacturing_time_sec: 600,
            max_production_limit: 10,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: Vec::new(),
            activities: HashMap::from([(
                "manufacturing".to_string(),
                BlueprintActivity {
                    time: 600,
                    materials: vec![BlueprintMaterial {
                        type_id: 34,
                        quantity: 1000,
                    }],
                    products: Vec::new(),
                    skills: vec![BlueprintSkill {
                        type_id: 3380,
                        level: 1,
                    }],
                },
            )]),
        },
    );

    let bp_by_mat = sde.blueprints_by_material_type();
    assert_eq!(bp_by_mat.get(&34), Some(&vec![688]));

    let idx = sde.required_skills_index();
    assert_eq!(idx[&3330][&1][&10][&2], vec![587]);
    assert_eq!(idx[&3380][&1][&2][&1], vec![688]);

    // Fighter ability slots join
    sde.fighter_abilities.insert(
        1,
        FighterAbilityInfo {
            ability_id: 1,
            target_mode: "pointTargeted".to_string(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: false,
            icon_id: None,
            display_name_id: None,
            ability_name_id: None,
            tooltip_text_id: None,
            turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
    sde.fighter_abilities_by_type.insert(
        23055,
        vec![
            FighterAbilitySlot {
                slot_index: 0,
                ability_id: 1,
                cooldown_seconds: Some(4.0),
                charges: None,
            },
            FighterAbilitySlot {
                slot_index: 1,
                ability_id: 999, // missing in fighter_abilities -> filtered out
                cooldown_seconds: None,
                charges: None,
            },
        ],
    );
    assert!(sde.fighter_ability_slots_with_info(999).is_empty());
    let fa_slots = sde.fighter_ability_slots_with_info(23055);
    assert_eq!(fa_slots.len(), 1);
    assert_eq!(fa_slots[0].1.target_mode, "pointTargeted");
}

#[test]
fn test_issue_426_default_required_skill_level_1_and_safe_fsd_helpers() {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Skill 3336 specified on attribute 182 with requiredSkill1Level (277) omitted -> defaults to 1
    sde.types.insert(
        77_001,
        sample_inv_type(77_001, 25, 6, true, None, None, &[(182, 3336.0)]),
    );
    let reqs = sde.required_skills_for_type(77_001);
    assert_eq!(reqs.get(&3336), Some(&1));

    // Truncated/underflow FSD dict buffers must never panic
    let truncated_fsd = [4_u8, 0, 0, 0, 1, 2]; // body_end - 4 = 2, footer_size large/corrupt
    assert!(crate::static_domain_loaders::fsd_dict_entries(&truncated_fsd, 0, 8).is_empty());
}
