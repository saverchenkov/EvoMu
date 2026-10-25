//! Integration test suite for login campaigns, milestones, community tracks, and telemetry RPCs:
//! - `loginCampaignManager`: `get_client_campaign_state`, `claim_reward`
//! - `seasonalLoginCampaignManager`: `get_active_campaign`, `get_claim_history`, `claim_reward`
//! - `infoGatheringMgr`: `GetStateAndConfig`, `LogInfoEventsFromClient`
//! - `milestoneMgr`: `ProcessCharacterLogon`, `ClaimRewards`
//! - `communityTrackMgr`: `get_instance_attributes_public`, `change_qa_mode`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(kv) => kv
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
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

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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

#[tokio::test]
// parity: rpc:loginCampaignManager.get_client_campaign_state
// parity: rpc:loginCampaignManager.claim_reward
// parity: rpc:seasonalLoginCampaignManager.get_active_campaign
// parity: rpc:seasonalLoginCampaignManager.get_claim_history
// parity: rpc:seasonalLoginCampaignManager.claim_reward
async fn test_login_and_seasonal_campaigns() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut auth_sess = SessionState::new(31_001);
    common::login_pilot(&ctx, &mut auth_sess, "demo", "demo-secret").await;

    // Character-selection screen before character selected
    let unselected_camp = rpc(
        &ctx,
        &mut auth_sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("unselected get_client_campaign_state");
    assert_eq!(unselected_camp, PyRep::None);

    let unselected_seasonal = rpc(
        &ctx,
        &mut auth_sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("unselected seasonal get_active_campaign");
    assert_eq!(
        unselected_seasonal,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])
    );

    // Select character PILOT_1
    let mut sess = login_pilot_char(&ctx, 31_002, "demo", "demo-secret", PILOT_1).await;

    let init_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().free_skill_points);

    // 1. loginCampaignManager.get_client_campaign_state: returns KeyVal with item_progress
    let state1 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state");
    assert_eq!(field(&state1, "campaignID").as_i64(), Some(101));
    assert_eq!(field(&state1, "canClaimToday").as_bool(), Some(true));
    let item_progress = field(&state1, "item_progress");
    assert_eq!(field(item_progress, "next_reward_index").as_i64(), Some(1));
    assert_eq!(field(item_progress, "reward_count").as_i64(), Some(30));

    // 2. loginCampaignManager.claim_reward: returns 4-tuple (reward_item, item_progress, reward_event, bucket_progress)
    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    let PyRep::Tuple(claim1_tuple) = claim1 else {
        panic!("expected 4-tuple from claim_reward, got {claim1:?}");
    };
    assert_eq!(claim1_tuple.len(), 4);
    let reward_item = &claim1_tuple[0];
    let next_prog = &claim1_tuple[1];
    let _reward_event = &claim1_tuple[2];
    let bucket_prog = &claim1_tuple[3];

    assert_eq!(field(reward_item, "campaignID").as_i64(), Some(101));
    assert_eq!(field(reward_item, "spGranted").as_f64(), Some(5_000.0));
    assert_eq!(field(reward_item, "reward_index").as_i64(), Some(1));
    assert_eq!(field(next_prog, "next_reward_index").as_i64(), Some(2));
    assert_eq!(field(bucket_prog, "progress").as_i64(), Some(1));

    // SP credited
    let after_sp1 = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().free_skill_points);
    assert_eq!(after_sp1, init_sp + 5_000.0);

    // Duplicate claim on same day must be rejected
    let repeat_err = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("duplicate claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_err.code, "RewardAlreadyClaimedToday");

    // Advance clock by 1 full day (86,400s)
    ctx.advance_virtual_clock(86_400);

    // Day 2 get_client_campaign_state reflects next_reward_index 2
    let state2 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state day 2");
    assert_eq!(field(&state2, "canClaimToday").as_bool(), Some(true));
    assert_eq!(
        field(field(&state2, "item_progress"), "next_reward_index").as_i64(),
        Some(2)
    );

    // Claim day 2
    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    let PyRep::Tuple(claim2_tuple) = claim2 else {
        panic!("expected 4-tuple from claim_reward day 2");
    };
    assert_eq!(field(&claim2_tuple[0], "reward_index").as_i64(), Some(2));

    // 3. seasonalLoginCampaignManager.get_active_campaign: returns 4-tuple
    let seasonal_active = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("seasonal get_active_campaign");
    let PyRep::Tuple(seasonal_tuple) = seasonal_active else {
        panic!("expected 4-tuple from seasonal get_active_campaign");
    };
    assert_eq!(seasonal_tuple.len(), 4);
    assert_eq!(seasonal_tuple[0].as_i64(), Some(202));
    assert!(seasonal_tuple[1].as_i64().is_some());
    assert_eq!(seasonal_tuple[2].as_str(), Some("2026-11-15 11:00:00"));
    assert_eq!(seasonal_tuple[3].as_i64(), Some(14));

    // 4. seasonalLoginCampaignManager.get_claim_history: initially empty Dict
    let history_pre = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("seasonal get_claim_history pre");
    let PyRep::Dict(hist_entries_pre) = history_pre else {
        panic!("expected Dict from get_claim_history");
    };
    assert!(hist_entries_pre.is_empty());

    // 5. seasonalLoginCampaignManager.claim_reward: returns PyRep::Bool(true)
    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("seasonal claim_reward");
    assert_eq!(seasonal_claim, PyRep::Bool(true));

    let after_seasonal_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().free_skill_points);
    assert_eq!(after_seasonal_sp, init_sp + 10_000.0 + 15_000.0);

    // Duplicate seasonal claim on same day must fail
    let repeat_seasonal_err = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect_err("duplicate seasonal claim must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_seasonal_err.code, "SeasonalRewardAlreadyClaimedToday");

    // History now has 1 entry
    let history_post = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("seasonal get_claim_history post");
    let PyRep::Dict(hist_entries_post) = history_post else {
        panic!("expected Dict from get_claim_history post");
    };
    assert_eq!(hist_entries_post.len(), 1);
    let claim_data = &hist_entries_post[0].1;
    assert_eq!(field(claim_data, "campaign_id").as_i64(), Some(202));
    assert_eq!(field(claim_data, "sp_granted").as_f64(), Some(15_000.0));
}

#[tokio::test]
// parity: rpc:infoGatheringMgr.GetStateAndConfig
// parity: rpc:infoGatheringMgr.LogInfoEventsFromClient
async fn test_info_gathering_telemetry_rpcs() {
    let ctx = ServerContext::new();

    let mut unauth_sess = SessionState::new(31_003);

    // Unauthenticated call must fail closed
    let unauth_err = rpc(&ctx, &mut unauth_sess, "infoGatheringMgr", "GetStateAndConfig", &[])
        .await
        .expect_err("unauthenticated GetStateAndConfig must fail");
    // rpc-lint: allow error code check
    assert_eq!(unauth_err.code, "Unauthorized");

    let mut sess = SessionState::new(31_004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. infoGatheringMgr.GetStateAndConfig: returns KeyVal config
    let cfg = rpc(&ctx, &mut sess, "infoGatheringMgr", "GetStateAndConfig", &[])
        .await
        .expect("GetStateAndConfig");
    assert_eq!(field(&cfg, "clientWorkerInterval").as_i64(), Some(300));
    assert_eq!(field(&cfg, "isEnabled").as_bool(), Some(true));
    assert!(matches!(field(&cfg, "infoTypeAggregates"), PyRep::Dict(_)));
    assert!(matches!(field(&cfg, "infoTypeParameters"), PyRep::Dict(_)));
    assert!(matches!(field(&cfg, "infoTypes"), PyRep::List(_)));
    assert!(matches!(field(&cfg, "infoTypesOncePerRun"), PyRep::List(_)));

    // 2. infoGatheringMgr.LogInfoEventsFromClient: requires 1 arg (dict or list)
    let missing_arg = rpc(&ctx, &mut sess, "infoGatheringMgr", "LogInfoEventsFromClient", &[])
        .await
        .expect_err("missing arg must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_arg.code, "InvalidArgument");

    let invalid_arg = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[PyRep::Integer(42)],
    )
    .await
    .expect_err("invalid arg type must fail");
    // rpc-lint: allow error code check
    assert_eq!(invalid_arg.code, "InvalidArgument");

    // Logging dict of events
    let events_dict = PyRep::Dict(vec![
        (PyRep::String("ui_click".into()), PyRep::Integer(1)),
        (PyRep::String("ui_window_open".into()), PyRep::Integer(2)),
    ]);
    let log_dict_rep = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[events_dict],
    )
    .await
    .expect("LogInfoEventsFromClient dict");
    let PyRep::Dict(status_entries) = log_dict_rep else {
        panic!("expected Dict mapping event IDs to status");
    };
    assert_eq!(status_entries.len(), 2);
    for (_, status) in &status_entries {
        assert_eq!(status, &PyRep::Integer(0));
    }

    // Logging list of events
    let events_list = PyRep::List(vec![
        PyRep::String("session_start".into()),
        PyRep::String("session_end".into()),
    ]);
    let log_list_rep = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[events_list],
    )
    .await
    .expect("LogInfoEventsFromClient list");
    let PyRep::Dict(list_status) = log_list_rep else {
        panic!("expected Dict from list logging");
    };
    assert_eq!(list_status.len(), 2);
}

#[tokio::test]
// parity: rpc:milestoneMgr.ProcessCharacterLogon
// parity: rpc:milestoneMgr.ClaimRewards
// parity: rpc:communityTrackMgr.get_instance_attributes_public
// parity: rpc:communityTrackMgr.change_qa_mode
async fn test_milestones_and_community_tracks() {
    let ctx = ServerContext::new();

    let mut sess = login_pilot_char(&ctx, 31_005, "demo", "demo-secret", PILOT_1).await;

    // 1. milestoneMgr.ProcessCharacterLogon: returns PyRep::None
    let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res, PyRep::None);

    // 2. milestoneMgr.ClaimRewards: invalid args
    let missing_milestone = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[])
        .await
        .expect_err("missing milestone arg must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_milestone.code, "InvalidArgument");

    let invalid_milestone = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("invalid milestone 0 must fail");
    // rpc-lint: allow error code check
    assert_eq!(invalid_milestone.code, "InvalidMilestoneID");

    // Claim milestone 1: succeeds, returns None
    let pre_claim_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().free_skill_points);
    let pre_claim_isk = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance);

    let claim_res = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("ClaimRewards milestone 1");
    assert_eq!(claim_res, PyRep::None);

    let post_claim_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().free_skill_points);
    let post_claim_isk = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance);
    assert_eq!(post_claim_sp, pre_claim_sp + 10_000.0);
    assert_eq!(
        post_claim_isk.0,
        pre_claim_isk.0 + eve_db::IskCents::from_isk(100_000).0
    );

    // Duplicate milestone claim fails
    let dup_claim = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("duplicate milestone claim must fail");
    // rpc-lint: allow error code check
    assert_eq!(dup_claim.code, "MilestoneAlreadyClaimed");

    // 3. communityTrackMgr.get_instance_attributes_public: returns 2-tuple (DefinitionIdentifier, InstanceAttributes)
    let missing_track = rpc(
        &ctx,
        &mut sess,
        "communityTrackMgr",
        "get_instance_attributes_public",
        &[],
    )
    .await
    .expect_err("missing instance_id must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_track.code, "InvalidArgument");

    let invalid_track = rpc(
        &ctx,
        &mut sess,
        "communityTrackMgr",
        "get_instance_attributes_public",
        &[PyRep::Integer(-5)],
    )
    .await
    .expect_err("negative instance_id must fail");
    // rpc-lint: allow error code check
    assert_eq!(invalid_track.code, "InvalidInstanceID");

    let track_attrs = rpc(
        &ctx,
        &mut sess,
        "communityTrackMgr",
        "get_instance_attributes_public",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("get_instance_attributes_public");
    let PyRep::Tuple(track_tuple) = track_attrs else {
        panic!("expected 2-tuple from get_instance_attributes_public");
    };
    assert_eq!(track_tuple.len(), 2);
    let def_id = &track_tuple[0];
    let attrs = &track_tuple[1];
    assert_eq!(field(def_id, "definition_id").as_i64(), Some(1001));
    assert_eq!(field(attrs, "instance_id").as_i64(), Some(1001));
    assert_eq!(field(attrs, "is_active").as_bool(), Some(true));

    // 4. communityTrackMgr.change_qa_mode: returns PyRep::Bool(true)
    let missing_qa = rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[])
        .await
        .expect_err("missing qa_mode must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_qa.code, "InvalidArgument");

    let qa_res = rpc(
        &ctx,
        &mut sess,
        "communityTrackMgr",
        "change_qa_mode",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("change_qa_mode true");
    assert_eq!(qa_res, PyRep::Bool(true));

    let qa_off = rpc(
        &ctx,
        &mut sess,
        "communityTrackMgr",
        "change_qa_mode",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("change_qa_mode false");
    assert_eq!(qa_off, PyRep::Bool(true));
}
