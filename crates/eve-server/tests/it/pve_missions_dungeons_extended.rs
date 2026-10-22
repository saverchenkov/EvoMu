//! `pve_missions_dungeons_extended` — Parity integration tests for extended PVE missions and dungeon features.
//!
//! Covers:
//! - feat:pve.dungeon-system
//! - feat:fw.fw-missions
//! - feat:fw.fw-site-dungeons
//! - feat:admin.gm-dungeon-editor
//! - feat:pve.mission-lp-isk-reward-formula
//! - feat:pve.mission-bonus-timer
//! - feat:pve.mission-offer-expiry-no-penalty
//! - feat:pve.mission-level-clone-and-space-restrictions
//! - feat:pve.research-agent-daily-mission

use crate::common;
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::*;
use eve_server::services::pve_dungeon_system;
use std::collections::HashMap;

const EXPECTED_ERR: &str = "InsufficientRole";

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

async fn rpc_call(
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

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(30_000_142);
    let sta_id = agent.station_id.unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
    (sys_id, sta_id)
}

// =========================================================================
// 1. feat:pve.dungeon-system
// =========================================================================

#[tokio::test]
async fn test_pve_dungeon_system() {
    // parity: feat:pve.dungeon-system
    eve_testkit::attest(&["feat:pve.dungeon-system"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 601, "demo", "demo-secret", char_id).await;

    let sys_id = 30000142; // Jita
    sess.solar_system_id = Some(sys_id);

    // 1. Spawn dungeon instance with rooms, acceleration gate, container, and NPCs
    let inst_id = pve_dungeon_system::spawn_dungeon_instance(
        &ctx,
        111,
        sys_id,
        [100_000.0, 50_000.0, -25_000.0],
    )
    .await
    .expect("Spawn dungeon instance");

    let initial = pve_dungeon_system::get_dungeon_instance(inst_id)
        .expect("Dungeon instance must be registered");
    assert_eq!(initial.rooms.len(), 2);
    assert_eq!(initial.current_room_idx, 0);
    assert!(!initial.rooms[0].gate_unlocked);
    assert!(!initial.completed);

    let _npc_ball_id = initial.rooms[0].npc_ball_ids[0];
    let container_id = initial.rooms[1].object_ids[0];

    // 2. Trigger on approach: spawns NPC wave
    let npc_count_before = pve_dungeon_system::get_dungeon_instance(inst_id)
        .unwrap()
        .rooms[0]
        .npc_ball_ids
        .len();
    pve_dungeon_system::trigger_on_approach(
        &ctx,
        inst_id,
        [100_000.0 + 3_000.0, 50_000.0, -25_000.0],
    )
    .await
    .expect("Trigger OnApproach");
    let npc_count_after = pve_dungeon_system::get_dungeon_instance(inst_id)
        .unwrap()
        .rooms[0]
        .npc_ball_ids
        .len();
    assert!(npc_count_after > npc_count_before, "Approach must spawn additional wave");

    // 3. Trigger on kill: killing NPCs triggers action (e.g. unlocks acceleration gate)
    let cur_npcs = pve_dungeon_system::get_dungeon_instance(inst_id)
        .unwrap()
        .rooms[0]
        .npc_ball_ids
        .clone();
    for id in cur_npcs {
        pve_dungeon_system::trigger_on_kill(&ctx, inst_id, id)
            .await
            .expect("Trigger OnKill");
    }
    let room_after_kill = &pve_dungeon_system::get_dungeon_instance(inst_id)
        .unwrap()
        .rooms[0];
    assert!(room_after_kill.gate_unlocked, "Killing NPC trigger must unlock gate");

    // 4. Verify acceleration gate SDE dogma hull classification and advance room
    assert!(pve_dungeon_system::validate_acceleration_gate_ship(&ctx, 111, 587).is_ok());
    let cap_err = pve_dungeon_system::validate_acceleration_gate_ship(&ctx, 111, 19720).unwrap_err();
    assert_eq!(cap_err.code, "ShipTooLargeForGate");

    let next_room = pve_dungeon_system::advance_dungeon_room(inst_id)
        .expect("Advance room");
    assert_eq!(next_room, 2);
    let inst_room2 = pve_dungeon_system::get_dungeon_instance(inst_id).unwrap();
    assert_eq!(inst_room2.current_room_idx, 1);

    // 5. Trigger on loot: container opened in final room completes dungeon
    pve_dungeon_system::trigger_on_loot(&ctx, inst_id, container_id)
        .await
        .expect("Trigger OnLoot");
    let completed_inst = pve_dungeon_system::get_dungeon_instance(inst_id).unwrap();
    assert!(completed_inst.completed, "Looting container must complete dungeon");

    // 6. Despawn dungeon when complete
    let removed = pve_dungeon_system::despawn_dungeon_instance(&ctx, inst_id)
        .await
        .expect("Despawn dungeon");
    assert!(removed > 0, "Despawn must remove spawned destiny balls");
    assert!(pve_dungeon_system::get_dungeon_instance(inst_id).is_none());

    // 7. Verify keeper RPCs: PlayDungeon, GetCurrentDungeonForCharacter
    let play_rep = rpc_call(
        &ctx,
        &mut sess,
        "keeper",
        "PlayDungeon",
        &[PyRep::Integer(111)],
    )
    .await
    .expect("keeper.PlayDungeon");
    let new_inst_id = play_rep
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("instanceID in response");
    assert!(new_inst_id > 0);
    assert_eq!(
        pve_dungeon_system::get_dungeon_instance(new_inst_id)
            .expect("new instance registered")
            .current_room_idx,
        0
    );

    let cur_rep = rpc_call(
        &ctx,
        &mut sess,
        "keeper",
        "GetCurrentDungeonForCharacter",
        &[PyRep::Integer(new_inst_id)],
    )
    .await
    .expect("keeper.GetCurrentDungeonForCharacter");
    assert!(cur_rep.dict_get("solarSystemID").is_some());
}

// =========================================================================
// 2. feat:fw.fw-missions and feat:fw.fw-site-dungeons
// =========================================================================

#[tokio::test]
async fn test_fw_missions_and_site_dungeons() {
    // parity: feat:fw.fw-missions, feat:fw.fw-site-dungeons
    eve_testkit::attest(&["feat:fw.fw-missions", "feat:fw.fw-site-dungeons"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 602, "demo", "demo-secret", char_id).await;

    // 1. FW Site Dungeons: GetFactionWarfareBattlefieldInstances returns battlefield sites
    let bf_rep = rpc_call(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[PyRep::Integer(30002062)], // Onnamon warzone
    )
    .await
    .expect("GetFactionWarfareBattlefieldInstances");

    let list = match bf_rep {
        PyRep::List(l) => l,
        _ => panic!("returns list of battlefield instances"),
    };
    assert!(!list.is_empty(), "Battlefield sites must be returned for warzone");
    let first = &list[0];
    assert!(first.dict_get("dungeonID").is_some());
    assert!(first.dict_get("solarSystemID").is_some());
    assert!(first.dict_get("archetypeID").is_some());
    assert!(first.dict_get("dungeonName").is_some());
    assert!(first.dict_get("completed").is_some());
    assert!(first.dict_get("progress").is_some());

    // 2. FW missions: FW agent offers missions in warzones
    let fw_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 7 && a.solar_system_id.is_some())
        .expect("FW agent in SDE");

    colocate_with_agent(&ctx, &mut sess, char_id, fw_agent.agent_id);

    // Request mission from FW agent
    let offer_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(fw_agent.agent_id)],
    )
    .await
    .expect("Request mission from FW agent");

    let mission_id = offer_rep
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    // Target solar system of the mission must be a warzone system
    let target_sys = ctx.db.with_snapshot(|snap| {
        snap.active_missions
            .get(&(char_id, mission_id))
            .map(|m| m.solar_system_id)
            .unwrap()
    });
    let expected_warzone_sys =
        resolve_fw_warzone_system(
            fw_agent.corporation_id,
        );
    assert_eq!(
        target_sys, expected_warzone_sys,
        "FW mission must be routed to appropriate warzone system"
    );
}

// =========================================================================
// 3. feat:admin.gm-dungeon-editor
// =========================================================================

#[tokio::test]
async fn test_gm_dungeon_editor() {
    // parity: feat:admin.gm-dungeon-editor
    eve_testkit::attest(&["feat:admin.gm-dungeon-editor"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", char_id).await;

    // 1. Regular player (non-GM) is rejected by role gate
    assert!(!sess.has_gm_role());
    for m in ["DEGetDungeons", "DEGetRooms", "DEGetArchetypes"] {
        let err = rpc_call(&ctx, &mut sess, "dungeon", m, &[])
            .await
            .unwrap_err();
        assert_eq!(err.code, EXPECTED_ERR);
    }
    let err = rpc_call(&ctx, &mut sess, "keeper", "GetLevelEditor", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, EXPECTED_ERR);

    // 2. Grant GM role
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    assert!(sess.has_gm_role());

    // 3. GM calls DEGetDungeons
    let d_rep = rpc_call(&ctx, &mut sess, "dungeon", "DEGetDungeons", &[])
        .await
        .expect("dungeon.DEGetDungeons");
    if let PyRep::Rowset { columns, rows } = d_rep {
        assert!(columns.contains(&"dungeonID".to_string()));
        assert!(columns.contains(&"dungeonName".to_string()));
        assert!(columns.contains(&"archetypeID".to_string()));
        assert!(!rows.is_empty());
    } else {
        panic!("expected rowset from DEGetDungeons");
    }

    // 4. GM calls DEGetRooms
    let r_rep = rpc_call(&ctx, &mut sess, "dungeon", "DEGetRooms", &[PyRep::Integer(111)])
        .await
        .expect("dungeon.DEGetRooms");
    if let PyRep::Rowset { columns, rows } = r_rep {
        assert!(columns.contains(&"roomID".to_string()));
        assert!(columns.contains(&"dungeonID".to_string()));
        assert!(!rows.is_empty());
    } else {
        panic!("expected rowset from DEGetRooms");
    }

    // 5. GM calls DEGetArchetypes
    let a_rep = rpc_call(&ctx, &mut sess, "dungeon", "DEGetArchetypes", &[])
        .await
        .expect("dungeon.DEGetArchetypes");
    if let PyRep::Rowset { columns, rows } = a_rep {
        assert!(columns.contains(&"archetypeID".to_string()));
        assert!(columns.contains(&"archetypeName".to_string()));
        assert!(!rows.is_empty());
    } else {
        panic!("expected rowset from DEGetArchetypes");
    }

    // 6. GM calls keeper.GetLevelEditor
    let ke_rep = rpc_call(&ctx, &mut sess, "keeper", "GetLevelEditor", &[])
        .await
        .expect("keeper.GetLevelEditor");
    assert!(matches!(ke_rep, PyRep::None));
}

// =========================================================================
// 4. feat:pve.mission-lp-isk-reward-formula
// =========================================================================

#[tokio::test]
async fn test_mission_lp_isk_reward_formula() {
    // parity: feat:pve.mission-lp-isk-reward-formula
    eve_testkit::attest(&["feat:pve.mission-lp-isk-reward-formula"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 604, "demo", "demo-secret", char_id).await;

    // Set pilot skills: Negotiation (3356) level 3 (+15% ISK), Security Connections (3895) level 4 (+40% LP)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3356,
            TrainedSkillRecord {
                type_id: 3356,
                level: 3,
                skill_points: 45_255.0,
            },
        );
        sk.insert(
            3895,
            TrainedSkillRecord {
                type_id: 3895,
                level: 4,
                skill_points: 135_765.0,
            },
        );
        // Ensure character is Omega
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.balance = IskCents(10_000_00);
        }
    });

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.division_id == 24 && a.solar_system_id.is_some())
        .expect("Level 1 security agent");
    colocate_with_agent(&ctx, &mut sess, char_id, agent.agent_id);

    // Request mission
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)],
    )
    .await
    .expect("Offer mission");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    let sys_id = agent.solar_system_id.unwrap();
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);

    let (expected_isk, expected_lp, expected_bonus) =
        compute_character_mission_rewards(
            &ctx,
            char_id,
            agent.agent_id,
            mission_id,
            sys_id,
        );

    // Verify ISK has +15% negotiation multiplier
    let base_lp = ctx.sde.missions.get(&mission_id).map_or(100, |t| t.lp_omega.max(100));
    let base_isk = base_lp * 1_000;
    let manual_expected_isk = ((base_isk as f64) * 1.15).round() as i64;
    assert_eq!(expected_isk, manual_expected_isk);

    // Verify LP has (1.6288 - true_sec) * (1 + 0.1 * 4) multiplier
    let sec_factor = (1.6288 - true_sec).max(0.01);
    let conn_factor = 1.40;
    let manual_expected_lp = ((base_lp as f64) * sec_factor * conn_factor).round() as i64;
    assert_eq!(expected_lp, manual_expected_lp);

    // Accept and complete mission
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("Accept mission");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.npcs_remaining = 0;
        }
    });

    let journal_count_before = ctx.db.with_snapshot(|snap| snap.wallet_journal.len());
    let complete_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("Complete mission");

    assert_eq!(
        complete_rep.dict_get("iskReward").and_then(|v| v.as_i64()),
        Some(expected_isk)
    );
    assert_eq!(
        complete_rep.dict_get("lpReward").and_then(|v| v.as_i64()),
        Some(expected_lp)
    );

    // Verify wallet journal entries were written for mission reward & bonus reward
    ctx.db.with_snapshot(|snap| {
        let added_entries = &snap.wallet_journal[journal_count_before..];
        let has_reward_journal = added_entries.iter().any(|e| {
            e.character_id == char_id
                && e.entry_type == "agent_mission_reward"
                && e.amount == IskCents::from_isk(expected_isk)
        });
        assert!(has_reward_journal, "agent_mission_reward must be journaled in wallet");

        let has_bonus_journal = added_entries.iter().any(|e| {
            e.character_id == char_id
                && e.entry_type == "agent_mission_time_bonus_reward"
                && e.amount == IskCents::from_isk(expected_bonus)
        });
        assert!(has_bonus_journal, "agent_mission_time_bonus_reward must be journaled");
    });
}

// =========================================================================
// 5. feat:pve.mission-bonus-timer
// =========================================================================

#[tokio::test]
async fn test_mission_bonus_timer() {
    // parity: feat:pve.mission-bonus-timer
    eve_testkit::attest(&["feat:pve.mission-bonus-timer"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 605, "demo", "demo-secret", char_id).await;

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .expect("Level 2 agent");
    colocate_with_agent(&ctx, &mut sess, char_id, agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(agent.corporation_id, 5.0);
    });

    // Request mission
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)],
    )
    .await
    .expect("Offer mission");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    // Accept mission -> bonus timer of 1h (3,600s) for L2 agent
    let accept_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("Accept mission");
    let bonus_expiry = accept_rep
        .dict_get("bonusExpiryUnix")
        .and_then(|v| v.as_i64())
        .unwrap() as u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.npcs_remaining = 0;
        }
    });

    // Case A: Complete before bonus expiry -> receives bonus reward
    assert!(ctx.now_unix() <= bonus_expiry);
    let comp_a = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("Complete within bonus window");
    let bonus_paid_a = comp_a
        .dict_get("bonusReward")
        .and_then(|v| v.as_i64())
        .unwrap();
    assert!(bonus_paid_a > 0, "Bonus reward must be paid within bonus window");

    // Case B: Expired bonus timer pays 0 bonus reward
    let agent_l1 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.division_id == 24 && a.agent_id != agent.agent_id && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, char_id, agent_l1.agent_id);

    let offer_b = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(agent_l1.agent_id)],
    )
    .await
    .expect("Offer mission B");
    let mid_b = offer_b
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid_b as i64)],
    )
    .await
    .expect("Accept mission B");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mid_b)) {
            m.npcs_remaining = 0;
        }
    });

    // Advance virtual clock past bonus expiry (advance 3 hours = 10,800s)
    ctx.advance_secs(10_800);

    let comp_b = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid_b as i64)],
    )
    .await
    .expect("Complete mission after bonus window");
    let bonus_paid_b = comp_b
        .dict_get("bonusReward")
        .and_then(|v| v.as_i64())
        .unwrap();
    assert_eq!(bonus_paid_b, 0, "Bonus reward must be 0 after bonus timer expires");
}

// =========================================================================
// 6. feat:pve.mission-offer-expiry-no-penalty
// =========================================================================

#[tokio::test]
async fn test_mission_offer_expiry_no_penalty() {
    // parity: feat:pve.mission-offer-expiry-no-penalty
    eve_testkit::attest(&["feat:pve.mission-offer-expiry-no-penalty"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 606, "demo", "demo-secret", char_id).await;

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, char_id, agent.agent_id);

    // 1. Offer mission
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)],
    )
    .await
    .expect("Offer mission");
    let mid = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    let standing_before = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&agent.corporation_id).copied())
            .unwrap_or(0.0)
    });

    // Advance clock past 7-day offer expiry (605,000s)
    ctx.advance_secs(605_000);

    // Call GetMyJournalDetails to trigger offer expiry sweep
    let _ = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;

    // Check mission state is now Expired
    let state = ctx.db.with_snapshot(|snap| {
        snap.active_missions
            .get(&(char_id, mid))
            .map(|m| m.state.clone())
            .unwrap()
    });
    assert_eq!(state, "Expired");

    // Unaccepted offer expiry must have zero standing change
    let standing_after = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&agent.corporation_id).copied())
            .unwrap_or(0.0)
    });
    assert_eq!(
        standing_before, standing_after,
        "Offer expiry must apply NO standing penalty"
    );

    // 2. COSMOS / Data Center agent offer expiration is never re-offered
    let cosmos_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5 && a.solar_system_id.is_some())
        .expect("Data Center agent");
    colocate_with_agent(&ctx, &mut sess, char_id, cosmos_agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(cosmos_agent.corporation_id, 5.0);
    });

    // Manually record an expired mission from this agent
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 99999),
            ActiveMissionRecord {
                mission_id: 99999,
                character_id: char_id,
                agent_id: cosmos_agent.agent_id,
                solar_system_id: 30000142,
                state: "Expired".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
            },
        );
    });

    let reoffer_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(cosmos_agent.agent_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(
        reoffer_err.code, "CosmosMissionExpired",
        "Expired COSMOS mission must never be re-offered"
    );

    // 3. Failed 7-day completion timer applies agent, corp, and faction penalties
    let agent3 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id != 18 && a.agent_id != agent.agent_id && a.solar_system_id.is_some())
        .expect("Agent 3");
    colocate_with_agent(&ctx, &mut sess, char_id, agent3.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(agent3.corporation_id, 5.0);
    });

    let offer3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(agent3.agent_id)],
    )
    .await
    .expect("Offer mission 3");
    let mid3 = offer3
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid3 as i64)],
    )
    .await
    .expect("Accept mission 3");

    let agent_st_before = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&agent3.agent_id).copied())
            .unwrap_or(0.0)
    });

    // Advance virtual clock past 7 days (610,000s)
    ctx.advance_secs(610_000);

    // Attempting to complete after 7-day accept expiry fails and penalizes
    let comp_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid3 as i64)],
    )
    .await
    .unwrap_err();
    assert_eq!(comp_err.code, "MissionExpired");

    let agent_st_after = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&agent3.agent_id).copied())
            .unwrap_or(0.0)
    });
    assert!(
        agent_st_after < agent_st_before,
        "Accepted mission expiration must apply standing penalty"
    );
}

// =========================================================================
// 7. feat:pve.mission-level-clone-and-space-restrictions
// =========================================================================

#[tokio::test]
async fn test_mission_level_clone_and_space_restrictions() {
    // parity: feat:pve.mission-level-clone-and-space-restrictions
    eve_testkit::attest(&["feat:pve.mission-level-clone-and-space-restrictions"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 607, "demo", "demo-secret", char_id).await;

    let l4_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 4 && a.solar_system_id.is_some())
        .expect("Level 4 agent in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, l4_agent.agent_id);

    // 1. Alpha clones restricted from Level 4 and Level 5 missions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1; // Alpha clone
            ch.standings.insert(l4_agent.corporation_id, 8.0); // high standing for access
        }
    });

    // Alpha pilot requests Level 4 mission -> CantDoMissionAlphaClone
    let alpha_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(alpha_err.code, "CantDoMissionAlphaClone");

    // Upgrade pilot to Omega clone
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
        }
    });

    // Omega pilot requests Level 4 mission -> succeeds!
    let omega_ok = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)],
    )
    .await;
    assert!(omega_ok.is_ok(), "Omega clone must be able to request L4 mission");

    // 2. Level 5 agents in high-sec space are restricted (CantDoMission)
    // SDE agent 3008513 is Level 5 in high-sec system 30000148 (sec 0.74)
    let l5_highsec_agent = ctx.sde.agents.get(&3008513).expect("L5 agent 3008513 in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, l5_highsec_agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(l5_highsec_agent.corporation_id, 9.0);
        }
    });

    let space_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(l5_highsec_agent.agent_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(
        space_err.code, "CantDoMission",
        "Level 5 agent in high-sec must be rejected with CantDoMission"
    );
}

// =========================================================================
// 8. feat:pve.research-agent-daily-mission
// =========================================================================

#[tokio::test]
async fn test_research_agent_daily_mission() {
    // parity: feat:pve.research-agent-daily-mission
    eve_testkit::attest(&["feat:pve.research-agent-daily-mission"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 608, "demo", "demo-secret", char_id).await;

    // SDE agent 3009318 is an R&D agent (type 4, division 18, level 1)
    let rnd_agent = ctx.sde.agents.get(&3009318).expect("R&D agent 3009318 in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, rnd_agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
            ch.standings.insert(rnd_agent.corporation_id, 6.0);
        }
    });

    // 1. Without active research, daily mission request is rejected
    let no_res_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(no_res_err.code, "ResearchNotActive");

    // 2. Start active research project with 120 RP/day
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let entries = vec![RndResearchEntry {
            agent_id: rnd_agent.agent_id,
            research_type_id: 11444,
            points_per_day: 120.0,
            initial_points: 50.0,
            started_unix: now,
        }];
        save_rnd_research_entries(snap, char_id, &entries);
    });

    // 3. With active research, daily mission is offered: requires Tritanium scaled by agent level
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)],
    )
    .await
    .expect("Offer daily mission");
    let mid = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    let meta = ctx.db.with_snapshot(|snap| {
        load_mission_meta(snap, char_id, mid).unwrap()
    });
    assert_eq!(meta.required_delivery_type_id, Some(34), "Tritanium required");
    assert_eq!(
        meta.required_delivery_qty,
        rnd_tritanium_required_qty(rnd_agent.level),
        "Tritanium quantity must scale by agent level: 300 * 3^(level-1)"
    );

    // 4. Declining daily mission has NO standing penalty and does not stop research
    let standing_before = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&rnd_agent.corporation_id).copied())
            .unwrap_or(0.0)
    });

    let decline_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(mid as i64)],
    )
    .await
    .expect("Decline daily mission");
    assert_eq!(
        decline_rep.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()),
        Some(false)
    );

    let standing_after = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&rnd_agent.corporation_id).copied())
            .unwrap_or(0.0)
    });
    assert_eq!(standing_before, standing_after);

    // Research project is still active
    let active_res = ctx.db.with_snapshot(|snap| {
        load_rnd_research_entries(snap, char_id)
    });
    assert_eq!(active_res.len(), 1);

    // 5. Request again, accept, supply Tritanium, complete
    let offer2 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)],
    )
    .await
    .expect("Offer daily mission again");
    let mid2 = offer2
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid2 as i64)],
    )
    .await
    .expect("Accept daily mission");

    // Put required Tritanium in character's hangar
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: rnd_agent.station_id.unwrap_or(60003760),
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let tally_before = ctx.db.with_snapshot(|snap| snap.mission_tally.get(&char_id).copied().unwrap_or(0));
    let initial_points_before = active_res[0].initial_points;

    // Complete daily mission
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid2 as i64)],
    )
    .await
    .expect("Complete daily mission");

    // Completing adds 1 day's RP to active research entry
    let active_res_after = ctx.db.with_snapshot(|snap| {
        load_rnd_research_entries(snap, char_id)
    });
    assert_eq!(
        active_res_after[0].initial_points,
        initial_points_before + active_res_after[0].points_per_day,
        "Completing daily mission must add 1 day of RP"
    );

    // Increments the 16-mission storyline tally
    let tally_after = ctx.db.with_snapshot(|snap| snap.mission_tally.get(&char_id).copied().unwrap_or(0));
    assert_eq!(
        tally_after,
        tally_before + 1,
        "Completing daily mission must increment storyline mission tally"
    );
}
