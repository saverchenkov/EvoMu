//! Login campaign (`loginCampaignManager`) and login milestone (`milestoneMgr`) claim lifecycle.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const ALREADY_CLAIMED_ERR: &str = "DailyRewardAlreadyClaimedToday";

const PILOT_ID: i64 = 90_000_001;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn login_char(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

#[tokio::test]
async fn test_login_campaign_and_milestone_claim_lifecycle() {
    let ctx = new_ctx();
    let mut unauth = SessionState::default();
    assert!(rpc(
        &ctx,
        &mut unauth,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .is_err());

    // Authenticated account without character selected -> None for get_client_campaign_state
    let mut charless = SessionState {
        user_id: Some(777),
        ..Default::default()
    };
    let none_state = rpc(
        &ctx,
        &mut charless,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("charless get_client_campaign_state");
    assert_eq!(none_state, PyRep::None);

    let mut sess = login_char(&ctx, 102, PILOT_ID).await;
    sess.station_id = Some(JITA_STATION);

    let state_before = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state");
    let prog_before = dict_get(&state_before, "item_progress").expect("item_progress");
    assert_eq!(
        dict_get(prog_before, "next_reward_timestamp").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        dict_get(prog_before, "num_claimed_rewards").and_then(|v| v.as_i64()),
        Some(0)
    );

    let claim_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_ne!(claim_rep, PyRep::None, "first daily claim must return reward tuple");

    let state_after = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state after claim");
    let prog_after = dict_get(&state_after, "item_progress").expect("item_progress");
    assert!(
        dict_get(prog_after, "next_reward_timestamp")
            .and_then(|v| v.as_i64())
            .is_some_and(|ts| ts > 0)
    );
    assert_eq!(
        dict_get(prog_after, "num_claimed_rewards").and_then(|v| v.as_i64()),
        Some(1)
    );

    // Second claim same day raises DailyRewardAlreadyClaimedToday
    let second_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("second claim_reward must fail");
    assert_eq!(second_claim.code, ALREADY_CLAIMED_ERR);

    // milestoneMgr: ClaimRewards before ProcessCharacterLogon fails
    let prereq_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
    assert_eq!(prereq_err.code, "MilestonePrerequisiteNotMet");

    let bad_id_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(0)])
        .await
        .expect_err("ClaimRewards(0) must fail");
    assert_eq!(bad_id_err.code, "InvalidMilestoneID");

    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);

    let persisted_logon = ctx.db.with_snapshot(|snap| {
        eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
            .milestone_logons
            .contains_key(&PILOT_ID)
    });
    assert!(persisted_logon, "ProcessCharacterLogon must persist milestone_logons");

    ctx.advance_virtual_clock(7_200);
    let claim = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect("ClaimRewards(1) succeeds once after ProcessCharacterLogon + 120m playtime");
    assert_eq!(claim, PyRep::None);
    let dup_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("duplicate ClaimRewards(1) must fail");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");
}
