#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use eve_db::WorldStateSnapshot;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

#[test]
fn season_fsd_catalog_merge_and_condition_matching_edges() {
    let ctx = test_ctx();
    let base_ft = 133_700_000_000_000_000_i64;

    // 1. default_fsd_seasons with canonical SDE vs empty SDE
    let full_seasons = default_fsd_seasons(&ctx.sde, base_ft);
    assert!(!full_seasons.is_empty());
    let empty_sde = SdeRegistry::default();
    let empty_seasons = default_fsd_seasons(&empty_sde, base_ft);
    assert!(empty_seasons.is_empty());

    // 2. load_seasons_catalog & load_season_config with invalid JSON, valid catalog, and SEASON_CONFIG_KEY merge
    let mut snap = WorldStateSnapshot::default();
    assert_eq!(load_season_config(&snap).season_id, 1);

    snap.seasons_and_campaigns.seasons_catalog = serde_json::from_str("{not-json").unwrap_or_default();
    snap.seasons_and_campaigns.season_configuration = serde_json::from_str("{bad-json").ok();
    let fallback_cat = load_seasons_catalog(&snap, &ctx.sde, base_ft);
    assert!(!fallback_cat.is_empty());
    assert_eq!(load_season_config(&snap).season_id, 1);

    // Valid SEASONS_CATALOG_KEY + SEASON_CONFIG_KEY override
    let mut custom_map = BTreeMap::new();
    custom_map.insert(
        5,
        SeasonConfig {
            season_id: 5,
            max_active_challenges: 3,
            max_season_points: 200,
            season_name: "Original".into(),
            start_time_ft: base_ft,
            end_time_ft: base_ft + 10_000,
            ..Default::default()
        },
    );
    snap.seasons_and_campaigns.seasons_catalog = serde_json::from_str(&String::from(serde_json::to_string(&custom_map).unwrap())).unwrap_or_default();
    let override_cfg = SeasonConfig {
        season_id: 5,
        max_active_challenges: 6,
        max_season_points: 500,
        season_name: "Winter Nexus".into(),
        start_time_ft: base_ft + 1_000,
        end_time_ft: base_ft + 999_999,
        ..Default::default()
    };
    snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&override_cfg).unwrap())).ok();
    assert_eq!(load_season_config(&snap).season_name, "Winter Nexus");
    let merged = load_seasons_catalog(&snap, &ctx.sde, base_ft);
    assert_eq!(merged[&5].max_season_points, 500);
    assert_eq!(merged[&5].season_name, "Winter Nexus");
    assert_eq!(merged[&5].end_time_ft, base_ft + 999_999);
    assert_eq!(merged[&1].season_name, "Winter Nexus");

    // 3. matches_challenge_conditions edge branches
    let mut cond_catalog = BTreeMap::new();
    cond_catalog.insert("ship_group".to_string(), 7);

    let mut def = SeasonalChallengeDef::default();
    // Empty allowed_vals vector inside event_conditions -> continues and returns true
    def.event_conditions.insert("ship_group".into(), vec![]);
    assert!(matches_challenge_conditions(&def, None, &cond_catalog));

    // Unknown condition key when cond_catalog is non-empty -> false
    def.event_conditions.insert("unknown_cond".into(), vec![1]);
    let ed_any = PyRep::Dict(vec![(PyRep::String("unknown_cond".into()), PyRep::Integer(1))]);
    assert!(!matches_challenge_conditions(&def, Some(&ed_any), &cond_catalog));
    def.event_conditions.remove("unknown_cond");

    def.event_conditions.insert("ship_group".into(), vec![25, 26]);
    // KeyVal extra_data
    let ed_kv = PyRep::keyval([("ship_group", PyRep::Integer(25))]);
    assert!(matches_challenge_conditions(&def, Some(&ed_kv), &cond_catalog));
    // Non-dict/non-keyval extra_data -> false
    assert!(!matches_challenge_conditions(&def, Some(&PyRep::Integer(25)), &cond_catalog));
    // Missing key in Dict -> false
    let ed_empty_dict = PyRep::Dict(vec![]);
    assert!(!matches_challenge_conditions(&def, Some(&ed_empty_dict), &cond_catalog));
    // Tuple value matching allowed_vals -> true
    let ed_tuple = PyRep::Dict(vec![(
        PyRep::String("ship_group".into()),
        PyRep::Tuple(vec![PyRep::Integer(26)]),
    )]);
    assert!(matches_challenge_conditions(&def, Some(&ed_tuple), &cond_catalog));
    // String value (non-i64/list/tuple) -> false
    let ed_str = PyRep::Dict(vec![(
        PyRep::String("ship_group".into()),
        PyRep::String("25".into()),
    )]);
    assert!(!matches_challenge_conditions(&def, Some(&ed_str), &cond_catalog));

    // 4. load_challenge_definitions & load_challenge_definitions_with_sde custom/empty/invalid JSON
    snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str("[]").unwrap_or_default();
    assert!(!load_challenge_definitions(&snap).is_empty());
    assert!(!load_challenge_definitions_with_sde(&snap, &empty_sde).is_empty());

    let custom_defs = vec![SeasonalChallengeDef {
        challenge_id: 77_001,
        season_id: 1,
        event_type: "client_test".into(),
        title: "Custom".into(),
        description: "Desc".into(),
        points: 5,
        max_progress: 1,
        ..Default::default()
    }];
    snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str(&String::from(serde_json::to_string(&custom_defs).unwrap())).unwrap_or_default();
    assert_eq!(load_challenge_definitions(&snap)[0].challenge_id, 77_001);
    assert_eq!(
        load_challenge_definitions_with_sde(&snap, &ctx.sde)[0].challenge_id,
        77_001
    );
}
