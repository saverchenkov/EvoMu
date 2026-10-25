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
    eve_testkit::attest(&[
        "rpc:operationsManager.complete_category_for_character",
        "rpc:operationsManager.get_active_category_id",
        "rpc:operationsManager.get_category_state",
        "rpc:operationsManager.get_character_progress",
        "rpc:operationsManager.get_tutorial_state",
        "rpc:operationsManager.has_skipped_tutorial",
        "rpc:operationsManager.is_category_complete",
        "rpc:operationsManager.is_main_tutorial_finished",
    ]);
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
    // parity: rpc:survey.ClaimSurveyRewards, rpc:survey.GetActiveSurveyID, rpc:survey.PerformSurveyChecks
    eve_testkit::attest(&[
        "rpc:operationsManager.activate_operation_in_solar_system",
        "rpc:operationsManager.block_task",
        "rpc:operationsManager.get_mission_avoidance_systems",
        "rpc:operationsManager.get_operations_completed_at_least_once",
        "rpc:operationsManager.process_client_event",
        "rpc:operationsManager.skip_task",
        "rpc:operationsManager.start_site",
        "rpc:operationsManager.unblock_task",
        "rpc:survey.ClaimSurveyRewards",
        "rpc:survey.GetActiveSurveyID",
        "rpc:survey.PerformSurveyChecks",
    ]);
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
    let cat_ev = notifs.iter().find(|n| n.event == "OnOperationCategoryChanged").expect("OnOperationCategoryChanged");
    assert_eq!(cat_ev.args.len(), 2, "OnOperationCategoryChanged must have 2 arguments");
    assert_eq!(cat_ev.args[1], PyRep::Integer(op_id));
    let cat_id = cat_ev.args[0].as_i64().unwrap_or(1);

    // 3. Block and unblock task with 3 positional arguments (category_id, operation_id, task_id)
    let task_id = 2001;
    let blk = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "block_task",
        &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_id)],
    )
    .await
    .expect("block_task");
    assert_eq!(blk, PyRep::Bool(true));

    let unblk = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "unblock_task",
        &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_id)],
    )
    .await
    .expect("unblock_task");
    assert_eq!(unblk, PyRep::Bool(true));

    // 4. Skip task with 3 positional arguments (category_id, operation_id, task_id)
    let task_to_skip = 2002;
    let skip = rpc(
        &ctx,
        &mut p1,
        "operationsManager",
        "skip_task",
        &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_to_skip)],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip, PyRep::Bool(true));

    let notifs_skip = ctx.drain_notifications_for_char(P1);
    let skip_ev = notifs_skip.iter().find(|n| n.event == "OnOperationTaskCompleted").expect("OnOperationTaskCompleted");
    assert_eq!(skip_ev.args, vec![PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_to_skip)]);

    // Progress computed from actual operation task count (1 of 3 tasks in op 501 finished = 33%)
    let mid_prog = rpc(&ctx, &mut p1, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress mid");
    let PyRep::Dict(mid_entries) = mid_prog else { panic!("expected Dict") };
    let op_prog = mid_entries.iter().find(|(k, _)| k.as_i64() == Some(op_id)).map(|(_, v)| v);
    assert_eq!(op_prog, Some(&PyRep::Integer(33)), "Active operation progress must be computed from actual task counts (1 of 3 tasks = 33%)");

    // Verify task-to-operation resolution from task definition when op_id is omitted (not task_id / 10 which would be 200)
    let p2_char: i64 = 90_000_002;
    let mut p2 = select(&ctx, 70_004, "other", "other-secret", p2_char).await;
    rpc(&ctx, &mut p2, "operationsManager", "skip_task", &[PyRep::Integer(task_id)]).await.expect("skip_task single arg");
    let p2_notifs = ctx.drain_notifications_for_char(p2_char);
    let p2_skip_ev = p2_notifs.iter().find(|n| n.event == "OnOperationTaskCompleted").expect("OnOperationTaskCompleted");
    assert_eq!(p2_skip_ev.args[1], PyRep::Integer(op_id), "op_id must resolve to 501 via task definition mapping, not 200 via task_id / 10");

    // 5. Start site (must be a valid SDE dungeon ID)
    assert!(
        rpc(
            &ctx,
            &mut p1,
            "operationsManager",
            "start_site",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .is_err(),
        "start_site must reject non-existent dungeon IDs"
    );
    let site_id = 4087;
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

    // 6. Process client event: blocked task does not transition; unblocked task transitions and updates progress
    let blocked_tid = 2003;
    rpc(&ctx, &mut p1, "operationsManager", "block_task", &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(blocked_tid)]).await.expect("block 2003");
    let blocked_ev = rpc(&ctx, &mut p1, "operationsManager", "process_client_event", &[PyRep::String("task_ping".into()), PyRep::Integer(blocked_tid)]).await.expect("process blocked");
    assert_eq!(blocked_ev, PyRep::Bool(false));
    let active_ev = rpc(&ctx, &mut p1, "operationsManager", "process_client_event", &[PyRep::String("task_done".into()), PyRep::Integer(task_id)]).await.expect("process unblocked");
    assert_eq!(active_ev, PyRep::Bool(true));
    let notifs_done = ctx.drain_notifications_for_char(P1);
    let done_ev = notifs_done.iter().find(|n| n.event == "OnOperationTaskCompleted").expect("OnOperationTaskCompleted");
    assert_eq!(done_ev.args, vec![PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_id)]);
    let prog = rpc(&ctx, &mut p1, "operationsManager", "get_character_progress", &[]).await.expect("get_character_progress");
    let PyRep::Dict(prog_entries) = prog else { panic!("expected Dict") };
    assert!(prog_entries.contains(&(PyRep::Integer(task_id), PyRep::Integer(100))));
    assert!(prog_entries.contains(&(PyRep::Integer(task_to_skip), PyRep::Integer(100))));
    assert!(prog_entries.contains(&(PyRep::Integer(blocked_tid), PyRep::Integer(0))));
    assert!(prog_entries.contains(&(PyRep::Integer(op_id), PyRep::Integer(100))));

    // 7. Multi-survey catalog & rewards (SP + ISK across surveys 1, 2, 3; granted once)
    let checks = rpc(&ctx, &mut p1, "survey", "PerformSurveyChecks", &[]).await.expect("PerformSurveyChecks");
    assert_eq!(checks, PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]));
    assert_eq!(rpc(&ctx, &mut p1, "survey", "GetActiveSurveyID", &[]).await.expect("GetActiveSurveyID"), PyRep::Integer(1));
    let sp0 = ctx.db.with_snapshot(|s| s.characters[&P1].free_skill_points);
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&P1].balance);
    // ClaimSurveyRewards with 0 positional arguments
    rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[]).await.expect("Claim survey 1 (0 args)");
    let s1_notifs = ctx.drain_notifications_for_char(P1);
    let s1_ev = s1_notifs.iter().find(|n| n.event == "OnSurveyRewardsAvailable").expect("OnSurveyRewardsAvailable");
    assert_eq!(s1_ev.args, vec![PyRep::Integer(1)], "OnSurveyRewardsAvailable must have 1 argument [survey_id]");
    assert!(rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[PyRep::Integer(1)]).await.is_err(), "duplicate claim must fail");

    // PerformSurveyChecks sets next active survey before 0-arg ClaimSurveyRewards
    rpc(&ctx, &mut p1, "survey", "PerformSurveyChecks", &[]).await.expect("PerformSurveyChecks for survey 2");
    assert_eq!(rpc(&ctx, &mut p1, "survey", "GetActiveSurveyID", &[]).await.expect("GetActiveSurveyID 2"), PyRep::Integer(2));
    rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[]).await.expect("Claim survey 2");
    let s2_notifs = ctx.drain_notifications_for_char(P1);
    let s2_ev = s2_notifs.iter().find(|n| n.event == "OnSurveyRewardsAvailable").expect("OnSurveyRewardsAvailable");
    assert_eq!(s2_ev.args, vec![PyRep::Integer(2)]);

    rpc(&ctx, &mut p1, "survey", "PerformSurveyChecks", &[]).await.expect("PerformSurveyChecks for survey 3");
    assert_eq!(rpc(&ctx, &mut p1, "survey", "GetActiveSurveyID", &[]).await.expect("GetActiveSurveyID 3"), PyRep::Integer(3));
    rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[]).await.expect("Claim survey 3");
    let s3_notifs = ctx.drain_notifications_for_char(P1);
    let s3_ev = s3_notifs.iter().find(|n| n.event == "OnSurveyRewardsAvailable").expect("OnSurveyRewardsAvailable");
    assert_eq!(s3_ev.args, vec![PyRep::Integer(3)]);
    assert_eq!(rpc(&ctx, &mut p1, "survey", "GetActiveSurveyID", &[]).await.expect("GetActiveSurveyID after"), PyRep::None);
    assert!(rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[]).await.is_err(), "claiming with no surveys remaining must fail");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P1].free_skill_points), sp0 + 50_000.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P1].balance), bal0.checked_add(eve_db::IskCents::from_isk(350_000)).unwrap());

    // 8. Verify survey rewards write MarketEscrow wallet journal entries
    let escrow_entries: Vec<_> = ctx.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .filter(|j| j.character_id == P1 && j.entry_type == "MarketEscrow")
            .cloned()
            .collect()
    });
    assert_eq!(escrow_entries.len(), 2, "Surveys 2 and 3 rewarded ISK and must write MarketEscrow wallet journal entries");
    assert_eq!(escrow_entries[0].amount, eve_db::IskCents::from_isk(250_000));
    assert_eq!(escrow_entries[0].reason, "SurveyReward:2");
    assert_eq!(escrow_entries[1].amount, eve_db::IskCents::from_isk(100_000));
    assert_eq!(escrow_entries[1].reason, "SurveyReward:3");

    // 9. Verify SDE surveys.fsdbinary definition was parsed and expired survey cannot be claimed
    let sde_surveys = ctx.sde.surveys().expect("sde surveys");
    assert!(sde_surveys.contains_key("28"), "SDE surveys.fsdbinary contains survey 28");
    assert_eq!(sde_surveys["28"]["name"], "12traits 2021");
    assert!(rpc(&ctx, &mut p1, "survey", "ClaimSurveyRewards", &[PyRep::Integer(28)]).await.is_err(), "claiming expired SDE survey 28 must fail");
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
    let PyRep::Dict(fw_outer) = f_inst else {
        panic!("expected Dict from GetFactionWarfareInstances");
    };
    let Some((_, PyRep::Dict(fw_by_dng))) = fw_outer.iter().find(|(k, _)| k.as_i64() == Some(sys_id as i64)) else {
        panic!("expected sys_id entry in GetFactionWarfareInstances");
    };
    assert!(
        fw_by_dng.iter().any(|(did, ids)| {
            let d_u32 = did.as_i64().unwrap() as u32;
            ctx.sde.dungeons.contains_key(&d_u32)
                && matches!(ids, PyRep::List(list) if list.contains(&PyRep::Integer(bf_id)))
        }),
        "GetFactionWarfareInstances must include spawned battlefield instance {bf_id} under an SDE FW dungeon ID"
    );

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

    // 4. Ore anomalies: count and range (multi-system stargate BFS)
    let ore_cnt = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[])
        .await
        .expect("GetOreAnomaliesCount");
    let PyRep::Integer(o_count) = ore_cnt else {
        panic!("expected Integer from GetOreAnomaliesCount");
    };
    assert!(o_count >= 0);

    let ore_range = rpc(
        &ctx,
        &mut p1,
        "dungeonInstanceCacheMgr",
        "GetOreAnomaliesCountInRange",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("GetOreAnomaliesCountInRange");
    let PyRep::Dict(ore_map) = ore_range else {
        panic!("expected Dict from GetOreAnomaliesCountInRange");
    };
    assert!(
        ore_map.len() > 1,
        "GetOreAnomaliesCountInRange(2) from Jita must include neighbor solar systems"
    );
    assert!(ore_map.iter().any(|(k, v)| k.as_i64() == Some(sys_id as i64) && v.as_i64() == Some(o_count)));
    assert!(
        ore_map.iter().any(|(k, _)| k.as_i64() == Some(30_000_144)),
        "Perimeter (30000144, 1 jump from Jita) must be in GetOreAnomaliesCountInRange(2)"
    );
    assert!(
        ore_map.iter().any(|(k, v)| k.as_i64() != Some(sys_id as i64) && v.as_i64().unwrap_or(0) > 0),
        "R-DNG-RNG-01: unvisited 0.5-0.8 neighbors within 2 jumps of Jita must have SDE mining sites seeded by GetOreAnomaliesCountInRange(2)"
    );

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

    // 6. Signatures and Bulwark mining counts (archetype 81 / BulwarkMiningSite)
    let sigs = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[])
        .await
        .expect("GetSignatureInstances");
    assert!(matches!(sigs, PyRep::Dict(_)));

    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: "DNG-13469-1".into(),
                solar_system_id: sys_id,
                name: "Phased Field Deposit".into(),
                anomaly_type: "BulwarkMiningSite".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 10_000.0,
                created_unix: now_unix,
                lifetime_seconds: 0,
                cleared: false,
            });
    });

    let bulwark = rpc(&ctx, &mut p1, "dungeonInstanceCacheMgr", "GetBulwarkMiningSiteCounts", &[])
        .await
        .expect("GetBulwarkMiningSiteCounts");
    let PyRep::Dict(bulwark_map) = bulwark else {
        panic!("expected Dict from GetBulwarkMiningSiteCounts");
    };
    assert!(
        bulwark_map.iter().any(|(k, v)| k.as_i64() == Some(sys_id as i64) && v.as_i64() == Some(1)),
        "GetBulwarkMiningSiteCounts must count active BulwarkMiningSite (archetype 81) in sys_id"
    );
}
