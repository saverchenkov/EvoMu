//! Integration tests for Seasons and Login Campaigns.
//!
//! Covers season selection, challenges, login campaigns, and milestone RPCs.
//! Note: Corresponding parity rows remain open as DATA GAP due to synthetic schedules/definitions.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::season_manager::{
    load_season_state_from_blobs, save_season_state_to_blobs,
};

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const STATION_ID: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    let (username, password) = if char_id == PILOT_2 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    common::login_pilot(ctx, &mut sess, username, password).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(STATION_ID);
            ch.structure_id = None;
            ch.home_station_id = STATION_ID;
        }
    });
    sess
}

#[tokio::test]
async fn test_seasons_data_end_time_selection_and_challenges() {
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;
    let now_ft = ctx.now_blueos_filetime_ticks();

    // 1. rpc:seasonManager.get_season_end_time
    let end_time_rep = rpc(&ctx, &mut sess, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time");
    let end_time_ft = match end_time_rep {
        PyRep::Integer(t) => t,
        other => panic!("expected Integer end_time_ft, got {other:?}"),
    };
    assert!(
        end_time_ft > now_ft,
        "Season end time ({end_time_ft}) must be in the future (now={now_ft})"
    );

    // 2. rpc:seasonManager.get_season_data_for_character (unselected initial state)
    let unselected_data = rpc(&ctx, &mut sess, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character unselected");
    let PyRep::List(ref list) = unselected_data else {
        panic!("Expected List of available seasons for unselected pilot, got {unselected_data:?}");
    };
    assert!(!list.is_empty(), "Season catalog list must not be empty");
    let first_season = &list[0];
    let sid = first_season.dict_get("seasonID").and_then(|v| v.as_i64()).expect("seasonID");
    assert!(sid > 0, "seasonID must be positive");
    assert!(first_season.dict_get("seasonName").is_some(), "seasonName must be present");
    assert!(first_season.dict_get("max_active_challenges").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // 3. rpc:seasonManager.select_season
    // Invalid season ID fails
    let bad_sel = rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
        .await;
    assert!(bad_sel.is_err(), "select_season with invalid ID must fail");

    // Valid season ID selection succeeds and returns true
    let sel_res = rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
        .await
        .expect("select_season 1");
    assert_eq!(sel_res, PyRep::Bool(true));

    // After selection, get_season_data_for_character returns selected season KeyVal with concrete fields
    let selected_data = rpc(&ctx, &mut sess, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character after selection");
    let PyRep::KeyVal(_) = selected_data else {
        panic!("Selected season data must be KeyVal, got {selected_data:?}");
    };
    assert_eq!(
        selected_data.dict_get("seasonID").and_then(|v| v.as_i64()),
        Some(1),
        "selected seasonID must match chosen season 1"
    );
    assert_eq!(
        selected_data.dict_get("season_id").and_then(|v| v.as_i64()),
        Some(1)
    );
    let s_end = selected_data.dict_get("end_time").and_then(|v| v.as_i64()).expect("end_time");
    assert!(s_end > now_ft, "season end_time ({s_end}) must be in the future (now={now_ft})");
    let max_act = selected_data.dict_get("max_active_challenges").and_then(|v| v.as_i64()).expect("max_active_challenges");
    assert!(max_act > 0, "max_active_challenges must be positive");
    assert!(selected_data.dict_get("tracked_event_types").is_some(), "tracked_event_types must be present");

    // 4. feat:pve.seasons-challenges: Filling challenges per character
    let filled_rep = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill_up_and_return_challenges_for_character");
    let challenges_list = match filled_rep {
        PyRep::List(list) => list,
        other => panic!("expected List of challenges, got {other:?}"),
    };
    assert!(
        challenges_list.len() >= 4,
        "Must populate at least 4 default seasonal challenges"
    );

    // 5. feat:pve.seasons-challenges: Progress from real V24 client_lookAtObject challenge (274)
    let kw = HashMap::new();
    let pce_res = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "process_client_event",
            &[PyRep::Integer(274), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("process_client_event progress");
    assert_eq!(pce_res, PyRep::None);

    let ch_274 = rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)])
        .await
        .expect("get_challenge 274");
    assert_eq!(ch_274.dict_get("progress").and_then(|v| v.as_i64()), Some(1));

    // 6. feat:pve.seasons-challenges: Challenge expiration and automatic refresh
    let exp_date = rpc(&ctx, &mut sess, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(274)])
        .await
        .expect("get_challenge_expiration_date 274");
    let exp_ticks = match exp_date {
        PyRep::Integer(t) => t,
        other => panic!("expected Integer expiration ticks, got {other:?}"),
    };
    assert!(exp_ticks > 0, "Expiration date ticks must be positive");

    // Simulate expiration: set expiration_date to the past in character season state
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, PILOT_1);
        if let Some(ch) = st.challenges.get_mut(&274) {
            ch.expiration_date = 100; // past timestamp
            ch.progress = 2;
        }
        save_season_state_to_blobs(snap, PILOT_1, &st);
    });

    // Calling fill_up_and_return_challenges_for_character cycles the expired challenge
    let refilled = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("refill challenges post-expiration");
    assert!(matches!(refilled, PyRep::List(_)));

    let ch_274_refreshed = rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)])
        .await
        .expect("get_challenge 274 refreshed");
    let new_exp = ch_274_refreshed.dict_get("expirationDate").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(
        new_exp > now_ft,
        "Expired challenge must be refreshed with future expiration date"
    );

    // 7. feat:pve.seasons-challenges: Claiming challenge rewards grants rewards and season points
    // Complete real V24 client_lookAtObject challenge 274 (max_progress = 4, points = 40).
    // Per-event increment is capped to 1 even if client supplies amount = 4.
    ctx.dispatch_rpc(
        &mut sess,
        "seasonManager",
        None,
        "process_client_event",
        &[PyRep::Integer(274), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("capped client event 1");
    let ch_274_capped = rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)])
        .await
        .expect("get_challenge 274 capped");
    assert_eq!(
        ch_274_capped.dict_get("progress").and_then(|v| v.as_i64()),
        Some(1),
        "Client-supplied amount=4 must be capped to 1 per event"
    );

    for _ in 0..3 {
        ctx.dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "process_client_event",
            &[PyRep::Integer(274), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("progress challenge 274");
    }

    let claim_res = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(274)])
        .await
        .expect("claim_challenge_rewards 274");
    assert_eq!(claim_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(claim_res.dict_get("rewardGranted").and_then(|v| v.as_bool()), Some(true));

    let pts = claim_res.dict_get("seasonPoints").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(pts >= 40, "Season points must increase after claiming challenge 274");

    // Repeat claim of already claimed challenge must fail
    let repeat_err = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(274)])
        .await;
    assert!(repeat_err.is_err(), "repeat claim_challenge_rewards must fail");

    // Negative trust checks: dormant, unassigned starter (when all active slots are full), and locked chain step claims must fail
    // with no change to ISK or items.
    let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill all active slots before testing unassigned claim");
    let (dormant_id, unassigned_starter_id, unassigned_client_id) = ctx.db.with_snapshot(|snap| {
        let st = load_season_state_from_blobs(snap, PILOT_1);
        let defs = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde);
        let dormant = defs.iter().find(|d| d.is_dormant && d.challenge_id != 2743).unwrap().challenge_id;
        let unassigned_starter = defs
            .iter()
            .find(|d| !d.is_dormant && !st.challenges.contains_key(&d.challenge_id))
            .unwrap()
            .challenge_id;
        let unassigned_client = defs
            .iter()
            .filter(|d| d.client_event_driven && !st.challenges.contains_key(&d.challenge_id))
            .nth(1)
            .unwrap()
            .challenge_id;
        (dormant, unassigned_starter, unassigned_client)
    });
    let (isk_before, items_before) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
            snap.items.len(),
        )
    });
    assert!(
        rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(dormant_id)])
            .await
            .is_err(),
        "Claiming a dormant challenge must fail"
    );
    let unassigned_err = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(unassigned_starter_id)])
        .await
        .expect_err("Claiming an unassigned starter challenge when all active slots are full must fail");
    assert_eq!(unassigned_err.code, "ChallengeNotFound"); // rpc-lint: allow error code assertion
    assert!(
        rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(2743)])
            .await
            .is_err(),
        "Claiming a chain step whose predecessor is unclaimed must fail"
    );
    assert!(
        rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(unassigned_client_id), PyRep::Integer(1)])
            .await
            .is_err(),
        "Progressing an unassigned client-driven challenge must fail"
    );
    let (isk_after, items_after) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
            snap.items.len(),
        )
    });
    assert_eq!(isk_after, isk_before, "Rejected claims must not alter ISK");
    assert_eq!(items_after, items_before, "Rejected claims must not mint items");

    // 8. feat:pve.seasons-challenges: Claim career goal reward (claim_goal_reward)
    // Pre-populate completed career goal in npe_progress
    ctx.db.with_mut_snapshot(|snap| {
        let npe = snap.npe_progress.entry(PILOT_1).or_insert_with(|| eve_db::NpeProgressRecord {
            character_id: PILOT_1,
            node_graph_id: 473,
            current_node_id: 1,
            completed: false,
            career_goal_progress: Default::default(),
            claimed_goals: Default::default(),
        });
        npe.career_goal_progress.insert(1, 500);
    });

    let sp_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.free_skill_points).unwrap_or(0.0));
    let goal_claim = rpc(&ctx, &mut sess, "seasonManager", "claim_goal_reward", &[PyRep::Integer(1)])
        .await
        .expect("claim_goal_reward 1");
    let sp_after = match goal_claim {
        PyRep::Real(sp) => sp,
        PyRep::Integer(sp) => sp as f64,
        PyRep::KeyVal(ref kv) => kv.get("freeSkillPoints").and_then(|v| v.as_f64()).unwrap_or(0.0),
        other => panic!("expected numeric free SP from claim_goal_reward, got {other:?}"),
    };
    assert!(
        sp_after > sp_before,
        "free_skill_points must increase after claiming goal reward (before={sp_before}, after={sp_after})"
    );

    // Duplicate claim of career goal fails
    let repeat_goal_err = rpc(&ctx, &mut sess, "seasonManager", "claim_goal_reward", &[PyRep::Integer(1)])
        .await;
    assert!(repeat_goal_err.is_err(), "duplicate claim_goal_reward must fail");
}

#[tokio::test]
async fn test_login_campaigns_seasonal_and_milestones() {
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_2).await;

    // 1. feat:pve.login-campaign: loginCampaignManager.get_client_campaign_state and claim_reward
    let state_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("loginCampaignManager.get_client_campaign_state");
    assert_eq!(
        state_rep.dict_get("can_claim_today").and_then(|v| v.as_bool()),
        Some(true),
        "Fresh pilot must be eligible to claim daily login reward today"
    );
    assert_eq!(
        state_rep.dict_get("campaign_id").and_then(|v| v.as_i64()),
        Some(2261)
    );

    let items_before = ctx.db.with_snapshot(|s| s.items.len());
    let claim_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("loginCampaignManager.claim_reward");

    // Returns authentic 4-tuple (reward_item, item_progress, reward_event, bucket_progress)
    let tuple = match claim_rep {
        PyRep::Tuple(ref items) if items.len() == 4 => items,
        other => panic!("expected 4-tuple from loginCampaignManager.claim_reward, got {other:?}"),
    };
    assert_eq!(tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(1));

    let items_after = ctx.db.with_snapshot(|s| s.items.len());
    assert!(
        items_after > items_before,
        "Daily login claim must deliver reward item into hangar"
    );

    let state_after = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("login state after claim");
    assert_eq!(
        state_after.dict_get("can_claim_today").and_then(|v| v.as_bool()),
        Some(false),
        "can_claim_today must be false after claiming today's reward"
    );

    // 2. feat:pve.seasonal-login: seasonalLoginCampaignManager.get_active_campaign, claim_reward, get_claim_history
    let active_seasonal = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
        .await
        .expect("seasonalLoginCampaignManager.get_active_campaign");
    let s_tuple = match active_seasonal {
        PyRep::Tuple(ref items) if items.len() == 4 => items,
        other => panic!("expected 4-tuple from get_active_campaign, got {other:?}"),
    };
    let s_cid = s_tuple[0].as_i64().expect("campaign_id");
    let s_ordinal = s_tuple[1].as_i64().expect("start_date_ordinal");
    let s_days_rem = s_tuple[3].as_i64().expect("days_remaining");
    assert!(s_cid > 0, "Seasonal campaign ID must be positive");
    assert!(s_ordinal > 0, "Start date ordinal must be positive");
    assert!(s_days_rem > 0, "Days remaining must be positive");

    // History before claim
    let hist_before = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("seasonal get_claim_history before claim");
    assert!(matches!(hist_before, PyRep::Dict(_)));

    // Claim seasonal reward
    let s_claim_res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("seasonalLoginCampaignManager.claim_reward");
    assert_eq!(s_claim_res, PyRep::Bool(true));

    // Repeat seasonal claim on same day fails
    let s_repeat_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await;
    assert!(s_repeat_err.is_err(), "repeat seasonal claim on same day must fail");

    // History after claim contains today's claim entry
    let hist_after = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("seasonal get_claim_history after claim");
    let entries = match hist_after {
        PyRep::Dict(e) => e,
        other => panic!("expected Dict claim history, got {other:?}"),
    };
    assert!(
        !entries.is_empty(),
        "Claim history must contain entry after claiming seasonal reward"
    );

    // 3. milestoneMgr.ClaimRewards: validates arguments and fails closed with UnimplementedRPC (DATA GAP: feat:pve.milestones)
    // Argument validation: non-positive milestone_id fails with InvalidMilestoneID
    let invalid_id = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(0)])
        .await
        .expect_err("zero milestone_id must fail");
    assert_eq!(invalid_id.code, "InvalidMilestoneID");

    let missing_arg = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[])
        .await
        .expect_err("missing milestone_id arg must fail");
    assert_eq!(missing_arg.code, "InvalidArgument");

    // Process logon succeeds
    let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res, PyRep::None);

    // Positive milestoneID fails closed with UnimplementedRPC because milestone reward
    // definitions are server-authored and not present in client FSD data (DATA GAP: feat:pve.milestones).
    let claim_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards must fail closed with UnimplementedRPC");
    assert_eq!(claim_err.code, "UnimplementedRPC");
}
