//! Integration tests for Phase 5 (Part 2): Salvage, Tractor Beam, Drones & CrimeWatch (`C-7, C-8`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_server::combat::crimewatch::{compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector};
use eve_server::combat::drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, DroneController,
    SalvageOutcome,
};
use eve_server::formulas::concord_response_delay_sec;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_c7_salvage_mechanics_tractor_beam_and_combat_drones() {
    // parity: plan:C-7
    // Salvage mechanics (salvager chance), tractor beam (pull wreck toward ship),
    // combat drone bandwidth & control (launch, recall, engage).

    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Set ship type to 593 (Tristan) which has 25 Mbit/s drone bandwidth in SDE
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 593;
        }
    });

    // Spawn player ship in space
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

    // 1. Salvage mechanics
    let wreck_item_id = 991_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_item_id,
            InventoryItemRecord {
                item_id: wreck_item_id,
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

    let wreck_pos = DVec3::new(2_000.0, 0.0, 0.0); // 2 km away (within 5 km salvager range)
    let wreck_ball = Ball {
        id: wreck_item_id,
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

    // Salvage attempt with failing roll (0.90 >= chance) -> wreck remains intact
    let fail_res =
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_item_id, 0.30, 0.90).await;
    assert_eq!(
        fail_res.unwrap(),
        SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials"
        }
    );
    assert!(sys_handle.get_ball_snapshot(wreck_item_id).await.unwrap().is_some());

    // Salvage attempt with succeeding roll (0.10 < chance) -> yields materials and consumes wreck
    let success_res =
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_item_id, 0.30, 0.10).await;
    match success_res.unwrap() {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert!(!yielded_salvage_type_ids.is_empty());
            assert!(!yielded_quantities.is_empty());
        }
        _ => panic!("Expected salvage success"),
    }
    assert!(
        sys_handle.get_ball_snapshot(wreck_item_id).await.unwrap().is_none(),
        "Wreck ball must be removed on successful salvage"
    );

    // 2. Tractor Beam pull simulation
    let far_wreck_pos = DVec3::new(15_000.0, 0.0, 0.0); // 15 km away
    let (pos_after_5s, arrived_5s) =
        step_tractor_beam(far_wreck_pos, ship_pos, 500.0, 5.0); // pulled 2500 m closer
    assert!(!arrived_5s);
    assert!((pos_after_5s.length() - 12_500.0).abs() < 1.0);

    let (pos_arrived, arrived) =
        step_tractor_beam(pos_after_5s, ship_pos, 500.0, 30.0); // arrives at stop distance
    assert!(arrived);
    assert!(pos_arrived.length() <= 1_201.0);

    // 3. Combat Drone Bandwidth & Control
    let valid_light_drones = vec![2454, 2454, 2454, 2454, 2454]; // 5x Hobgoblin I
    let bw_res = validate_drone_bandwidth(&ctx, ship_id, &valid_light_drones, &[]);
    assert!(bw_res.is_ok());

    let too_many_drones = vec![2454, 2454, 2454, 2454, 2454, 2454]; // 6 drones
    let count_err = validate_drone_bandwidth(&ctx, ship_id, &too_many_drones, &[]);
    assert!(count_err.is_err());

    let heavy_drones = vec![2444, 2444]; // 2x Ogre I (25 Mbit/s each = 50 Mbit/s > 25 Mbit/s limit)
    let bw_err = validate_drone_bandwidth(&ctx, ship_id, &heavy_drones, &[]);
    assert!(bw_err.is_err());

    // Drone Launch, Engage, and Recall
    let mut drone_ctrl = DroneController::new();
    let d1 = 890_001;
    let d2 = 890_002;
    ctx.db.with_mut_snapshot(|snap| {
        for &did in &[d1, d2] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454, // Hobgoblin I
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

    let launched_ids = drone_ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[d1, d2])
        .await
        .expect("launch_drones");
    assert_eq!(launched_ids.len(), 2);
    assert_eq!(drone_ctrl.active_drones.len(), 2);

    // Engage target
    let target_ball_id = 999_001;
    let target_ball = Ball {
        id: target_ball_id,
        type_id: 587,
        position: DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(target_ball, ctx.build_type_combat_state(587))
        .await;

    let engaged_count = drone_ctrl
        .engage_target(&ctx, sys_id, target_ball_id)
        .await
        .expect("engage_target");
    assert_eq!(engaged_count, 2);

    // Recall drones back to bay
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
async fn test_c8_crimewatch_state_machine_timers_flags_and_concord() {
    // parity: plan:C-8
    // CrimeWatch state machine (weapon timer, PvP aggression timer,
    // suspect/criminal flags, CONCORD response timer by system security status).

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut cw_director = CrimeWatchDirector::new();
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142; // Jita 0.95
    let now = 1_700_000_000;

    // 1. Weapon timer: 60s
    cw_director.trigger_weapon_timer(char_id, now);
    let state = cw_director.get_or_create(char_id);
    assert!(state.is_weapon_timer_active(now + 30));
    assert!(!state.is_weapon_timer_active(now + 61));

    // 2. CONCORD response timer mapping by security status
    assert_eq!(concord_response_delay_sec(1.0), 6.0);
    assert_eq!(concord_response_delay_sec(0.9), 6.0);
    assert_eq!(concord_response_delay_sec(0.8), 7.0);
    assert_eq!(concord_response_delay_sec(0.7), 10.0);
    assert_eq!(concord_response_delay_sec(0.6), 14.0);
    assert_eq!(concord_response_delay_sec(0.5), 19.0);
    assert!(concord_response_delay_sec(0.4).is_infinite());

    // 3. Low-sec unprovoked attack -> Suspect flag (900s), no CONCORD
    let lowsec_eval = cw_director.handle_combat_aggression(
        char_id,
        victim_char_id,
        ship_id,
        30_002_813, // Tama 0.3
        0.3,
        false,
        now,
    );
    assert_eq!(lowsec_eval, AggressionEvaluation::SuspectFlagTriggered);
    assert!(cw_director.get_or_create(char_id).is_suspect(now + 300));
    assert!(!cw_director.get_or_create(char_id).is_criminal(now));

    // 4. High-sec unprovoked attack -> Criminal flag + CONCORD response
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
    assert!(cw_director.get_or_create(char_id).is_criminal(now + 300));

    // Security status loss applied at aggression time per canonical mechanics (R-CONCORD-02)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            let loss = compute_sec_status_loss(c.security_status, false, 0.95);
            c.security_status = (c.security_status - loss).max(-10.0);
        }
    });

    // 5. CONCORD arrival after security-based delay:
    // Spawns real CONCORD ships and executes criminal through the canonical death pipeline (C-3)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let crim_pos = DVec3::new(50_000.0, 0.0, 0.0);
    let crim_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: crim_pos,
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
    let reports = cw_director.step_concord(&ctx, now + 6, 9999).await;
    assert_eq!(reports.len(), 1);

    let report = &reports[0];
    assert_eq!(report.criminal_char_id, char_id);
    assert_eq!(report.victim_char_id, victim_char_id);

    // Verify real CONCORD Police Battleship was spawned in space
    let concord_ball = sys_handle
        .get_ball_snapshot(report.concord_ball_id)
        .await
        .unwrap()
        .expect("concord ball in ballpark");
    assert_eq!(concord_ball.1.type_id, 11957); // CONCORD Police Commander

    // Verify death pipeline was executed: criminal ship destroyed, wreck created, killmail persisted
    let death = report.death_result.as_ref().expect("death pipeline result");
    assert!(death.wreck_item_id > 0);
    assert!(death.killmail_id > 0);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    // Verify 30-day Kill Right granted to victim
    ctx.db.with_snapshot(|snap| {
        let kr = snap.kill_rights.get(&report.kill_right_id).expect("kill right in db");
        assert_eq!(kr.owner_id, victim_char_id);
        assert_eq!(kr.target_id, char_id);
        assert_eq!(kr.activation_fee, IskCents(0));
        assert!(!kr.is_active_suspect);

        // Verify criminal security status penalized
        let crim = snap.characters.get(&char_id).unwrap();
        assert!(crim.security_status <= 0.0);
    });
}
