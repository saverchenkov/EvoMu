//! Unit tests for `season_manager` (season state blobs, challenge progress, reward claims, career goals).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{IndustryJobRecord, KillmailRecord, MiningLedgerRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

#[cfg(test)]
#[path = "season_manager_ops_edge_tests.rs"]
mod season_manager_ops_edge_tests;

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn season_session(char_id: i64, gm: bool) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    if gm {
        s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    }
    s
}

#[test]
fn season_state_activity_counters_and_challenge_helpers() {
    let ctx = test_ctx();
    let cid = 970_001_i64;
    let now_ft = ctx.now_blueos_filetime_ticks();

    let ch = default_challenge(9_000_001, 1, now_ft);
    assert_eq!(ch.challenge_id, 9_000_001);
    assert_eq!(ch.season_id, 1);
    assert!(ch.client_event_driven);

    let kv = challenge_to_keyval(&ch, 250);
    assert_eq!(kv.dict_get("challengeID").and_then(|v| v.as_i64()), Some(9_000_001));
    assert_eq!(kv.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(250));

    let mut st = CharacterSeasonState::default();
    st.selected_season_id = 1;
    st.season_points = 100;
    st.challenges.insert(ch.challenge_id, ch.clone());

    ctx.db.with_mut_snapshot(|snap| {
        save_season_state_to_blobs(snap, cid, &st);
        let loaded = load_season_state(snap, cid);
        assert_eq!(loaded.season_points, 100);
        assert!(loaded.challenges.contains_key(&9_000_001));

        // Seed mining ledger, killmail, hacking win, and delivered industry job
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: cid,
            solar_system_id: 30000142,
            type_id: 1230,
            quantity: 450,
            date: "2026-11-05".to_string(),
            timestamp_unix: 1_000,
        });
        snap.killmails.insert(
            77_001,
            KillmailRecord {
                kill_id: 77_001,
                victim_character_id: 3_000_123, // NPC character ID not in snap.characters
                attacker_character_id: cid,
                solar_system_id: 30000142,
                ship_type_id: 587,
                total_value_isk: 50_000.0,
                ..Default::default()
            },
        );
        snap.seasons_and_campaigns.hacking_wins.insert(cid, 3);
        snap.industry_jobs.insert(
            88_001,
            IndustryJobRecord {
                job_id: 88_001,
                installer_id: cid,
                facility_id: 60003760,
                blueprint_item_id: 99_001,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 2,
                product_type_id: 587,
                status: "Delivered".to_string(),
                duration_seconds: 60,
                ..Default::default()
            },
        );

        assert_eq!(get_current_activity_count("volumeMined", snap, cid), 450);
        assert_eq!(get_current_activity_count("destroyHostileNPC", snap, cid), 1);
        assert_eq!(get_current_activity_count("hackingMiniGameWon", snap, cid), 3);
        assert_eq!(get_current_activity_count("industryJobsCompleted", snap, cid), 1);
        assert_eq!(get_current_activity_count("unknownEvent", snap, cid), 0);

        ensure_challenges(&mut st, now_ft);
        sync_character_challenges(&mut st, snap, cid);
        save_season_state_to_blobs(snap, cid, &st);

        let quasar_list = get_character_challenges_for_quasar(snap, Some(&ctx.sde), now_ft, cid);
        assert!(!quasar_list.is_empty());
    });
}

#[test]
fn season_manager_rpcs_events_claims_and_goals() {
    let ctx = test_ctx();
    let cid = 970_010_i64;
    let kw = HashMap::new();
    let mut sess = season_session(cid, false);

    // Unauthenticated session fails
    let mut unauth = SessionState::default();
    let unauth_err = handle_season_manager(
        &ctx,
        &mut unauth,
        "seasonManager",
        "get_max_active_challenges",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert!(!unauth_err.code.is_empty());

    let max_act = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_max_active_challenges",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_max_active_challenges");
    assert!(max_act.as_i64().unwrap_or(0) >= 1);

    let end_time = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_end_time",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_end_time");
    assert!(matches!(end_time, PyRep::Integer(_) | PyRep::None));

    let sdata = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_data_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_data_for_character");
    assert!(sdata.dict_get("seasonID").is_some() || sdata.dict_get("season_id").is_some());

    let challenges = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("fill_up_and_return_challenges_for_character");
    assert!(matches!(challenges, PyRep::Dict(_) | PyRep::List(_)));

    // Seed a completable client-driven challenge & test process_client_event + claim_challenge_rewards
    let test_ch_id = 55_001_i64;
    let now_ft = ctx.now_blueos_filetime_ticks();
    with_season_state(&ctx, cid, |st, _snap| {
        st.selected_season_id = 1;
        let mut c = default_challenge(test_ch_id, 1, now_ft);
        c.max_progress = 2;
        c.points = 50;
        st.challenges.insert(test_ch_id, c);
        Ok(())
    })
    .unwrap();

    // Challenge status queries
    let get_ch = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(test_ch_id)],
        &kw,
    )
    .unwrap()
    .expect("get_challenge");
    assert_eq!(get_ch.dict_get("challengeID").and_then(|v| v.as_i64()), Some(test_ch_id));

    assert_eq!(
        handle_season_manager(
            &ctx,
            &mut sess,
            "seasonManager",
            "challenge_is_dormant",
            &[PyRep::Integer(test_ch_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    assert!(handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(test_ch_id)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Process client event twice to complete challenge (step is capped at 1 per event)
    for _ in 0..2 {
        handle_season_manager(
            &ctx,
            &mut sess,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(test_ch_id), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .expect("process_client_event");
    }

    // Claim completed challenge reward
    handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(test_ch_id)],
        &kw,
    )
    .unwrap()
    .expect("claim_challenge_rewards");

    let prog = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_character_progress_by_character_id");
    assert!(prog.dict_get("seasonPoints").is_some() || prog.dict_get("season_points").is_some());

    // reload_fsd_data requires GM role
    let role_err =
        handle_season_manager(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[], &kw)
            .unwrap()
            .unwrap_err();
    assert!(!role_err.code.is_empty());
    let mut gm = season_session(cid, true);
    let gm_ok = handle_season_manager(&ctx, &mut gm, "seasonManager", "reload_fsd_data", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(gm_ok, PyRep::Bool(true));

    assert!(handle_season_manager(&ctx, &mut gm, "", "", &[], &kw).is_none());
    assert!(handle_season_manager(&ctx, &mut gm, "seasonManager", "", &[], &kw).is_none());
}

#[test]
fn season_manager_catalog_queries_dormancy_expiration_and_claim_ops() {
    let ctx = test_ctx();
    let cid = 940_022_i64;
    let now_ft = ctx.now_blueos_filetime_ticks() + 864_000_000_000;
    let mut sess = season_session(cid, false);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.corporation_id = 1_000_165;
        snap.characters.insert(cid, ch);

        let mut state = CharacterSeasonState::default();
        state.selected_season_id = 1;
        state.season_points = 500;
        let mut c1 = default_challenge(101, 1, now_ft);
        c1.progress = 5;
        c1.max_progress = 10;
        c1.points = 50;
        state.challenges.insert(101, c1);
        let mut c2 = default_challenge(102, 1, now_ft);
        c2.progress = 3;
        c2.max_progress = 3;
        c2.completed = true;
        c2.claimed = false;
        c2.points = 75;
        state.challenges.insert(102, c2);
        save_season_state_to_blobs(snap, cid, &state);
    });

    // Max active challenges, season end time, season selection, and character challenge list
    let max_active = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_max_active_challenges",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_max_active_challenges");
    assert!(max_active.as_i64().unwrap() >= 1);

    let end_time = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_end_time",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_end_time");
    assert!(end_time.as_i64().unwrap_or(0) > 0);

    let season_data = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_data_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_data_for_character");
    assert!(season_data != PyRep::None);

    let filled = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("fill_up_and_return_challenges_for_character");
    assert!(matches!(filled, PyRep::Dict(_) | PyRep::List(_)));

    let is_dormant = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "challenge_is_dormant",
        &[PyRep::Integer(101)],
        &kw,
    )
    .unwrap()
    .expect("challenge_is_dormant");
    assert_eq!(is_dormant, PyRep::Bool(false));

    let exp_ft = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(101)],
        &kw,
    )
    .unwrap()
    .expect("get_challenge_expiration_date");
    assert!(exp_ft.as_i64().unwrap_or(0) > now_ft);

    // Claim completed challenge 102 and verify season_points increased by 75
    let claim_rep = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(102)],
        &kw,
    )
    .unwrap()
    .expect("claim challenge 102");
    assert!(claim_rep != PyRep::None);

    let updated = ctx.db.with_snapshot(|s| load_season_state(s, cid));
    assert_eq!(updated.season_points, 575);
}

fn assert_v24_season_keyval_contract(kv: &PyRep, expected_title_id: Option<i64>, expected_hint_id: Option<i64>) {
    assert!(kv.dict_get("season_id").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("template_id").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("title").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("news").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("navigation_card_picture_path").and_then(|v| v.as_str()).is_some());
    assert!(kv.dict_get("content_card_picture_path").and_then(|v| v.as_str()).is_some());
    assert!(kv.dict_get("background_no_logo").and_then(|v| v.as_str()).is_some());
    assert!(kv.dict_get("start_time").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("end_time").and_then(|v| v.as_i64()).is_some());
    assert!(kv.dict_get("multiple_season_running").and_then(|v| v.as_bool()).is_some());
    assert!(matches!(kv.dict_get("tracked_event_types"), Some(PyRep::List(_))));
    assert_eq!(
        kv.dict_get("agency_card_title").and_then(|v| v.as_i64()),
        expected_title_id
    );
    assert_eq!(
        kv.dict_get("agency_card_hint").and_then(|v| v.as_i64()),
        expected_hint_id
    );
    if expected_title_id.is_none() {
        assert_eq!(kv.dict_get("agency_card_title"), Some(&PyRep::None));
    }
    if expected_hint_id.is_none() {
        assert_eq!(kv.dict_get("agency_card_hint"), Some(&PyRep::None));
    }
    // Must stay None to avoid CCP's `self.agencyCardDescription` typo crash in CustomContentGroupCard.
    assert_eq!(kv.dict_get("agency_card_description"), Some(&PyRep::None));
    assert_eq!(kv.dict_get("is_auto_claim_enabled"), Some(&PyRep::Bool(false)));
    assert_eq!(kv.dict_get("feature_tag_id"), Some(&PyRep::None));
}

#[test]
fn season_config_keyval_emits_v24_agency_selection_and_service_fields() {
    let cfg = eve_db::SeasonConfig::default();
    let kv = season_config_to_keyval(&cfg, &[]);
    assert_v24_season_keyval_contract(
        &kv,
        Some(cfg.title_message_id),
        Some(cfg.hint_message_id),
    );

    let zero_msg_cfg = eve_db::SeasonConfig {
        title_message_id: 0,
        hint_message_id: 0,
        ..Default::default()
    };
    let zero_kv = season_config_to_keyval(&zero_msg_cfg, &[]);
    assert_v24_season_keyval_contract(&zero_kv, None, None);

    // Multi-season unselected list & selected single KeyVal contract via RPC handler (#1029)
    let ctx = test_ctx();
    let cid = 970_099_i64;
    let kw = HashMap::new();
    let mut sess = season_session(cid, false);
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.season_configuration = None;
        snap.seasons_and_campaigns.seasons_catalog.clear();
        for sid in [10_i64, 20_i64] {
            snap.seasons_and_campaigns.seasons_catalog.insert(
                sid,
                eve_db::SeasonConfig {
                    season_id: sid,
                    template_id: sid,
                    start_time_ft: now_ft - 1_000,
                    end_time_ft: now_ft + 100_000_000,
                    multiple_season_running: true,
                    ..Default::default()
                },
            );
        }
    });

    let unsel = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_data_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_data_for_character unselected");
    let PyRep::List(cards) = unsel else {
        panic!("expected List of season KeyVals when unselected, got {unsel:?}");
    };
    assert_eq!(cards.len(), 2);
    for card in &cards {
        assert_v24_season_keyval_contract(card, Some(583_333), Some(583_335));
    }

    let sel_res = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "select_season",
        &[PyRep::Integer(10)],
        &kw,
    )
    .unwrap()
    .expect("select_season");
    assert_eq!(sel_res, PyRep::Bool(true));

    let selected = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_season_data_for_character",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_season_data_for_character selected");
    assert_eq!(
        selected.dict_get("season_id").and_then(|v| v.as_i64()),
        Some(10)
    );
    assert_v24_season_keyval_contract(&selected, Some(583_333), Some(583_335));
}

#[test]
fn r11_k12_challenge_keyval_completed_event_noop_and_progress_fields() {
    let ctx = test_ctx();
    let cid = 970_199_i64;
    let kw = HashMap::new();
    let mut sess = season_session(cid, false);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        snap.characters.insert(cid, ch.clone());
        ch.user_id
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 1, Some(cid), true, ctx.now_unix());

    let ch = default_challenge(9_110_001, 1, now_ft);
    let kv = challenge_to_keyval(&ch, 250);
    assert_eq!(kv.dict_get("challenge_id").and_then(|v| v.as_i64()), Some(9_110_001));
    assert_eq!(kv.dict_get("season_id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(kv.dict_get("season_points").and_then(|v| v.as_i64()), Some(250));
    assert_eq!(kv.dict_get("max_progress").and_then(|v| v.as_i64()), kv.dict_get("maxProgress").and_then(|v| v.as_i64()));
    assert_eq!(kv.dict_get("is_dormant").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(kv.dict_get("expiration_date").and_then(|v| v.as_i64()), kv.dict_get("expirationDate").and_then(|v| v.as_i64()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.challenge_definitions = vec![eve_db::SeasonalChallengeDef {
            challenge_id: 9_110_001,
            season_id: 1,
            event_type: "client_r11_test_event".into(),
            title: "R11 Test Challenge".into(),
            description: "Complete once".into(),
            points: 45,
            max_progress: 1,
            client_event_driven: true,
            expiration_time_minutes: 60,
            ..Default::default()
        }];
        let mut st = CharacterSeasonState::default();
        st.selected_season_id = 1;
        let mut c = default_challenge(9_110_001, 1, now_ft);
        c.points = 45;
        c.max_progress = 1;
        st.challenges.insert(9_110_001, c);
        save_season_state_to_blobs(snap, cid, &st);
    });

    let _ = ctx.drain_notifications_for_char(cid);
    handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::Integer(9_110_001)],
        &kw,
    )
    .unwrap()
    .expect("first completion");
    let first_notices = ctx.drain_notifications_for_char(cid);
    assert!(first_notices.iter().any(|n| n.event == "OnChallengeProgressUpdate"));
    assert!(first_notices.iter().any(|n| n.event == "OnChallengeCompleted"));

    // Subsequent events against an already-completed challenge must not emit duplicate progress updates
    handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::Integer(9_110_001)],
        &kw,
    )
    .unwrap()
    .expect("idempotent by id");
    handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("client_r11_test_event".into())],
        &kw,
    )
    .unwrap()
    .expect("idempotent by event name");
    let dup_notices = ctx.drain_notifications_for_char(cid);
    assert!(
        !dup_notices.iter().any(|n| n.event == "OnChallengeProgressUpdate"),
        "expected no duplicate OnChallengeProgressUpdate, got {dup_notices:?}"
    );

    handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(9_110_001)],
        &kw,
    )
    .unwrap()
    .expect("claim challenge");

    let prog = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
        &kw,
    )
    .unwrap()
    .expect("progress keyval");
    assert_eq!(prog.dict_get("character_id").and_then(|v| v.as_i64()), Some(cid));
    assert_eq!(prog.dict_get("seasonal_points").and_then(|v| v.as_i64()), Some(45));
    assert_eq!(prog.dict_get("claimedRewards").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(prog.dict_get("current_reward_index").and_then(|v| v.as_i64()), Some(1));
    let goal_notifs = ctx.drain_notifications_for_char(cid);
    let goal_completed = goal_notifs
        .iter()
        .find(|n| n.event == "OnSeasonalGoalCompleted")
        .expect("OnSeasonalGoalCompleted emitted");
    assert!(matches!(goal_completed.args[1].dict_get("omega_only"), Some(PyRep::Bool(_))));
    assert!(
        goal_completed.args[1]
            .dict_get("reward_type_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );
}

fn assert_v24_character_progress_payload(
    cp: &PyRep,
    expected_char_id: i64,
    expected_season_id: i64,
    expected_points: i64,
    expected_max_points: i64,
) {
    assert_eq!(cp.dict_get("character_id").and_then(|v| v.as_i64()), Some(expected_char_id));
    assert_eq!(cp.dict_get("characterID").and_then(|v| v.as_i64()), Some(expected_char_id));
    assert_eq!(cp.dict_get("season_id").and_then(|v| v.as_i64()), Some(expected_season_id));
    assert_eq!(cp.dict_get("seasonID").and_then(|v| v.as_i64()), Some(expected_season_id));
    // #1044: seasonal_points and max_seasonal_points (> 0)
    assert_eq!(cp.dict_get("seasonal_points").and_then(|v| v.as_i64()), Some(expected_points));
    assert_eq!(cp.dict_get("season_points").and_then(|v| v.as_i64()), Some(expected_points));
    assert_eq!(cp.dict_get("total_points").and_then(|v| v.as_i64()), Some(expected_points));
    assert_eq!(cp.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(expected_points));
    assert_eq!(
        cp.dict_get("max_seasonal_points").and_then(|v| v.as_i64()),
        Some(expected_max_points)
    );

    // #1043: seasonal_goals Dict with goal entries + next_seasonal_goal / last_seasonal_goal
    let Some(PyRep::Dict(goals)) = cp.dict_get("seasonal_goals") else {
        panic!("character_progress.seasonal_goals must be PyRep::Dict, got {:?}", cp.dict_get("seasonal_goals"));
    };
    assert!(!goals.is_empty(), "seasonal_goals must not be empty");
    for (idx, (k, goal)) in goals.iter().enumerate() {
        assert_eq!(k.as_i64(), Some(idx as i64));
        assert!(matches!(goal, PyRep::Dict(_)), "each seasonal goal must be a PyRep::Dict");
        let req_pts = goal.dict_get("points_required").and_then(|v| v.as_i64()).expect("points_required");
        assert!(req_pts > 0);
        assert!(goal.dict_get("reward_type_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert!(goal.dict_get("reward_amount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
        assert!(matches!(goal.dict_get("omega_only"), Some(PyRep::Bool(_))));
        assert_eq!(goal.dict_get("completed"), Some(&PyRep::Bool(expected_points >= req_pts)));
        assert_eq!(goal.dict_get("claimed"), Some(&PyRep::Bool(false)));
    }
    if expected_points >= expected_max_points {
        assert_eq!(cp.dict_get("next_seasonal_goal"), Some(&PyRep::None));
    } else {
        assert!(matches!(cp.dict_get("next_seasonal_goal"), Some(PyRep::Dict(_))));
    }
    assert!(matches!(cp.dict_get("last_seasonal_goal"), Some(PyRep::Dict(_))));

    // challenges must be a Dict of seasons.common.challenge.Challenge ObjectEx2 instances
    let Some(PyRep::Dict(ch_entries)) = cp.dict_get("challenges") else {
        panic!("character_progress.challenges must be PyRep::Dict, got {:?}", cp.dict_get("challenges"));
    };
    for (k, v) in ch_entries {
        let Some(cid) = k.as_i64() else {
            panic!("challenge dict key must be Integer, got {k:?}");
        };
        assert_eq!(v.dict_get("challenge_id").and_then(|x| x.as_i64()), Some(cid));
        assert_eq!(v.dict_get("challenge_id").and_then(|x| x.as_i64()), Some(cid));
        assert_eq!(v.dict_get("challengeID").and_then(|x| x.as_i64()), Some(cid));
        assert_eq!(v.dict_get("character_id").and_then(|x| x.as_i64()), Some(expected_char_id));
        assert!(v.dict_get("is_dormant").and_then(|x| x.as_bool()).is_some());
        assert!(v.dict_get("is_expired").and_then(|x| x.as_bool()).is_some());
        assert!(v.dict_get("points_awarded").and_then(|x| x.as_i64()).is_some());
        assert!(v.dict_get("max_progress").and_then(|x| x.as_i64()).unwrap_or(0) >= 1);
        let PyRep::ObjectEx { is_type_2: true, header, .. } = v else {
            panic!("challenge entry must be PyRep::ObjectEx2 for seasons.common.challenge.Challenge, got {v:?}");
        };
        let PyRep::Tuple(parts) = header.as_ref() else {
            panic!("Challenge ObjectEx header must be Tuple");
        };
        assert_eq!(
            parts.first(),
            Some(&PyRep::Tuple(vec![PyRep::Token("seasons.common.challenge.Challenge".into())]))
        );
    }
}

#[test]
fn r11_k12_issues_1043_1044_1045_1046_select_season_and_character_progress_fields() {
    let ctx = test_ctx();
    let cid = 970_243_i64;
    let kw = HashMap::new();
    let mut sess = season_session(cid, false);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        snap.characters.insert(cid, ch.clone());
        ch.user_id
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 1, Some(cid), true, ctx.now_unix());

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.seasons_catalog.insert(
            2211,
            eve_db::SeasonConfig {
                season_id: 2211,
                template_id: 2211,
                max_active_challenges: 4,
                max_season_points: 100,
                season_name: "Issue 1043/1044/1045/1046 Season".into(),
                start_time_ft: now_ft - 3_600 * 10_000_000,
                end_time_ft: now_ft + 7 * 86_400 * 10_000_000,
                ..Default::default()
            },
        );
        let mut st = CharacterSeasonState::default();
        st.season_points = 25;
        let mut incomplete_ch = default_challenge(9_221_101, 2211, now_ft);
        incomplete_ch.points = 15;
        incomplete_ch.max_progress = 3;
        st.challenges.insert(9_221_101, incomplete_ch);

        let mut completed_ch = default_challenge(9_221_102, 2211, now_ft);
        completed_ch.points = 20;
        completed_ch.progress = 1;
        completed_ch.max_progress = 1;
        completed_ch.completed = true;
        completed_ch.claimed = false;
        st.challenges.insert(9_221_102, completed_ch);
        save_season_state_to_blobs(snap, cid, &st);
    });

    // Login path before select_season (#1045 challenges Dict + #1046 character_id + #1043/#1044)
    let login_prog = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_character_progress_by_character_id on login");
    assert_v24_character_progress_payload(&login_prog, cid, 2211, 25, 100);

    let _ = ctx.drain_notifications_for_char(cid);
    let sel = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "select_season",
        &[PyRep::Integer(2211)],
        &kw,
    )
    .unwrap()
    .expect("select_season 2211");
    assert_eq!(sel, PyRep::Bool(true));

    let notifs = ctx.drain_notifications_for_char(cid);
    let on_sel = notifs
        .iter()
        .find(|n| n.event == "OnSeasonSelected")
        .expect("OnSeasonSelected emitted");
    assert_eq!(on_sel.args.len(), 3);
    assert_v24_character_progress_payload(&on_sel.args[2], cid, 2211, 25, 100);

    // Completed & unclaimed challenge must carry non-None reward_date and None finish_date
    // so V24 Challenge.has_unclaimed_completion_rewards() evaluates True without KeyVal AttributeError.
    let sel_challenges = on_sel.args[2].dict_get("challenges").unwrap();
    let PyRep::Dict(sel_ch_map) = sel_challenges else { panic!("expected challenges Dict") };
    let ch_unclaimed = sel_ch_map
        .iter()
        .find(|(k, _)| k.as_i64() == Some(9_221_102))
        .map(|(_, v)| v)
        .expect("completed challenge 9_221_102");
    assert_ne!(ch_unclaimed.dict_get("reward_date"), Some(&PyRep::None));
    assert_eq!(ch_unclaimed.dict_get("finish_date"), Some(&PyRep::None));
    assert_eq!(ch_unclaimed.dict_get("points_awarded").and_then(|v| v.as_i64()), Some(20));

    // get_character_progress_by_character_id must return the same V24 Dict + goal structure
    let prog = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_character_progress_by_character_id");
    assert_v24_character_progress_payload(&prog, cid, 2211, 25, 100);

    // Next goal with 25 points (on 10-pt steps) is points_required=30; when >= 100 next_seasonal_goal is None
    assert_eq!(
        prog.dict_get("next_seasonal_goal")
            .and_then(|g| g.dict_get("points_required"))
            .and_then(|v| v.as_i64()),
        Some(30)
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, cid);
        st.season_points = 100;
        save_season_state_to_blobs(snap, cid, &st);
    });
    let prog_full = handle_season_manager(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
        &kw,
    )
    .unwrap()
    .expect("get_character_progress_by_character_id full");
    assert_v24_character_progress_payload(&prog_full, cid, 2211, 100, 100);
}
