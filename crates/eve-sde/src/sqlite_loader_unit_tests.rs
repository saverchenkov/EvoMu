//! Unit and mutation-killing tests for `sqlite_loader`.

use super::*;

fn create_base_sde_schema(conn: &Connection, include_rand_mats_col: bool) {
    let type_mats_ddl = if include_rand_mats_col {
        "CREATE TABLE sde_type_materials (type_id INTEGER, materials_json TEXT, randomized_materials_json TEXT);"
    } else {
        "CREATE TABLE sde_type_materials (type_id INTEGER, materials_json TEXT);"
    };
    conn.execute_batch(&format!(
        "CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT);
         INSERT INTO sde_meta VALUES ('schema_version', '1');
         CREATE TABLE sde_resources (res_path TEXT, cdn_relative_path TEXT, md5 TEXT, byte_size INTEGER, format TEXT, record_count INTEGER, spot TEXT);
         CREATE TABLE sde_categories (category_id INTEGER, name TEXT, published INTEGER);
         CREATE TABLE sde_groups (group_id INTEGER, category_id INTEGER, name TEXT, anchorable INTEGER, anchored INTEGER, fittable_non_singleton INTEGER, use_base_price INTEGER, published INTEGER);
         CREATE TABLE sde_types (type_id INTEGER, group_id INTEGER, category_id INTEGER, name TEXT, mass REAL, volume REAL, capacity REAL, portion_size INTEGER, base_price REAL, radius REAL, market_group_id INTEGER, race_id INTEGER, faction_id INTEGER, variation_parent_type_id INTEGER, wreck_type_id INTEGER, published INTEGER, attributes_json TEXT, effects_json TEXT, description_id INTEGER, icon_id INTEGER, graphic_id INTEGER, sound_id INTEGER, meta_group_id INTEGER, meta_level INTEGER, tech_level INTEGER, default_effect_id INTEGER, description TEXT);
         CREATE TABLE sde_dogma_attributes (attribute_id INTEGER, name TEXT, default_value REAL, high_is_good INTEGER, stackable INTEGER, published INTEGER);
         CREATE TABLE sde_dogma_effects (effect_id INTEGER, name TEXT, effect_category INTEGER, is_offensive INTEGER, is_assistance INTEGER, is_warp_safe INTEGER, disallow_auto_repeat INTEGER, electronic_chance INTEGER, propulsion_chance INTEGER, range_chance INTEGER, guid TEXT, published INTEGER, modifiers_json TEXT);
         {type_mats_ddl}
         CREATE TABLE sde_blueprints (blueprint_type_id INTEGER, product_type_id INTEGER, product_quantity INTEGER, manufacturing_time_sec INTEGER, max_production_limit INTEGER, materials_json TEXT, activities_json TEXT);
         CREATE TABLE sde_regions (region_id INTEGER, name TEXT, x REAL, y REAL, z REAL, faction_id INTEGER, wormhole_class_id INTEGER);
         CREATE TABLE sde_constellations (constellation_id INTEGER, region_id INTEGER, name TEXT, x REAL, y REAL, z REAL, faction_id INTEGER, wormhole_class_id INTEGER);
         CREATE TABLE sde_station_operations (operation_id INTEGER, name TEXT, activity_id INTEGER, services_json TEXT);
         CREATE TABLE sde_station_services (service_id INTEGER, name TEXT);
         CREATE TABLE sde_factions (faction_id INTEGER, name TEXT, corporation_id INTEGER, militia_corporation_id INTEGER, solar_system_id INTEGER, size_factor REAL, description TEXT);
         CREATE TABLE sde_npc_corporations (corporation_id INTEGER, name TEXT, ticker_name TEXT, faction_id INTEGER, solar_system_id INTEGER, station_id INTEGER, tax_rate REAL);
         CREATE TABLE sde_stations (station_id INTEGER, system_id INTEGER, constellation_id INTEGER, region_id INTEGER, owner_id INTEGER, type_id INTEGER, operation_id INTEGER, name TEXT, x REAL, y REAL, z REAL, reprocessing_efficiency REAL, reprocessing_stations_take REAL, is_conquerable INTEGER, orbit_id INTEGER);
         CREATE TABLE sde_solar_systems (system_id INTEGER, constellation_id INTEGER, region_id INTEGER, name TEXT, security REAL, x REAL, y REAL, z REAL, radius REAL, planet_ids_json TEXT, wormhole_class_id INTEGER);
         CREATE TABLE sde_stargates (stargate_id INTEGER, system_id INTEGER, destination_gate_id INTEGER, destination_system_id INTEGER, x REAL, y REAL, z REAL);
         CREATE TABLE sde_celestials (celestial_id INTEGER);
         CREATE TABLE sde_races (race_id INTEGER, name TEXT, ship_type_id INTEGER, skills_json TEXT, description TEXT);
         CREATE TABLE sde_bloodlines (bloodline_id INTEGER, race_id INTEGER, name TEXT, corporation_id INTEGER, charisma INTEGER, intelligence INTEGER, memory INTEGER, perception INTEGER, willpower INTEGER, ship_type_id INTEGER, description TEXT);
         CREATE TABLE sde_ancestries (ancestry_id INTEGER, bloodline_id INTEGER, name TEXT, charisma INTEGER, intelligence INTEGER, memory INTEGER, perception INTEGER, willpower INTEGER, description TEXT, short_description TEXT);
         CREATE TABLE sde_schools (school_id INTEGER, race_id INTEGER, corporation_id INTEGER, starting_station_id INTEGER, starting_system_id INTEGER, name TEXT);
         CREATE TABLE sde_agents (agent_id INTEGER, name TEXT, corporation_id INTEGER, station_id INTEGER, solar_system_id INTEGER, level INTEGER, division_id INTEGER, agent_type_id INTEGER, is_locator INTEGER);
         CREATE TABLE sde_market_groups (market_group_id INTEGER, parent_group_id INTEGER, name TEXT, has_types INTEGER, icon_id INTEGER);
         CREATE TABLE sde_planet_schematics (schematic_id INTEGER, name TEXT, cycle_time INTEGER, pins_json TEXT, types_json TEXT);
         CREATE TABLE sde_wormhole_types (type_id INTEGER, name TEXT, target_system_class INTEGER, max_stable_time_min REAL, max_stable_mass REAL, mass_regeneration REAL, max_jump_mass REAL);
         CREATE TABLE sde_npc_spawn_groups (spawn_group_id INTEGER, faction_id INTEGER, ship_class INTEGER, group_id INTEGER, group_name TEXT, npc_type_ids_json TEXT);
         CREATE TABLE sde_clone_grades (grade_id INTEGER, skills_json TEXT);
         CREATE TABLE sde_dbuff_collections (dbuff_id INTEGER, operation_name TEXT, data_json TEXT);
         CREATE TABLE sde_fighter_abilities (ability_id INTEGER, data_json TEXT);
         CREATE TABLE sde_compressible_types (type_id INTEGER, compressed_type_id INTEGER);
         CREATE TABLE sde_type_lists (type_list_id INTEGER, name TEXT, included_category_ids_json TEXT, included_group_ids_json TEXT, included_type_ids_json TEXT, excluded_category_ids_json TEXT, excluded_group_ids_json TEXT, excluded_type_ids_json TEXT);
         CREATE TABLE sde_dungeon_asteroid_counts (dungeon_id INTEGER, room_id INTEGER, type_id INTEGER, asteroid_count INTEGER);
         CREATE TABLE sde_dungeon_gas_types (dungeon_id INTEGER, type_id INTEGER);
         CREATE TABLE sde_dynamic_item_attributes (mutaplasmid_type_id INTEGER, mappings_json TEXT, attribute_ranges_json TEXT);
         CREATE TABLE sde_space_components (type_id INTEGER, components_json TEXT);
         CREATE TABLE sde_missions (mission_id INTEGER, name TEXT, is_courier INTEGER, lp_alpha INTEGER, lp_omega INTEGER, has_standing_rewards INTEGER, content_template TEXT);
         CREATE TABLE sde_dungeons (dungeon_id INTEGER, name TEXT, archetype_id INTEGER, faction_id INTEGER, difficulty INTEGER);
         CREATE TABLE sde_epic_arcs (epic_arc_id INTEGER, name TEXT, faction_id INTEGER, restart_interval INTEGER, mission_count INTEGER);
         CREATE TABLE sde_node_graphs (node_graph_id INTEGER, name TEXT, parent_graph_id INTEGER, node_count INTEGER, tags_json TEXT);
         CREATE TABLE sde_fighter_abilities_by_type (type_id INTEGER, data_json TEXT);
         CREATE TABLE sde_skin_licenses (license_type_id INTEGER, skin_id INTEGER, duration INTEGER);
         CREATE TABLE sde_skins (skin_id INTEGER, internal_name TEXT, skin_material_id INTEGER, types_json TEXT, allow_ccp_devs INTEGER, visible_serenity INTEGER, visible_tranquility INTEGER, is_structure_skin INTEGER, skin_description TEXT);"
    ))
    .unwrap();
}

#[test]
fn open_missing_directory_and_invalid_schema_errors() {
    let dir = std::env::temp_dir();
    assert!(matches!(
        SdeRegistry::open(dir.join("evomu_no_such_sde_file.sqlite")),
        Err(SdeError::MissingDatabase(_))
    ));
    // Directory instead of file -> MissingDatabase
    assert!(matches!(
        SdeRegistry::open(&dir),
        Err(SdeError::MissingDatabase(_))
    ));

    let bad_path = dir.join(format!("evomu_bad_schema_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&bad_path);
    {
        let conn = Connection::open(&bad_path).unwrap();
        conn.execute_batch("CREATE TABLE dummy (x INTEGER);")
            .unwrap();
    }
    assert!(matches!(
        SdeRegistry::open(&bad_path),
        Err(SdeError::InvalidSchema(_))
    ));

    {
        let conn = Connection::open(&bad_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO sde_meta VALUES ('schema_version', '2');",
        )
        .unwrap();
    }
    assert!(matches!(
        SdeRegistry::open(&bad_path),
        Err(SdeError::InvalidSchema(_))
    ));
    let _ = std::fs::remove_file(&bad_path);
}

#[test]
fn load_uncached_populates_all_domain_tables_and_handles_edge_cases() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_full_synthetic_sde_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);
    {
        let conn = Connection::open(&db_path).unwrap();
        create_base_sde_schema(&conn, true);
        conn.execute_batch(
            r#"
            INSERT INTO sde_resources VALUES ('res:/a', 'a/b', 'md5', 128, 'cfsd', 4, 'spot');
            INSERT INTO sde_categories VALUES (6, 'Ship', 1), (99, 'Hidden', 0);
            INSERT INTO sde_groups VALUES (25, 6, 'Frigate', 1, 0, 1, 0, 1);
            INSERT INTO sde_types VALUES
                (587, 25, 6, 'Rifter', 1000.0, 27289.0, 140.0, 0, 250000.0, 35.0, 64, 2, 500002, NULL, 26553, 1, '{"37":365.0,"bad":1.0}', '[16,999999]', 93841, 21, 46, 20078, 1, 0, 1, 16, 'Fast frigate'),
                (588, 25, 6, 'Slasher', 1000.0, 25000.0, 110.0, 2, 200000.0, 30.0, NULL, NULL, NULL, 587, NULL, 0, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
                (586, 25, 6, 'Probe', 1000.0, 25000.0, 110.0, 1, 200000.0, 30.0, NULL, NULL, NULL, 587, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, '');
            INSERT INTO sde_dogma_attributes VALUES (37, 'maxVelocity', 100.0, 1, 0, 1);
            INSERT INTO sde_dogma_effects VALUES (
                16, 'loPower', 0, 1, 0, 1, 0, 1, 0, 1, 'guid16', 1,
                '[{"func":"ItemModifier","modified_attribute_id":37,"modifying_attribute_id":20,"operator":6},{"func":"EffectStopper","domain":"targetID","effect_id":10},{"modified_attribute_id":37}]'
            );
            INSERT INTO sde_type_materials VALUES
                (587, '[{"material_type_id":34,"quantity":100}]', '[{"material_type_id":35,"quantity_min":1,"quantity_max":5}]'),
                (588, '[]', NULL);
            INSERT INTO sde_blueprints VALUES
                (688, 587, 1, 600, -5, '[{"material_type_id":34,"quantity":500}]', '{"research_material":{"time":10},"research_time":{"time":20},"copying":{"time":30},"invention":{"time":40,"products":[{"type_id":588,"quantity":1,"probability":0.45}]}}'),
                (689, 588, 1, 300, 10, '[]', '{}');
            INSERT INTO sde_regions VALUES (10000002, 'The Forge', 1.0, 2.0, 3.0, 500001, 7);
            INSERT INTO sde_constellations VALUES (20000020, 10000002, 'Kimotoro', 4.0, 5.0, 6.0, 500001, 7);
            INSERT INTO sde_station_operations VALUES
                (14, 'Caldari Navy', 5, '[10]'),
                (15, 'Cloning24', NULL, '[24]'),
                (16, 'NoClone', NULL, '[1, 2]');
            INSERT INTO sde_station_services VALUES (10, 'Cloning');
            INSERT INTO sde_factions VALUES
                (500001, 'Caldari State', 1000035, 1000180, 30000142, NULL, NULL),
                (500002, 'Minmatar', 1000050, NULL, NULL, 2.5, 'Republic');
            INSERT INTO sde_npc_corporations VALUES (1000035, 'Caldari Navy', 'CN', 500001, 30000142, 60003760, 0.1);
            INSERT INTO sde_stations VALUES
                (60003760, 30000142, 20000020, 10000002, 1000035, 1529, 14, 'Jita 4-4', 10.0, 20.0, 30.0, 0.5, 0.05, 0, 40009087),
                (60003761, 30000142, 20000020, 10000002, 9999999, 1529, 16, 'NoCloneSta', 0.0, 0.0, 0.0, 0.5, 0.05, 1, NULL),
                (60003762, 30000142, 20000020, 10000002, 9999999, 1529, 999, 'DefaultCloneSta', 0.0, 0.0, 0.0, 0.5, 0.05, 0, NULL);
            INSERT INTO sde_solar_systems VALUES (30000142, 20000020, 10000002, 'Jita', 0.9, 9460730472580800.0, 0.0, 0.0, 1000.0, '[40009080]', 3);
            INSERT INTO sde_stargates VALUES (50001249, 30000142, 50002185, 30000144, 1.0, 2.0, 3.0);
            INSERT INTO sde_celestials VALUES (40009080), (40009087);
            INSERT INTO sde_races VALUES (1, 'Caldari', 601, '{"3300":3}', NULL);
            INSERT INTO sde_bloodlines VALUES (1, 1, 'Deteis', 1000006, 5, 7, 7, 5, 6, 601, NULL);
            INSERT INTO sde_ancestries VALUES (1, 1, 'Mercs', 0, 1, 1, 2, 0, NULL, NULL);
            INSERT INTO sde_schools VALUES (17, 1, 1000167, 60003760, 30000142, 'SWA');
            INSERT INTO sde_agents VALUES (3008416, 'Agent1', 1000035, 60003760, 30000142, 4, 24, 2, 1);
            INSERT INTO sde_market_groups VALUES (64, NULL, 'Frigates', 1, 21);
            INSERT INTO sde_planet_schematics VALUES (65, 'Water', 1800, '[2469]', '[{"type_id":2305,"quantity":10,"is_input":true}]');
            INSERT INTO sde_wormhole_types VALUES (30001, 'K162', 7, 1440.0, 2000000000.0, 0.0, 300000000.0);
            INSERT INTO sde_npc_spawn_groups VALUES (1, 500001, 1, 25, 'Guristas', '[1001]');
            INSERT INTO sde_clone_grades VALUES (1, '[{"typeID":3300,"maxTrainingLevel":4}]');
            INSERT INTO sde_dbuff_collections VALUES (
                10, 'PostPercent',
                '{"developerDescription":"ShieldBoost","aggregateMode":"Maximum","displayNameID":111,"showOutputValueInUI":"ShowNormal","itemModifiers":[{"dogmaAttributeID":37}],"locationModifiers":[{"dogmaAttributeID":263}],"locationGroupModifiers":[{"dogmaAttributeID":265,"groupID":25}],"locationRequiredSkillModifiers":[{"dogmaAttributeID":68,"skillID":3300}]}'
            );
            INSERT INTO sde_fighter_abilities VALUES (
                1, '{"targetMode":"pointTargeted","disallowInHighSec":true,"disallowInLowSec":false,"iconID":10,"displayNameID":20,"tooltipTextID":30,"turretGraphicID":40}'
            );
            INSERT INTO sde_compressible_types VALUES (1230, 28432);
            INSERT INTO sde_type_lists VALUES (334, 'Ores', '[25]', '[462]', '[1230]', '[]', '[]', '[11396]');
            INSERT INTO sde_dungeon_asteroid_counts VALUES (1347, 19522, 1230, 4);
            INSERT INTO sde_dungeon_gas_types VALUES (1347, 25268);
            INSERT INTO sde_dynamic_item_attributes VALUES
                (47700, '[{"resulting_type_id":47701,"applicable_type_ids":[587]}]', '{"37":[0.9,1.2]}'),
                (47702, '[]', '{}');
            INSERT INTO sde_space_components VALUES (587, '["itemTrader"]');
            INSERT INTO sde_missions VALUES (1, 'Worlds Collide', 0, 100, 200, 1, 'tmpl');
            INSERT INTO sde_dungeons VALUES (1347, 'Hideaway', 10, 500001, 2);
            INSERT INTO sde_epic_arcs VALUES (1, 'Blood Stained Stars', 500001, 90, -1);
            INSERT INTO sde_node_graphs VALUES (10, 'NPE Graph', 5, -3, '["npe"]');
            INSERT INTO sde_fighter_abilities_by_type VALUES (
                23055,
                '{"abilitySlot0":{"abilityID":1,"cooldownSeconds":5.0,"charges":{"chargeCount":3,"rearmTimeSeconds":10.0}},"abilitySlot2":{"abilityID":1}}'
            );
            "#
        )
        .unwrap();
    }

    let sde = SdeRegistry::open(&db_path).expect("open synthetic sde");
    // Second open hits SDE_CACHE
    let sde_cached = SdeRegistry::open(&db_path).expect("open cached synthetic sde");
    assert_eq!(sde_cached.types.len(), 3);

    assert_eq!(sde.types[&587].portion_size, 1);
    assert_eq!(sde.types[&587].default_attributes.get(&37), Some(&365.0));
    assert_eq!(sde.types[&587].effect_ids, vec![16]);
    assert_eq!(sde.wrecks.get(&587), Some(&26553));
    assert_eq!(sde.variations_of(587), &[586, 588]);

    let eff = &sde.effects[&16];
    assert_eq!(eff.modifiers.len(), 2);
    assert_eq!(eff.modifiers[0].domain, "shipID");
    assert_eq!(eff.modifiers[0].operator, 6);
    assert_eq!(eff.modifiers[1].func, "EffectStopper");
    assert_eq!(eff.modifiers[1].domain, "targetID");
    assert_eq!(eff.modifiers[1].effect_id, Some(10));

    assert_eq!(sde.type_materials_for(587).len(), 1);
    assert_eq!(sde.randomized_type_materials_for(587).len(), 1);
    assert!(sde.randomized_type_materials_for(588).is_empty());

    let bp688 = &sde.blueprints[&688];
    assert_eq!(bp688.max_production_limit, 0);
    assert_eq!(bp688.research_me_time_sec, 10);
    assert_eq!(bp688.research_te_time_sec, 20);
    assert_eq!(bp688.copy_time_sec, 30);
    assert_eq!(bp688.invention_time_sec, 40);
    assert_eq!(bp688.invention_base_probability, 0.45);
    assert_eq!(sde.blueprints[&689].invention_base_probability, 0.34);

    assert_eq!(sde.factions[&500_001].size_factor, 1.0);
    assert_eq!(sde.factions[&500_002].size_factor, 2.5);
    assert!(sde.stations[&60_003_760].has_clone_bay);
    assert_eq!(sde.stations[&60_003_760].owner_faction_id, Some(500_001));
    assert!(!sde.stations[&60_003_761].has_clone_bay);
    assert!(sde.stations[&60_003_762].has_clone_bay);
    assert_eq!(sde.solar_systems[&30_000_142].galactic_pos_ly, [1.0, 0.0, 0.0]);
    assert_eq!(sde.solar_systems[&30_000_142].stations.len(), 3);
    assert_eq!(sde.celestial_count, 2);
    assert_eq!(sde.races[&1].starter_skills.get(&3300), Some(&3));
    assert_eq!(sde.clone_grades[&1].alpha_skill_caps.get(&3300), Some(&4));

    let dbuff = &sde.dbuff_collections[&10];
    assert_eq!(dbuff.name, "ShieldBoost");
    assert_eq!(dbuff.target_attribute_ids, vec![37, 263, 265, 68]);

    let fa = &sde.fighter_abilities[&1];
    assert_eq!(fa.ability_name_id, Some(20));
    assert!(fa.disallow_in_high_sec);
    assert!(!fa.disallow_in_low_sec);

    let slots = &sde.fighter_abilities_by_type[&23055];
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].slot_index, 0);
    assert_eq!(
        slots[0].charges,
        Some(FighterAbilityCharges {
            charge_count: 3,
            rearm_time_seconds: 10.0,
        })
    );
    assert_eq!(slots[1].slot_index, 2);

    assert_eq!(sde.dynamic_item_attributes[&47700].resulting_type_id, 47701);
    assert_eq!(sde.dynamic_item_attributes[&47702].resulting_type_id, 47702);
    assert_eq!(sde.epic_arcs[&1].missions_count, 0);
    assert_eq!(sde.node_graphs[&10].node_count, 0);

    let _ = std::fs::remove_file(&db_path);

    // Fallback when randomized_materials_json column is absent + dbuff / fighter ability error cases
    let legacy_path = std::env::temp_dir().join(format!(
        "evomu_legacy_sde_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&legacy_path);
    {
        let conn = Connection::open(&legacy_path).unwrap();
        create_base_sde_schema(&conn, false);
        conn.execute_batch(
            "INSERT INTO sde_type_materials VALUES (34, '[]');",
        )
        .unwrap();
    }
    let sde_leg = SdeRegistry::load_uncached(legacy_path.clone()).expect("legacy type_materials");
    assert!(sde_leg.type_materials.contains_key(&34));
    let _ = std::fs::remove_file(&legacy_path);
}

#[test]
fn load_uncached_reads_belt_asteroid_tables_from_sqlite_without_resfiles() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_belt_sqlite_only_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);
    {
        let conn = Connection::open(&db_path).unwrap();
        create_base_sde_schema(&conn, true);
        conn.execute_batch(
            "ALTER TABLE sde_types ADD COLUMN isis_group_id INTEGER;
             CREATE TABLE sde_belt_asteroid_counts (belt_id INTEGER PRIMARY KEY, counts_json TEXT NOT NULL);
             CREATE TABLE sde_system_asteroid_types (system_id INTEGER PRIMARY KEY, type_ids_json TEXT NOT NULL);
             INSERT INTO sde_solar_systems VALUES (30000142, 20000020, 10000002, 'Jita', 0.9, 0.0, 0.0, 0.0, 1000.0, '[]', NULL);
             INSERT INTO sde_types (type_id, group_id, category_id, name, mass, volume, capacity, portion_size, base_price, radius, market_group_id, race_id, faction_id, variation_parent_type_id, wreck_type_id, published, attributes_json, effects_json, isis_group_id) VALUES
                (1230, 462, 25, 'Veldspar', 1.0, 0.1, 0.0, 100, 10.0, 200.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', 8),
                (1228, 460, 25, 'Scordite', 1.0, 0.15, 0.0, 100, 12.0, 200.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', 0),
                (1229, 460, 25, 'Pyroxeres', 1.0, 0.15, 0.0, 100, 12.0, 200.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', -3);
             INSERT INTO sde_belt_asteroid_counts VALUES
                (40009080, '{\"1228\":8,\"1230\":21}');
             INSERT INTO sde_system_asteroid_types VALUES
                (30000142, '[1230,1228,1230]');",
        )
        .unwrap();
    }

    let sde = SdeRegistry::load_uncached(db_path.clone()).expect("sqlite-only belt tables must load");
    assert_eq!(sde.type_isis_group_id(1230), Some(8));
    assert_eq!(sde.type_isis_group_id(1228), None);
    assert_eq!(sde.type_isis_group_id(1229), None);
    assert!(sde.resources.is_empty(), "No ResFiles catalog needed for SQLite belt tables");
    assert_eq!(sde.asteroid_counts_by_belt.len(), 1);
    let counts = sde.belt_asteroid_counts(40_009_080).expect("belt counts present");
    assert_eq!(counts.get(&1228), Some(&8));
    assert_eq!(counts.get(&1230), Some(&21));
    assert_eq!(sde.system_asteroid_types(30_000_142), &[1228, 1230]);
    let belt_ores: Vec<TypeId> = sde
        .belt_ores_for_system(30_000_142)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert_eq!(belt_ores, vec![1230, 1228]);

    // Empty SQLite tables should gracefully keep empty maps when ResFiles also do not exist
    let empty_db = std::env::temp_dir().join(format!(
        "evomu_belt_sqlite_empty_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&empty_db);
    {
        let conn = Connection::open(&empty_db).unwrap();
        create_base_sde_schema(&conn, true);
        conn.execute_batch(
            "CREATE TABLE sde_belt_asteroid_counts (belt_id INTEGER PRIMARY KEY, counts_json TEXT NOT NULL);
             CREATE TABLE sde_system_asteroid_types (system_id INTEGER PRIMARY KEY, type_ids_json TEXT NOT NULL);",
        )
        .unwrap();
    }
    let sde_empty = SdeRegistry::load_uncached(empty_db.clone()).expect("empty tables fallback");
    assert!(sde_empty.asteroid_counts_by_belt.is_empty());
    assert!(sde_empty.asteroid_types_by_system.is_empty());

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&empty_db);
}

#[test]
fn load_belt_and_system_asteroid_tables_sqlite_fallback_and_sanitization() {
    let conn = Connection::open_in_memory().unwrap();
    // Missing tables -> None
    assert!(SdeRegistry::load_belt_asteroid_counts_from_sqlite(&conn).is_none());
    assert!(SdeRegistry::load_system_asteroid_types_from_sqlite(&conn).is_none());

    conn.execute_batch(
        "CREATE TABLE sde_belt_asteroid_counts (belt_id INTEGER PRIMARY KEY, counts_json TEXT NOT NULL);
         CREATE TABLE sde_system_asteroid_types (system_id INTEGER PRIMARY KEY, type_ids_json TEXT NOT NULL);
         INSERT INTO sde_belt_asteroid_counts VALUES (40000003, 'not-json');
         INSERT INTO sde_system_asteroid_types VALUES (30000144, 'not-json');",
    )
    .unwrap();
    // Corrupt JSON rows -> None fallback
    assert!(SdeRegistry::load_belt_asteroid_counts_from_sqlite(&conn).is_none());
    assert!(SdeRegistry::load_system_asteroid_types_from_sqlite(&conn).is_none());

    conn.execute_batch(
        "DELETE FROM sde_belt_asteroid_counts;
         DELETE FROM sde_system_asteroid_types;
         INSERT INTO sde_belt_asteroid_counts VALUES (40000001, '{\"1230\":15,\"1228\":5}');
         INSERT INTO sde_system_asteroid_types VALUES (30000142, '[1230, 1228, 1230]');",
    )
    .unwrap();

    let belts = SdeRegistry::load_belt_asteroid_counts_from_sqlite(&conn).expect("loaded belts");
    assert_eq!(belts.len(), 1);
    assert_eq!(
        belts[&40_000_001],
        HashMap::from([(1230u32, 15u32), (1228u32, 5u32)])
    );

    let sys = SdeRegistry::load_system_asteroid_types_from_sqlite(&conn).expect("loaded sys");
    assert_eq!(sys.len(), 1);
    assert_eq!(sys[&30_000_142], vec![1228u32, 1230u32]);
}

#[test]
fn load_uncached_reads_npc_corporation_ceo_id_when_present_and_filters_non_positive() {
    let legacy_path = std::env::temp_dir().join(format!(
        "evomu_npc_ceo_legacy_{}.sqlite",
        std::process::id()
    ));
    let with_ceo_path = std::env::temp_dir().join(format!(
        "evomu_npc_ceo_present_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&legacy_path);
    let _ = std::fs::remove_file(&with_ceo_path);

    {
        let conn = Connection::open(&legacy_path).unwrap();
        create_base_sde_schema(&conn, true);
        conn.execute(
            "INSERT INTO sde_npc_corporations VALUES (1000035, 'Caldari Navy', 'CN', 500001, 30000142, 60003760, 0.1)",
            [],
        )
        .unwrap();
    }
    let sde_legacy = SdeRegistry::load_uncached(legacy_path.clone()).expect("legacy npc corps");
    assert_eq!(sde_legacy.npc_corporations[&1_000_035].ceo_id, None);

    {
        let conn = Connection::open(&with_ceo_path).unwrap();
        create_base_sde_schema(&conn, true);
        conn.execute_batch(
            "ALTER TABLE sde_npc_corporations ADD COLUMN ceo_id INTEGER;
             INSERT INTO sde_npc_corporations VALUES (1000035, 'Caldari Navy', 'CN', 500001, 30000142, 60003760, 0.1, 3004069);
             INSERT INTO sde_npc_corporations VALUES (1000173, 'Polaris Corporation', 'PLR', 500021, NULL, NULL, 0.0, NULL);
             INSERT INTO sde_npc_corporations VALUES (1000999, 'Zero Ceo Test', 'ZCT', NULL, NULL, NULL, 0.0, 0);",
        )
        .unwrap();
    }
    let sde_ceo = SdeRegistry::load_uncached(with_ceo_path.clone()).expect("npc corps with ceo_id");
    assert_eq!(sde_ceo.npc_corporations[&1_000_035].ceo_id, Some(3_004_069));
    assert_eq!(sde_ceo.npc_corporations[&1_000_173].ceo_id, None);
    assert_eq!(sde_ceo.npc_corporations[&1_000_999].ceo_id, None);

    let _ = std::fs::remove_file(&legacy_path);
    let _ = std::fs::remove_file(&with_ceo_path);
}

#[test]
fn load_uncached_rejects_every_missing_required_table() {
    let tables = [
        "sde_resources",
        "sde_categories",
        "sde_groups",
        "sde_types",
        "sde_dogma_attributes",
        "sde_dogma_effects",
        "sde_type_materials",
        "sde_blueprints",
        "sde_regions",
        "sde_constellations",
        "sde_station_operations",
        "sde_station_services",
        "sde_factions",
        "sde_npc_corporations",
        "sde_stations",
        "sde_solar_systems",
        "sde_stargates",
        "sde_races",
        "sde_bloodlines",
        "sde_ancestries",
        "sde_schools",
        "sde_agents",
        "sde_market_groups",
        "sde_planet_schematics",
        "sde_wormhole_types",
        "sde_npc_spawn_groups",
        "sde_clone_grades",
        "sde_dbuff_collections",
        "sde_fighter_abilities",
        "sde_compressible_types",
        "sde_type_lists",
        "sde_dungeon_asteroid_counts",
        "sde_dungeon_gas_types",
        "sde_dynamic_item_attributes",
        "sde_space_components",
        "sde_missions",
        "sde_dungeons",
        "sde_epic_arcs",
        "sde_node_graphs",
        "sde_fighter_abilities_by_type",
    ];
    let db_path = std::env::temp_dir().join(format!(
        "evomu_missing_tbl_{}.sqlite",
        std::process::id()
    ));
    for tbl in tables {
        let _ = std::fs::remove_file(&db_path);
        {
            let conn = Connection::open(&db_path).unwrap();
            create_base_sde_schema(&conn, true);
            conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        }
        assert!(
            SdeRegistry::load_uncached(db_path.clone()).is_err(),
            "expected error when {tbl} is dropped"
        );
    }
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn load_uncached_rejects_bad_column_types_in_early_tables() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_bad_cols_sde_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);
    {
        let conn = Connection::open(&db_path).unwrap();
        create_base_sde_schema(&conn, true);
    }
    let stmts = [
        // sde_resources (7 cols: res_path, cdn_relative_path, md5, byte_size, format, record_count, spot)
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES (X'FF', 'a', 'm', 1, 'cfsd', 0, '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', X'FF', 'm', 1, 'cfsd', 0, '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', 'a', X'FF', 1, 'cfsd', 0, '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', 'a', 'm', 'bad', 'cfsd', 0, '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', 'a', 'm', 1, X'FF', 0, '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', 'a', 'm', 1, 'cfsd', 'bad', '')"),
        ("DELETE FROM sde_resources", "INSERT INTO sde_resources VALUES ('r', 'a', 'm', 1, 'cfsd', 0, X'FF')"),
        // sde_categories
        ("DELETE FROM sde_resources; DELETE FROM sde_categories", "INSERT INTO sde_categories VALUES (1, X'FF', 1)"),
        ("DELETE FROM sde_categories", "INSERT INTO sde_categories VALUES (1, 'Cat', 'bad')"),
        // sde_groups (8 cols)
        ("DELETE FROM sde_categories; DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 'bad', 'G', 0, 0, 0, 0, 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, X'FF', 0, 0, 0, 0, 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, 'G', 'bad', 0, 0, 0, 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, 'G', 0, 'bad', 0, 0, 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, 'G', 0, 0, 'bad', 0, 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, 'G', 0, 0, 0, 'bad', 1)"),
        ("DELETE FROM sde_groups", "INSERT INTO sde_groups VALUES (1, 1, 'G', 0, 0, 0, 0, 'bad')"),
        // sde_types bad non-PK cols & JSON parse failures
        ("DELETE FROM sde_groups; DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{bad', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '{bad', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, 'bad', 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', 'bad', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, 'bad', NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, 'bad', NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, 'bad', NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, 'bad', NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, 'bad', NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, 'bad', NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 'bad', NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, X'FF')"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 'bad', 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 'bad', 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, X'FF', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 'bad', 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 'bad', 0.0, 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 'bad', 1, 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 'bad', 0.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 'bad', 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 'bad', NULL, NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, 'bad', NULL, NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, 'bad', NULL, NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
        ("DELETE FROM sde_types", "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 0.0, 1, 0.0, 1.0, NULL, NULL, 'bad', NULL, NULL, 1, '{}', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"),
    ];
    for (prep, ins) in stmts {
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(prep).unwrap();
            conn.execute_batch(ins).unwrap();
        }
        assert!(
            SdeRegistry::load_uncached(db_path.clone()).is_err(),
            "expected error for {ins}"
        );
    }
    let _ = std::fs::remove_file(&db_path);
}
