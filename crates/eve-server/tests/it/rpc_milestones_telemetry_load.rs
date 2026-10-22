//! Parity integration tests for Milestones (`milestoneMgr`), Monolith Task Tracker (`monolithTaskTrackerMgr`),
//! Fighters (`fighterMgr`), Rewards (`rewardMgr`), Colony Skyhooks (`colonyResourcesMgr`),
//! Dev Indices (`devIndexManager`), Epic Arcs (`epicArcStatus`), Client Telemetry (`infoGatheringMgr`, `clientStatLogger`),
//! External Queue (`externalQueueMgr`), and Server Load Monitoring (`loadService`).

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{ActiveFighterSquadron, ServerContext};
use eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK;

use crate::common;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

#[tokio::test]
// parity: rpc:milestoneMgr.ClaimRewards
// parity: rpc:milestoneMgr.ProcessCharacterLogon
// parity: rpc:monolithTaskTrackerMgr.HasEverWarped
// parity: rpc:monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
// parity: rpc:fighterMgr.CmdScoopAbandonedFighterFromSpace
async fn test_milestones_task_tracker_and_fighters() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;

    // 1. milestoneMgr.ProcessCharacterLogon
    let logon_rep = ctx
        .dispatch_rpc(&mut sess, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
        .await
        .expect("ProcessCharacterLogon");
    assert!(is_keyval(&logon_rep));
    assert_eq!(logon_rep.dict_get("characterID"), Some(&PyRep::Integer(sess.character_id.unwrap())));

    // 2. milestoneMgr.ClaimRewards
    let claim_rep = ctx
        .dispatch_rpc(&mut sess, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect("ClaimRewards");
    assert!(is_keyval(&claim_rep));
    assert_eq!(claim_rep.dict_get("milestoneID"), Some(&PyRep::Integer(1)));

    // 3. monolithTaskTrackerMgr.HasEverWarped
    let warped_rep = ctx
        .dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "HasEverWarped", &[], &kw, None)
        .await
        .expect("HasEverWarped");
    assert_eq!(warped_rep, PyRep::Bool(true));

    // 4. monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
    let update_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "monolithTaskTrackerMgr",
            None,
            "UpdateClientAchievmentsAndCounters",
            &[
                PyRep::List(vec![PyRep::Integer(101)]),
                PyRep::Dict(vec![(PyRep::String("jumps".into()), PyRep::Integer(10))]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateClientAchievmentsAndCounters");
    assert_eq!(update_rep, PyRep::None);

    // 5. fighterMgr.CmdScoopAbandonedFighterFromSpace
    let fighter_ball_id = 88_001i64;
    let fighter_item_id = 99_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item_id,
            eve_db::InventoryItemRecord {
                item_id: fighter_item_id,
                type_id: 40_555, // Fighter type
                owner_id: sess.character_id.unwrap(),
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: fighter_ball_id,
            item_id: fighter_item_id,
            type_id: 40_555,
            owner_char_id: sess.character_id.unwrap(),
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: true,
            mwd_active: false,
        },
    );

    let scoop_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(fighter_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdScoopAbandonedFighterFromSpace");
    assert_eq!(scoop_rep, PyRep::Bool(true));

    // Verify fighter item moved into ship's FighterBay
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&fighter_item_id).expect("fighter item in db");
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, eve_db::InventoryFlag::FighterBay);
    });
    // Verify fighter removed from active_fighters
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&fighter_ball_id));
}

#[tokio::test]
// parity: rpc:rewardMgr.GetRewardLPLogs
// parity: rpc:rewardMgr.GetDelayedRewardsByGroupIDs
// parity: rpc:colonyResourcesMgr.GetMyCorporationSkyhooks
// parity: rpc:devIndexManager.GetAllDevelopmentIndices
// parity: rpc:epicArcStatus.AgentHasEpicMissionsForCharacter
async fn test_rewards_sov_and_epic_arc_status() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = sess.character_id.expect("char_id");
    let corp_id = 98_000_101i64;
    let sys_id = 30_000_142u32;
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);

    // 1. rewardMgr.GetRewardLPLogs
    let lplogs_rep = ctx
        .dispatch_rpc(&mut sess, "rewardMgr", None, "GetRewardLPLogs", &[], &kw, None)
        .await
        .expect("GetRewardLPLogs");
    let PyRep::List(logs) = &lplogs_rep else { panic!("Expected List") };
    assert!(logs.is_empty() || is_keyval(&logs[0]));

    // 2. rewardMgr.GetDelayedRewardsByGroupIDs
    let delayed_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "rewardMgr",
            None,
            "GetDelayedRewardsByGroupIDs",
            &[PyRep::List(vec![PyRep::Integer(501)])],
            &kw,
            None,
        )
        .await
        .expect("GetDelayedRewardsByGroupIDs");
    let PyRep::Dict(delayed_dict) = &delayed_rep else { panic!("Expected Dict") };
    let group_rewards = delayed_dict.iter().find(|(k, _)| k == &PyRep::Integer(501));
    assert!(group_rewards.is_some(), "Group 501 delayed rewards found");
    let (_, PyRep::List(rew_list)) = group_rewards.unwrap() else { panic!("Expected List") };
    assert_eq!(rew_list[0].dict_get("rewardQuantity"), Some(&PyRep::Integer(5000)));

    // 3. colonyResourcesMgr.GetMyCorporationSkyhooks
    let skyhook_id = 60_000_501i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            skyhook_id,
            eve_db::UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Corp Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let skyhooks_rep = ctx
        .dispatch_rpc(&mut sess, "colonyResourcesMgr", None, "GetMyCorporationSkyhooks", &[], &kw, None)
        .await
        .expect("GetMyCorporationSkyhooks");
    let PyRep::List(skyhook_list) = &skyhooks_rep else { panic!("Expected List") };
    assert!(!skyhook_list.is_empty(), "Skyhook list not empty");
    assert_eq!(skyhook_list[0].dict_get("skyhookID"), Some(&PyRep::Integer(skyhook_id)));
    assert_eq!(skyhook_list[0].dict_get("corporationID"), Some(&PyRep::Integer(corp_id)));

    // 4. devIndexManager.GetAllDevelopmentIndices
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.military_index = 3;
        st.industrial_index = 2;
        st.strategic_index = 1;
    });

    let indices_rep = ctx
        .dispatch_rpc(&mut sess, "devIndexManager", None, "GetAllDevelopmentIndices", &[], &kw, None)
        .await
        .expect("GetAllDevelopmentIndices");
    let PyRep::Dict(idx_dict) = &indices_rep else { panic!("Expected Dict") };
    let sys_indices = idx_dict.iter().find(|(k, _)| k == &PyRep::Integer(sys_id as i64));
    assert!(sys_indices.is_some(), "System indices found");
    let (_, idx_kv) = sys_indices.unwrap();
    assert_eq!(idx_kv.dict_get("military"), Some(&PyRep::Integer(3)));
    assert_eq!(idx_kv.dict_get("industrial"), Some(&PyRep::Integer(2)));

    // 5. epicArcStatus.AgentHasEpicMissionsForCharacter
    let agent_id = 3_019_356i64; // Blood-Stained Stars starter agent
    let epic_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "epicArcStatus",
            None,
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(agent_id)],
            &kw,
            None,
        )
        .await
        .expect("AgentHasEpicMissionsForCharacter");
    if ctx.sde.agents.contains_key(&agent_id) {
        assert_eq!(epic_rep, PyRep::Bool(true));
    } else {
        assert!(matches!(epic_rep, PyRep::Bool(_)));
    }
}

#[tokio::test]
// parity: rpc:infoGatheringMgr.GetStateAndConfig
// parity: rpc:infoGatheringMgr.LogInfoEventsFromClient
// parity: rpc:clientStatLogger.LogString
// parity: rpc:externalQueueMgr.PublishProxyEvent
// parity: rpc:loadService.GetTotalStats
// parity: rpc:loadService.SetConfig
async fn test_telemetry_logging_and_load_service() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. infoGatheringMgr.GetStateAndConfig
    let cfg_rep = ctx
        .dispatch_rpc(&mut sess, "infoGatheringMgr", None, "GetStateAndConfig", &[], &kw, None)
        .await
        .expect("GetStateAndConfig");
    assert!(is_keyval(&cfg_rep));
    assert_eq!(cfg_rep.dict_get("isEnabled"), Some(&PyRep::Integer(0)));
    assert_eq!(cfg_rep.dict_get("clientWorkerInterval"), Some(&PyRep::Integer(600_000)));

    // 2. infoGatheringMgr.LogInfoEventsFromClient
    let log_info = ctx
        .dispatch_rpc(
            &mut sess,
            "infoGatheringMgr",
            None,
            "LogInfoEventsFromClient",
            &[PyRep::List(vec![PyRep::String("ui_open_window".into())])],
            &kw,
            None,
        )
        .await
        .expect("LogInfoEventsFromClient");
    assert_eq!(log_info, PyRep::None);

    // 3. clientStatLogger.LogString
    let log_str = ctx
        .dispatch_rpc(
            &mut sess,
            "clientStatLogger",
            None,
            "LogString",
            &[PyRep::String("render_fps: 60".into())],
            &kw,
            None,
        )
        .await
        .expect("LogString");
    assert_eq!(log_str, PyRep::None);

    // 4. externalQueueMgr.PublishProxyEvent
    let pub_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "externalQueueMgr",
            None,
            "PublishProxyEvent",
            &[PyRep::String("ClusterNodeHeartbeat".into()), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect("PublishProxyEvent");
    assert_eq!(pub_rep, PyRep::Bool(true));

    // 5. loadService.GetTotalStats
    let stats_rep = ctx
        .dispatch_rpc(&mut sess, "loadService", None, "GetTotalStats", &[], &kw, None)
        .await
        .expect("GetTotalStats");
    assert!(is_keyval(&stats_rep));
    assert_eq!(stats_rep.dict_get("isOverloaded"), Some(&PyRep::Bool(false)));
    assert_eq!(stats_rep.dict_get("nodeID"), Some(&PyRep::Integer(1001)));

    // 6. loadService.SetConfig (GM role required)
    let unauth_set = ctx
        .dispatch_rpc(
            &mut sess,
            "loadService",
            None,
            "SetConfig",
            &[PyRep::Dict(vec![(PyRep::String("sampleInterval".into()), PyRep::Integer(1000))])],
            &kw,
            None,
        )
        .await;
    assert!(unauth_set.is_err());
    assert_eq!(unauth_set.unwrap_err().code, "InsufficientRole");

    sess.role = eve_machonet::service_roles::ROLE_GMS;
    let auth_set = ctx
        .dispatch_rpc(
            &mut sess,
            "loadService",
            None,
            "SetConfig",
            &[PyRep::Dict(vec![(PyRep::String("sampleInterval".into()), PyRep::Integer(1000))])],
            &kw,
            None,
        )
        .await
        .expect("SetConfig GM");
    assert_eq!(auth_set, PyRep::Bool(true));
}
