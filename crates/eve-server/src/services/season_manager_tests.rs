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

fn test_ctx() -> ServerContext {
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
        let loaded = load_season_state_from_blobs(snap, cid);
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

    let updated = ctx.db.with_snapshot(|s| load_season_state_from_blobs(s, cid));
    assert_eq!(updated.season_points, 575);
}
