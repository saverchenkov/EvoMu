//! Round-6 Track-3 P1 Module Cycle, Burst Projector & Passive Targeter Tests (`#395`, `#396`).

#![forbid(unsafe_code)]

use super::r6_t3_p1b_fitting_tests::{build_r6_fitting_ctx, make_session};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

fn simple_ball(id: i64, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn r6_t3_12_pre_cycle_resources_and_burst_safety() {
    let ctx = build_r6_fitting_ctx();
    let sys_id = 30_000_142;
    let attacker_ship = 1_000_800_030;
    let victim_ship = 1_000_800_002;
    let char_id = 9001;
    let burst_id = 1_000_800_031;
    let siege_id = 1_000_800_032;
    let stront_id = 1_000_800_033;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = attacker_ship;
        }
        snap.items.insert(
            attacker_ship,
            InventoryItemRecord {
                item_id: attacker_ship,
                type_id: 19720,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_id,
            InventoryItemRecord {
                item_id: burst_id,
                type_id: 40635,
                owner_id: char_id,
                location_id: attacker_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            siege_id,
            InventoryItemRecord {
                item_id: siege_id,
                type_id: 20280,
                owner_id: char_id,
                location_id: attacker_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stront_id,
            InventoryItemRecord {
                item_id: stront_id,
                type_id: 16275,
                owner_id: char_id,
                location_id: attacker_ship,
                flag: InventoryFlag::Cargo,
                quantity: 300,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(burst_id, true);
        snap.module_online_states.insert(siege_id, true);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(simple_ball(attacker_ship, DVec3::ZERO, 500.0)).await.unwrap();
    sys.spawn_ball(simple_ball(victim_ship, DVec3::new(5_000.0, 0.0, 0.0), 40.0)).await.unwrap();
    sys.set_capacitor(victim_ship, 1000.0).await.unwrap();

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    let mut sess = make_session(char_id, 98_000_001, attacker_ship, None, Some(sys_id));
    let kw = HashMap::new();
    let err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(sys.get_combat_state(victim_ship).await.unwrap().unwrap().capacitor, 1000.0);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    sys.set_capacitor(attacker_ship, 10.0).await.unwrap();
    let err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(siege_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughCapacitor");
    let rem_stront = ctx.db.with_snapshot(|snap| snap.items.get(&stront_id).unwrap().quantity);
    assert_eq!(rem_stront, 300);

    let err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughCapacitor");
    assert_eq!(sys.get_combat_state(victim_ship).await.unwrap().unwrap().capacitor, 1000.0);
}

#[tokio::test]
async fn r6_t3_13_passive_targeter_validation_and_timed_lock() {
    let ctx = build_r6_fitting_ctx();
    let sys_id = 30_000_142;
    let attacker_ship = 1_000_800_040;
    let victim_ship = 1_000_800_002;
    let char_id = 9001;
    let pass_id = 1_000_800_041;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = attacker_ship;
        }
        snap.items.insert(
            attacker_ship,
            InventoryItemRecord {
                item_id: attacker_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            pass_id,
            InventoryItemRecord {
                item_id: pass_id,
                type_id: 1952,
                owner_id: char_id,
                location_id: attacker_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(pass_id, true);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(simple_ball(attacker_ship, DVec3::ZERO, 40.0)).await.unwrap();
    sys.spawn_ball(simple_ball(victim_ship, DVec3::new(90_000.0, 0.0, 0.0), 40.0)).await.unwrap();

    let mut sess = make_session(char_id, 98_000_001, attacker_ship, None, Some(sys_id));
    let kw = HashMap::new();

    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(pass_id), PyRep::String("default".into()), PyRep::Integer(attacker_ship)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "CannotActivateOnSelf"
    );

    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(pass_id), PyRep::String("default".into()), PyRep::Integer(victim_ship)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    sys.set_ball_position(victim_ship, DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(pass_id), PyRep::String("default".into()), PyRep::Integer(victim_ship)],
        &kw,
        None,
    )
    .await
    .unwrap();

    let locks_now = sys.get_target_locks(attacker_ship).await.unwrap();
    assert_eq!(locks_now.len(), 1);
    assert!(locks_now[0].lock_time_ms > 0.0);
    assert!(!locks_now[0].locked);

    for _ in 0..10 {
        sys.tick().await.unwrap();
    }
    let locks_done = sys.get_target_locks(attacker_ship).await.unwrap();
    assert!(locks_done[0].locked);
}
