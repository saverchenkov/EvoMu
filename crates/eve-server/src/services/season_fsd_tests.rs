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
