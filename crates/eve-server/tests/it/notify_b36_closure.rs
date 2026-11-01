//! Live integration tests for Batch 36 state-backed V24 notification emitters:
//! - `notify:OnProgressionAdvanced`
//! - `notify:OnProgressionTaskCompleted`
//! - `notify:OnCombatMessage`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const NULLSEC_SYSTEM: u32 = 30_004_759;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &args,
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

fn grant_all_combat_skills(ctx: &ServerContext, snap: &mut eve_db::WorldStateSnapshot, cid: i64, type_ids: &[u32]) {
    let sk = snap.skills.entry(cid).or_default();
    for sid in [
        3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3319, 3320, 3334, 3413,
        3418, 3426, 20327, 21071, 21667,
    ] {
        sk.insert(
            sid,
            TrainedSkillRecord {
                type_id: sid,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
    }
    for &tid in type_ids {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for sa in [182_u16, 183, 184] {
                if let Some(&req_f) = t.default_attributes.get(&sa) {
                    let rsid = req_f as u32;
                    if rsid > 0 {
                        sk.insert(
                            rsid,
                            TrainedSkillRecord {
                                type_id: rsid,
                                level: 5,
                                skill_points: 1_280_000.0,
                            },
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn test_b36_progression_notifications() {
    // parity: notify:OnProgressionAdvanced
    // parity: notify:OnProgressionTaskCompleted
    eve_testkit::attest(&[
        "notify:OnProgressionAdvanced",
        "notify:OnProgressionTaskCompleted",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 913, "demo", "demo-secret", P1).await;
    let kw = HashMap::new();

    // Negative check 1 (`F-02`): NPE `operationsManager` emits `OnOperationTaskCompleted` ONLY —
    // never dungeon `ProgressionService` `OnProgressionTaskCompleted` / `OnProgressionAdvanced`!
    let op_defs = eve_server::services::operations_manager::get_operation_defs(&ctx.sde);
    let (&real_op, _) = op_defs
        .op_tasks
        .iter()
        .find(|(_, ts)| ts.iter().any(|t| op_defs.client_completable_tasks.contains(t)))
        .expect("SDE operation with client-completable tasks");
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(real_op)],
            &kw,
            None,
        )
        .await
        .expect("activate_operation");
    let res_npe = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[PyRep::String("complete_task".into())],
            &kw,
            None,
        )
        .await
        .expect("complete_task");
    assert_eq!(res_npe, PyRep::Bool(true));
    assert!(!events(&ctx, P1, "OnOperationTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Set up P1 undocked in space with a live turret ship to enter and clear a multi-room dungeon
    let ship_p1 = 969_001_i64;
    let gun_p1 = 969_010_i64;
    let ammo_p1 = 969_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.solar_system_id = NULLSEC_SYSTEM;
            c1.station_id = None;
            c1.active_ship_id = ship_p1;
        }
        snap.items.insert(
            ship_p1,
            InventoryItemRecord {
                item_id: ship_p1,
                type_id: 639,
                owner_id: P1,
                location_id: i64::from(NULLSEC_SYSTEM),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_p1,
            InventoryItemRecord {
                item_id: gun_p1,
                type_id: 2961,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_p1,
            InventoryItemRecord {
                item_id: ammo_p1,
                type_id: 21894,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_p1, true);
        grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894]);
    });
    ctx.invalidate_ship_fitting(ship_p1);
    s1.solar_system_id = Some(NULLSEC_SYSTEM);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let base_pos = [120_000_000.0, 0.0, 80_000_000.0];
    let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        P1,
        NULLSEC_SYSTEM,
        2001,
        "Deadspace",
        base_pos,
        true,
    )
    .await
    .expect("spawn_sde_dungeon_runtime_instance");

    let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
        .expect("get_dungeon_instance_state");
    let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();

    let sys_h = ctx.systems.get_or_spawn(NULLSEC_SYSTEM);
    let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_p1,
                type_id: 639,
                position: entry_gate_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;
    let _ = ctx.drain_notifications_for_char(P1);

    // Enter Room 1 via live `keeper.ActivateAccelerationGate` -> emits `OnProgressionRoomJoined`, zero task completions yet
    let enter_r1 = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(entry_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate entry acceleration gate");
    let room1_id = enter_r1.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let r1_gate_id = enter_r1.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let relay_obj_id = match enter_r1.dict_get("objectBallIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().unwrap(),
        other => panic!("expected objectBallIDs list, got {other:?}"),
    };
    let guard_npc_ids: Vec<i64> = match enter_r1.dict_get("npcBallIDs") {
        Some(PyRep::List(v)) => v.iter().map(|x| x.as_i64().unwrap()).collect(),
        other => panic!("expected npcBallIDs list, got {other:?}"),
    };
    assert_eq!(guard_npc_ids.len(), 2);
    for _ in 0..45 {
        if !matches!(
            sys_h.get_ball_snapshot(ship_p1).await.unwrap().unwrap().1.mode,
            MovementMode::Warp { .. }
        ) {
            break;
        }
        let _ = sys_h.tick().await;
    }

    let joined_ev = events(&ctx, P1, "OnProgressionRoomJoined");
    assert_eq!(joined_ev.len(), 1, "Entering dungeon pocket must emit OnProgressionRoomJoined");
    assert_eq!(joined_ev[0].len(), 5);
    assert_eq!(joined_ev[0][4], PyRep::Integer(room1_id));
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Negative check 2: activating locked Room 1 gate before completing Room 1 kill objective fails with GateLocked and emits nothing
    let r1_gate_pos = sys_h.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let locked_err = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(r1_gate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(locked_err.code, "GateLocked");
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Positive check 1 (`OnApproach` trigger 1 -> `SpawnNpcGroup(2)`): live `beyonce.CmdGotoPoint` completes task 1 and advances objective
    let relay_pos = sys_h.get_ball_snapshot(relay_obj_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_p1, relay_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let b1 = ctx.bind_object(&mut s1, "beyonce", i64::from(NULLSEC_SYSTEM)).unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "beyonce",
        Some(b1),
        "CmdGotoPoint",
        &[PyRep::Real(relay_pos.x), PyRep::Real(relay_pos.y), PyRep::Real(relay_pos.z)],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoPoint relay object");

    let expected_task1 = room1_id * 100 + 1;
    let expected_obj_wave2 = room1_id * 10 + 2;
    let task_ev1 = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev1, vec![vec![PyRep::Integer(expected_task1)]]);
    let adv_ev1 = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(
        adv_ev1,
        vec![vec![PyRep::Integer(expected_obj_wave2), PyRep::Integer(expected_task1)]]
    );

    // Positive check 2 (`OnKill` trigger 2 -> `UnlockGate`): destroy both Room 1 Group 1 gate guards via live weapon combat
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(b1), "CmdStop", &[], &kw, None)
        .await
        .expect("CmdStop before shooting guard NPCs");
    for (idx, &guard_npc_id) in guard_npc_ids.iter().enumerate() {
        let guard_pos = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, guard_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(guard_npc_id)], &kw, None)
            .await
            .expect("AddTarget guard NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == guard_npc_id && l.locked) {
                break;
            }
            let _ = sys_h.tick().await;
        }
        if let Ok(Some(mut cs)) = sys_h.get_combat_state(guard_npc_id).await {
            cs.shield_hp = 5.0;
            cs.armor_hp = 5.0;
            cs.structure_hp = 5.0;
            cs.base_signature_radius = Some(400.0);
            cs.signature_radius = 400.0;
            let _ = sys_h.set_combat_state(guard_npc_id, cs).await;
        }
        let guard_pos_now = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, guard_pos_now + DVec3::new(1_500.0, 0.0, 0.0)).await;
        ctx.set_invention_roll_override(Some(0.001));
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gun_p1), PyRep::Integer(guard_npc_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate gun_p1 on guard NPC");
        for _ in 0..25 {
            if sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none() {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        ctx.set_invention_roll_override(None);
        assert!(sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none());
        if idx == 0 {
            assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
        }
    }

    let expected_task2 = room1_id * 100 + 2;
    let expected_obj_gate = room1_id * 10 + 9;
    let task_ev2 = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev2, vec![vec![PyRep::Integer(expected_task2)]]);
    let adv_ev2 = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(
        adv_ev2,
        vec![vec![PyRep::Integer(expected_obj_gate), PyRep::Integer(expected_task2)]]
    );

    let post_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    assert_eq!(post_state.dict_get("activeObjectiveID"), Some(&PyRep::Integer(expected_obj_gate)));
    assert_eq!(post_state.dict_get("lastCompletedTaskID"), Some(&PyRep::Integer(expected_task2)));

    // Room 1 acceleration gate is now unlocked -> traversing into Room 2 emits Room 2 OnProgressionRoomJoined & OnProgressionAdvanced
    let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let enter_r2 = ctx
        .dispatch_rpc(&mut s1, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate unlocked Room 1 acceleration gate into Room 2");
    let room2_id = enter_r2.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(events(&ctx, P1, "OnProgressionRoomJoined").len(), 1);
    assert_eq!(
        events(&ctx, P1, "OnProgressionAdvanced"),
        vec![vec![PyRep::Integer(room2_id * 10 + 1), PyRep::Integer(expected_task2)]]
    );
}

#[tokio::test]
async fn test_b36_combat_message_live_weapon_cycle() {
    // parity: notify:OnCombatMessage
    eve_testkit::attest(&["notify:OnCombatMessage"]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 914, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 915, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    let ship_p1 = 970_001_i64;
    let ship_p2 = 970_002_i64;
    let gun_p1 = 970_010_i64;
    let ammo_p1 = 970_011_i64;
    let lml_p1 = 970_012_i64;
    let missile_p1 = 970_013_i64;

    let light_missile_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 384 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(213);

    let fw_sys = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.keys().copied().min().unwrap_or(NULLSEC_SYSTEM));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.solar_system_id = fw_sys;
            c1.station_id = None;
            c1.active_ship_id = ship_p1;
            c1.war_faction_id = Some(500_001);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.solar_system_id = fw_sys;
            c2.station_id = None;
            c2.active_ship_id = ship_p2;
            c2.war_faction_id = Some(500_004);
        }
        for (sid, owner) in [(ship_p1, P1), (ship_p2, P2)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 639,
                    owner_id: owner,
                    location_id: i64::from(fw_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            gun_p1,
            InventoryItemRecord {
                item_id: gun_p1,
                type_id: 2961,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_p1,
            InventoryItemRecord {
                item_id: ammo_p1,
                type_id: 21894,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            lml_p1,
            InventoryItemRecord {
                item_id: lml_p1,
                type_id: 2404,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            missile_p1,
            InventoryItemRecord {
                item_id: missile_p1,
                type_id: light_missile_tid,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_p1, true);
        snap.module_online_states.insert(lml_p1, true);
        grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894, 2404, light_missile_tid]);
    });
    ctx.invalidate_ship_fitting(ship_p1);
    ctx.invalidate_ship_fitting(ship_p2);

    s1.solar_system_id = Some(fw_sys);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let sys_h = ctx.systems.get_or_spawn(fw_sys);
    for (sid, pos) in [(ship_p1, DVec3::ZERO), (ship_p2, DVec3::new(3_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(639);
        cs.signature_radius = 450.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 639,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // Negative check: activating weapon before target lock fails and emits no OnCombatMessage
    assert!(ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)],
            &kw,
            None,
        )
        .await
        .is_err());
    assert!(events(&ctx, P1, "OnCombatMessage").is_empty());
    assert!(events(&ctx, P2, "OnCombatMessage").is_empty());

    // Lock target via dogmaIM.AddTarget + SystemActor ticks
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("AddTarget ship_p2");
    for _ in 0..30 {
        if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == ship_p2 && l.locked) {
            break;
        }
        let _ = sys_h.tick().await;
    }

    // 1. Live turret cycle -> TacticalSvc.OnCombatMessage(messageName, messageArgs) on P1 and P2
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate weapon on locked ship_p2");
    let _ = sys_h.tick().await;

    let atk_ev = events(&ctx, P1, "OnCombatMessage");
    assert!(!atk_ev.is_empty(), "Attacker P1 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(atk_ev[0].len(), 2);
    assert!(matches!(atk_ev[0][0].as_str(), Some("AttackHit" | "AttackMiss")));
    assert_eq!(atk_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p2)));
    assert_eq!(atk_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));
    assert_eq!(atk_ev[0][1].dict_get("weapon"), Some(&PyRep::Integer(2961)));
    assert!(atk_ev[0][1].dict_get("damage").is_some());
    assert!(atk_ev[0][1].dict_get("hitQualityText").and_then(|v| v.as_str()).is_some());

    let vic_ev = events(&ctx, P2, "OnCombatMessage");
    assert!(!vic_ev.is_empty(), "Victim P2 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(vic_ev[0].len(), 2);
    assert!(matches!(vic_ev[0][0].as_str(), Some("AttackedBy" | "AttackMissBy")));
    assert_eq!(vic_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));

    // 2. `F-01` regression: live missile launcher impact (`combat_missiles_aoe.rs` passes `victim_char_opt = None`!)
    // Verify `resolved_victim` delivers `OnCombatMessage`, `OnDamageMessage`, and `OnDamageMessages` to victim P2!
    let _ = ctx.drain_notifications_for_char(P1);
    let _ = ctx.drain_notifications_for_char(P2);

    let m_res = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(lml_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate Light Missile Launcher II on locked ship_p2");
    assert_eq!(m_res.dict_get("impacted"), Some(&PyRep::Bool(true)), "In-flight missile must impact ship_p2");

    let vic_missile_combat = events(&ctx, P2, "OnCombatMessage");
    assert_eq!(vic_missile_combat.len(), 1, "Victim P2 must receive OnCombatMessage when caller passes victim_char_opt = None");
    assert_eq!(vic_missile_combat[0][0].as_str(), Some("AttackedBy"));
    assert_eq!(vic_missile_combat[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_missile_combat[0][1].dict_get("type"), Some(&PyRep::Integer(i64::from(light_missile_tid))));
    assert!(!events(&ctx, P2, "OnDamageMessage").is_empty(), "Victim P2 must receive OnDamageMessage when victim_char_opt = None");
    assert!(!events(&ctx, P2, "OnDamageMessages").is_empty(), "Victim P2 must receive OnDamageMessages when victim_char_opt = None");
}
