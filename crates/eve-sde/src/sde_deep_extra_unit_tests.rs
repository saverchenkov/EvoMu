use super::*;
use crate::cfsd_flat_tables::{CfsdRecord, CfsdValue, CFSD_FLAT_LAYOUTS};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

fn shared_sde() -> &'static SdeRegistry {
    static SDE: OnceLock<SdeRegistry> = OnceLock::new();
    SDE.get_or_init(SdeRegistry::with_canonical_seed)
}

#[test]
fn test_localization_lang_normalization_all_branches() {
    assert_eq!(normalize_localization_fsd_lang("en"), Some("en-us"));
    assert_eq!(normalize_localization_fsd_lang("en-US"), Some("en-us"));
    assert_eq!(
        normalize_localization_fsd_lang("  en-US,en;q=0.9,de;q=0.8 "),
        Some("en-us")
    );
    assert_eq!(normalize_localization_fsd_lang("de"), Some("de"));
    assert_eq!(normalize_localization_fsd_lang("DE-AT"), Some("de"));
    assert_eq!(normalize_localization_fsd_lang("es"), Some("es"));
    assert_eq!(normalize_localization_fsd_lang("es-ES"), Some("es"));
    assert_eq!(normalize_localization_fsd_lang("fr"), Some("fr"));
    assert_eq!(normalize_localization_fsd_lang("fr-CA"), Some("fr"));
    assert_eq!(normalize_localization_fsd_lang("it"), Some("it"));
    assert_eq!(normalize_localization_fsd_lang("it-IT"), Some("it"));
    assert_eq!(normalize_localization_fsd_lang("ja"), Some("ja"));
    assert_eq!(normalize_localization_fsd_lang("ja-JP"), Some("ja"));
    assert_eq!(normalize_localization_fsd_lang("ko"), Some("ko"));
    assert_eq!(normalize_localization_fsd_lang("ko-KR"), Some("ko"));
    assert_eq!(normalize_localization_fsd_lang("ru"), Some("ru"));
    assert_eq!(normalize_localization_fsd_lang("ru-RU"), Some("ru"));
    assert_eq!(normalize_localization_fsd_lang("zh"), Some("zh"));
    assert_eq!(normalize_localization_fsd_lang("zh-CN"), Some("zh"));
    assert_eq!(normalize_localization_fsd_lang("zh-hans-sg"), Some("zh"));
    assert_eq!(normalize_localization_fsd_lang("pt-BR"), None);
    assert_eq!(normalize_localization_fsd_lang(""), None);
    assert_eq!(normalize_localization_fsd_lang(",;;"), None);
}

#[test]
fn test_canonical_sde_localization_endpoints() {
    let sde = shared_sde();
    let main_bundle = sde.load_localization_main().expect("localization main");
    assert!(main_bundle.languages.contains(&"en-us".to_string()));
    assert!(main_bundle.labels.len() > 10_000);

    let all_en = sde.load_all_localized_messages_en_us().expect("all en-us");
    assert!(all_en.len() > 100_000);

    let mut needed = HashSet::new();
    needed.insert(93841);
    let subset = sde
        .load_localized_messages_en_us(&needed)
        .expect("subset en-us");
    assert!(subset.contains_key(&93841));
    assert!(sde
        .load_localized_messages("de", &HashSet::new())
        .expect("empty filter")
        .is_empty());

    assert!(sde.localized_message_en_us(93841).is_some());
    assert!(sde.localized_message_en_us(u32::MAX).is_none());

    // Unsupported language falls back cleanly to None / Err
    assert!(sde.localized_message("xx-invalid", 93841).is_none());
    assert!(sde.load_all_localized_messages("xx-invalid").is_err());
    assert!(sde.load_localized_messages("xx-invalid", &needed).is_err());

    // Type description fallback via self.types[tid].description_id + market group descriptions
    let rifter_desc = sde
        .localized_type_description("en-us", 587)
        .expect("rifter en-us description");
    assert!(!rifter_desc.is_empty());
    assert!(sde.localized_type_description("en-us", u32::MAX).is_none());
    let _ = sde.localized_type_name("de", 587);

    let mg_descs = sde.load_market_group_descriptions().expect("mg descs");
    assert!(!mg_descs.is_empty());
    let first_mg_id = *mg_descs.keys().next().expect("at least one mg description");
    assert!(sde.market_group_description(first_mg_id).is_some());
    assert!(sde.market_group_description(u32::MAX).is_none());
}

#[test]
fn test_cfsd_flat_layouts_and_record_helpers() {
    let sde = shared_sde();
    for lay in CFSD_FLAT_LAYOUTS {
        let table = sde
            .load_cfsd_table(lay.res_path)
            .unwrap_or_else(|e| panic!("failed loading {}: {e}", lay.res_path));
        assert!(
            !table.is_empty(),
            "expected non-empty table for {}",
            lay.res_path
        );
    }
    assert!(sde
        .load_cfsd_table("res:/staticdata/does_not_exist.fsdbinary")
        .is_err());

    let mut fields = BTreeMap::new();
    fields.insert("count".to_string(), CfsdValue::Int(7));
    fields.insert("label".to_string(), CfsdValue::Str("Caldari".to_string()));
    fields.insert("items".to_string(), CfsdValue::IntList(vec![1, 2, 3]));
    let rec = CfsdRecord { key: 42, fields };
    assert_eq!(rec.int("count"), Some(7));
    assert_eq!(rec.int("label"), None);
    assert_eq!(rec.str("label"), Some("Caldari"));
    assert_eq!(rec.str("count"), None);
    assert_eq!(rec.list("items"), Some(&[1i64, 2, 3][..]));
    assert_eq!(rec.list("count"), None);
    assert_eq!(rec.int("missing"), None);
}

#[test]
fn test_sde_domain_queries_empty_and_populated() {
    let sde = shared_sde();
    // Station shortcut vs SQLite celestial fallback vs unknown location
    assert_eq!(sde.location_solar_system(60003760), Some(30000142));
    assert_eq!(sde.location_solar_system(40009087), Some(30000142));
    assert_eq!(sde.location_solar_system(9_999_999_999_999), None);

    let loc_map = sde.location_solar_system_map();
    assert_eq!(loc_map.get(&60003760), Some(&30000142));
    assert_eq!(loc_map.get(&40009087), Some(&30000142));

    let factions = sde.factions_owning_solar_systems();
    assert!(factions.contains(&500001));

    let ores = sde.dungeon_ore_types(1347).expect("dungeon 1347 ores");
    assert!(ores.contains(&20) && ores.contains(&1227));
    assert!(sde.dungeon_ore_types(u32::MAX).is_none());

    // Find a dungeon containing ice (group 465) if present and exercise dungeon_ice_types
    if let Some((&ice_dng, _)) = sde.dungeon_asteroids.iter().find(|(_, entries)| {
        entries
            .iter()
            .any(|e| sde.types.get(&e.type_id).is_some_and(|t| t.group_id == 465))
    }) {
        assert!(!sde.dungeon_ice_types(ice_dng).unwrap().is_empty());
    }
    assert!(sde.dungeon_ice_types(u32::MAX).is_none());

    let bp_by_mat = sde.blueprints_by_material_type();
    assert!(!bp_by_mat.get(&34).expect("Tritanium blueprints").is_empty());

    let req_skills = sde.required_skills_for_type(587);
    assert!(!req_skills.is_empty());

    // Variation parent branch (Wolf 11371 has parent Rifter 587)
    let wolf_info = sde.show_info_for_type(11371).expect("Wolf show_info");
    assert_eq!(wolf_info.variation_parent_type_id, Some(587));
    assert!(wolf_info.variations.contains(&587));
    assert!(!wolf_info.variations.contains(&11371));

    // Market group queries & simple getters
    assert!(!sde.published_market_types().is_empty());
    let root_mgs = sde.market_groups_for_parent(None);
    assert!(!root_mgs.is_empty());
    let _ = sde.types_in_market_group(root_mgs[0].market_group_id);
    assert_eq!(sde.get_type_category(587), Some(6));
    assert!(sde.dogma_attribute(u16::MAX).is_none());
    assert!(sde.dogma_effect(u16::MAX).is_none());
    assert!(sde.blueprint(u32::MAX).is_none());
    assert!(sde.solar_system(u32::MAX).is_none());
    assert!(sde.stargate(-1).is_none());
    assert!(sde.station(-1).is_none());
    assert!(sde.agent(-1).is_none());
    assert!(sde.planet_schematic(u32::MAX).is_none());
    assert!(sde.school(u32::MAX).is_none());
    assert!(sde.race(u32::MAX).is_none());
    assert!(sde.bloodline(u32::MAX).is_none());
    assert!(sde.ancestry(u32::MAX).is_none());
    assert!(sde.faction(u32::MAX).is_none());
    assert!(sde.npc_corporation(-1).is_none());
    assert!(sde.randomized_type_materials_for(587).is_empty());

    // Empty SdeRegistry fallback paths when db_path is None
    let empty = SdeRegistry::default();
    assert!(empty.celestials_in_system(30000142).is_empty());
    assert!(empty.all_stars().is_empty());
    assert!(empty.location_solar_system(60003760).is_none());
    assert!(empty.location_solar_system_map().is_empty());
    assert!(empty.factions_owning_solar_systems().is_empty());
}

#[test]
fn test_corrupt_and_missing_sqlite_errors() {
    assert!(!SdeRegistry::default_db_path().as_os_str().is_empty());

    // Open nonexistent path -> error
    assert!(SdeRegistry::open("/tmp/nonexistent_evomu_sde_path_xyz.sqlite").is_err());

    // Open an empty SQLite file missing sde_resources -> error
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_sde_err_test_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let bad_db = tmp_dir.join("empty.sqlite");
    {
        let conn = rusqlite::Connection::open(&bad_db).expect("create empty sqlite");
        conn.execute("CREATE TABLE dummy (id INTEGER PRIMARY KEY)", [])
            .expect("create dummy");
    }
    assert!(SdeRegistry::open(&bad_db).is_err());

    // Clone canonical schema with empty tables to test minimal valid open + corrupt JSON/SQLite rows
    let sde = shared_sde();
    let (schema_ver, ddls): (String, Vec<String>) = {
        let src_conn = rusqlite::Connection::open(sde.db_path.as_ref().expect("seed db path"))
            .expect("open seed db");
        let ver: String = src_conn
            .query_row(
                "SELECT value FROM sde_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("read schema_version");
        let mut stmt = src_conn
            .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL")
            .expect("query ddl");
        let list = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("map ddl")
            .flatten()
            .collect();
        (ver, list)
    };

    let init_db_from_seed = |name: &str| -> std::path::PathBuf {
        let p = tmp_dir.join(name);
        let conn = rusqlite::Connection::open(&p).expect("open synth db");
        for ddl in &ddls {
            conn.execute(ddl, []).expect("exec ddl");
        }
        conn.execute(
            "INSERT INTO sde_meta (key, value) VALUES ('schema_version', ?1)",
            [&schema_ver],
        )
        .expect("insert sde_meta");
        p
    };

    // Open a directory path (!meta.is_file()) -> MissingDatabase error
    assert!(SdeRegistry::open(&tmp_dir).is_err());

    let synth_db = init_db_from_seed("synth_valid.sqlite");
    let synth_reg = SdeRegistry::open(&synth_db).expect("open empty valid schema");
    assert!(synth_reg.types.is_empty());

    // Wrong schema_version -> InvalidSchema error
    let bad_ver_db = init_db_from_seed("synth_bad_ver.sqlite");
    {
        let conn = rusqlite::Connection::open(&bad_ver_db).expect("open bad_ver_db");
        conn.execute(
            "UPDATE sde_meta SET value = '99.99' WHERE key = 'schema_version'",
            [],
        )
        .expect("update schema_version");
    }
    assert!(SdeRegistry::open(&bad_ver_db).is_err());

    // Missing developerDescription in sde_dbuff_collections -> error
    let bad_dbuff1 = init_db_from_seed("synth_dbuff1.sqlite");
    {
        let conn = rusqlite::Connection::open(&bad_dbuff1).expect("open bad_dbuff1");
        conn.execute(
            "INSERT INTO sde_dbuff_collections (dbuff_id, operation_name, data_json) VALUES (1, 'PostPercent', '{}')",
            [],
        )
        .expect("insert bad dbuff 1");
    }
    assert!(SdeRegistry::open(&bad_dbuff1).is_err());

    // Missing aggregateMode in sde_dbuff_collections -> error
    let bad_dbuff2 = init_db_from_seed("synth_dbuff2.sqlite");
    {
        let conn = rusqlite::Connection::open(&bad_dbuff2).expect("open bad_dbuff2");
        conn.execute(
            "INSERT INTO sde_dbuff_collections (dbuff_id, operation_name, data_json) VALUES (1, 'PostPercent', '{\"developerDescription\":\"desc\"}')",
            [],
        )
        .expect("insert bad dbuff 2");
    }
    assert!(SdeRegistry::open(&bad_dbuff2).is_err());

    // Missing targetMode in sde_fighter_abilities -> error
    let bad_ability = init_db_from_seed("synth_ability.sqlite");
    {
        let conn = rusqlite::Connection::open(&bad_ability).expect("open bad_ability");
        conn.execute(
            "INSERT INTO sde_fighter_abilities (ability_id, data_json) VALUES (1, '{}')",
            [],
        )
        .expect("insert bad ability");
    }
    assert!(SdeRegistry::open(&bad_ability).is_err());

    // Synthetic localization pickles using absolute paths in SdeResourceInfo (no env mutation)
    let main_path = tmp_dir.join("synth_loc_main.pickle");
    let de_path = tmp_dir.join("synth_loc_de.pickle");
    let valid_main = b"(dp0\nI1\nI2\nsS'languages'\n(lp1\nS'en-us'\np2\naS\"de\"\nasS'labels'\n(dp3\nI42\n(dp4\nS'FullPath'\nS'UI/Generic'\nsS'label'\nS'Ok'\nsS'messageID'\nI42\nsI9\nS'k'\nssI99\n(lsS'bad_id'\n(dI1\nS'v'\nss.";
    let valid_de = b"plain\nsI10\n(VAlpha\\u0020\\U0001f63a \\\\u0041 \\\\\\u0042 \\\\uZZZZ \\uD800 \\U00110000\nsI-5\n(VNegId\nsI20\n(VBeta\ns.";
    std::fs::write(&main_path, valid_main).expect("write synth main");
    std::fs::write(&de_path, valid_de).expect("write synth de");

    let mut loc_reg = SdeRegistry::default();
    let mk_abs = |res: &str, p: &std::path::Path| SdeResourceInfo {
        res_path: res.to_string(),
        cdn_relative_path: p.display().to_string(),
        md5: String::new(),
        byte_size: 16,
        format: String::new(),
        record_count: 1,
        spot: String::new(),
    };
    loc_reg.resources.insert(
        "res:/localizationfsd/localization_fsd_main.pickle".to_string(),
        mk_abs("res:/localizationfsd/localization_fsd_main.pickle", &main_path),
    );
    loc_reg.resources.insert(
        "res:/localizationfsd/localization_fsd_de.pickle".to_string(),
        mk_abs("res:/localizationfsd/localization_fsd_de.pickle", &de_path),
    );

    let bundle = loc_reg.load_localization_main().expect("parse synth main");
    assert_eq!(bundle.languages, vec!["en-us".to_string(), "de".to_string()]);
    assert_eq!(bundle.labels[&42].full_path, "UI/Generic");

    let mut sub_ids = HashSet::new();
    sub_ids.insert(10);
    let sub_de = loc_reg
        .load_localized_messages("de", &sub_ids)
        .expect("subset de");
    assert_eq!(sub_de.len(), 1);
    assert!(loc_reg.localized_message("de", 20).as_deref() == Some("Beta"));
    assert_eq!(loc_reg.localized_message("de", 999_999), None);

    // Corrupt main pickle variants
    for bad_main in [
        &b"(lp0\na."[..],
        &b"(dp0\nInot_an_int\n."[..],
        &b"(dp0\nS'unterminated\n."[..],
        &b"(dp0\ngbad\n."[..],
        &b"(dp0\ng55\n."[..],
    ] {
        std::fs::write(&main_path, bad_main).expect("write bad main");
        assert!(loc_reg.load_localization_main().is_err());
    }

    // Wrong SQLite column types on celestials/solar systems -> graceful fallbacks
    let weird_db = tmp_dir.join("weird_cols.sqlite");
    {
        let conn = rusqlite::Connection::open(&weird_db).expect("open weird db");
        conn.execute(
            "CREATE TABLE sde_celestials (celestial_id TEXT, system_id TEXT, type_id TEXT, group_id TEXT, name TEXT, radius REAL, x REAL, y REAL, z REAL, orbit_id INTEGER, orbit_index INTEGER, celestial_index INTEGER)",
            [],
        ).expect("create weird celestials");
        conn.execute(
            "INSERT INTO sde_celestials (celestial_id, system_id, group_id) VALUES ('bad', 'bad', 'bad')",
            [],
        ).expect("insert weird celestial");
        conn.execute(
            "CREATE TABLE sde_solar_systems (system_id TEXT, faction_id TEXT, region_id INTEGER)",
            [],
        ).expect("create weird systems");
        conn.execute(
            "INSERT INTO sde_solar_systems VALUES ('bad', 'bad', 10001004)",
            [],
        ).expect("insert weird system");
    }
    let mut weird_reg = SdeRegistry::default();
    weird_reg.db_path = Some(weird_db);
    assert!(weird_reg.celestials_in_system(30000142).is_empty());
    assert!(weird_reg.all_stars().is_empty());
    assert!(weird_reg.location_solar_system(40009087).is_none());
    assert!(weird_reg.location_solar_system_map().is_empty());
    assert!(weird_reg.factions_owning_solar_systems().is_empty());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
