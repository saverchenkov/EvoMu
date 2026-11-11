//! Edge-case and branch-coverage unit tests for `services::season_manager_ops`.

use super::*;
use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[test]
fn localization_and_challenge_definitions_cover_blob_override_and_fsd_fallback() {
    let ctx = test_ctx();
    let fsd_defs = fsd_challenge_definitions(&ctx.sde);
    assert!(!fsd_defs.is_empty());

    // Pick a challenge present in FSD so fsd_localized_challenge_texts has an entry
    let sample_id = fsd_defs[0].challenge_id;
    let mut def = fsd_defs[0].clone();
    def.title = "Category #1 (msg:12345)".to_string();
    def.description = "msg:12345".to_string();
    localize_challenge_def(&mut def, &ctx.sde);
    // Calling again on already-localized text leaves it intact
    let localized_title = def.title.clone();
    let localized_desc = def.description.clone();
    localize_challenge_def(&mut def, &ctx.sde);
    assert_eq!(def.title, localized_title);
    assert_eq!(def.description, localized_desc);

    // load_localized_challenge_definitions: absent blob -> FSD; empty blob -> FSD; custom blob -> custom
    ctx.db.with_mut_snapshot(|snap| {
        let from_fsd = load_localized_challenge_definitions(snap, &ctx.sde);
        assert_eq!(from_fsd.len(), fsd_defs.len());

        snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str(&String::from("[]".to_string())).unwrap_or_default();
        let from_empty = load_localized_challenge_definitions(snap, &ctx.sde);
        assert_eq!(from_empty.len(), fsd_defs.len());

        let custom = vec![SeasonalChallengeDef {
            challenge_id: sample_id,
            season_id: 1,
            event_type: "totalMined".to_string(),
            title: String::new(),
            description: String::new(),
            points: 25,
            max_progress: 100,
            ..Default::default()
        }];
        snap.seasons_and_campaigns.challenge_definitions = serde_json::from_str(&String::from(serde_json::to_string(&custom).unwrap())).unwrap_or_default();
        let from_blob = load_localized_challenge_definitions(snap, &ctx.sde);
        assert_eq!(from_blob.len(), 1);
        assert_eq!(from_blob[0].points, 25);
    });
}

#[test]
fn downtime_epoch_catalog_override_and_season_bounds_cover_all_branches() {
    let ctx = test_ctx();
    let base_unix = 1_700_006_400_i64; // 2023-11-15T00:00:00Z
    let before_dt_ft = (base_unix + 10 * 3_600 + EPOCH_DIFF_SECS) * FILETIME_TICKS_PER_SEC;
    let at_dt_ft = (base_unix + 11 * 3_600 + EPOCH_DIFF_SECS) * FILETIME_TICKS_PER_SEC;
    let after_dt_ft = (base_unix + 12 * 3_600 + EPOCH_DIFF_SECS) * FILETIME_TICKS_PER_SEC;

    assert_eq!(next_downtime_filetime_ticks(before_dt_ft), at_dt_ft);
    assert_eq!(
        next_downtime_filetime_ticks(at_dt_ft),
        at_dt_ft + 86_400 * FILETIME_TICKS_PER_SEC
    );
    assert_eq!(
        next_downtime_filetime_ticks(after_dt_ft),
        at_dt_ft + 86_400 * FILETIME_TICKS_PER_SEC
    );

    let (s1, e1) = active_fsd_epoch_window_ft(DEFAULT_SEASON_BASE_FT + 500);
    assert_eq!(s1, DEFAULT_SEASON_BASE_FT);
    assert_eq!(e1 - s1, DEFAULT_SEASON_DURATION_FT);
    let (s2, e2) = active_fsd_epoch_window_ft(DEFAULT_SEASON_BASE_FT - 500);
    assert_eq!(e2, DEFAULT_SEASON_BASE_FT);
    assert_eq!(e2 - s2, DEFAULT_SEASON_DURATION_FT);

    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Clean FSD load (no blobs)
        let cat = load_fsd_seasons_catalog_clean(snap, &ctx.sde, now_ft);
        assert!(!cat.is_empty());

        // 2. SEASON_CONFIG_KEY override applied on top of FSD catalog
        let first_sid = *cat.keys().next().unwrap();
        let override_cfg = SeasonConfig {
            season_id: first_sid,
            template_id: first_sid,
            max_active_challenges: 6,
            max_season_points: 2_500,
            season_name: "Overridden Season".to_string(),
            end_time_ft: now_ft + 90 * 86_400 * FILETIME_TICKS_PER_SEC,
            start_time_ft: now_ft,
            ..Default::default()
        };
        snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&override_cfg).unwrap())).ok();
        let cat_ov = load_fsd_seasons_catalog_clean(snap, &ctx.sde, now_ft);
        assert_eq!(cat_ov[&first_sid].max_active_challenges, 6);
        assert_eq!(cat_ov[&first_sid].max_season_points, 2_500);
        assert_eq!(cat_ov[&first_sid].season_name, "Overridden Season");

        // resolve_season_end_time_readonly with explicit future end_time_ft -> Some
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, first_sid, now_ft),
            Some(override_cfg.end_time_ft)
        );
        // resolve_season_end_time_readonly with explicit past end_time_ft -> None
        let past_cfg = SeasonConfig {
            end_time_ft: now_ft - 1_000,
            max_season_points: 0,
            season_name: String::new(),
            ..override_cfg.clone()
        };
        snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&past_cfg).unwrap())).ok();
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, first_sid, now_ft),
            None
        );

        // SEASONS_CATALOG_KEY present + SEASON_CONFIG_KEY with end_time_ft == 0
        let mut custom_cat = BTreeMap::new();
        custom_cat.insert(
            42,
            SeasonConfig {
                season_id: 42,
                max_active_challenges: 3,
                max_season_points: 500,
                season_name: "Custom 42".to_string(),
                start_time_ft: now_ft - 1_000,
                end_time_ft: now_ft + 50_000_000_000,
                ..Default::default()
            },
        );
        snap.seasons_and_campaigns.seasons_catalog = serde_json::from_str(&String::from(serde_json::to_string(&custom_cat).unwrap())).unwrap_or_default();
        snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::json!({
                "season_id": 42,
                "max_active_challenges": 5,
                "max_season_points": 0,
                "season_name": "",
                "end_time_ft": 0
            })
            .to_string())).ok();
        let cat_blob = load_fsd_seasons_catalog_clean(snap, &ctx.sde, now_ft);
        assert_eq!(cat_blob[&42].max_active_challenges, 5);
        assert_eq!(cat_blob[&42].max_season_points, 500);
        assert_eq!(cat_blob[&42].season_name, "Custom 42");
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, 42, now_ft),
            Some(now_ft + 50_000_000_000)
        );
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, 0, now_ft),
            Some(now_ft + 50_000_000_000)
        );

        // resolve_season_bounds_clean
        let mut st = CharacterSeasonState::default();
        st.selected_season_id = 9999; // falls back to active season 42
        let (end_ft, max_ch) = resolve_season_bounds_clean(&st, snap, &ctx.sde, now_ft);
        assert_eq!(end_ft, now_ft + 50_000_000_000);
        assert_eq!(max_ch, 5);
    });
}

#[test]
fn challenge_expiration_build_and_fill_normalize_cover_all_branches() {
    let ctx = test_ctx();
    let now_ft = ctx.now_blueos_filetime_ticks();
    let season_end_ft = now_ft + 10_000_000_000;
    let past_end_ft = now_ft - 1_000;

    let timed_def = SeasonalChallengeDef {
        challenge_id: 101,
        season_id: 1,
        event_type: "totalMined".to_string(),
        title: "Timed".to_string(),
        description: "Timed desc".to_string(),
        points: 10,
        max_progress: 50,
        expiration_time_minutes: 60,
        ..Default::default()
    };
    let expected_timed = now_ft + 60 * 60 * FILETIME_TICKS_PER_SEC;
    assert_eq!(
        compute_challenge_expiration_ft(&timed_def, now_ft, now_ft + 1_000),
        now_ft + 1_000
    );
    assert_eq!(
        compute_challenge_expiration_ft(&timed_def, now_ft, past_end_ft),
        expected_timed
    );

    let dt_def = SeasonalChallengeDef {
        expiration_time_minutes: 0,
        should_not_expire_at_downtime: false,
        ..timed_def.clone()
    };
    assert_eq!(
        compute_challenge_expiration_ft(&dt_def, now_ft, now_ft + 100),
        now_ft + 100
    );
    assert_eq!(
        compute_challenge_expiration_ft(&dt_def, now_ft, past_end_ft),
        next_downtime_filetime_ticks(now_ft)
    );

    let season_def = SeasonalChallengeDef {
        expiration_time_minutes: 0,
        should_not_expire_at_downtime: true,
        ..timed_def.clone()
    };
    assert_eq!(
        compute_challenge_expiration_ft(&season_def, now_ft, season_end_ft),
        season_end_ft
    );
    assert_eq!(
        compute_challenge_expiration_ft(&season_def, now_ft, past_end_ft),
        now_ft + 86_400 * FILETIME_TICKS_PER_SEC
    );

    ctx.db.with_snapshot(|snap| {
        let ch = build_character_challenge(&timed_def, 1, snap, 90_000_001, now_ft, season_end_ft, true);
        assert!(ch.is_dormant);
        assert_eq!(ch.expiration_date, expected_timed.min(season_end_ft));

        let mut st = CharacterSeasonState::default();
        st.selected_season_id = 1;
        // Pre-populate an expired placeholder challenge and an active placeholder challenge
        st.challenges.insert(
            101,
            CharacterChallenge {
                challenge_id: 101,
                season_id: 1,
                title: "Category #1 (msg:1)".to_string(),
                description: "msg:1".to_string(),
                points: 10,
                progress: 0,
                max_progress: 50,
                completed: false,
                claimed: false,
                is_dormant: false,
                expiration_date: now_ft - 1, // expired!
                client_event_driven: false,
                reward_type_id: 0,
                reward_quantity: 0,
                baseline_progress: 0,
            },
        );
        let defs = vec![
            timed_def.clone(),
            SeasonalChallengeDef {
                challenge_id: 102,
                title: "Second".to_string(),
                description: "Second desc".to_string(),
                ..season_def.clone()
            },
        ];
        let expired = fill_and_normalize_character_challenges(
            &mut st,
            &defs,
            snap,
            90_000_001,
            now_ft,
            season_end_ft,
            2,
        );
        assert!(!expired.is_empty());
        for c in st.challenges.values() {
            assert!(!c.title.contains("(msg:"));
            assert!(!c.description.starts_with("msg:"));
        }
    });
}

#[test]
fn season_end_time_negative_exact_boundary_zero_with_negative_now_and_bounds_mutants() {
    let ctx = test_ctx();
    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.seasons_catalog = BTreeMap::from([(
            77,
            SeasonConfig {
                season_id: 77,
                max_active_challenges: 3,
                max_season_points: 600,
                season_name: "Boundary Season".into(),
                start_time_ft: now_ft - 10_000,
                end_time_ft: now_ft + 25_000,
                ..Default::default()
            },
        )]);

        // 1. Negative cfg.end_time_ft (< 0) falls back to active catalog season instead of returning None (kills L285 `> -> <`)
        snap.seasons_and_campaigns.season_configuration = Some(SeasonConfig {
            season_id: 77,
            end_time_ft: -5,
            max_active_challenges: 3,
            ..Default::default()
        });
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, 77, now_ft),
            Some(now_ft + 25_000)
        );

        // 2. cfg.end_time_ft == 0 with negative now_ft skips override & uses catalog instead of >= 0 returning Some(0) (kills L285 `> -> >=`)
        snap.seasons_and_campaigns.season_configuration.as_mut().unwrap().end_time_ft = 0;
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, 77, -100),
            Some(now_ft + 25_000)
        );

        // 3. cfg.end_time_ft == now_ft (> 0) returns None because end_time must be strictly > now_ft (kills L286 `> -> ==` & `> -> >=`)
        snap.seasons_and_campaigns.season_configuration.as_mut().unwrap().end_time_ft = now_ft;
        assert_eq!(
            resolve_season_end_time_readonly(snap, &ctx.sde, 77, now_ft),
            None
        );

        // 4. resolve_season_bounds_clean returns exact non-zero/non-one (end_ft, max_ch) tuple (kills L305)
        snap.seasons_and_campaigns.season_configuration = Some(SeasonConfig {
            season_id: 77,
            end_time_ft: now_ft + 25_000,
            max_active_challenges: 6,
            ..Default::default()
        });
        let st = CharacterSeasonState {
            selected_season_id: 77,
            ..Default::default()
        };
        assert_eq!(
            resolve_season_bounds_clean(&st, snap, &ctx.sde, now_ft),
            (now_ft + 25_000, 6)
        );
    });
}
