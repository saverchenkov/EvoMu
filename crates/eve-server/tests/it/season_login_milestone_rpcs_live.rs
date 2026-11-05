//! Live integration tests for `seasonManager`, `loginCampaignManager`, and `milestoneMgr` RPCs.

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::season_manager::fsd_challenge_definitions;

use crate::common::{self, pin_clock};

const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_gm_char(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let user = format!("season_gm_{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("Season GM {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn login_player_char(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let user = format!("season_plr_{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("Season Player {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.role = 0;
    sess
}

fn dict_entries(rep: &PyRep) -> &[(PyRep, PyRep)] {
    match rep {
        PyRep::Dict(entries) => entries,
        other => panic!("expected PyRep::Dict, got {other:?}"),
    }
}

// parity: rpc:seasonManager.get_max_active_challenges
// parity: rpc:seasonManager.reload_fsd_data
// parity: rpc:seasonManager.fill_up_and_return_challenges_for_character
// parity: rpc:seasonManager.get_challenge
// parity: rpc:seasonManager.challenge_is_dormant
// parity: rpc:seasonManager.get_challenge_expiration_date
// parity: rpc:seasonManager.process_client_event
// parity: rpc:seasonManager.claim_challenge_rewards
#[tokio::test]
async fn test_season_manager_challenges_and_fsd_live() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let gm_id = 94_100_001i64;
    let plr_id = 94_100_002i64;
    let mut gm = login_gm_char(&ctx, 31_001, gm_id).await;
    let mut plr = login_player_char(&ctx, 31_002, plr_id).await;

    // 1. Non-GM player cannot reload_fsd_data; GM can reload_fsd_data and emits OnSeasonsFsdDataChanged
    let non_gm_reload = rpc(&ctx, &mut plr, "seasonManager", "reload_fsd_data", &[])
        .await
        .expect_err("non-GM reload_fsd_data must fail");
    assert!(!non_gm_reload.code.is_empty());

    let _ = ctx.drain_notifications_for_char(gm_id);
    let reload_ok = rpc(&ctx, &mut gm, "seasonManager", "reload_fsd_data", &[])
        .await
        .expect("GM reload_fsd_data");
    assert_eq!(reload_ok.as_bool(), Some(true));
    let reload_notifs = ctx.drain_notifications_for_char(gm_id);
    assert!(
        reload_notifs
            .iter()
            .any(|n| n.event == "OnSeasonsFsdDataChanged"),
        "reload_fsd_data must emit OnSeasonsFsdDataChanged"
    );

    // 2. get_max_active_challenges & fill_up_and_return_challenges_for_character
    let max_active = rpc(&ctx, &mut gm, "seasonManager", "get_max_active_challenges", &[])
        .await
        .expect("get_max_active_challenges")
        .as_i64()
        .expect("max_active int");
    assert!(max_active >= 1, "max_active_challenges must be >= 1");

    let filled = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    let active_entries = dict_entries(&filled);
    assert!(
        !active_entries.is_empty() && (active_entries.len() as i64) <= max_active,
        "filled challenges count {} must be in 1..={max_active}",
        active_entries.len()
    );

    let first_cid = active_entries[0]
        .0
        .as_i64()
        .expect("challenge_id integer key");
    let ch_kv = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("get_challenge");
    assert_eq!(
        ch_kv.dict_get("challengeID").and_then(|v| v.as_i64()),
        Some(first_cid)
    );
    assert_eq!(
        ch_kv.dict_get("progress").and_then(|v| v.as_i64()),
        Some(0)
    );

    // 3. challenge_is_dormant & get_challenge_expiration_date
    let active_dormant = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "challenge_is_dormant",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("challenge_is_dormant active")
    .as_bool();
    assert_eq!(active_dormant, Some(false));

    let fsd_defs = fsd_challenge_definitions(&ctx.sde);
    if let Some(dormant_def) = fsd_defs.iter().find(|d| d.is_dormant) {
        let is_dormant = rpc(
            &ctx,
            &mut gm,
            "seasonManager",
            "challenge_is_dormant",
            &[PyRep::Integer(dormant_def.challenge_id)],
        )
        .await
        .expect("challenge_is_dormant dormant")
        .as_bool();
        assert_eq!(is_dormant, Some(true));
    }

    let exp_ft = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("get_challenge_expiration_date")
    .as_i64()
    .expect("expiration_date int");
    assert!(
        exp_ft > ctx.now_blueos_filetime_ticks(),
        "expiration_date must be in the future"
    );

    // 4. process_client_event & claim_challenge_rewards
    // Uncompleted challenge claim must fail
    let uncomp_err = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect_err("uncompleted claim must fail");
    assert_eq!(uncomp_err.code, ERR_CHALLENGE_NOT_COMPLETED);

    let active_ids: Vec<i64> = active_entries
        .iter()
        .filter_map(|(k, _)| k.as_i64())
        .collect();
    let client_def = fsd_defs
        .iter()
        .find(|d| d.client_event_driven && active_ids.contains(&d.challenge_id))
        .expect("active FSD client_event_driven challenge");
    let cid = client_def.challenge_id;
    let extra_entries: Vec<(PyRep, PyRep)> = client_def
        .event_conditions
        .iter()
        .filter_map(|(k, vals)| {
            vals.first()
                .map(|&v| (PyRep::String(k.clone()), PyRep::Integer(v)))
        })
        .collect();
    let extra_rep = PyRep::Dict(extra_entries);
    let _ = ctx.drain_notifications_for_char(gm_id);
    for _ in 0..client_def.max_progress {
        rpc(
            &ctx,
            &mut gm,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(cid), PyRep::Integer(1), extra_rep.clone()],
        )
        .await
        .expect("process_client_event");
    }
    let prog_notifs = ctx.drain_notifications_for_char(gm_id);
    assert!(
        prog_notifs
            .iter()
            .any(|n| n.event == "OnChallengeProgressUpdate"),
        "process_client_event must emit OnChallengeProgressUpdate"
    );
    assert!(
        prog_notifs.iter().any(|n| n.event == "OnChallengeCompleted"),
        "completing challenge must emit OnChallengeCompleted"
    );

    let updated = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(cid)],
    )
    .await
    .expect("get_challenge after progress");
    assert_eq!(
        updated.dict_get("completed").and_then(|v| v.as_bool()),
        Some(true)
    );

    let _ = ctx.drain_notifications_for_char(gm_id);
    let claim_rep = rpc(
        &ctx,
        &mut gm,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(cid)],
    )
    .await
    .expect("claim_challenge_rewards");
    assert_eq!(
        claim_rep.dict_get("claimed").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        claim_rep
            .dict_get("rewardGranted")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    let claim_notifs = ctx.drain_notifications_for_char(gm_id);
    assert!(
        claim_notifs
            .iter()
            .any(|n| n.event == "OnChallengeRewardsGranted"),
        "claim_challenge_rewards must emit OnChallengeRewardsGranted"
    );
    assert!(
        claim_notifs
            .iter()
            .any(|n| n.event == "OnSeasonalPointsUpdated"),
        "claim_challenge_rewards must emit OnSeasonalPointsUpdated"
    );
}

// parity: rpc:loginCampaignManager.get_client_campaign_state
// parity: rpc:loginCampaignManager.claim_reward
#[tokio::test]
async fn test_login_campaign_lifecycle_live() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let char_id = 94_100_003i64;
    let mut sess = login_player_char(&ctx, 31_003, char_id).await;

    // 1. Initial campaign state: can_claim_today == true, total_days_claimed == 0
    let state0 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state");
    assert_eq!(
        state0.dict_get("can_claim_today").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        state0
            .dict_get("total_days_claimed")
            .and_then(|v| v.as_i64()),
        Some(0)
    );

    // 2. Claim day 1 reward
    let _ = ctx.drain_notifications_for_char(char_id);
    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    let PyRep::Tuple(parts1) = &claim1 else {
        panic!("expected 4-tuple from claim_reward, got {claim1:?}");
    };
    assert_eq!(parts1.len(), 4);
    assert_eq!(
        parts1[0].dict_get("reward_index").and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        parts1[1]
            .dict_get("num_claimed_rewards")
            .and_then(|v| v.as_i64()),
        Some(1)
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnRedeemingTokensUpdated"),
        "claim_reward must emit OnRedeemingTokensUpdated"
    );

    // 3. Same-day repeat claim returns None and state shows can_claim_today == false
    let state1 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state after claim");
    assert_eq!(
        state1.dict_get("can_claim_today").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        state1
            .dict_get("total_days_claimed")
            .and_then(|v| v.as_i64()),
        Some(1)
    );

    let repeat_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("repeat claim_reward");
    assert_eq!(repeat_claim, PyRep::None);

    // 4. Advance clock by 24h -> day 2 claim succeeds
    ctx.advance_secs(86_400);
    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    let PyRep::Tuple(parts2) = &claim2 else {
        panic!("expected 4-tuple from day 2 claim_reward, got {claim2:?}");
    };
    assert_eq!(
        parts2[0].dict_get("reward_index").and_then(|v| v.as_i64()),
        Some(2)
    );
}

// parity: rpc:milestoneMgr.ProcessCharacterLogon
// parity: rpc:milestoneMgr.ClaimRewards
#[tokio::test]
async fn test_milestone_logon_and_claims_live() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let char_id = 94_100_004i64;
    let mut sess = login_player_char(&ctx, 31_004, char_id).await;

    // 1. ClaimRewards before ProcessCharacterLogon fails
    let pre_err = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
    assert_eq!(pre_err.code, "MilestonePrerequisiteNotMet");

    // 2. ProcessCharacterLogon succeeds and emits OnMilestoneTimerStarted
    let _ = ctx.drain_notifications_for_char(char_id);
    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);
    let logon_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        logon_notifs
            .iter()
            .any(|n| n.event == "OnMilestoneTimerStarted"),
        "ProcessCharacterLogon must emit OnMilestoneTimerStarted"
    );

    // 3. Out-of-order milestone 2 before 1 fails
    let order_err = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("milestone 2 before 1 must fail");
    assert_eq!(order_err.code, "MilestonePrerequisiteNotMet");

    // 4. Claim milestone 1 -> credits 100,000 ISK and emits OnLoginMilestoneAchieved
    let bal_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.0);
    let _ = ctx.drain_notifications_for_char(char_id);
    let claim1 = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("ClaimRewards(1)");
    assert_eq!(claim1, PyRep::None);

    let bal_after = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.0);
    assert_eq!(bal_after - bal_before, 100_000 * 100);
    let claim_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        claim_notifs
            .iter()
            .any(|n| n.event == "OnLoginMilestoneAchieved"),
        "ClaimRewards must emit OnLoginMilestoneAchieved"
    );

    // 5. Duplicate claim fails with MilestoneAlreadyClaimed
    let dup_err = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("duplicate ClaimRewards(1) must fail");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");
}
