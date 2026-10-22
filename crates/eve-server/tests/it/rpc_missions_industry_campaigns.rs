//! Integration tests for Mission Tracking, Standings, Industry, and Login Campaigns RPCs:
//! - `missionTrackerMgr`: `GetAllMissionObjectives`, `GetMissionInfoItems`, `IsInActiveDungeonID`, `UpdateAllMissions`
//! - `standingMgr`: `GetCorpStandings`, `GetNPCNPCStandings`, `GetStandingCompositions`
//! - `industryManager`: `CancelJob`, `GetJob`, `GetJobCounts`
//! - `seasonalLoginCampaignManager`: `claim_reward`, `get_active_campaign`, `get_claim_history`
//! - `loginCampaignManager`: `claim_reward`, `get_client_campaign_state`

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
// parity: rpc:standingMgr.GetNPCNPCStandings
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
                spawned_npc_ball_ids: Vec::new(),
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

    // 6. standingMgr.GetNPCNPCStandings: returns static inter-NPC standings
    let npc_standings = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    match npc_standings {
        PyRep::Rowset { rows, columns, .. } => {
            assert!(!rows.is_empty());
            assert_eq!(columns, vec!["fromID", "toID", "standing"]);
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
// parity: rpc:industryManager.CancelJob
// parity: rpc:industryManager.GetJob
// parity: rpc:industryManager.GetJobCounts
// parity: rpc:seasonalLoginCampaignManager.claim_reward
// parity: rpc:seasonalLoginCampaignManager.get_active_campaign
// parity: rpc:seasonalLoginCampaignManager.get_claim_history
// parity: rpc:loginCampaignManager.claim_reward
// parity: rpc:loginCampaignManager.get_client_campaign_state
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
                start_unix: now_unix,
            },
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
    assert_eq!(field(&job_rep, "status").as_str(), Some("Active"));

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

    // 4. loginCampaignManager.get_client_campaign_state: returns daily login campaign progress
    let login_state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state");
    assert_eq!(field(&login_state, "campaignID").as_i64(), Some(101));
    assert_eq!(field(&login_state, "canClaimToday").as_bool(), Some(true));

    // 5. loginCampaignManager.claim_reward: claims daily reward granting SP
    let login_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_eq!(field(&login_claim, "campaignID").as_i64(), Some(101));
    assert_eq!(field(&login_claim, "spGranted").as_f64(), Some(5_000.0));

    // Calling claim_reward a second time should fail
    let login_claim_err = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await;
    assert!(login_claim_err.is_err());

    // 6. seasonalLoginCampaignManager.get_active_campaign: returns seasonal event info
    let seasonal_active = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("get_active_campaign");
    assert_eq!(field(&seasonal_active, "campaignID").as_i64(), Some(202));
    assert!(field(&seasonal_active, "daysRemaining").as_i64().unwrap_or(0) > 0);

    // 7. seasonalLoginCampaignManager.claim_reward: claims seasonal reward granting SP
    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("claim_reward");
    assert_eq!(field(&seasonal_claim, "campaignID").as_i64(), Some(202));
    assert_eq!(field(&seasonal_claim, "spGranted").as_f64(), Some(15_000.0));

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

    // 8. seasonalLoginCampaignManager.get_claim_history: returns claimed timestamps
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
        PyRep::List(items) => assert_eq!(items.len(), 1),
        other => panic!("expected List from get_claim_history, got {other:?}"),
    }
}
