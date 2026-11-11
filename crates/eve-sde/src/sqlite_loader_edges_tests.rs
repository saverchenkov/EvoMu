use super::*;
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(tag: &str, init: impl FnOnce(&Connection)) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "evomu_sde_edges_{tag}_{}_{seq}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("create temp sqlite");
        create_edges_sde_schema(&conn);
        init(&conn);
        drop(conn);
        Self { path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn create_edges_sde_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT);
         INSERT INTO sde_meta (key, value) VALUES ('schema_version', '1');
         CREATE TABLE sde_resources (res_path TEXT, cdn_relative_path TEXT, md5 TEXT, byte_size INTEGER, format TEXT, record_count INTEGER, spot TEXT);
         CREATE TABLE sde_categories (category_id INTEGER, name TEXT, published INTEGER);
         CREATE TABLE sde_groups (group_id INTEGER, category_id INTEGER, name TEXT, anchorable INTEGER, anchored INTEGER, fittable_non_singleton INTEGER, use_base_price INTEGER, published INTEGER);
         CREATE TABLE sde_types (type_id INTEGER, group_id INTEGER, category_id INTEGER, name TEXT, mass REAL, volume REAL, capacity REAL, portion_size INTEGER, base_price REAL, radius REAL, market_group_id INTEGER, race_id INTEGER, faction_id INTEGER, variation_parent_type_id INTEGER, wreck_type_id INTEGER, published INTEGER, attributes_json TEXT, effects_json TEXT, description_id INTEGER, icon_id INTEGER, graphic_id INTEGER, sound_id INTEGER, meta_group_id INTEGER, meta_level INTEGER, tech_level INTEGER, default_effect_id INTEGER, description TEXT);
         CREATE TABLE sde_dogma_attributes (attribute_id INTEGER, name TEXT, default_value REAL, high_is_good INTEGER, stackable INTEGER, published INTEGER);
         CREATE TABLE sde_dogma_effects (effect_id INTEGER, name TEXT, effect_category INTEGER, is_offensive INTEGER, is_assistance INTEGER, is_warp_safe INTEGER, disallow_auto_repeat INTEGER, electronic_chance INTEGER, propulsion_chance INTEGER, range_chance INTEGER, guid TEXT, published INTEGER, modifiers_json TEXT);
         CREATE TABLE sde_type_materials (type_id INTEGER, materials_json TEXT, randomized_materials_json TEXT);
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
    )
    .expect("create schema");
}

#[test]
fn open_caching_invalid_schema_and_file_errors() {
    // Non-existent path -> MissingDatabase
    let missing = std::env::temp_dir().join(format!("evomu_missing_sde_{}.sqlite", std::process::id()));
    assert!(matches!(SdeRegistry::open(&missing), Err(SdeError::MissingDatabase(_))));

    // Unsupported schema_version & missing schema_version row.
    let db_bad_ver = TempDb::new("bad_ver", |c| {
        c.execute("UPDATE sde_meta SET value = '99' WHERE key = 'schema_version'", [])
            .unwrap();
    });
    assert!(matches!(
        SdeRegistry::open(&db_bad_ver.path),
        Err(SdeError::InvalidSchema(_))
    ));
    let db_no_ver = TempDb::new("no_ver", |c| {
        c.execute("DELETE FROM sde_meta WHERE key = 'schema_version'", [])
            .unwrap();
    });
    assert!(matches!(
        SdeRegistry::open(&db_no_ver.path),
        Err(SdeError::InvalidSchema(_))
    ));

    // Cache hit returns cached entry even after an in-place same-length UPDATE when file size matches.
    let db_ok = TempDb::new("cache_hit", |c| {
        c.execute(
            "INSERT INTO sde_categories VALUES (6, 'Ship', 1)",
            [],
        )
        .unwrap();
    });
    let r1 = SdeRegistry::open(&db_ok.path).expect("open 1");
    {
        let conn = Connection::open(&db_ok.path).unwrap();
        conn.execute("UPDATE sde_categories SET name = 'Boat' WHERE category_id = 6", [])
            .unwrap();
    }
    let r2 = SdeRegistry::open(&db_ok.path).expect("open 2");
    assert_eq!(r2.categories[&6].name, "Ship");

    // Rewriting file with a different byte size invalidates cache entry and reloads new data.
    {
        let conn = Connection::open(&db_ok.path).unwrap();
        let big_name = "X".repeat(8_192);
        conn.execute(
            "INSERT INTO sde_categories VALUES (7, ?1, 1)",
            rusqlite::params![big_name],
        )
        .unwrap();
    }
    let r3 = SdeRegistry::open(&db_ok.path).expect("open 3 after resize");
    assert_eq!(r3.categories[&6].name, "Boat");
    assert!(r3.categories.contains_key(&7));
    let _ = r1;
}

#[test]
fn json_column_error_paths_across_tables() {
    let cases: &[(&str, &str)] = &[
        (
            "sde_types_bad_attrs",
            "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 1.0, 1, 1.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{bad', '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, '')",
        ),
        (
            "sde_types_bad_effects",
            "INSERT INTO sde_types VALUES (1, 1, 1, 'T', 1.0, 1.0, 1.0, 1, 1.0, 1.0, NULL, NULL, NULL, NULL, NULL, 1, '{}', '{bad', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, '')",
        ),
        (
            "sde_dogma_effects_bad_mods",
            "INSERT INTO sde_dogma_effects VALUES (1, 'eff', 0, 0, 0, 0, 0, 0, 0, 0, NULL, 1, '{bad')",
        ),
        (
            "sde_type_materials_bad_mats",
            "INSERT INTO sde_type_materials VALUES (1, '{bad', '[]')",
        ),
        (
            "sde_type_materials_bad_rand",
            "INSERT INTO sde_type_materials VALUES (1, '[]', '{bad')",
        ),
        (
            "sde_blueprints_bad_mats",
            "INSERT INTO sde_blueprints VALUES (1, 2, 1, 10, 10, '{bad', '{}')",
        ),
        (
            "sde_blueprints_bad_acts",
            "INSERT INTO sde_blueprints VALUES (1, 2, 1, 10, 10, '[]', '{bad')",
        ),
        (
            "sde_station_ops_bad_svcs",
            "INSERT INTO sde_station_operations VALUES (1, 'Op', 1, '{bad')",
        ),
        (
            "sde_solar_systems_bad_planets",
            "INSERT INTO sde_solar_systems VALUES (30000001, 20000001, 10000001, 'Sys', 0.5, 0.0, 0.0, 0.0, 100.0, '{bad', NULL)",
        ),
        (
            "sde_npc_spawn_groups_bad",
            "INSERT INTO sde_npc_spawn_groups VALUES (1, 500001, 1, 550, 'Grp', '{bad')",
        ),
        (
            "sde_clone_grades_bad",
            "INSERT INTO sde_clone_grades VALUES (1, '{bad')",
        ),
        (
            "sde_dbuff_bad_json",
            "INSERT INTO sde_dbuff_collections VALUES (1, 'PostPercent', '{bad')",
        ),
        (
            "sde_dbuff_missing_desc",
            "INSERT INTO sde_dbuff_collections VALUES (1, 'PostPercent', '{\"aggregateMode\":\"Maximum\"}')",
        ),
        (
            "sde_dbuff_missing_agg",
            "INSERT INTO sde_dbuff_collections VALUES (1, 'PostPercent', '{\"developerDescription\":\"d\"}')",
        ),
        (
            "sde_fighter_abilities_bad",
            "INSERT INTO sde_fighter_abilities VALUES (1, '{bad')",
        ),
        (
            "sde_fighter_abilities_no_target_mode",
            "INSERT INTO sde_fighter_abilities VALUES (1, '{\"disallowInHighSec\":true}')",
        ),
        (
            "sde_dyn_attrs_bad_mappings",
            "INSERT INTO sde_dynamic_item_attributes VALUES (1, '{bad', '{}')",
        ),
        (
            "sde_dyn_attrs_bad_ranges",
            "INSERT INTO sde_dynamic_item_attributes VALUES (1, '[]', '{bad')",
        ),
        (
            "sde_space_components_bad",
            "INSERT INTO sde_space_components VALUES (1, '{bad')",
        ),
        (
            "sde_node_graphs_bad",
            "INSERT INTO sde_node_graphs VALUES (1, 'NG', NULL, 0, '{bad')",
        ),
        (
            "sde_fighter_by_type_bad",
            "INSERT INTO sde_fighter_abilities_by_type VALUES (1, '{bad')",
        ),
    ];

    for &(tag, sql) in cases {
        let db = TempDb::new(tag, |c| {
            c.execute(sql, []).unwrap_or_else(|e| panic!("{tag}: {e}"));
        });
        assert!(
            SdeRegistry::load_uncached(db.path.clone()).is_err(),
            "{tag} must return Err on invalid row data"
        );
    }
}

#[test]
fn valid_edge_filtering_fallbacks_and_optional_tables() {
    let db = TempDb::new("edge_filtering", |c| {
        // Non-EffectStopper missing modified/modifying attrs is filtered out; EffectStopper without them is kept.
        c.execute(
            "INSERT INTO sde_dogma_effects VALUES (10, 'eff', 0, 0, 0, 0, 0, 0, 0, 0, NULL, 1,
             '[{\"func\":\"ItemModifier\",\"domain\":\"shipID\",\"modifiedAttributeID\":100},
               {\"func\":\"EffectStopper\",\"domain\":\"shipID\",\"effectID\":1}]')",
            [],
        )
        .unwrap();

        // Dbuff collection with optional fields omitted + non-u64 modifier entries ignored.
        c.execute(
            "INSERT INTO sde_dbuff_collections VALUES (55, 'PostPercent',
             '{\"developerDescription\":\"Armor\",\"aggregateMode\":\"Minimum\",
               \"itemModifiers\":[{\"dogmaAttributeID\":267},{\"dogmaAttributeID\":\"bad\"}],
               \"locationModifiers\":[{\"dogmaAttributeID\":268},{}],
               \"locationGroupModifiers\":[{\"dogmaAttributeID\":269,\"groupID\":100},{\"dogmaAttributeID\":1}],
               \"locationRequiredSkillModifiers\":[{\"dogmaAttributeID\":270,\"skillID\":3300},{\"skillID\":3300}]}')",
            [],
        )
        .unwrap();

        // Fighter ability with explicit abilityNameID.
        c.execute(
            "INSERT INTO sde_fighter_abilities VALUES (12,
             '{\"targetMode\":\"targeted\",\"abilityNameID\":777}')",
            [],
        )
        .unwrap();

        // Fighter ability by type: missing abilityID on slot2 skipped, complete vs incomplete charges on slots 0/1.
        c.execute(
            "INSERT INTO sde_fighter_abilities_by_type VALUES (23055,
             '{\"abilitySlot0\":{\"abilityID\":12,\"charges\":{\"chargeCount\":3}},
               \"abilitySlot1\":{\"abilityID\":12,\"cooldownSeconds\":10.0,\"charges\":{\"chargeCount\":2,\"rearmTimeSeconds\":30.0}},
               \"abilitySlot2\":{}}')",
            [],
        )
        .unwrap();

        // Dynamic item attributes: non-u16 key skipped, valid key kept.
        c.execute(
            "INSERT INTO sde_dynamic_item_attributes VALUES (47740,
             '[{\"applicable_type_ids\":[597],\"resulting_type_id\":47745}]',
             '{\"not_a_u16\":[0.9,1.1],\"20\":[0.8,1.2]}')",
            [],
        )
        .unwrap();

        // Races: invalid skills_json and non-numeric keys fall back without erroring.
        c.execute(
            "INSERT INTO sde_races VALUES (1, 'Caldari', 601, '{bad-json', 'desc'),
                                          (2, 'Minmatar', 588, '{\"badkey\":1,\"3300\":2}', 'desc')",
            [],
        )
        .unwrap();

        // Type lists: invalid JSON columns fall back to empty vectors without erroring.
        c.execute(
            "INSERT INTO sde_type_lists VALUES (99, 'BrokenList', '{bad', '{bad', '[1,2]', '{bad', '{bad', '{bad')",
            [],
        )
        .unwrap();

        // Drop sde_celestials to verify missing-table fallback sets celestial_count = 0.
        c.execute("DROP TABLE sde_celestials", []).unwrap();
    });

    let reg = SdeRegistry::load_uncached(db.path.clone()).expect("load with fallbacks");
    assert_eq!(reg.celestial_count, 0);

    let eff = reg.dogma_effects().get(&10).expect("eff 10");
    assert_eq!(eff.modifiers.len(), 1);
    assert_eq!(eff.modifiers[0].func, "EffectStopper");

    let dbuff = reg.dbuff_collections.get(&55).expect("dbuff 55");
    assert_eq!(dbuff.display_name_id, None);
    assert_eq!(dbuff.item_modifiers, vec![267]);
    assert_eq!(dbuff.location_modifiers, vec![268]);
    assert_eq!(dbuff.location_group_modifiers.len(), 1);
    assert_eq!(dbuff.location_required_skill_modifiers.len(), 1);

    let fa = reg.fighter_abilities.get(&12).expect("fa 12");
    assert_eq!(fa.ability_name_id, Some(777));

    let fabt = reg.fighter_abilities_by_type.get(&23055).expect("fabt");
    assert_eq!(fabt.len(), 2);
    assert!(fabt[0].charges.is_none());
    assert!(fabt[1].charges.is_some());

    let muta = reg.dynamic_item_attributes.get(&47740).expect("muta");
    assert_eq!(muta.attribute_ranges.len(), 1);
    assert!(muta.attribute_ranges.contains_key(&20));

    assert!(reg.races.get(&1).expect("race 1").starter_skills.is_empty());
    assert_eq!(reg.races.get(&2).expect("race 2").starter_skills.get(&3300), Some(&2));
    assert_eq!(reg.type_lists.get(&99).expect("tl 99").included_type_ids, vec![1, 2]);
}

#[test]
fn missing_required_tables_propagate_errors() {
    for table in [
        "sde_categories",
        "sde_groups",
        "sde_types",
        "sde_dogma_attributes",
        "sde_dogma_effects",
        "sde_type_materials",
        "sde_blueprints",
        "sde_regions",
        "sde_constellations",
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
        "sde_wormhole_types",
        "sde_npc_spawn_groups",
        "sde_clone_grades",
        "sde_dbuff_collections",
        "sde_fighter_abilities",
        "sde_compressible_types",
        "sde_type_lists",
        "sde_dynamic_item_attributes",
        "sde_space_components",
        "sde_missions",
        "sde_dungeons",
        "sde_epic_arcs",
        "sde_node_graphs",
        "sde_fighter_abilities_by_type",
    ] {
        let db = TempDb::new(&format!("drop_{table}"), |c| {
            c.execute(&format!("DROP TABLE {table}"), []).unwrap();
        });
        assert!(
            SdeRegistry::load_uncached(db.path.clone()).is_err(),
            "dropping {table} must fail load_uncached"
        );
    }
}
