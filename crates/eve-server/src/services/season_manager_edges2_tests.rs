//! Additional edge-case and branch coverage tests for `services::season_manager`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
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

fn season_session(char_id: i64, gm: bool, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.station_id = station_id;
    if gm {
        s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    }
    s
}

#[test]
fn season_routing_selection_challenges_and_dormant_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = season_session(char_id, false, Some(60_003_760));
    let kw = HashMap::new();

    // Non-seasonManager service returns None
    assert!(handle_season_manager(&ctx, &mut sess, "otherSvc", "get_challenge", &[], &kw).is_none());

    // get_max_active_challenges & get_season_end_time
    let max_act = call_sm(&ctx, &mut sess, "get_max_active_challenges", &[], &kw).unwrap();
    assert!(max_act.as_i64().unwrap() >= 1);

    let end_t = call_sm(&ctx, &mut sess, "get_season_end_time", &[], &kw).unwrap();
    assert!(end_t.as_i64().unwrap() > 0);

    // select_season: invalid negative / unknown ID -> InvalidSeasonId, 0 clears selection
    assert_eq!(
        call_sm(&ctx, &mut sess, "select_season", &[PyRep::Integer(999_888_777)], &kw)
            .unwrap_err()
            .code,
        "InvalidSeasonId"
    );
    assert_eq!(
        call_sm(&ctx, &mut sess, "select_season", &[PyRep::Integer(0)], &kw).unwrap(),
        PyRep::Bool(true)
    );

    // ChallengeNotFound on get_challenge / challenge_is_dormant / get_challenge_expiration_date
    for method in [
        "get_challenge",
        "challenge_is_dormant",
        "get_challenge_expiration_date",
    ] {
        assert_eq!(
            call_sm(&ctx, &mut sess, method, &[PyRep::Integer(999_888_777)], &kw)
                .unwrap_err()
                .code,
            "ChallengeNotFound",
            "{method}"
        );
    }

    // Seed challenges to test dormant, non-client-event-driven, completed & not claimed dormancy
    let now_ft = ctx.now_blueos_filetime_ticks();
    with_season_state(&ctx, char_id, |st, _snap| {
        st.selected_season_id = 1;
        let mut dormant = default_challenge(66_001, 1, now_ft);
        dormant.is_dormant = true;
        st.challenges.insert(66_001, dormant);

        let mut auto_only = default_challenge(66_002, 1, now_ft);
        auto_only.client_event_driven = false;
        st.challenges.insert(66_002, auto_only);

        let mut completed_unclaimed = default_challenge(66_003, 1, now_ft);
        completed_unclaimed.progress = 1;
        completed_unclaimed.max_progress = 1;
        completed_unclaimed.completed = true;
        completed_unclaimed.claimed = false;
        completed_unclaimed.points = 25;
        completed_unclaimed.reward_type_id = 34;
        completed_unclaimed.reward_quantity = 10;
        st.challenges.insert(66_003, completed_unclaimed);

        let mut dormant_next = default_challenge(66_004, 1, now_ft);
        dormant_next.is_dormant = true;
        st.challenges.insert(66_004, dormant_next);
        Ok(())
    })
    .unwrap();

    // completed && !claimed counts as dormant in challenge_is_dormant
    assert_eq!(
        call_sm(&ctx, &mut sess, "challenge_is_dormant", &[PyRep::Integer(66_003)], &kw).unwrap(),
        PyRep::Bool(true)
    );

    // process_client_event errors: ChallengeNotFound, InvalidEventProgress, InvalidChallengeEvent, ChallengeDormant
    assert_eq!(
        call_sm(&ctx, &mut sess, "process_client_event", &[], &kw)
            .unwrap_err()
            .code,
        "ChallengeNotFound"
    );
    assert_eq!(
        call_sm(
            &ctx,
            &mut sess,
            "process_client_event",
            &[PyRep::Integer(66_001), PyRep::Integer(0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidEventProgress"
    );
    assert_eq!(
        call_sm(
            &ctx,
            &mut sess,
            "process_client_event",
            &[PyRep::Integer(66_002), PyRep::Integer(1)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidChallengeEvent"
    );
    assert_eq!(
        call_sm(
            &ctx,
            &mut sess,
            "process_client_event",
            &[PyRep::Integer(66_001), PyRep::Integer(1)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ChallengeDormant"
    );

    // claim_challenge_rewards errors: ChallengeDormant, ChallengeNotCompleted, ChallengeNotFound
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(66_001)], &kw)
            .unwrap_err()
            .code,
        "ChallengeDormant"
    );
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(66_002)], &kw)
            .unwrap_err()
            .code,
        "ChallengeNotCompleted"
    );
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(999_888_777)], &kw)
            .unwrap_err()
            .code,
        "ChallengeNotFound"
    );

    // Claiming 66_003 credits ISK, activates next_challenge 66_004 (is_dormant -> false), and second claim errors ChallengeAlreadyClaimed
    let _ = call_sm(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(66_003)], &kw).unwrap();
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(66_003)], &kw)
            .unwrap_err()
            .code,
        "ChallengeAlreadyClaimed"
    );
    // Checking dormant challenge remains dormant while unclaimed next challenge 66_004 stays true
    assert_eq!(
        call_sm(&ctx, &mut sess, "challenge_is_dormant", &[PyRep::Integer(66_004)], &kw).unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn season_loot_table_career_goal_claims_and_gm_reload_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = season_session(char_id, false, None);
    let mut gm_sess = season_session(char_id, true, None);
    let kw = HashMap::new();

    // get_loot_table_by_typeID: invalid typeID vs valid lookup
    assert_eq!(
        call_sm(&ctx, &mut sess, "get_loot_table_by_typeID", &[PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidTypeId"
    );
    let valid_tid = ctx.sde.types.keys().copied().next().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.season_loot_tables = serde_json::from_str(&String::from(format!(r#"{{"{valid_tid}":[[34,100,0.5]]}}"#))).unwrap_or_default();
    });
    let loot = call_sm(
        &ctx,
        &mut sess,
        "get_loot_table_by_typeID",
        &[PyRep::Integer(valid_tid as i64)],
        &kw,
    )
    .unwrap();
    match loot {
        PyRep::List(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].dict_get("typeID"), Some(&PyRep::Integer(34)));
        }
        other => panic!("expected List, got {other:?}"),
    }

    // get_character_progress_by_character_id defaults to caller when omitted
    let prog = call_sm(&ctx, &mut sess, "get_character_progress_by_character_id", &[], &kw).unwrap();
    assert!(prog.dict_get("season_id").and_then(|v| v.as_i64()).unwrap_or(-1) >= 0);
    assert!(prog.dict_get("challenges").is_some());

    // claim_goal_reward error paths: InvalidGoalId, ChallengeNotFound, GoalProgressIncomplete, GoalAlreadyClaimed
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(-1)], &kw)
            .unwrap_err()
            .code,
        "InvalidGoalId"
    );
    assert_eq!(
        call_sm(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(999_888_777)], &kw)
            .unwrap_err()
            .code,
        "ChallengeNotFound"
    );

    if let Some((goal_id, target)) = ctx
        .sde
        .career_goals
        .values()
        .next()
        .map(|g| (g.goal_id, g.target_progress.max(1)))
    {
        assert_eq!(
            call_sm(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(goal_id as i64)], &kw)
                .unwrap_err()
                .code,
            "GoalProgressIncomplete"
        );
        ctx.db.with_mut_snapshot(|snap| {
            let entry = snap.npe_progress.entry(char_id).or_insert_with(|| eve_db::NpeProgressRecord {
                character_id: char_id,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            });
            entry.career_goal_progress.insert(goal_id, target);
        });
        let claimed = call_sm(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(goal_id as i64)], &kw).unwrap();
        assert_eq!(claimed.dict_get("goalID"), Some(&PyRep::Integer(goal_id as i64)));
        assert_eq!(
            call_sm(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(goal_id as i64)], &kw)
                .unwrap_err()
                .code,
            "GoalAlreadyClaimed"
        );
    }

    // reload_fsd_data denied for non-GM, succeeds for GM
    assert!(call_sm(&ctx, &mut sess, "reload_fsd_data", &[], &kw).is_err());
    assert_eq!(
        call_sm(&ctx, &mut gm_sess, "reload_fsd_data", &[], &kw).unwrap(),
        PyRep::Bool(true)
    );
}

fn call_sm(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_season_manager(ctx, sess, "seasonManager", method, args, kw).unwrap()
}
