//! Parity closure integration tests for PvE features (part 2):
//! - AIR NPE & Career Program: `feat:pve.air-npe`, `feat:pve.air-career-program`
//! - Daily & Seasonal Login campaigns: `feat:pve.login-campaign`, `feat:pve.seasonal-login`
//! - Agency Content Provider: `feat:pve.agency-content`
//! - COSMOS constellations, storyline agents & unique sites: `feat:pve.cosmos`

use crate::common;
use eve_db::MiningLedgerRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::campaigns_and_milestones::SEASONAL_LOGIN_SCHEDULES;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
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

#[tokio::test]
async fn test_air_npe_state_persistence_and_career_goals() {
    // parity: feat:pve.air-npe, feat:pve.air-career-program
    eve_testkit::attest(&[
        "feat:pve.air-npe",
        "feat:pve.air-career-program",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8003, "demo", "demo-secret", char_id).await;

    // 1. Initial AIR NPE state
    let init_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state initial");
    assert_eq!(init_state.dict_get("nodeGraphID").and_then(|v| v.as_i64()), Some(473));
    assert_eq!(init_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(1));

    // 2. Skip AIR NPE
    let skip_state = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[])
        .await
        .expect("skip_air_npe");
    assert_eq!(skip_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(skip_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 3. Verify persistence of completed state
    let get_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state after skip");
    assert_eq!(get_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(get_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 4. AIR Career Program Goal Progress (Mining Goal 2)
    let initial_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress initial");
    assert_eq!(
        initial_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(0)
    );

    // Record server-observed mining events in the mining ledger
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230, // Veldspar
            quantity: 1_000,
            date: "2026-01-01".into(),
            timestamp_unix: 1_704_067_200,
        });
    });

    // Verify progress updated from server events
    let updated_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress after mining");
    assert_eq!(
        updated_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(1000)
    );

    // Claim goal reward
    let sp_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("claim_goal_reward");
    assert_eq!(claim_res.dict_get("goalID").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        claim_res.dict_get("spReward").and_then(|v| v.as_f64()),
        Some(50_000.0)
    );

    let sp_after = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert!(sp_after > sp_before, "SP reward must be credited to character");

    // Second claim must be rejected
    let repeat_claim = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("Duplicate claim must be rejected");
    // rpc-lint: allow error code check
    assert_eq!(repeat_claim.code, "GoalAlreadyClaimed");
}

#[tokio::test]
async fn test_daily_and_seasonal_login_campaign_rewards() {
    // parity: feat:pve.login-campaign, feat:pve.seasonal-login
    eve_testkit::attest(&["feat:pve.login-campaign", "feat:pve.seasonal-login"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8005, "demo", "demo-secret", char_id).await;

    let init_items = ctx.db.with_snapshot(|s| s.items.len());

    // 1. Daily login campaign (SDE campaign 2261)
    let state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state");
    assert_eq!(state.dict_get("campaignID").and_then(|v| v.as_i64()), Some(2261));
    assert_eq!(state.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    let PyRep::Tuple(claim1_tuple) = &claim1 else {
        panic!("expected Tuple from claim_reward day 1");
    };
    assert_eq!(claim1_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(claim1_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(80_937));
    assert!(claim1_tuple[0].dict_get("itemID").and_then(|v| v.as_i64()).is_some());

    // Duplicate claim on same day must be rejected
    let repeat_day1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("Duplicate daily claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_day1.code, "RewardAlreadyClaimedToday");

    // Advance clock by 1 full day (86,400 seconds)
    ctx.advance_virtual_clock(86_400);

    let state_day2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state day 2");
    assert_eq!(state_day2.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    let PyRep::Tuple(claim2_tuple) = &claim2 else {
        panic!("expected Tuple from claim_reward day 2");
    };
    assert_eq!(claim2_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(claim2_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(82_052));

    // 2. Seasonal login campaign (SDE campaign 2323)
    let active_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("get_active_campaign");
    let PyRep::Tuple(seasonal_tuple) = &active_seasonal else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(
        seasonal_tuple[0].as_i64(),
        Some(2323)
    );
    let sched = &SEASONAL_LOGIN_SCHEDULES[0];
    let expected_days = (sched.end_unix - ctx.now_unix() + 86_399) / 86_400;
    assert_eq!(seasonal_tuple[3].as_i64(), Some(expected_days as i64));
    assert_eq!(seasonal_tuple[2].as_str(), Some(sched.end_date_str().as_str()));

    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("seasonal claim_reward");
    assert_eq!(
        seasonal_claim,
        PyRep::Bool(true)
    );

    // Verify all 6 SDE reward items (day 1 alpha+omega, day 2 alpha+omega, seasonal day 1 alpha+omega) were delivered
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), init_items + 6);
        for expected_type in [80_937, 49_809, 82_052, 29_266, 87_900] {
            assert!(
                s.items.values().any(|it| {
                    it.owner_id == char_id
                        && it.location_id == 60_003_760
                        && it.flag == eve_db::InventoryFlag::Hangar
                        && it.type_id == expected_type
                }),
                "missing delivered SDE reward type {expected_type}"
            );
        }
    });

    // Duplicate seasonal claim on same day must fail
    let repeat_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect_err("Duplicate seasonal claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_seasonal.code, "SeasonalRewardAlreadyClaimedToday");

    let history = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("get_claim_history");
    let PyRep::Dict(days) = history else {
        panic!("Expected dict from get_claim_history");
    };
    assert!(!days.is_empty(), "Claim history must record claimed day");
}

#[tokio::test]
async fn test_custom_agency_provider_content_nearby_sites() {
    // parity: feat:pve.agency-content
    eve_testkit::attest(&["feat:pve.agency-content"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let sys_id = 30_002_053; // HEK
    let mut sess = setup_pilot(&ctx, 8006, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;

    let content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("get_content_data");
    let PyRep::List(entries) = content else {
        panic!("Expected list from get_content_data");
    };
    assert!(!entries.is_empty(), "Agency must return nearby system content");

    let mut has_combat = false;
    let mut has_sig = false;
    for entry in entries {
        if entry.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_id as i64) {
            match entry.dict_get("contentType").and_then(|v| v.as_str()) {
                Some("combatAnomaly") => has_combat = true,
                Some("signature") => has_sig = true,
                _ => {}
            }
        }
    }
    assert_eq!(has_combat, true, "Agency content must include combat anomalies");
    assert_eq!(has_sig, true, "Agency content must include signatures");
}

#[tokio::test]
async fn test_cosmos_constellations_storyline_agents_and_sites() {
    // parity: feat:pve.cosmos
    eve_testkit::attest(&["feat:pve.cosmos"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8007, "demo", "demo-secret", char_id).await;

    // 1. Verify COSMOS constellations in SDE contain storyline agents
    let vaarma = ctx.sde.constellations.get(&20_000_449).expect("Vaarma constellation");
    assert_eq!(vaarma.name, "Vaarma");

    let cosmos_agents_in_vaarma: Vec<i64> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 9
                && a.solar_system_id
                    .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    .map(|s| s.constellation_id == 20_000_449)
                    .unwrap_or(false)
        })
        .map(|a| a.agent_id)
        .collect();
    assert!(
        !cosmos_agents_in_vaarma.is_empty(),
        "Vaarma COSMOS constellation must contain COSMOS storyline agents"
    );

    // 2. Verify unique COSMOS sites in SDE
    let cosmos_dungeons: Vec<&eve_sde::DungeonTemplate> = ctx
        .sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id == Some(26) || d.name.contains("Contested"))
        .collect();
    assert!(!cosmos_dungeons.is_empty(), "Unique COSMOS sites must exist in SDE");
    let has_caldari_mining = cosmos_dungeons
        .iter()
        .any(|d| d.name.contains("Contested Caldari Mining Facilities"));
    assert!(
        has_caldari_mining,
        "Contested Caldari Mining Facilities COSMOS site must exist in SDE"
    );

    // 3. COSMOS Storyline Agent Interaction (Level 1 COSMOS Agent 3019001)
    let agent_id = 3_019_001;
    let cosmos_agent = ctx.sde.agents.get(&agent_id).expect("COSMOS agent 3019001");
    assert_eq!(cosmos_agent.agent_type_id, 9, "Agent type must be 9 (COSMOS)");
    let agent_corp_id = cosmos_agent.corporation_id;
    let agent_sys_id = cosmos_agent.solar_system_id.expect("Agent system");
    let agent_station_id = cosmos_agent.station_id.expect("Agent station");

    // Colocate pilot with agent and set standing >= 2.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys_id;
            ch.station_id = Some(agent_station_id);
            ch.structure_id = None;
            ch.standings.insert(agent_corp_id, 5.0);
        }
    });
    sess.solar_system_id = Some(agent_sys_id);
    sess.station_id = Some(agent_station_id);
    sess.structure_id = None;

    // Request mission offer from COSMOS agent
    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("COSMOS agent offer");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    // Accept mission
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)])
        .await
        .expect("Accept COSMOS mission");

    // Complete mission
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.state = "Completed".into();
        }
    });

    // Subsequent mission requests must be rejected due to one-time completion
    let repeat_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect_err("Repeat COSMOS agent mission must be rejected");
    assert_eq!(repeat_err.code, "OneTimeAgentMissionCompleted");
}
