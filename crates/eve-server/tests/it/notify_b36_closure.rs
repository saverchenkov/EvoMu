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

    // Negative check: completing a task before any operation is active returns false and emits nothing
    let res_inactive = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[PyRep::String("complete_task".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("complete_task without active operation");
    assert_eq!(res_inactive, PyRep::Bool(false));
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Positive check: activate operation 1 and complete task -> mutates progression state and emits both notifications
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("activate_operation");
    let res_task = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[PyRep::String("complete_task".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("complete_task");
    assert_eq!(res_task, PyRep::Bool(true));

    let task_ev = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev.len(), 1);
    assert_eq!(task_ev[0].len(), 1);
    assert!(task_ev[0][0].as_i64().unwrap_or(0) > 0);

    let adv_ev = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(adv_ev.len(), 1);
    assert_eq!(adv_ev[0].len(), 2);
    assert_eq!(adv_ev[0][0].as_i64(), Some(1));
    assert_eq!(adv_ev[0][1], task_ev[0][0]);
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
        snap.items.insert(
            ship_p1,
            InventoryItemRecord {
                item_id: ship_p1,
                type_id: 639,
                owner_id: P1,
                location_id: i64::from(fw_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_p2,
            InventoryItemRecord {
                item_id: ship_p2,
                type_id: 639,
                owner_id: P2,
                location_id: i64::from(fw_sys),
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
        let sk = snap.skills.entry(P1).or_default();
        for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 20327, 21667] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 1_280_000.0,
                },
            );
        }
        for tid in [639_u32, 2961, 21894] {
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
    });
    ctx.invalidate_ship_fitting(ship_p1);
    ctx.invalidate_ship_fitting(ship_p2);

    s1.solar_system_id = Some(fw_sys);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let sys_h = ctx.systems.get_or_spawn(fw_sys);
    for (sid, pos) in [(ship_p1, DVec3::ZERO), (ship_p2, DVec3::new(5_000.0, 0.0, 0.0))] {
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
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(ship_p2)],
        &kw,
        None,
    )
    .await
    .expect("AddTarget ship_p2");
    for _ in 0..30 {
        if sys_h
            .get_target_locks(ship_p1)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == ship_p2 && l.locked)
        {
            break;
        }
        let _ = sys_h.tick().await;
    }

    // Activate weapon via live dogmaIM.Activate + SystemActor tick -> emits OnCombatMessage to attacker P1 and victim P2
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)],
        &kw,
        None,
    )
    .await
    .expect("Activate weapon on locked ship_p2");
    let _ = sys_h.tick().await;

    let atk_ev = events(&ctx, P1, "OnCombatMessage");
    assert!(!atk_ev.is_empty(), "Attacker P1 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(atk_ev[0].len(), 2);
    assert!(matches!(atk_ev[0][0].as_str(), Some("AttackHit" | "AttackMiss")));
    assert_eq!(atk_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p2)));

    let vic_ev = events(&ctx, P2, "OnCombatMessage");
    assert!(!vic_ev.is_empty(), "Victim P2 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(vic_ev[0].len(), 2);
    assert!(matches!(vic_ev[0][0].as_str(), Some("AttackedBy" | "AttackMissBy")));
    assert_eq!(vic_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
}
