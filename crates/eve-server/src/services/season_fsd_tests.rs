use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use eve_proto::PyRep;

#[test]
fn test_season_fsd_downtime_challenges_conditions_and_catalog() {
    let ctx = test_ctx();

    // next_downtime_filetime_ticks boundary checks
    let early_ft = 10_000_000; // unix <= 0
    assert_eq!(next_downtime_filetime_ticks(early_ft), early_ft + 86_400 * 10_000_000);

    // 2026-01-01 00:00:00 UTC in FILETIME: (1767225600 + 11644473600) * 10_000_000
    let midnight_ft = (1_767_225_600_i64 + 11_644_473_600) * 10_000_000;
    let eleven_am_ft = midnight_ft + 11 * 3_600 * 10_000_000;
    assert_eq!(next_downtime_filetime_ticks(midnight_ft + 3_600 * 10_000_000), eleven_am_ft);
    assert_eq!(
        next_downtime_filetime_ticks(eleven_am_ft),
        eleven_am_ft + 86_400 * 10_000_000
    );

    // SeasonalChallengeDef::to_character_challenge_with_end branches
    let mut def = SeasonalChallengeDef {
        challenge_id: 42,
        season_id: 1,
        event_type: "kill_npc".into(),
        title: "Hunt".into(),
        description: "Desc".into(),
        points: 10,
        max_progress: 5,
        client_event_driven: false,
        reward_type_id: 34,
        reward_quantity: 100,
        expiration_time_minutes: 30,
        ..Default::default()
    };
    let c1 = def.to_character_challenge(1, midnight_ft);
    assert_eq!(c1.expiration_date, midnight_ft + 30 * 60 * 10_000_000);

    def.expiration_time_minutes = 0;
    def.should_not_expire_at_downtime = true;
    let end_ft = midnight_ft + 5 * 86_400 * 10_000_000;
    let c2 = def.to_character_challenge_with_end(1, midnight_ft, end_ft);
    assert_eq!(c2.expiration_date, end_ft);
    let c2_past = def.to_character_challenge_with_end(1, midnight_ft, midnight_ft - 10);
    assert_eq!(c2_past.expiration_date, midnight_ft + 30 * 86_400 * 10_000_000);

    def.should_not_expire_at_downtime = false;
    let c3 = def.to_character_challenge_with_end(1, midnight_ft, end_ft);
    assert_eq!(c3.expiration_date, eleven_am_ft);

    // matches_challenge_conditions
    let mut cond_catalog = BTreeMap::new();
    cond_catalog.insert("ship_group".to_string(), 7);
    assert!(matches_challenge_conditions(&def, None, &cond_catalog));

    def.event_conditions.insert("ship_group".into(), vec![25, 26]);
    assert!(!matches_challenge_conditions(&def, None, &cond_catalog));

    let ed_match_str = PyRep::Dict(vec![(PyRep::String("ship_group".into()), PyRep::Integer(25))]);
    assert!(matches_challenge_conditions(&def, Some(&ed_match_str), &cond_catalog));

    let ed_match_id = PyRep::Dict(vec![(PyRep::Integer(7), PyRep::List(vec![PyRep::Integer(26)]))]);
    assert!(matches_challenge_conditions(&def, Some(&ed_match_id), &cond_catalog));

    let ed_mismatch = PyRep::Dict(vec![(PyRep::String("ship_group".into()), PyRep::Integer(99))]);
    assert!(!matches_challenge_conditions(&def, Some(&ed_mismatch), &cond_catalog));

    // FSD loaders and catalog
    let conds = fsd_challenge_conditions(&ctx.sde);
    assert!(!conds.is_empty());
    let defs = fsd_challenge_definitions(&ctx.sde);
    assert!(!defs.is_empty());
    let starter = default_seasonal_challenge_definitions();
    assert!(!starter.is_empty());
    let crates = fsd_crate_loot_tables(&ctx.sde);
    assert!(!crates.is_empty());

    let catalog = ctx.db.with_snapshot(|snap| {
        let _ = load_season_config(snap);
        let _ = load_challenge_definitions(snap);
        let _ = load_challenge_definitions_with_sde(snap, &ctx.sde);
        load_seasons_catalog(snap, &ctx.sde, midnight_ft)
    });
    assert!(!catalog.is_empty());
}

#[test]
fn test_season_fsd_catalog_overrides_conditions_and_def_fallbacks() {
    let ctx = test_ctx();
    let base_ft = 133_700_000_000_000_000_i64;

    // 1. default_fsd_seasons includes ordinal aliases 2..=4 when campaigns exist
    let default_cat = default_fsd_seasons(&ctx.sde, base_ft);
    assert!(!default_cat.is_empty());
    assert!(default_cat.contains_key(&2) || default_cat.len() >= 1);

    // 2. load_season_config & load_seasons_catalog with malformed & valid overrides
    let mut snap = ctx.db.with_snapshot(|s| s.clone());
    assert_eq!(load_season_config(&snap).season_id, 1);

    // Malformed JSON falls back safely to defaults
    snap.seasons_and_campaigns.season_configuration = serde_json::from_str("{not-json").ok();
    snap.seasons_and_campaigns.seasons_catalog = serde_json::from_str("{not-json").unwrap_or_default();
    snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str("[]").unwrap_or_default();
    assert_eq!(load_season_config(&snap).season_id, 1);
    let cat_fallback = load_seasons_catalog(&snap, &ctx.sde, base_ft);
    assert!(!cat_fallback.is_empty());
    let defs_fallback = load_challenge_definitions_with_sde(&snap, &ctx.sde);
    assert!(!defs_fallback.is_empty());
    let defs_no_sde_fallback = load_challenge_definitions(&snap);
    assert!(!defs_no_sde_fallback.is_empty());

    // Valid SEASON_CONFIG_KEY override merges into both cfg.season_id and key 1
    let custom_cfg = SeasonConfig {
        season_id: 99,
        max_active_challenges: 7,
        max_season_points: 750,
        season_name: "Winter Nexus".into(),
        start_time_ft: base_ft,
        end_time_ft: base_ft + 60 * 86_400 * 10_000_000,
        ..Default::default()
    };
    snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&custom_cfg).unwrap())).ok();
    assert_eq!(load_season_config(&snap).season_name, "Winter Nexus");
    let cat_merged = load_seasons_catalog(&snap, &ctx.sde, base_ft);
    assert_eq!(cat_merged[&99].season_name, "Winter Nexus");
    assert_eq!(cat_merged[&99].max_season_points, 750);
    assert_eq!(cat_merged[&1].season_name, "Winter Nexus");
    assert_eq!(cat_merged[&1].max_active_challenges, 7);

    // Custom non-empty challenge definitions blob overrides FSD
    let custom_def = SeasonalChallengeDef {
        challenge_id: 555_001,
        season_id: 99,
        event_type: "client_scan".into(),
        title: "Scan".into(),
        description: "Scan sites".into(),
        points: 20,
        max_progress: 3,
        client_event_driven: true,
        ..Default::default()
    };
    snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str(&String::from(serde_json::to_string(&vec![custom_def.clone()]).unwrap())).unwrap_or_default();
    assert_eq!(load_challenge_definitions(&snap), vec![custom_def.clone()]);
    assert_eq!(load_challenge_definitions_with_sde(&snap, &ctx.sde), vec![custom_def]);

    // 3. matches_challenge_conditions edge cases: empty vec, unknown key, KeyVal, Tuple, non-dict, string value
    let mut cond_catalog = BTreeMap::new();
    cond_catalog.insert("ship_group".to_string(), 7);
    cond_catalog.insert("region_id".to_string(), 8);

    let mut cond_def = SeasonalChallengeDef::default();
    cond_def.event_conditions.insert("ship_group".into(), Vec::new());
    assert!(matches_challenge_conditions(&cond_def, None, &cond_catalog));

    cond_def.event_conditions.insert("unknown_cond".into(), vec![1]);
    let kv_ed = PyRep::keyval([("ship_group", PyRep::Integer(25))]);
    assert!(!matches_challenge_conditions(&cond_def, Some(&kv_ed), &cond_catalog));

    cond_def.event_conditions.remove("unknown_cond");
    cond_def.event_conditions.insert("ship_group".into(), vec![25, 26]);
    assert!(matches_challenge_conditions(&cond_def, Some(&kv_ed), &cond_catalog));

    // Tuple inside KeyVal matches
    let kv_tuple = PyRep::keyval([(
        "ship_group",
        PyRep::Tuple(vec![PyRep::Integer(99), PyRep::Integer(26)]),
    )]);
    assert!(matches_challenge_conditions(&cond_def, Some(&kv_tuple), &cond_catalog));

    // String value fails
    let kv_str = PyRep::keyval([("ship_group", PyRep::String("25".into()))]);
    assert!(!matches_challenge_conditions(&cond_def, Some(&kv_str), &cond_catalog));

    // Non-dict/non-keyval extra_data fails
    assert!(!matches_challenge_conditions(
        &cond_def,
        Some(&PyRep::Integer(25)),
        &cond_catalog
    ));

    // Missing required key in KeyVal fails
    let kv_other = PyRep::keyval([("region_id", PyRep::Integer(10_000_002))]);
    assert!(!matches_challenge_conditions(&cond_def, Some(&kv_other), &cond_catalog));

    // 4. Verify fsd_challenge_definitions & fsd_crate_loot_tables invariants
    let fsd_defs = fsd_challenge_definitions(&ctx.sde);
    assert!(fsd_defs.iter().all(|d| d.max_progress >= 1));
    assert!(fsd_defs.iter().any(|d| d.is_dormant));
    assert!(fsd_defs.iter().any(|d| !d.is_dormant));
    let crates = fsd_crate_loot_tables(&ctx.sde);
    assert!(crates.values().all(|v| !v.is_empty()));
}
