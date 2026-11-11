//! Comprehensive edge, error, and happy-path unit tests for `season_manager_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::season_manager::ops::{
    active_fsd_epoch_window_ft, build_character_challenge, compute_challenge_expiration_ft,
    fill_and_normalize_character_challenges, load_fsd_seasons_catalog_clean,
    load_localized_challenge_definitions, localize_challenge_def, next_downtime_filetime_ticks,
    resolve_season_bounds_clean, resolve_season_end_time_readonly,
};
use eve_db::MiningLedgerRecord;
use std::collections::BTreeMap;

#[test]
fn season_ops_localization_schedule_expiration_and_challenge_fill() {
    let ctx = test_ctx();
    let now_ft = ctx.now_blueos_filetime_ticks();

    // 1. next_downtime_filetime_ticks boundaries (unix <= 0 clamped, before/after 11:00 UTC)
    let dt_zero = next_downtime_filetime_ticks(0);
    assert!(dt_zero > 0);
    let midnight_ft = (1_767_225_600_i64 + 11_644_473_600) * 10_000_000;
    let eleven_am_ft = midnight_ft + 11 * 3_600 * 10_000_000;
    assert_eq!(next_downtime_filetime_ticks(midnight_ft + 3_600 * 10_000_000), eleven_am_ft);
    assert_eq!(next_downtime_filetime_ticks(eleven_am_ft), eleven_am_ft + 86_400 * 10_000_000);

    // 2. active_fsd_epoch_window_ft covers now_ft with exact 30-day span
    let (ep_start, ep_end) = active_fsd_epoch_window_ft(now_ft);
    assert!(ep_start <= now_ft && now_ft < ep_end);
    assert_eq!(ep_end - ep_start, 30 * 86_400 * 10_000_000);

    // 3. compute_challenge_expiration_ft across all 6 branches
    let mut def = SeasonalChallengeDef {
        challenge_id: 700_001,
        season_id: 1,
        event_type: "totalMined".into(),
        title: "PvE #700001 (msg:0)".into(),
        description: "msg:0".into(),
        points: 15,
        max_progress: 100,
        expiration_time_minutes: 60,
        ..Default::default()
    };
    let future_end = now_ft + 10 * 86_400 * 10_000_000;
    let soon_end = now_ft + 600 * 10_000_000; // 10 min < 60 min
    assert_eq!(
        compute_challenge_expiration_ft(&def, now_ft, future_end),
        now_ft + 3_600 * 10_000_000
    );
    assert_eq!(compute_challenge_expiration_ft(&def, now_ft, soon_end), soon_end);
    assert_eq!(
        compute_challenge_expiration_ft(&def, now_ft, now_ft - 10),
        now_ft + 3_600 * 10_000_000
    );

    // Zero expiration_time_minutes, expires at downtime (capped by season_end_ft)
    def.expiration_time_minutes = 0;
    def.should_not_expire_at_downtime = false;
    assert_eq!(compute_challenge_expiration_ft(&def, now_ft, soon_end), soon_end);
    assert_eq!(
        compute_challenge_expiration_ft(&def, now_ft, now_ft - 1),
        next_downtime_filetime_ticks(now_ft)
    );

    // Zero expiration_time_minutes, should_not_expire_at_downtime = true
    def.should_not_expire_at_downtime = true;
    assert_eq!(compute_challenge_expiration_ft(&def, now_ft, future_end), future_end);
    assert_eq!(
        compute_challenge_expiration_ft(&def, now_ft, now_ft - 1),
        now_ft + 86_400 * 10_000_000
    );

    // 4. localize_challenge_def & load_localized_challenge_definitions (custom blob vs FSD fallback)
    let fsd_defs = crate::services::season_fsd::fsd_challenge_definitions(&ctx.sde);
    if let Some(first) = fsd_defs.first() {
        let mut copy = first.clone();
        copy.title = format!("Category #{} (msg:1)", copy.challenge_id);
        copy.description = "msg:1".into();
        localize_challenge_def(&mut copy, &ctx.sde);
        // Should not panic; if localized text exists in SDE it replaces placeholder
        assert!(!copy.title.is_empty());
    }

    let custom_defs = vec![def.clone()];
    let mut snap_custom = ctx.db.with_snapshot(|s| s.clone());
    snap_custom.seasons_and_campaigns.challenge_definitions = serde_json::from_str(&String::from(serde_json::to_string(&custom_defs).unwrap())).unwrap_or_default();
    let loaded_custom = load_localized_challenge_definitions(&snap_custom, &ctx.sde);
    assert_eq!(loaded_custom.len(), 1);
    assert_eq!(loaded_custom[0].challenge_id, 700_001);

    let snap_empty = ctx.db.with_snapshot(|s| s.clone());
    let loaded_fsd = load_localized_challenge_definitions(&snap_empty, &ctx.sde);
    assert!(!loaded_fsd.is_empty());

    // 5. load_fsd_seasons_catalog_clean, resolve_season_end_time_readonly, resolve_season_bounds_clean
    let clean_cat = load_fsd_seasons_catalog_clean(&snap_empty, &ctx.sde, now_ft);
    assert!(!clean_cat.is_empty());
    // Clean catalog does NOT invent ordinal 2..=4 aliases unless present in FSD
    let (first_sid, first_end) = clean_cat.iter().next().map(|(&id, c)| (id, c.end_time_ft)).unwrap();
    assert_eq!(
        resolve_season_end_time_readonly(&snap_empty, &ctx.sde, first_sid, now_ft),
        Some(first_end)
    );
    assert_eq!(
        resolve_season_end_time_readonly(&snap_empty, &ctx.sde, 999_888_777, now_ft),
        None
    );

    // Override via SEASON_CONFIG_KEY & SEASONS_CATALOG_KEY
    let override_cfg = SeasonConfig {
        season_id: 77,
        max_active_challenges: 6,
        max_season_points: 500,
        season_name: "Custom Override".into(),
        start_time_ft: now_ft,
        end_time_ft: now_ft + 20 * 86_400 * 10_000_000,
        ..Default::default()
    };
    let mut cat_map = BTreeMap::new();
    cat_map.insert(
        77_i64,
        SeasonConfig {
            season_id: 77,
            max_active_challenges: 2,
            max_season_points: 100,
            season_name: "Original".into(),
            start_time_ft: now_ft,
            end_time_ft: now_ft + 5 * 86_400 * 10_000_000,
            ..Default::default()
        },
    );
    snap_custom.seasons_and_campaigns.seasons_catalog = serde_json::from_str(&String::from(serde_json::to_string(&cat_map).unwrap())).unwrap_or_default();
    snap_custom.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&override_cfg).unwrap())).ok();
    let merged_cat = load_fsd_seasons_catalog_clean(&snap_custom, &ctx.sde, now_ft);
    assert_eq!(merged_cat[&77].season_name, "Custom Override");
    assert_eq!(merged_cat[&77].max_season_points, 500);
    assert_eq!(merged_cat[&77].end_time_ft, override_cfg.end_time_ft);
    assert_eq!(
        resolve_season_end_time_readonly(&snap_custom, &ctx.sde, 77, now_ft),
        Some(override_cfg.end_time_ft)
    );

    let mut ch_st = CharacterSeasonState::default();
    ch_st.selected_season_id = 77;
    let (b_end, b_max) = resolve_season_bounds_clean(&ch_st, &snap_custom, &ctx.sde, now_ft);
    assert_eq!(b_end, override_cfg.end_time_ft);
    assert_eq!(b_max, 6);

    // 6. build_character_challenge sets activity baseline + fill_and_normalize_character_challenges
    let char_id = 90_000_001;
    snap_custom.mining_ledger.push(MiningLedgerRecord {
        character_id: char_id,
        solar_system_id: 30_000_142,
        type_id: 1230,
        quantity: 250,
        date: "2026-11-08".into(),
        timestamp_unix: 1_800_000_000,
    });
    let built = build_character_challenge(&def, 77, &snap_custom, char_id, now_ft, future_end, false);
    assert_eq!(built.baseline_progress, 250);
    assert_eq!(built.expiration_date, future_end);
    assert!(!built.is_dormant);

    // Placeholder title/description on existing challenge gets normalized by fill_and_normalize_character_challenges
    let mut norm_def = def.clone();
    norm_def.challenge_id = 700_002;
    norm_def.title = "Real Localized Title".into();
    norm_def.description = "Real Localized Desc".into();
    norm_def.is_dormant = false;
    norm_def.expiration_time_minutes = 15;
    ch_st.challenges.insert(
        700_002,
        CharacterChallenge {
            challenge_id: 700_002,
            season_id: 77,
            title: "Category #700002 (msg:123)".into(),
            description: "msg:123".into(),
            points: 10,
            progress: 0,
            max_progress: 100,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date: now_ft - 10, // expired! triggers replacement & expiration normalization
            client_event_driven: false,
            reward_type_id: 0,
            reward_quantity: 0,
            baseline_progress: 0,
        },
    );
    let mut active_fill_def = norm_def.clone();
    active_fill_def.challenge_id = 700_003;
    let expired_pairs = fill_and_normalize_character_challenges(
        &mut ch_st,
        &[norm_def, active_fill_def],
        &snap_custom,
        char_id,
        now_ft,
        future_end,
        2,
    );
    assert!(!expired_pairs.is_empty());
    for (_, new_ch) in &expired_pairs {
        assert_eq!(
            new_ch.expiration_date,
            now_ft + 15 * 60 * 10_000_000
        );
        if let Some(live) = ch_st.challenges.get(&new_ch.challenge_id) {
            assert_eq!(live.expiration_date, new_ch.expiration_date);
        }
    }
}
