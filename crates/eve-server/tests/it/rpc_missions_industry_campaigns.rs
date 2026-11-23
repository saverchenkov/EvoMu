//! Integration tests for Mission Tracking, Standings, Industry, and Login Campaigns RPCs:
//! - `missionTrackerMgr`: `GetAllMissionObjectives`, `GetMissionInfoItems`, `IsInActiveDungeonID`, `UpdateAllMissions`
//! - `standingMgr`: `GetCorpStandings`, `GetNPCNPCStandings`, `GetStandingCompositions`
//! - `industryManager`: `CancelJob`, `GetJob`, `GetJobCounts`
//! - `seasonalLoginCampaignManager`: `claim_reward`, `get_active_campaign`, `get_claim_history`
//! - `loginCampaignManager`: `claim_reward`, `get_client_campaign_state`

#![forbid(unsafe_code)]

const ALREADY_CLAIMED_ERR: &str = "DailyRewardAlreadyClaimedToday";

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
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
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
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// parity: rpc:missionTrackerMgr.GetAllMissionObjectives
// parity: rpc:missionTrackerMgr.GetMissionInfoItems
// parity: rpc:missionTrackerMgr.IsInActiveDungeonID
// parity: rpc:missionTrackerMgr.UpdateAllMissions
// parity: rpc:standingMgr.GetCorpStandings
// parity: rpc:standingMgr.GetStandingCompositions
async fn test_mission_tracker_and_standing_mgr_rpcs() {
    let ctx = ServerContext::new();
    let mut sess = login_pilot_char(&ctx, 28_004, "demo", "demo-secret", PILOT_1).await;
    sess.solar_system_id = Some(30_000_142);

    // Setup active mission in snapshot
    let mission_id: u32 = 10_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (PILOT_1, mission_id),
            eve_db::ActiveMissionRecord {
                mission_id,
                character_id: PILOT_1,
                agent_id: 3010001,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            },
        );
        // Setup standing in snapshot for PILOT_1
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.standings.insert(500_001, 8.5); // Caldari State
            ch.standings.insert(1_000_001, 5.0);
        }
    });

    // 1. missionTrackerMgr.GetAllMissionObjectives: returns all active mission objectives
    let all_objs = rpc(&ctx, &mut sess, "missionTrackerMgr", "GetAllMissionObjectives", &[])
        .await
        .expect("GetAllMissionObjectives");
    match all_objs {
        PyRep::List(items) => {
            assert!(!items.is_empty());
            let m_entry = items.iter().find(|i| field(i, "missionID").as_i64() == Some(mission_id as i64));
            assert!(m_entry.is_some());
            assert_eq!(field(m_entry.unwrap(), "state").as_str(), Some("Accepted"));
            assert_eq!(field(m_entry.unwrap(), "objectivesCompleted").as_bool(), Some(true));
        }
        other => panic!("expected List from GetAllMissionObjectives, got {other:?}"),
    }

    // 2. missionTrackerMgr.GetMissionInfoItems: queries filtered mission items
    let info_items = rpc(
        &ctx,
        &mut sess,
        "missionTrackerMgr",
        "GetMissionInfoItems",
        &[PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("GetMissionInfoItems");
    match info_items {
        PyRep::List(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(field(&items[0], "missionID").as_i64(), Some(mission_id as i64));
        }
        other => panic!("expected List from GetMissionInfoItems, got {other:?}"),
    }

    // 3. missionTrackerMgr.IsInActiveDungeonID: returns boolean whether in active mission system
    let in_dungeon = rpc(&ctx, &mut sess, "missionTrackerMgr", "IsInActiveDungeonID", &[])
        .await
        .expect("IsInActiveDungeonID");
    assert_eq!(in_dungeon, PyRep::Bool(true));

    // 4. missionTrackerMgr.UpdateAllMissions: refreshes all mission states
    let update_rep = rpc(&ctx, &mut sess, "missionTrackerMgr", "UpdateAllMissions", &[])
        .await
        .expect("UpdateAllMissions");
    match update_rep {
        PyRep::List(items) => {
            assert!(!items.is_empty());
            assert!(items.iter().any(|i| field(i, "missionID").as_i64() == Some(mission_id as i64)));
        }
        other => panic!("expected List from UpdateAllMissions, got {other:?}"),
    }

    // 5. standingMgr.GetCorpStandings: returns corporate aggregated standings
    let corp_standings = rpc(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[])
        .await
        .expect("GetCorpStandings");
    match corp_standings {
        PyRep::Rowset { rows, columns, .. } => {
            assert!(columns.contains(&"fromID".into()));
            assert!(columns.contains(&"standing".into()));
            let caldari = rows.iter().find(|r| r.first().and_then(|v| v.as_i64()) == Some(500_001));
            assert!(caldari.is_some());
        }
        other => panic!("expected Rowset from GetCorpStandings, got {other:?}"),
    }

    // 6. standingMgr.GetNPCNPCStandings: structural Rowset contract
    let npc_standings = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    match npc_standings {
        PyRep::Rowset { rows, columns, .. } => {
            assert!(!rows.is_empty(), "GetNPCNPCStandings must return non-empty Rowset");
            assert_eq!(columns, vec!["fromID", "toID", "standing"]);
            assert!(rows.iter().all(|r| r.len() == 3 && r[2].as_f64().is_some_and(|v| (-10.0..=10.0).contains(&v))));
        }
        other => panic!("expected Rowset from GetNPCNPCStandings, got {other:?}"),
    }

    // 7. standingMgr.GetStandingCompositions: returns standing composition descriptors
    let comp_rep = rpc(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingCompositions",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("GetStandingCompositions");
    match comp_rep {
        PyRep::List(items) => {
            assert!(!items.is_empty());
            assert_eq!(field(&items[0], "fromID").as_i64(), Some(500_001));
        }
        other => panic!("expected List from GetStandingCompositions, got {other:?}"),
    }
}

#[tokio::test]
// parity: feat:pve.mission-complete
// parity: feat:pve.mission-lp-isk-reward-formula
// parity: plan:E-5
async fn test_plan_e5_mission_complete_tracker_standings_and_reward_payouts() {
    eve_testkit::attest(&[
        "feat:pve.mission-complete",
        "feat:pve.mission-lp-isk-reward-formula",
        "plan:E-5",
    ]);

    use eve_db::{ActiveMissionRecord, IskCents, TrainedSkillRecord};
    use eve_server::services::agents_missions::{save_mission_meta, MissionTimingAndCargoMeta};

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 51_990, "demo", "demo-secret", PILOT_1).await;

    let agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.division_id == 24 && a.agent_type_id == 2 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 security agent")
        .clone();
    let ag_sys = agent.solar_system_id.unwrap();
    let ag_st = agent.station_id.unwrap();
    let true_sec = ctx.sde.solar_systems[&ag_sys].security_status;
    let now0 = ctx.now_unix();

    // Train Negotiation 2 (+10% ISK & bonus) and Security Connections 5 (+50% LP), clear standings
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.clone_grade = 2;
            ch.solar_system_id = ag_sys;
            ch.station_id = Some(ag_st);
            ch.balance = IskCents::from_isk(1_000_000);
            ch.standings.remove(&agent.corporation_id);
        }
        let sk = snap.skills.entry(PILOT_1).or_default();
        sk.insert(3356, TrainedSkillRecord { type_id: 3356, level: 2, skill_points: 10_000.0 });
        sk.insert(3895, TrainedSkillRecord { type_id: 3895, level: 5, skill_points: 256_000.0 });
        snap.active_missions.insert(
            (PILOT_1, 13_493),
            ActiveMissionRecord {
                character_id: PILOT_1,
                mission_id: 13_493, // Interstellar Railroad (4 of 4): 15,000 ISK, 16,000 bonus, 3,600s, 19 base LP
                agent_id: agent.agent_id,
                solar_system_id: ag_sys,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            PILOT_1,
            &MissionTimingAndCargoMeta {
                mission_id: 13_493,
                agent_id: agent.agent_id,
                offered_at_unix: now0,
                offer_expiry_unix: now0 + 86_400,
                offered_level: Some(1),
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(ag_sys);
    sess.station_id = Some(ag_st);

    let acc = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(agent.agent_id), PyRep::Integer(13_493)])
        .await
        .expect("Accept 13_493");
    assert_eq!(field(&acc, "bonusWindowSecs").as_i64(), Some(3_600));

    let tracked = rpc(&ctx, &mut sess, "missionTrackerMgr", "GetAllMissionObjectives", &[])
        .await
        .expect("GetAllMissionObjectives");
    let PyRep::List(obj_list) = tracked else { panic!("expected List") };
    assert!(obj_list.iter().any(|o| o.dict_get("missionID").and_then(|v| v.as_i64()) == Some(13_493)));

    let info_items = rpc(&ctx, &mut sess, "missionTrackerMgr", "GetMissionInfoItems", &[])
        .await
        .expect("GetMissionInfoItems");
    assert!(matches!(info_items, PyRep::Dict(_) | PyRep::List(_)));

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());
    let lp_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points.get(&agent.corporation_id).copied().unwrap_or(0));

    let done = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(agent.agent_id), PyRep::Integer(13_493)])
        .await
        .expect("Complete 13_493");
    // 15,000 * 1.10 = 16,500 ISK; 16,000 * 1.10 = 17,600 bonus; 19 * (1.6288 - true_sec) * 1.50 LP
    let exp_lp = (19.0 * (1.6288 - true_sec).max(0.01) * 1.50).round() as i64;
    assert_eq!(field(&done, "iskReward").as_i64(), Some(16_500));
    assert_eq!(field(&done, "bonusReward").as_i64(), Some(17_600));
    assert_eq!(field(&done, "lpReward").as_i64(), Some(exp_lp));
    assert_eq!(field(&done, "rewardItemID"), &PyRep::None);

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());
    let lp_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points[&agent.corporation_id]);
    assert_eq!(bal_after - bal_before, (16_500 + 17_600) * 100);
    assert_eq!(lp_after - lp_before, exp_lp);

    let corp_standings = rpc(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[])
        .await
        .expect("GetCorpStandings after mission completion");
    let has_corp_standing = match &corp_standings {
        PyRep::Rowset { rows, .. } => rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(agent.corporation_id) && r.get(1).and_then(|v| v.as_f64()).is_some_and(|s| s > 0.0)),
        PyRep::Dict(entries) => entries.iter().any(|(k, v)| k.as_i64() == Some(agent.corporation_id) && v.as_f64().is_some_and(|s| s > 0.0)),
        _ => false,
    };
    assert!(has_corp_standing, "Corp standing must increase and appear in standingMgr.GetCorpStandings");
}

#[tokio::test]
// parity: rpc:industryManager.CancelJob
// parity: rpc:industryManager.GetJob
// parity: rpc:industryManager.GetJobCounts
async fn test_industry_and_campaign_mgr_rpcs() {
    let ctx = ServerContext::new();
    let mut sess = login_pilot_char(&ctx, 28_005, "demo", "demo-secret", PILOT_1).await;

    // 1. industryManager.GetJobCounts: returns max and used slot counts
    let counts_rep = rpc(&ctx, &mut sess, "industryManager", "GetJobCounts", &[])
        .await
        .expect("GetJobCounts");
    let mfg_max = field(&counts_rep, "manufacturingMax").as_i64().expect("mfgMax");
    assert!(mfg_max >= 1);

    // Setup an active industry job in snapshot
    let job_id: i64 = 77_001;
    let bp_item_id: i64 = 55_001;
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            job_id,
            eve_db::IndustryJobRecord {
                job_id,
                installer_id: PILOT_1,
                facility_id: 60_003_760,
                blueprint_item_id: bp_item_id,
                blueprint_type_id: 688,
                activity_id: 1, // Manufacturing
                runs: 1,
                product_type_id: 688,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: now_unix, ..Default::default() },
        );
        snap.items.insert(
            bp_item_id,
            eve_db::InventoryItemRecord {
                item_id: bp_item_id,
                type_id: 688,
                owner_id: PILOT_1,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // 2. industryManager.GetJob: queries specific industry job by jobID
    let job_rep = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect("GetJob");
    assert_eq!(field(&job_rep, "jobID").as_i64(), Some(job_id));
    assert_eq!(field(&job_rep, "status").as_i64(), Some(1));

    // 3. industryManager.CancelJob: cancels active job, releasing blueprint to Hangar
    let cancel_rep = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect("CancelJob");
    assert_eq!(field(&cancel_rep, "jobID").as_i64(), Some(job_id));
    assert_eq!(field(&cancel_rep, "status").as_str(), Some("Cancelled"));

    // Verify snapshot state updated
    ctx.db.with_snapshot(|snap| {
        let job = snap.industry_jobs.get(&job_id).expect("job exists");
        assert_eq!(job.status, "Cancelled");
        let item = snap.items.get(&bp_item_id).expect("item exists");
        assert_eq!(item.flag, eve_db::InventoryFlag::Hangar);
    });

    // 4. loginCampaignManager.get_client_campaign_state: returns SDE daily login campaign progress
    let login_state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state");
    let active_cid = field(field(&login_state, "static_data"), "campaign_id")
        .as_i64()
        .expect("active campaign_id");
    let expected_alpha_tid = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)[&active_cid]
        .alpha_rewards
        .values()
        .next()
        .expect("day 1 alpha reward")
        .type_id;
    assert_eq!(
        field(field(&login_state, "item_progress"), "num_claimed_rewards").as_i64(),
        Some(0)
    );

    // 5. loginCampaignManager.claim_reward: delivers SDE day 0 rewards and returns 4-tuple
    let login_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    let PyRep::Tuple(login_claim_tuple) = &login_claim else {
        panic!("expected Tuple from claim_reward");
    };
    assert_eq!(field(&login_claim_tuple[0], "typeID").as_i64(), Some(i64::from(expected_alpha_tid)));
    let alpha_item_id = field(&login_claim_tuple[0], "itemID")
        .as_i64()
        .expect("delivered alpha itemID");
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&alpha_item_id).expect("minted login reward item");
        assert_eq!(item.owner_id, PILOT_1);
        assert_eq!(item.type_id, expected_alpha_tid);
        assert_eq!(item.flag, eve_db::InventoryFlag::Hangar);
    });

    // Calling claim_reward a second time same day raises DailyRewardAlreadyClaimedToday
    let login_claim_repeat = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("claim_reward repeat raises UserError");
    assert_eq!(login_claim_repeat.code, ALREADY_CLAIMED_ERR);

    // 6. seasonalLoginCampaignManager.get_active_campaign: schedule SDE seasonal campaign 2323 via GM slash command
    let mut gm_sess = SessionState::new(28_099);
    gm_sess.user_id = Some(999);
    gm_sess.character_id = Some(PILOT_1);
    gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())],
    )
    .await
    .expect("GM schedule seasonal campaign 2323");
    let seasonal_active = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("get_active_campaign");
    let PyRep::Tuple(seasonal_tuple) = &seasonal_active else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(seasonal_tuple[0].as_i64(), Some(2323));
    assert_eq!(seasonal_tuple[2].as_str(), Some("11:00:00"));
    assert_eq!(seasonal_tuple[3].as_i64(), Some(30));

    // 7. seasonalLoginCampaignManager.claim_reward: delivers SDE seasonal reward (87900 + 49809) and returns Bool
    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("claim_reward");
    assert_eq!(seasonal_claim, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .items
            .values()
            .any(|it| it.owner_id == PILOT_1 && it.type_id == 87_900));
    });

    // Calling seasonal claim a second time should fail
    let seasonal_claim_err = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await;
    assert!(seasonal_claim_err.is_err());

    // 8. seasonalLoginCampaignManager.get_claim_history: returns claimed timestamps Dict
    let seasonal_history = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("get_claim_history");
    match seasonal_history {
        PyRep::Dict(items) => assert_eq!(items.len(), 1),
        other => panic!("expected Dict from get_claim_history, got {other:?}"),
    }
}

#[tokio::test]
// parity: feat:account.redeem-tokens
// parity: rpc:userSvc.ClaimRedeemTokens
// parity: rpc:userSvc.GetRedeemTokens
async fn test_docked_login_reward_does_not_enqueue_redeem_token() {
    let ctx = ServerContext::new();
    let pilot_id: i64 = PILOT_1;
    let mut sess = login_pilot_char(&ctx, 28_006, "demo", "demo-secret", pilot_id).await;

    // 1. Schedule SDE campaign 2323 and claim login reward while docked
    let mut gm_sess = SessionState::new(28_098);
    gm_sess.user_id = Some(999);
    gm_sess.character_id = Some(pilot_id);
    gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())],
    )
    .await
    .expect("GM schedule seasonal campaign 2323");
    assert!(sess.station_id.is_some());
    let init_items = ctx.db.with_snapshot(|s| s.items.len());
    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("claim seasonal reward while docked");
    assert_eq!(seasonal_claim, PyRep::Bool(true));

    // Confirm items were delivered directly to hangar
    ctx.db.with_snapshot(|s| {
        assert!(s.items.len() > init_items);
        assert!(s.items.values().any(|it| it.owner_id == pilot_id && it.type_id == 87_900));
    });

    // P0-LOGIN-REWARD-DUP: Ensure no redeem tokens were created for the docked claim
    let tokens_rep = rpc(&ctx, &mut sess, "userSvc", "GetRedeemTokens", &[])
        .await
        .expect("GetRedeemTokens");
    let PyRep::List(tokens) = tokens_rep else {
        panic!("expected List from GetRedeemTokens");
    };
    assert!(tokens.is_empty(), "no redeem tokens should exist after docked claim");

    let claim_err = rpc(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(999_999)]), PyRep::Integer(pilot_id)],
    )
    .await
    .expect_err("claiming non-existent redeem token must fail");
    assert_eq!(claim_err.code, "RedeemTokenNotFound");

    // 2. Test expiration check in ClaimRedeemTokens
    let uid = sess.user_id.unwrap();
    let now_ft = ctx.now_blueos_filetime_ticks();
    let expired_tok_id = ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::account_services::enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            34, // Tritanium
            100,
            "Expired Token".into(),
            None,
            Some(now_ft - 1_000_000), // Expired in past
        )
    });

    // Claiming expired token should fail with RedeemTokenNotFound
    let expired_claim_err = rpc(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(expired_tok_id)]), PyRep::Integer(pilot_id)],
    )
    .await
    .expect_err("claiming expired token must fail");
    assert_eq!(expired_claim_err.code, "RedeemTokenNotFound");

    // 3. Test unexpired token can still be claimed
    let valid_tok_id = ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::account_services::enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            34,
            500,
            "Valid Token".into(),
            None,
            Some(now_ft + 86_400 * 10_000_000), // Future expiry
        )
    });
    let valid_claim = rpc(
        &ctx,
        &mut sess,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(valid_tok_id)]), PyRep::Integer(pilot_id)],
    )
    .await
    .expect("claiming valid token must succeed");
    let PyRep::KeyVal(kv) = valid_claim else {
        panic!("expected KeyVal from ClaimRedeemTokens");
    };
    let Some(PyRep::List(claimed)) = kv.get("claimedTokenIDs") else {
        panic!("claimedTokenIDs");
    };
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].as_i64(), Some(valid_tok_id));
}
