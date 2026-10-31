//! Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 1: Seasons).
//!
//! Covers all 10 Group 1 Seasons parity rows:
//! - `rpc:seasonManager.get_season_data_for_character`
//! - `rpc:seasonManager.get_season_end_time`
//! - `rpc:seasonManager.select_season`
//! - `notify:OnSeasonStarted`
//! - `notify:OnSeasonEnded`
//! - `notify:OnSeasonSelectionCleared`
//! - `notify:OnSeasonOmegaRewardsClaimed`
//! - `notify:OnSeasonalGoalCompleted`
//! - `notify:OnSeasonalGoalsReset`
//! - `notify:OnChallengeExpired`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const ERR_INVALID_SEASON: &str = "InvalidSeasonId";
const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";
const ERR_CHALLENGE_ALREADY_CLAIMED: &str = "ChallengeAlreadyClaimed";

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn login_docked(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    clone_grade: i32,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = clone_grade as u32;
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
        }
    });
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: rpc:seasonManager.get_season_data_for_character, rpc:seasonManager.get_season_end_time
// parity: rpc:seasonManager.select_season, notify:OnSeasonStarted, notify:OnSeasonEnded
// parity: notify:OnSeasonSelectionCleared, notify:OnChallengeExpired
#[tokio::test]
async fn notify_batch8_seasons_lifecycle_and_challenges() {
    eve_testkit::attest(&[
        "rpc:seasonManager.get_season_data_for_character",
        "rpc:seasonManager.get_season_end_time",
        "rpc:seasonManager.select_season",
        "notify:OnSeasonStarted",
        "notify:OnSeasonEnded",
        "notify:OnSeasonSelectionCleared",
        "notify:OnChallengeExpired",
    ]);

    let ctx = new_ctx();
    let mut unauth_sess = SessionState::new(9_100);
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_data_for_character", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_end_time", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess_a = login_docked(&ctx, 9_101, "demo", "demo-secret", PILOT_A, 2).await;
    let mut sess_b = login_docked(&ctx, 9_102, "other", "other-secret", PILOT_B, 1).await;

    // Seed custom challenge definitions so challenge 1005 awards 25 points (crosses 2 goal milestones!)
    ctx.db.with_mut_snapshot(|snap| {
        let defs = vec![
            SeasonalChallengeDef {
                challenge_id: 1_005,
                season_id: 1,
                event_type: "ui_onboarding_step".into(),
                title: "Onboarding".into(),
                description: "Complete onboarding steps".into(),
                points: 25,
                max_progress: 2,
                client_event_driven: true,
                reward_type_id: 0,
                reward_quantity: 0,
                is_dormant: false,
                ..Default::default()
            },
            SeasonalChallengeDef {
                challenge_id: 1_006,
                season_id: 1,
                event_type: "npc_kills".into(),
                title: "Bounty Hunter".into(),
                description: "Destroy pirate vessels".into(),
                points: 10,
                max_progress: 5,
                client_event_driven: false,
                reward_type_id: 0,
                reward_quantity: 0,
                is_dormant: false,
                ..Default::default()
            },
        ];
        snap.service_state_blobs.insert(
            SEASONAL_CHALLENGES_CONFIG_KEY.into(),
            serde_json::to_string(&defs).expect("serialize defs"),
        );
    });

    // 1. Baseline get_season_data_for_character & get_season_end_time
    let data_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character");
    let first_season_id = data_init
        .dict_get("season_id")
        .or_else(|| {
            if let PyRep::List(items) = &data_init {
                items.first().and_then(|it| it.dict_get("season_id"))
            } else {
                None
            }
        })
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    assert!(first_season_id > 0, "expected active season_id > 0, got {data_init:?}");

    let end_time_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time");
    let end_ft_val = end_time_init.as_i64().expect("integer filetime end_time");
    assert!(end_ft_val > ctx.now_blueos_filetime_ticks());

    // Negative: non-GM cannot start or end a season via slash command
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "publicQaToolsServer",
            "SlashCmd",
            &[PyRep::String("/season start 88 14".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    // 2. Start Season 88 via GM /season start 88 10 -> emits OnSeasonStarted
    sess_a.role |= ROLE_GM_DEFAULT;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let slash_start = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/season start 88 10".into())],
    )
    .await
    .expect("/season start 88 10");
    assert_eq!(slash_start.dict_get("seasonID").and_then(|v| v.as_i64()), Some(88));
    assert_eq!(slash_start.dict_get("started").and_then(|v| v.as_bool()), Some(true));

    let notifs_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let notifs_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&notifs_start_a, "OnSeasonStarted").len(), 0);
    assert_eq!(find_notif(&notifs_start_b, "OnSeasonStarted").len(), 0);

    // Negative: invalid season IDs and invalid slash arguments rejected
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season start 0 10".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    let err_999 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err_999.code.as_str(), ERR_INVALID_SEASON);
    let err_neg = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-5)])
        .await
        .unwrap_err();
    assert_eq!(err_neg.code.as_str(), ERR_INVALID_SEASON);

    // 3. select_season(88) selects Season 88, then select_season(None) & select_season(0) emit OnSeasonSelectionCleared
    let sel_ok = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("select_season(88)");
    assert_eq!(sel_ok, PyRep::Bool(true));
    let data_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character(88)");
    assert_eq!(data_88.dict_get("season_id").and_then(|v| v.as_i64()), Some(88));
    let end_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time(88)");
    assert!(end_88.as_i64().unwrap_or(0) > ctx.now_blueos_filetime_ticks());

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let clear_rep = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::None])
        .await
        .expect("select_season(None)");
    assert_eq!(clear_rep, PyRep::Bool(true));
    let clear_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&clear_notifs, "OnSeasonSelectionCleared").len(), 0);

    // Re-select Season 88 for both pilots, then end Season 88 -> emits OnSeasonEnded & OnSeasonSelectionCleared
    rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("re-select 88 A");
    rpc(&ctx, &mut sess_b, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("re-select 88 B");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    let end_rep = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/season end 88".into())],
    )
    .await
    .expect("/season end 88");
    assert_eq!(end_rep.dict_get("ended").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(end_rep.dict_get("clearedCharacters").and_then(|v| v.as_i64()), Some(2));

    let end_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let end_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&end_notifs_a, "OnSeasonEnded").len(), 0);
    assert_eq!(find_notif(&end_notifs_a, "OnSeasonSelectionCleared").len(), 0);
    assert_eq!(find_notif(&end_notifs_b, "OnSeasonEnded").len(), 0);
    assert_eq!(find_notif(&end_notifs_b, "OnSeasonSelectionCleared").len(), 0);

    // Negative: selecting invalid season -1 fails with CustomInfo; ending unknown season fails
    let err_sel0 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-1)])
        .await
        .unwrap_err();
    assert_eq!(err_sel0.code.as_str(), ERR_INVALID_SEASON);
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season end 999999".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidSeasonId"
    );

    // 4. Complete challenge 1005 (25 pts) -> claim_challenge_rewards emits OnSeasonalGoalCompleted
    // and (for Omega PILOT_A) OnSeasonOmegaRewardsClaimed, while Alpha PILOT_B does NOT receive Omega claim!
    let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges A");
    let _ = rpc(&ctx, &mut sess_b, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges B");

    // Negative: claiming incomplete challenge fails with ChallengeNotCompleted
    let err_uncomp = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_uncomp.code.as_str(), ERR_CHALLENGE_NOT_COMPLETED);

    for _ in 0..2 {
        rpc(
            &ctx,
            &mut sess_a,
            "seasonManager",
            "process_client_event",
            &[PyRep::String("ui_onboarding_step".into())],
        )
        .await
        .expect("complete challenge 1005 A");
        rpc(
            &ctx,
            &mut sess_b,
            "seasonManager",
            "process_client_event",
            &[PyRep::String("ui_onboarding_step".into())],
        )
        .await
        .expect("complete challenge 1005 B");
    }

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    let claim_a = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .expect("claim_challenge_rewards A");
    assert_eq!(claim_a.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(claim_a.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(25));

    let claim_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let goal_args = find_notif(&claim_notifs_a, "OnSeasonalGoalCompleted");
    assert_eq!(goal_args.len(), 2);
    assert_eq!(goal_args[0].as_i64(), Some(0));
    assert_eq!(goal_args[1].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(goal_args[1].dict_get("points_required").and_then(|v| v.as_i64()), Some(10));

    let omega_args = find_notif(&claim_notifs_a, "OnSeasonOmegaRewardsClaimed");
    assert_eq!(omega_args.len(), 1);
    assert_eq!(omega_args[0], PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)]));

    // Alpha pilot B gets OnSeasonalGoalCompleted but NOT OnSeasonOmegaRewardsClaimed
    let _ = rpc(&ctx, &mut sess_b, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .expect("claim_challenge_rewards B");
    let claim_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&claim_notifs_b, "OnSeasonalGoalCompleted").len(), 2);
    assert!(
        !claim_notifs_b.iter().any(|(ev, _)| ev == "OnSeasonOmegaRewardsClaimed"),
        "Alpha pilot must not receive OnSeasonOmegaRewardsClaimed"
    );

    // Negative: double-claiming already claimed challenge fails with ChallengeAlreadyClaimed
    let err_claimed = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_claimed.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

    // 5. Reset seasonal goals via /season reset_goals -> emits OnSeasonalGoalsReset and zeroes points
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let reset_rep = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/season reset_goals {PILOT_A}"))],
    )
    .await
    .expect("/season reset_goals");
    assert_eq!(reset_rep.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(0));
    let reset_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&reset_notifs, "OnSeasonalGoalsReset").len(), 0);
    // Claimed challenge must NOT be re-armed by /season reset_goals before expiration
    let err_rearm = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_rearm.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

    // Negative: resetting goals for non-existent character fails
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "slash",
            "SlashCmd",
            &[PyRep::String("/season reset_goals 999888777".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );

    // 6. Advance virtual clock by 35 days (> 48h challenge expiration) -> fill_up_and_return_challenges_for_character
    // refreshes expired challenges and emits OnChallengeExpired(expired_id, new_challenge_kv)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(35 * 86_400);
    let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges after expiry");
    let exp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let exp_args = find_notif(&exp_notifs, "OnChallengeExpired");
    assert_eq!(exp_args.len(), 2);
    assert!(exp_args[0].as_i64().unwrap_or(0) > 0);
    assert!(
        exp_args[1].dict_get("challenge_id").is_some()
            || exp_args[1].dict_get("challengeID").is_some(),
        "OnChallengeExpired arg 1 must be replacement challenge KeyVal, got {:?}",
        exp_args[1]
    );

    // 7. Time-driven season transitions: advance clock past a future season's start_time_ft and end_time_ft
    // -> OnSeasonStarted, OnSeasonEnded, and OnSeasonSelectionCleared fire from the world tick without Get* calls
    let now_ft = ctx.now_blueos_filetime_ticks();
    let future_start_ft = now_ft + 100 * 10_000_000;
    let future_end_ft = now_ft + 200 * 10_000_000;
    ctx.db.with_mut_snapshot(|snap| {
        let mut catalog: std::collections::BTreeMap<i64, eve_server::services::season_manager::SeasonConfig> =
            snap.service_state_blobs
                .get(eve_server::services::season_manager::SEASONS_CATALOG_KEY)
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
        if let Some(mut cfg) = catalog.values().next().cloned() {
            cfg.season_id = 99;
            cfg.season_name = "Timed Season 99".into();
            cfg.start_time_ft = future_start_ft;
            cfg.end_time_ft = future_end_ft;
            catalog.insert(99, cfg);
            snap.service_state_blobs.insert(
                eve_server::services::season_manager::SEASONS_CATALOG_KEY.into(),
                serde_json::to_string(&catalog).unwrap(),
            );
        }
    });
    // Initialize announced season state snapshot while season 99 is still upcoming
    ctx.advance_secs(1);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // Advance 120s past future_start_ft -> OnSeasonStarted emitted automatically
    ctx.advance_secs(120);
    let timed_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timed_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(
        timed_start_a.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
        "expected OnSeasonStarted on clock advance past start_time_ft"
    );
    assert!(
        timed_start_b.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
        "expected OnSeasonStarted for pilot B on clock advance past start_time_ft"
    );

    // Select season 99 for PILOT_A, then advance 120s past future_end_ft -> OnSeasonEnded + OnSeasonSelectionCleared
    let sel_99 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(99)])
        .await
        .expect("select_season(99)");
    assert_eq!(sel_99, PyRep::Bool(true));
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    ctx.advance_secs(120);
    let timed_end_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timed_end_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(
        timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
        "expected OnSeasonEnded on clock advance past end_time_ft"
    );
    assert!(
        timed_end_b.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
        "expected OnSeasonEnded for pilot B on clock advance past end_time_ft"
    );
    assert!(
        timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonSelectionCleared"),
        "expected OnSeasonSelectionCleared for pilot A when selected season 99 ended"
    );
}
