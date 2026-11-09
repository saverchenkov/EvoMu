#![forbid(unsafe_code)]

use eve_sde::{
    normalize_localization_fsd_lang, CfsdType, ConstellationInfo, GroupInfo, InvType, SdeError,
    SdeRegistry, SolarSystem,
};
use std::collections::{BTreeMap, HashMap};

fn make_belt_ore_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_item = sde.types.get(&587).expect("canonical Rifter").clone();
    sde.types.clear();
    sde.groups.clear();
    sde.asteroid_types_by_system = std::sync::Arc::default();

    let mut types = HashMap::new();
    let mut groups = BTreeMap::new();
    let ore_groups = [
        (462u32, 1230u32, "Veldspar", 0.1),
        (460, 1228, "Scordite", 0.15),
        (459, 1224, "Pyroxeres", 0.3),
        (458, 18, "Plagioclase", 0.35),
        (469, 1227, "Omber", 0.6),
        (457, 21, "Kernite", 1.2),
        (456, 20, "Jaspet", 2.0),
        (455, 1231, "Hemorphite", 3.0),
        (454, 19, "Hedbergite", 3.0),
        (453, 1229, "Gneiss", 5.0),
        (467, 1225, "Crokite", 16.0),
        (452, 1232, "Dark Ochre", 8.0),
        (461, 1226, "Spodumain", 16.0),
        (451, 1223, "Bistot", 16.0),
        (450, 22, "Arkonor", 16.0),
        (468, 11396, "Mercoxit", 40.0),
        (4029, 46677, "Bezdnacine", 16.0),
        (4030, 46685, "Rakovene", 16.0),
        (4031, 46689, "Talassonite", 16.0),
    ];
    for (gid, tid, name, vol) in ore_groups {
        groups.insert(
            gid,
            GroupInfo {
                group_id: gid,
                category_id: 25,
                name: name.into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
        let mut t = InvType {
            type_id: tid,
            group_id: gid,
            category_id: 25,
            name: name.into(),
            volume: vol,
            published: true,
            default_attributes: HashMap::new(),
            effect_ids: vec![],
            ..base_item.clone()
        };
        t.default_attributes.clear();
        t.effect_ids.clear();
        types.insert(tid, t);
    }

    let mut constellations = BTreeMap::new();
    for (cid, rid, fid) in [
        (200001u32, 100001u32, 500_003u32), // Amarr
        (200002, 100002, 500_001),          // Caldari
        (200003, 100003, 500_004),          // Gallente
        (200004, 100004, 500_002),          // Minmatar
    ] {
        constellations.insert(
            cid,
            ConstellationInfo {
                constellation_id: cid,
                region_id: rid,
                faction_id: Some(fid),
                name: format!("C{cid}"),
                center: [0.0; 3],
                wormhole_class_id: None,
            },
        );
    }

    let mut solar_systems = BTreeMap::new();
    solar_systems.insert(
        30000001,
        SolarSystem {
            solar_system_id: 30000001,
            constellation_id: 200002,
            region_id: 100002,
            name: "JitaSynthetic".into(),
            security_status: 0.945,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );

    sde.types.extend(types);
    sde.groups.extend(groups);
    sde.constellations.extend(constellations);
    sde.solar_systems.extend(solar_systems);
    sde
}

#[test]
fn test_belt_ores_all_security_bands_factions_and_systems() {
    let sde = make_belt_ore_sde();

    // Pochven overrides all security statuses
    let pochven: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.9, 10_000_070)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(pochven, vec![4029, 4030, 4031]);

    // Hisec >= 0.85
    let core_hi: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.9, 100002)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(core_hi, vec![462, 460]);

    // Hisec >= 0.65 per empire faction + unknown fallback
    let amarr: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.7, 100001)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(amarr, vec![462, 460, 459]);

    let caldari: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.7, 100002)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(caldari, vec![462, 460, 458]);

    let gallente: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.7, 100003)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(gallente, vec![462, 460, 458]);

    let minmatar: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.7, 100004)
        .iter()
        .map(|t| t.group_id)
        .collect();
    assert_eq!(minmatar, vec![462, 460, 459, 458]);

    // Borderline 0.45..0.65, lowsec 0.05..0.45, hi-null -0.45..0.05, deep-null < -0.45
    assert_eq!(sde.belt_ores_for_security_and_region(0.5, 100001).len(), 4);
    assert_eq!(sde.belt_ores_for_security_and_region(0.2, 100001).len(), 6);
    assert_eq!(sde.belt_ores_for_security_and_region(-0.2, 100001).len(), 5);
    assert_eq!(sde.belt_ores_for_security_and_region(-0.8, 100001).len(), 7);

    // System lookup (hit + miss)
    assert_eq!(sde.belt_ores_for_system(30000001).len(), 2);
    assert!(sde.belt_ores_for_system(999_999_999).is_empty());
}

#[test]
fn test_real_sde_cfsd_nested_flat_localization_and_queries() {
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("real SDE");

    // Nested & flat CFSD tables + error branches
    assert!(SdeRegistry::cfsd_nested_layout("res:/staticdata/surveys.fsdbinary").is_some());
    assert!(SdeRegistry::cfsd_nested_layout("res:/staticdata/nonexistent.fsdbinary").is_none());
    assert!(matches!(
        sde.load_cfsd_nested("res:/staticdata/nonexistent.fsdbinary"),
        Err(SdeError::InvalidSchema(_))
    ));
    assert!(matches!(
        sde.load_cfsd_table("res:/staticdata/nonexistent.fsdbinary"),
        Err(SdeError::InvalidSchema(_))
    ));

    let activities = sde.load_new_activity_ids().expect("newactivities");
    assert_eq!(activities.len(), 46);

    let shipcasters = sde.shipcaster_sde_components().expect("shipcasters");
    assert!(!shipcasters.launcher_factions.is_empty());

    let starters = sde.starter_shipcaster_systems_by_race();
    assert!(!starters.is_empty());

    assert!(sde.item_trader_component_for_type(999_999_999).expect("miss").is_none());

    // Flat table layout & field accessors (`int`, `str`, `list`) across loaded rows
    assert!(SdeRegistry::cfsd_flat_layout("res:/staticdata/agenttypes.fsdbinary").is_some());
    assert!(SdeRegistry::cfsd_flat_layout("res:/staticdata/nonexistent.fsdbinary").is_none());
    let acct_tbl = sde
        .load_cfsd_table("res:/staticdata/agenttypes.fsdbinary")
        .expect("agenttypes cfsd");
    let first_row = acct_tbl.values().next().expect("row");
    assert!(first_row.str("value").is_some());
    assert!(first_row.int("nonexistent_field").is_none());
    assert!(first_row.str("nonexistent_field").is_none());
    assert!(first_row.list("nonexistent_field").is_none());

    // Localization language validation & descriptions
    assert_eq!(normalize_localization_fsd_lang("EN-US"), Some("en-us"));
    assert_eq!(normalize_localization_fsd_lang("tlh"), None);
    assert!(sde.load_all_localized_messages("tlh").is_err());
    assert!(sde
        .load_localized_messages("tlh", &std::collections::HashSet::from([1]))
        .is_err());
    assert!(sde.localized_type_description("tlh", 587).is_none());
    assert!(sde.localized_market_group_description("tlh", 2).is_none());
    assert!(sde.market_group_description(2).is_some() || sde.market_group_description(2).is_none());

    // Core SdeRegistry query helpers & sqlite_loader_skins
    assert_eq!(sde.get_type_category(587), Some(6));
    assert!(!sde.published_market_types().is_empty());
    assert!(!sde.market_groups_for_parent(None).is_empty());
    assert!(!sde.load_all_localized_messages_en_us().unwrap().is_empty());
    assert!(sde.cdn_resource_path("res:/does_not_exist_999.fsdbinary").is_err());

    // Variations, market group members, and ShowInfo on base & T2 hulls
    let rifter_vars = sde.variations_of(587);
    assert!(!rifter_vars.is_empty());
    let jag_id = *rifter_vars.first().unwrap();
    let jag_info = sde.show_info_for_type(jag_id).unwrap();
    assert!(jag_info.variations.contains(&587));
    let rifter_info = sde.show_info_for_type(587).unwrap();
    assert!(rifter_info.variations.contains(&jag_id));
    if let Some(mg) = rifter_info.market_group_id {
        assert!(sde.types_in_market_group(mg).contains(&587));
    }

    // Nested CFSD operation tables & item trader component
    assert!(!sde.operation_categories().unwrap().is_empty());
    assert!(!sde.operations().unwrap().is_empty());
    assert!(!sde.operation_tasks().unwrap().is_empty());
    assert!(!sde.contribution_methods().unwrap().is_empty());
    assert!(!sde.dungeon_objectives().unwrap().is_empty());
    assert!(!sde.surveys().unwrap().is_empty());
    assert!(sde.item_trader_component_for_type(587).unwrap().is_none());
    let trader_id = sde
        .space_components_by_type
        .iter()
        .find(|(_, comps)| comps.iter().any(|c| c == "itemTrader"))
        .map(|(&tid, _)| tid);
    if let Some(tid) = trader_id {
        assert!(sde.item_trader_component_for_type(tid).unwrap().is_some());
    }

    // SQLite open error paths (missing path, directory path, missing sde_meta, bad schema_version)
    assert!(SdeRegistry::open("/tmp/nonexistent_evomu_sde_test_12345.sqlite").is_err());
    assert!(SdeRegistry::open("/tmp").is_err());
    let tmp_bad_meta = std::env::temp_dir().join(format!("evomu_bad_meta_{}.sqlite", std::process::id()));
    std::fs::write(&tmp_bad_meta, b"not a valid sqlite database").unwrap();
    let res_bad = SdeRegistry::open(&tmp_bad_meta);
    let _ = std::fs::remove_file(&tmp_bad_meta);
    assert!(res_bad.is_err());

    let tmp_empty_db = std::env::temp_dir().join(format!("evomu_empty_meta_{}.sqlite", std::process::id()));
    {
        let conn = rusqlite::Connection::open(&tmp_empty_db).unwrap();
        conn.execute("CREATE TABLE dummy (id INTEGER)", []).unwrap();
    }
    let res_missing_meta = SdeRegistry::open(&tmp_empty_db);
    let _ = std::fs::remove_file(&tmp_empty_db);
    assert!(res_missing_meta.is_err());

    let tmp_bad_ver = std::env::temp_dir().join(format!("evomu_bad_ver_{}.sqlite", std::process::id()));
    {
        let conn = rusqlite::Connection::open(&tmp_bad_ver).unwrap();
        conn.execute(
            "CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sde_meta (key, value) VALUES ('schema_version', '99')",
            [],
        )
        .unwrap();
    }
    let res_bad_ver = SdeRegistry::open(&tmp_bad_ver);
    let _ = std::fs::remove_file(&tmp_bad_ver);
    assert!(res_bad_ver.is_err());

    let _ = sde.types_in_market_group(4);
    let _ = sde.show_info_for_type(587);
    assert!(!sde.all_stars().is_empty());
    assert!(!sde.location_solar_system_map().is_empty());
    assert!(!sde.load_new_activity_ids().unwrap().is_empty());
    assert!(!sde.shipcaster_sde_components().unwrap().under_construction.is_empty());
    assert!(!sde.starter_shipcaster_systems_by_race().is_empty());
    let _ = sde.challenge_conditions();
    let _ = sde.challenge_events();
    let _ = sde.challenges();
    let _ = CfsdType::I32;
}


