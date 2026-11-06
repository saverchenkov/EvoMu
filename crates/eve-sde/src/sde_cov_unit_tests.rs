//! Additional unit and mutation-killing tests for `eve-sde`.

use super::*;
use crate::cfsd_nested_tables::CfsdBuf;
use std::collections::{BTreeMap, HashMap};

#[test]
fn test_cov_cfsd_buf_and_record_bounds_and_strings() {
    let small = CfsdBuf(&[1, 2, 3, 4]);
    assert!(small.bytes(0, 8).is_err());
    assert!(small.u8(10).is_err());
    assert!(small.u32(2).is_err());
    assert!(small.u64(0).is_err());
    assert_eq!(small.str_at(0).unwrap(), "");
    assert!(small.counted(0).is_err());

    // Valid relative offset: abs = 32 + 8 = 40; len at 32..40 = 5; UTF-8 "Jita4" at 40..45
    let mut raw = vec![0u8; 64];
    raw[32..40].copy_from_slice(&5u64.to_le_bytes());
    raw[40..45].copy_from_slice(b"Jita4");
    raw[48..52].copy_from_slice(&2u32.to_le_bytes());

    let cbuf = CfsdBuf(&raw);
    assert_eq!(cbuf.str_at(0).unwrap(), "");
    assert_eq!(cbuf.str_at(8).unwrap(), "Jita4");
    assert_eq!(cbuf.u32(48).unwrap(), 2);

    // Invalid UTF-8 bytes at offset 40..42 -> InvalidSchema error
    let mut bad_utf8 = vec![0u8; 48];
    bad_utf8[32..40].copy_from_slice(&2u64.to_le_bytes());
    bad_utf8[40..42].copy_from_slice(&[0xFF, 0xFE]);
    let cbuf_bad = CfsdBuf(&bad_utf8);
    assert!(cbuf_bad.str_at(8).is_err());

    // CfsdRecord accessors and type mismatches
    let mut fields = BTreeMap::new();
    fields.insert("int_k".to_string(), CfsdValue::Int(123_456));
    fields.insert("neg_k".to_string(), CfsdValue::Int(-7));
    fields.insert("str_k".to_string(), CfsdValue::Str("Amarr".to_string()));
    fields.insert("list_k".to_string(), CfsdValue::IntList(vec![10, 20, 30]));
    let rec = CfsdRecord { key: 500, fields };
    assert_eq!(rec.int("int_k"), Some(123_456));
    assert_eq!(rec.int("neg_k"), Some(-7));
    assert_eq!(rec.int("str_k"), None);
    assert_eq!(rec.str("str_k"), Some("Amarr"));
    assert_eq!(rec.str("int_k"), None);
    assert_eq!(rec.list("list_k"), Some(&[10_i64, 20, 30][..]));
    assert_eq!(rec.list("int_k"), None);
    assert_eq!(rec.int("missing"), None);
}

#[test]
fn test_cov_real_sde_localization_secondary_suns_and_queries() {
    let sde = SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE");

    // Direct message tables across languages & missing entries
    let en_all = sde.load_all_localized_messages_en_us().unwrap();
    assert!(en_all.len() > 100_000);
    assert!(en_all.values().any(|v| v == "Rifter"));
    let de_all = sde.load_all_localized_messages("de").unwrap();
    assert!(de_all.len() > 100_000);

    let _ = sde.localized_type_description("de", 587);
    assert!(sde.localized_type_name("de", 999_999_999).is_none());
    let _ = sde.localized_type_name("de", 587);
    let _ = sde.localized_group_name("de", 25);
    assert!(sde.localized_group_name("de", 999_999).is_none());
    let _ = sde.localized_category_name("de", 6);
    assert!(sde.localized_category_name("de", 999_999).is_none());
    let _ = sde.localized_market_group_name("de", 4);
    let _ = sde.localized_market_group_description("de", 4);
    assert!(sde.localized_market_group_name("de", 999_999_999).is_none());
    let _ = sde.market_group_description(4);

    let mg_descs = sde.load_market_group_descriptions().unwrap();
    assert!(!mg_descs.is_empty());

    // Secondary suns & wormhole class lookup on real W-space systems
    let suns = sde.load_secondary_suns().unwrap();
    assert!(!suns.is_empty());
    let (&sample_wh_sys, &sample_sun) = suns.iter().next().unwrap();
    assert!(sample_sun.effect_beacon_type_id > 0);
    let looked_up = sde.secondary_sun_for_system(sample_wh_sys).unwrap();
    assert_eq!(looked_up.effect_beacon_type_id, sample_sun.effect_beacon_type_id);
    assert!(sde.secondary_sun_for_system(30_000_142).is_none());

    // Real SQLite skin & celestial helper queries
    let factions = sde.factions_owning_solar_systems();
    assert!(factions.len() >= 10);
    let cel_map = sde.location_solar_system_map();
    assert!(cel_map.len() >= 1000);
    assert_eq!(cel_map.get(&60_003_760), Some(&30_000_142));

    // TypeListInfo inclusion and exclusion checks
    let tl = TypeListInfo {
        type_list_id: 90_001,
        name: "CustomCovList".into(),
        included_type_ids: vec![587],
        excluded_type_ids: vec![588],
        included_group_ids: vec![25],
        excluded_group_ids: vec![29],
        included_category_ids: vec![],
        excluded_category_ids: vec![16],
    };
    assert!(tl.contains(sde.get_type(587).unwrap()));
    assert!(!tl.contains(sde.get_type(588).unwrap()));
    assert!(!tl.contains(sde.get_type(3300).unwrap()));
}

#[test]
fn test_cov_empty_and_error_fallbacks_across_sde() {
    let mut empty = SdeRegistry::default();
    assert!(empty.factions_owning_solar_systems().is_empty());
    assert!(empty.location_solar_system_map().is_empty());

    // Required skills with explicit 3 vs default omitted level (1) vs explicit 0
    let ship_tid = 88_001;
    let skill_a = 3300_u32;
    let skill_b = 3301_u32;
    let mut attrs = HashMap::new();
    attrs.insert(182, f64::from(skill_a));
    attrs.insert(277, 3.0);
    attrs.insert(183, f64::from(skill_b));
    let mut custom_type = SdeRegistry::with_canonical_seed()
        .types
        .into_values()
        .next()
        .unwrap();
    custom_type.type_id = ship_tid;
    custom_type.default_attributes = attrs;
    empty.types.insert(ship_tid, custom_type);
    let reqs = empty.required_skills_for_type(ship_tid);
    assert_eq!(reqs, HashMap::from([(skill_a, 3), (skill_b, 1)]));

    if let Some(t) = empty.types.get_mut(&ship_tid) {
        t.default_attributes.insert(278, 0.0);
    }
    let reqs_zero = empty.required_skills_for_type(ship_tid);
    assert_eq!(reqs_zero, HashMap::from([(skill_a, 3), (skill_b, 0)]));

    // fsd_dict_entries boundary cases (item_size < 8, overflowed pos, out-of-bounds body, empty footer)
    assert!(crate::static_domain_loaders::fsd_dict_entries(&[], 0, 4).is_empty());
    assert!(crate::static_domain_loaders::fsd_dict_entries(&[0; 16], usize::MAX, 8).is_empty());
    assert!(crate::static_domain_loaders::fsd_dict_entries(&[0; 8], 100, 8).is_empty());
    let mut bad_body = vec![0_u8; 16];
    bad_body[0..4].copy_from_slice(&500_u32.to_le_bytes());
    assert!(crate::static_domain_loaders::fsd_dict_entries(&bad_body, 0, 8).is_empty());

    // Missing SQLite path fallbacks on SdeRegistry queries
    let mut bad_db = SdeRegistry::default();
    bad_db.db_path = Some(std::path::PathBuf::from("/nonexistent/evomu_missing.sqlite"));
    assert!(bad_db.celestials_in_system(30_000_142).is_empty());
    assert!(bad_db.factions_owning_solar_systems().is_empty());
    assert!(bad_db.location_solar_system_map().is_empty());
}

#[test]
fn test_cov_skills_index_fighters_and_cfsd_mutants() {
    let mut sde = SdeRegistry::default();
    let template = SdeRegistry::with_canonical_seed()
        .types
        .into_values()
        .next()
        .unwrap();

    // All 6 skill/level attribute pairs + sk_id == 0 guard + unknown type
    assert!(sde.required_skills_for_type(999_999).is_empty());
    let mut six_skill_type = template.clone();
    six_skill_type.type_id = 91_001;
    six_skill_type.published = true;
    six_skill_type.category_id = 6;
    six_skill_type.market_group_id = Some(102);
    six_skill_type.meta_group_id = Some(4);
    six_skill_type.default_attributes = HashMap::from([
        (182, 3300.0),
        (277, 1.0),
        (183, 3301.0),
        (278, 2.0),
        (184, 3302.0),
        (279, 3.0),
        (1285, 3303.0),
        (1286, 4.0),
        (1289, 3304.0),
        (1287, 5.0),
        (1290, 0.0),
        (1288, 2.0),
    ]);
    sde.types.insert(91_001, six_skill_type);
    assert_eq!(
        sde.required_skills_for_type(91_001),
        HashMap::from([(3300, 1), (3301, 2), (3302, 3), (3303, 4), (3304, 5)])
    );

    // root_market_group_id multi-hop walk
    sde.market_groups.insert(
        100,
        MarketGroupInfo {
            market_group_id: 100,
            parent_group_id: None,
            name: "Root".into(),
            icon_id: None,
            has_types: false,
        },
    );
    sde.market_groups.insert(
        101,
        MarketGroupInfo {
            market_group_id: 101,
            parent_group_id: Some(100),
            name: "Mid".into(),
            icon_id: None,
            has_types: false,
        },
    );
    sde.market_groups.insert(
        102,
        MarketGroupInfo {
            market_group_id: 102,
            parent_group_id: Some(101),
            name: "Leaf".into(),
            icon_id: None,
            has_types: true,
        },
    );
    assert_eq!(sde.root_market_group_id(102), 100);
    assert_eq!(sde.root_market_group_id(999), 999);

    // Unpublished skipped, category 9 fallback root_mg=2, blueprint manufacturing skills merged, sorted tids
    let mut unpub = template.clone();
    unpub.type_id = 91_002;
    unpub.published = false;
    unpub.default_attributes = HashMap::from([(182, 3300.0), (277, 1.0)]);
    sde.types.insert(91_002, unpub);

    let mut no_mg = template.clone();
    no_mg.type_id = 91_003;
    no_mg.published = true;
    no_mg.category_id = 6;
    no_mg.market_group_id = None;
    no_mg.default_attributes = HashMap::from([(182, 3300.0), (277, 1.0)]);
    sde.types.insert(91_003, no_mg);

    let mut bp_high = template.clone();
    bp_high.type_id = 91_010;
    bp_high.published = true;
    bp_high.category_id = 9;
    bp_high.market_group_id = None;
    bp_high.meta_group_id = None;
    bp_high.default_attributes = HashMap::from([(182, 3300.0), (277, 2.0)]);
    sde.types.insert(91_010, bp_high);

    let mut bp_low = template;
    bp_low.type_id = 91_005;
    bp_low.published = true;
    bp_low.category_id = 9;
    bp_low.market_group_id = None;
    bp_low.meta_group_id = None;
    bp_low.default_attributes = HashMap::from([(182, 3300.0), (277, 2.0)]);
    sde.types.insert(91_005, bp_low);

    sde.blueprints.insert(
        91_010,
        BlueprintInfo {
            blueprint_type_id: 91_010,
            product_type_id: 91_001,
            product_quantity: 1,
            manufacturing_time_sec: 600,
            max_production_limit: 10,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![],
            activities: HashMap::from([(
                "manufacturing".into(),
                BlueprintActivity {
                    time: 600,
                    materials: vec![],
                    products: vec![],
                    skills: vec![
                        BlueprintSkill { type_id: 3300, level: 5 },
                        BlueprintSkill { type_id: 3380, level: 3 },
                    ],
                },
            )]),
        },
    );

    let idx = sde.required_skills_index();
    assert_eq!(idx[&3300][&1][&100][&4], vec![91_001]);
    assert_eq!(idx[&3300][&2][&2][&1], vec![91_005, 91_010]);
    assert_eq!(idx[&3380][&3][&2][&1], vec![91_010]);

    // fighter_ability_slots_with_info
    assert!(sde.fighter_ability_slots_with_info(91_001).is_empty());
    sde.fighter_abilities_by_type.insert(
        91_001,
        vec![
            FighterAbilitySlot {
                slot_index: 0,
                ability_id: 77,
                cooldown_seconds: Some(5.0),
                charges: None,
            },
            FighterAbilitySlot {
                slot_index: 1,
                ability_id: 88,
                cooldown_seconds: None,
                charges: None,
            },
        ],
    );
    sde.fighter_abilities.insert(
        77,
        FighterAbilityInfo {
            ability_id: 77,
            target_mode: "target".into(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: true,
            icon_id: None,
            display_name_id: Some(12),
            ability_name_id: None,
            tooltip_text_id: None,
            turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
    let fighter_slots = sde.fighter_ability_slots_with_info(91_001);
    assert_eq!(fighter_slots.len(), 1);
    assert_eq!(fighter_slots[0].0.ability_id, 77);
    assert!(fighter_slots[0].1.disallow_in_low_sec);

    // item_trader_component_for_type + load_cfsd_nested error paths
    assert_eq!(sde.item_trader_component_for_type(91_001).unwrap(), None);
    sde.space_components_by_type
        .insert(91_001, vec!["otherComponent".into(), "itemTrader".into()]);
    assert!(sde.item_trader_component_for_type(91_001).is_err());
    assert!(sde.load_cfsd_nested("res:/staticdata/unknown_nested.fsdbinary").is_err());
}

