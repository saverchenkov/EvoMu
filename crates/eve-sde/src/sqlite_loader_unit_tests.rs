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
