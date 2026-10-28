//! Integration tests for Drones, Salvage, Tractor Beam & CrimeWatch (`C-7, C-8`, part 2).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, DroneController,
    SalvageOutcome,
};
use eve_server::combat::npc_ai::compute_sec_status_gain;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_c7_salvager_tractor_beam_and_drones() {
    // parity: feat:modules.salvager
    // parity: feat:modules.tractor-beam
    // parity: feat:drones.launch-drones
    // parity: feat:drones.recall-drones
    // parity: feat:drones.return-orbit
    eve_testkit::attest(&[
        "feat:modules.salvager",
        "feat:modules.tractor-beam",
        "feat:drones.launch-drones",
        "feat:drones.recall-drones",
        "feat:drones.return-orbit",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Tristan (type 593): 25 Mbit/s bandwidth
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.type_id = 593;
        }
    });

    let ship_pos = DVec3::new(0.0, 0.0, 0.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 593,
        position: ship_pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(593))
        .await;

    // 1. Salvager module (feat:modules.salvager)
    let wreck_id = 660_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26557,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
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

    let wreck_pos = DVec3::new(3_000.0, 0.0, 0.0); // 3 km away (within 5 km range)
    let wreck_ball = Ball {
        id: wreck_id,
        type_id: 26557,
        position: wreck_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(wreck_ball, ctx.build_type_combat_state(26557))
        .await;

    // Successful salvage extracts materials and removes empty wreck
    let salvage_res = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_id, 0.30, 0.10)
        .await
        .expect("attempt_salvage_wreck");
    match salvage_res {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert!(!yielded_salvage_type_ids.is_empty());
            assert!(!yielded_quantities.is_empty());
        }
        SalvageOutcome::Failed { .. } => panic!("Expected salvage success on roll 0.10"),
    }
    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_none());

    // 2. Tractor beam mechanics (feat:modules.tractor-beam)
    let far_wreck = DVec3::new(18_000.0, 0.0, 0.0);
    let (pulled_pos, arrived_1) = step_tractor_beam(far_wreck, ship_pos, 500.0, 10.0);
    assert!(!arrived_1);
    assert!((pulled_pos.length() - 13_000.0).abs() < 1.0);

    let (final_pos, arrived_2) = step_tractor_beam(pulled_pos, ship_pos, 500.0, 30.0);
    assert!(arrived_2);
    assert!(final_pos.length() <= 1_201.0);

    // 3. Drones: launch, return-orbit, recall (feat:drones.launch-drones, feat:drones.return-orbit, feat:drones.recall-drones)
    let d1 = 810_001;
    let d2 = 810_002;
    ctx.db.with_mut_snapshot(|snap| {
        for &did in &[d1, d2] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454, // Hobgoblin I (5 Mbit/s)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
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
        }
    });

    // Validate drone bandwidth (feat:drones.launch-drones)
    let bw_check = validate_drone_bandwidth(&ctx, ship_id, &[2454, 2454], &[]);
    assert!(bw_check.is_ok());

    let mut drone_ctrl = DroneController::new();

    // Launch drones (feat:drones.launch-drones)
    let launched = drone_ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[d1, d2])
        .await
        .expect("launch_drones");
    assert_eq!(launched.len(), 2);
    assert_eq!(drone_ctrl.active_drones.len(), 2);

    // Return and orbit ship (feat:drones.return-orbit)
    let orbit_count = drone_ctrl
        .return_and_orbit(&ctx, sys_id, ship_id)
        .await
        .expect("return_and_orbit");
    assert_eq!(orbit_count, 2);

    for drone in drone_ctrl.active_drones.values() {
        assert!(drone.target_id.is_none());
        let b = sys_handle
            .get_ball_snapshot(drone.ball_id)
            .await
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(
            b.mode,
            MovementMode::Orbit {
                target_id: ship_id,
                range: 1_000.0
            }
        );
    }

    // Recall drones to DroneBay (feat:drones.recall-drones)
    let recalled = drone_ctrl
        .recall_drones(&ctx, sys_id, ship_id)
        .await
        .expect("recall_drones");
    assert_eq!(recalled.len(), 2);
    assert!(drone_ctrl.active_drones.is_empty());
    ctx.db.with_snapshot(|snap| {
        for &did in &[d1, d2] {
            assert_eq!(snap.items[&did].location_id, ship_id);
            assert_eq!(snap.items[&did].flag, InventoryFlag::DroneBay);
        }
    });
}

#[tokio::test]
async fn test_c8_crimewatch_concord_timers_and_security_status() {
    // parity: feat:dogma.weapon-timer
    // parity: feat:combat.aggression-tracking
    // parity: feat:crimewatch.security-status
    // parity: feat:crimewatch.sec-status-loss
    // parity: feat:crimewatch.sec-status-gain, rpc:charUnboundMgr.SelectCharacterID, rpc:crimewatch.GetMySecurityStatus
    eve_testkit::attest(&[
        "feat:dogma.weapon-timer",
        "feat:combat.aggression-tracking",
        "feat:crimewatch.security-status",
        "feat:crimewatch.sec-status-loss",
        "feat:crimewatch.sec-status-gain",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142; // Jita 0.95
    let now = 1_700_000_000;

    let mut cw_director = CrimeWatchDirector::new();

    // 1. Weapons timer (feat:dogma.weapon-timer)
    cw_director.trigger_weapon_timer(char_id, now);
    let state = cw_director.get_or_create(char_id);
    assert!(state.is_weapon_timer_active(now + 30));
    assert!(state.is_weapon_timer_active(now + 59));
    assert!(!state.is_weapon_timer_active(now + 61));

    // 2. Aggression tracking (feat:combat.aggression-tracking)
    let aggro = cw_director.aggression_mgr.record_aggression(
        char_id,
        victim_char_id,
        sys_id,
        false,
        false,
        now,
    );
    assert_eq!(aggro.aggressor_char_id, char_id);
    assert_eq!(aggro.victim_char_id, victim_char_id);
    assert!(cw_director
        .aggression_mgr
        .is_aggressor_against(char_id, victim_char_id, now + 100));

    let aggressors = cw_director
        .aggression_mgr
        .list_active_aggressors_for_victim(victim_char_id, now + 100);
    assert!(aggressors.contains(&char_id));

    // 3. Security status loss and gain formulas (feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-gain)
    let loss_ship = compute_sec_status_loss(0.5, false, 0.95);
    assert!(loss_ship >= 0.5, "Ship unprovoked attack penalty >= 0.5");

    let loss_pod = compute_sec_status_loss(0.5, true, 0.95);
    assert!(loss_pod >= 2.0, "Pod kill penalty >= 2.0");

    let gain = compute_sec_status_gain(0.0, 500_000.0);
    assert!(gain > 0.0, "Ratting bounty must increase security status");
    assert!(gain <= 0.5);

    // 4. Character Security Status persistence & RPC (feat:crimewatch.security-status)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.security_status = 2.4;
        }
    });

    let mut sess = SessionState::new(9902);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
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
    let sec_rep = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "GetMySecurityStatus", &[], &kw, None)
        .await
        .expect("GetMySecurityStatus");
    match sec_rep {
        PyRep::Real(v) => assert!((v - 2.4).abs() < 1e-6),
        other => panic!("Expected Real, got {other:?}"),
    }

    // 5. High-sec aggression -> Criminal flag + CONCORD response
    let highsec_eval = cw_director.handle_combat_aggression(
        char_id,
        victim_char_id,
        ship_id,
        sys_id,
        0.95,
        false,
        now,
    );
    match highsec_eval {
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec,
            arrival_unix,
        } => {
            assert_eq!(concord_delay_sec, 6.0);
            assert_eq!(arrival_unix, now + 6);
        }
        _ => panic!("Expected CriminalFlagTriggered"),
    }
    assert!(cw_director.get_or_create(char_id).is_criminal(now + 100));

    // Spawn criminal ship in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let crim_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(30_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(crim_ball, ctx.build_type_combat_state(587))
        .await;

    // Advance clock to CONCORD arrival (now + 6s)
    let reports = cw_director.step_concord(&ctx, now + 6, 7777).await;
    assert_eq!(reports.len(), 1);

    // CONCORD destroyed the criminal ship
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
    assert!(reports[0].death_result.is_ok());

    // Criminal security status penalized
    let crim_sec = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(crim_sec < 2.4, "Criminal security status must be reduced");
}
