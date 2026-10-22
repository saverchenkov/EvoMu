//! Comprehensive integration tests for Operations Manager (`operationsManager`)
//! and Dungeon Instance Cache (`dungeonInstanceCacheMgr`).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::{fw_services, insurgency_services};
use std::collections::HashMap;

const P1: i64 = 90_000_001;

async fn select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_operations_tutorial_and_career_progression() {
    // parity: rpc:operationsManager.complete_category_for_character, rpc:operationsManager.get_active_category_id
    // parity: rpc:operationsManager.get_category_state, rpc:operationsManager.get_character_progress
    // parity: rpc:operationsManager.get_tutorial_state, rpc:operationsManager.has_skipped_tutorial
    // parity: rpc:operationsManager.is_category_complete, rpc:operationsManager.is_main_tutorial_finished
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 70_001, "demo", "demo-secret", P1).await;

    // 1. Tutorial state queries
    let tut_state = rpc(&ctx, &mut p1, "operationsManager", "get_tutorial_state", &[])
        .await
        .expect("get_tutorial_state");
    let PyRep::Dict(tut_dict) = tut_state else {
        panic!("expected Dict from get_tutorial_state");
    };
    let is_tut_active = tut_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("is_tutorial_active"))
        .map(|(_, v)| v);
    assert_eq!(is_tut_active, Some(&PyRep::Bool(false)));

    let has_skipped = rpc(&ctx, &mut p1, "operationsManager", "has_skipped_tutorial", &[])
        .await
        .expect("has_skipped_tutorial");
    assert_eq!(has_skipped, PyRep::Bool(false));

    let main_tut_finished = rpc(&ctx, &mut p1, "operationsManager", "is_main_tutorial_finished", &[])
        .await
        .expect("is_main_tutorial_finished");
    assert_eq!(main_tut_finished, PyRep::Bool(true));

    // 2. Category state checks
    let active_cat = rpc(&ctx, &mut p1, "operationsManager", "get_active_category_id", &[])
        .await
        .expect("get_active_category_id");
    assert_eq!(active_cat, PyRep::None);

    let cat_id = 101;
    let is_comp_initial = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("is_category_complete initial");
    assert_eq!(is_comp_initial, PyRep::Bool(false));

    let state_avail = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("get_category_state initial");
    assert_eq!(state_avail, PyRep::Integer(1)); // AVAILABLE = 1

    // 3. Complete category
    let comp_res = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("complete_category_for_character");
    assert_eq!(comp_res, PyRep::Bool(true));

    let notifs = ctx.drain_notifications_for_char(P1);
    assert!(notifs.iter().any(|n| n.event == "OnOperationCategoryCompleted"));

    // 4. Verify category completed and progress updated
    let is_comp_after = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("is_category_complete after");
    assert_eq!(is_comp_after, PyRep::Bool(true));

    let state_comp = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(cat_id)],
    )
    .await
    .expect("get_category_state after");
    assert_eq!(state_comp, PyRep::Integer(3)); // COMPLETE = 3

    let progress = rpc(&ctx, &mut p1, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress");
    let PyRep::Dict(prog_dict) = progress else {
        panic!("expected Dict from get_character_progress");
    };
    let cat_prog = prog_dict
        .iter()
        .find(|(k, _)| k.as_i64() == Some(cat_id))
        .map(|(_, v)| v);
    assert_eq!(cat_prog, Some(&PyRep::Integer(100)));
}

#[tokio::test]
async fn test_operations_tasks_operations_and_telemetry() {
    // parity: rpc:operationsManager.activate_operation_in_solar_system, rpc:operationsManager.block_task
    // parity: rpc:operationsManager.get_mission_avoidance_systems, rpc:operationsManager.get_operations_completed_at_least_once
    // parity: rpc:operationsManager.process_client_event, rpc:operationsManager.skip_task
    // parity: rpc:operationsManager.start_site, rpc:operationsManager.unblock_task
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 70_002, "demo", "demo-secret", P1).await;

    // 1. Initial avoidance systems and completed operations
    let avoid = rpc(&ctx, &mut p1, "operationsManager", "get_mission_avoidance_systems", &[])
        .await
        .expect("get_mission_avoidance_systems");
    assert!(matches!(avoid, PyRep::List(_)));

    let completed_ops = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "get_operations_completed_at_least_once",
        &[],
    )
    .await
    .expect("get_operations_completed_at_least_once");
    assert!(matches!(completed_ops, PyRep::List(_)));

    // 2. Activate operation
    let op_id = 501;
    let act_op = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id)],
    )
    .await
    .expect("activate_operation_in_solar_system");
    assert_eq!(act_op, PyRep::Bool(true));

    let notifs = ctx.drain_notifications_for_char(P1);
    assert!(notifs.iter().any(|n| n.event == "OnOperationCategoryChanged"));

    // 3. Block and unblock task
    let task_id = 2001;
    let blk = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "block_task",
        &[PyRep::Integer(task_id)],
    )
    .await
    .expect("block_task");
    assert_eq!(blk, PyRep::Bool(true));

    let unblk = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "unblock_task",
        &[PyRep::Integer(task_id)],
    )
    .await
    .expect("unblock_task");
    assert_eq!(unblk, PyRep::Bool(true));

    // 4. Skip task
    let task_to_skip = 2002;
    let skip = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "skip_task",
        &[PyRep::Integer(task_to_skip)],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip, PyRep::Bool(true));

    let notifs_skip = ctx.drain_notifications_for_char(P1);
    assert!(notifs_skip.iter().any(|n| n.event == "OnOperationTaskCompleted"));

    // 5. Start site
    let site_id = 9001;
    let start = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(site_id)],
    )
    .await
    .expect("start_site");
    assert_eq!(start, PyRep::Bool(true));

    // 6. Process client event (telemetry)
    let event_res = rpc(&ctx, &mut p1, "operationsManager", "process_client_event", &[])
        .await
        .expect("process_client_event");
    assert_eq!(event_res, PyRep::None);
}

#[tokio::test]
async fn test_dungeon_instance_cache_queries_and_counts() {
    // parity: rpc:dungeonInstanceCacheMgr.GetBulwarkMiningSiteCounts, rpc:dungeonInstanceCacheMgr.GetCombatAnomaliesCount
    // parity: rpc:dungeonInstanceCacheMgr.GetCombatAnomalyInstances, rpc:dungeonInstanceCacheMgr.GetFactionWarfareBattlefieldInstances
    // parity: rpc:dungeonInstanceCacheMgr.GetFactionWarfareInstances, rpc:dungeonInstanceCacheMgr.GetIceBeltInstances
    // parity: rpc:dungeonInstanceCacheMgr.GetIceBeltsCount, rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCount
    // parity: rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCountInRange, rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyIceHeistInstances
    // parity: rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyInstances, rpc:dungeonInstanceCacheMgr.GetSignatureInstances
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 70_003, "demo", "demo-secret", P1).await;
    let sys_id: u32 = 30_000_142; // Jita solar system
    p1.solar_system_id = Some(sys_id);

    // 1. Seed live FW battlefield instance & Pirate Insurgency sites
    let bf_id = fw_services::spawn_battlefield_site(&ctx, sys_id, 500001, "Caldari-Gallente Frontline Battlefield");
    assert!(bf_id > 0);

    let heist_id = insurgency_services::spawn_ice_heist(&ctx, sys_id);
    assert!(heist_id > 0);

    insurgency_services::with_insurgency_runtime(&ctx, |rt| {
        rt.insurgency_instances.push(insurgency_services::InsurgencySiteRecord {
            instance_id: 888_001,
            solar_system_id: sys_id,
            site_type: "MiningRaid".into(),
            name: "Guristas Mining Ambush".into(),
            corruption_reward: 15.0,
            completed: false,
        });
    });

    // 2. Query FW and Insurgency instances
    let fw_bf = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[])
        .await
        .expect("GetFactionWarfareBattlefieldInstances");
    let PyRep::List(bf_list) = fw_bf else {
        panic!("expected List from GetFactionWarfareBattlefieldInstances");
    };
    assert!(bf_list.iter().any(|item| item.dict_get("instanceID").and_then(|v| v.as_i64()) == Some(bf_id)));

    let f_inst = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[])
        .await
        .expect("GetFactionWarfareInstances");
    assert!(matches!(f_inst, PyRep::Dict(_)));

    let p_ins = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[])
        .await
        .expect("GetPirateInsurgencyInstances");
    let PyRep::List(ins_list) = p_ins else {
        panic!("expected List from GetPirateInsurgencyInstances");
    };
    assert!(ins_list.iter().any(|item| item.dict_get("instanceID").and_then(|v| v.as_i64()) == Some(888_001)));

    let p_heist = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[])
        .await
        .expect("GetPirateInsurgencyIceHeistInstances");
    let PyRep::List(heist_list) = p_heist else {
        panic!("expected List from GetPirateInsurgencyIceHeistInstances");
    };
    assert!(heist_list.iter().any(|item| item.dict_get("instanceID").and_then(|v| v.as_i64()) == Some(heist_id)));

    // 3. Combat anomalies: instances and count
    let combat_inst = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[])
        .await
        .expect("GetCombatAnomalyInstances");
    assert!(matches!(combat_inst, PyRep::Dict(_)));

    let combat_cnt = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetCombatAnomaliesCount", &[])
        .await
        .expect("GetCombatAnomaliesCount");
    let PyRep::Integer(c_count) = combat_cnt else {
        panic!("expected Integer from GetCombatAnomaliesCount");
    };
    assert!(c_count >= 0);

    // 4. Ore anomalies: count and range
    let ore_cnt = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[])
        .await
        .expect("GetOreAnomaliesCount");
    let PyRep::Integer(o_count) = ore_cnt else {
        panic!("expected Integer from GetOreAnomaliesCount");
    };
    assert!(o_count >= 0);

    let ore_range = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetOreAnomaliesCountInRange", &[])
        .await
        .expect("GetOreAnomaliesCountInRange");
    let PyRep::Dict(ore_map) = ore_range else {
        panic!("expected Dict from GetOreAnomaliesCountInRange");
    };
    assert!(ore_map.iter().any(|(k, _)| k.as_i64() == Some(sys_id as i64)));

    // 5. Ice belts: instances and count
    let ice_inst = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetIceBeltInstances", &[])
        .await
        .expect("GetIceBeltInstances");
    assert!(matches!(ice_inst, PyRep::Dict(_)));

    let ice_cnt = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetIceBeltsCount", &[])
        .await
        .expect("GetIceBeltsCount");
    let PyRep::Integer(i_count) = ice_cnt else {
        panic!("expected Integer from GetIceBeltsCount");
    };
    assert!(i_count >= 0);

    // 6. Signatures and Bulwark mining counts
    let sigs = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[])
        .await
        .expect("GetSignatureInstances");
    assert!(matches!(sigs, PyRep::Dict(_)));

    let bulwark = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetBulwarkMiningSiteCounts", &[])
        .await
        .expect("GetBulwarkMiningSiteCounts");
    let PyRep::Dict(bulwark_map) = bulwark else {
        panic!("expected Dict from GetBulwarkMiningSiteCounts");
    };
    assert!(bulwark_map.iter().any(|(k, _)| k.as_i64() == Some(sys_id as i64)));
}
