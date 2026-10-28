//! Integration tests for `SP-3` (Cloaking, Subwarp Navigation, Nullifier Scaling, Cyno Inhibitor, & E-Warp Reconnect) — Part 3.

use crate::common;
use crate::space_propulsion_interdiction::{grant_all_test_skills, make_item};

use eve_db::types::InventoryFlag;
use eve_destiny::{Ball, MovementMode};
use eve_dogma::DogmaItemState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp3_cloaking_prototype_covert_decloak_and_stabilization() {
    // parity: plan:SP-3
    // parity: feat:modules.cloak-prototype, feat:modules.cloak-decloak, feat:modules.covert-ops-cloak, feat:modules.cloak-stabilization-timer, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:SP-3", "feat:modules.cloak-prototype", "feat:modules.cloak-decloak", "feat:modules.covert-ops-cloak", "feat:modules.cloak-stabilization-timer"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(2099);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let proto_cloak_id = 60_000_301;
    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 11188; // Anathema (Covert Ops, group 830)
        snap.items.insert(
            proto_cloak_id,
            make_item(proto_cloak_id, 11370, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&11579); // Untrained Cloaking skill to assert full 30s base delay
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Position ship in open space far from station (so station doesn't proximity-decloak us immediately)
    let open_pos = DVec3::new(15_000_000.0, 0.0, 0.0);
    let (_, mut start_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    start_ball.position = open_pos;
    start_ball.velocity = DVec3::ZERO;
    start_ball.speed_fraction = 0.0;
    start_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(start_ball).await.unwrap();

    // 1. Prototype Cloaking Device I (`11370`, `feat:modules.cloak-prototype`, `feat:modules.cloak-stabilization-timer`)
    let (_, pre_sync_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let orig_vmax = pre_sync_ball.max_velocity;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(proto_cloak_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Prototype Cloaking Device I");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Sync cloak state");

    let (_, proto_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let proto_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(proto_cs.is_cloaked && !proto_cs.is_covert_cloak);
    assert_eq!(
        proto_cs.cloak_stabilization_remaining_s, 900.0,
        "Cloak activation must start 15-minute (900s) Cloak Stabilization timer"
    );
    assert!(
        (proto_ball.max_velocity - orig_vmax * 0.1).abs() < 1e-3,
        "Prototype Cloak I must apply -90% max_velocity penalty (0.1x): got {} vs orig {}",
        proto_ball.max_velocity,
        orig_vmax
    );

    // Prototype cloak blocks warp initiation with CloakedCannotWarp
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(warp_err.code, "CloakedCannotWarp");

    // 2. Proximity Decloak within 2,000m & Post-Decloak Targeting Delay (`feat:modules.cloak-decloak`)
    let hunter_id = 940_001;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: hunter_id,
                type_id: 587,
                position: open_pos + DVec3::new(1_500.0, 0.0, 0.0), // within 2,000 m
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    sys_handle.step_manual(1.0).await.expect("step proximity decloak");
    let decloaked_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, decloaked_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(!decloaked_cs.is_cloaked, "Object within 2,000m must decloak the ship");
    assert!(
        decloaked_cs.decloak_targeting_delay_remaining_s >= 29.0,
        "Prototype Cloak decloak must apply 30s sensor recalibration targeting delay (got {})",
        decloaked_cs.decloak_targeting_delay_remaining_s
    );
    assert!(
        (decloaked_ball.max_velocity - orig_vmax).abs() < 1e-3,
        "Decloaking must restore original max_velocity"
    );

    // While decloak_targeting_delay_remaining_s > 0, ship cannot lock targets
    assert!(
        sys_handle
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: hunter_id,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 1_000.0,
                    locked: false,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .is_err()
    );
    assert!(
        sys_handle.get_target_locks(ship_id).await.unwrap().is_empty(),
        "Post-decloak sensor recalibration delay must prevent target locking"
    );
    let _ = sys_handle.remove_ball(hunter_id).await;

    // 3. Covert Ops Cloaking Device II (`11578`, `feat:modules.covert-ops-cloak`) & 15-min Stabilization Expiry
    // Clear targeting delay and switch module to Covert Ops Cloaking Device II (11578)
    let mut clear_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    clear_cs.decloak_targeting_delay_remaining_s = 0.0;
    sys_handle.set_combat_state(ship_id, clear_cs).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&proto_cloak_id).unwrap().type_id = 11578; // Covert Ops Cloaking Device II
        snap.module_active_states.insert(proto_cloak_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .unwrap();

    let cov_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, cov_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(cov_cs.is_cloaked && cov_cs.is_covert_cloak);
    assert!((cov_ball.max_velocity - orig_vmax).abs() < 1e-3, "Covert Ops Cloak II has no velocity penalty");

    // Covert Ops Cloak allows initiating warp while cloaked!
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60003760), PyRep::Real(50_000.0)],
        &kw,
        None,
    )
    .await
    .expect("Covert Ops Cloak II must allow warping while cloaked");
    let (_, cov_warping_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(cov_warping_ball.mode, MovementMode::Warp { .. }));

    // Test 15-minute (900s) Cloak Stabilization timer countdown to 0.0
    let mut exp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    exp_cs.cloak_stabilization_remaining_s = 1.0;
    sys_handle.set_combat_state(ship_id, exp_cs).await.unwrap();
    sys_handle.step_manual(1.0).await.unwrap();
    let after_exp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(
        after_exp_cs.cloak_stabilization_remaining_s, 0.0,
        "15-minute (900s) Cloak Stabilization timer must count down to 0.0"
    );
}

#[tokio::test]
async fn test_sp3_subwarp_navigation_orbit_keep_at_range_align_goto_stop() {
    // parity: plan:SP-3
    // parity: feat:navigation.orbit, feat:navigation.keep-at-range, feat:navigation.align-to, feat:navigation.goto-point, feat:navigation.stop-ship, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.orbit", "feat:navigation.keep-at-range", "feat:navigation.align-to", "feat:navigation.goto-point", "feat:navigation.stop-ship"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let anchor_origin = DVec3::new(500_000.0, 0.0, 0.0);
    let (_, mut ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = anchor_origin;
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.mode = MovementMode::Stop;
    let mover_radius = ship_ball.radius;
    sys_handle.spawn_ball(ship_ball).await.unwrap();

    let target_id = 910_001;
    let target_pos = anchor_origin + DVec3::new(3_500.0, 0.0, 0.0);
    let target_radius = 50.0;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: target_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: target_radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    // 1. Keep at Range (`feat:navigation.keep-at-range`)
    let desired_range = 1_500.0;
    let expected_center_dist = target_radius + mover_radius + desired_range;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdFollowBall",
        &[PyRep::Integer(target_id), PyRep::Real(desired_range)],
        &kw,
        None,
    )
    .await
    .unwrap();

    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_kar_far) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((b_kar_far.position.distance(target_pos) - expected_center_dist).abs() < 250.0);

    let (_, mut close_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    close_ball.position = target_pos - DVec3::new(300.0, 0.0, 0.0);
    close_ball.velocity = DVec3::ZERO;
    close_ball.speed_fraction = 1.0;
    close_ball.mode = MovementMode::Follow {
        target_id,
        range: desired_range,
    };
    sys_handle.spawn_ball(close_ball).await.unwrap();
    for _ in 0..20 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_kar_close) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_from_close = b_kar_close.position.distance(target_pos);
    assert!(dist_from_close > 900.0 && (dist_from_close - expected_center_dist).abs() < 300.0);

    // 2. Orbit (`feat:navigation.orbit`)
    let orbit_range = 2_000.0;
    let expected_orbit_radius = target_radius + mover_radius + orbit_range;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdOrbit",
        &[PyRep::Integer(target_id), PyRep::Real(orbit_range)],
        &kw,
        None,
    )
    .await
    .unwrap();

    let (_, b_orbit_start) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let rel_start = (b_orbit_start.position - target_pos).normalize();
    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_orbit_end) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((b_orbit_end.position.distance(target_pos) - expected_orbit_radius).abs() < 350.0);
    let rel_end = (b_orbit_end.position - target_pos).normalize();
    let radial_speed = b_orbit_end.velocity.dot(rel_end).abs();
    let tangential_speed = (b_orbit_end.velocity - rel_end * b_orbit_end.velocity.dot(rel_end)).length();
    assert!(tangential_speed > radial_speed && tangential_speed > 100.0 && rel_start.dot(rel_end) < 0.98);

    // 3. Goto Direction (`feat:navigation.goto-point`)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..12 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_goto) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(b_goto.velocity.normalize().y > 0.90 && b_goto.velocity.length() > 200.0);

    // 4. Stop Ship (`feat:navigation.stop-ship`)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStop",
        &[],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_stopped) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(b_stopped.mode, MovementMode::Stop);
    assert!(b_stopped.velocity.length() < 1.0);

    // 5. Align To (`feat:navigation.align-to`)
    for &(hull_type_id, expected_align_ticks) in &[(587_u32, 5_usize), (24692_u32, 21_usize)] {
        let hull_dogma = DogmaItemState::from_sde(ship_id, hull_type_id, &ctx.sde);
        let vmax = hull_dogma.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
        let mass = hull_dogma.get_attribute(eve_sde::attr_ids::MASS, &ctx.sde);
        let inertia = hull_dogma.get_attribute(eve_sde::attr_ids::AGILITY, &ctx.sde);
        let tau = inertia * mass / 1_000_000.0;
        assert_eq!((4.0_f64.ln() * tau).ceil() as usize, expected_align_ticks);

        sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: hull_type_id,
                position: anchor_origin,
                velocity: DVec3::ZERO,
                max_velocity: vmax,
                inertia,
                mass,
                radius: ctx.sde.types[&hull_type_id].radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();

        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdAlignTo",
            &[PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .unwrap();

        for t in 1..=expected_align_ticks {
            sys_handle.step_manual(1.0).await.unwrap();
            let (_, b_now) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
            if t < expected_align_ticks {
                assert!(b_now.velocity.length() < 0.75 * vmax);
            } else {
                assert!(b_now.velocity.length() >= 0.75 * vmax);
            }
        }
    }
}

#[tokio::test]
async fn test_r2_track1_nullifier_skill_scaling_cyno_guards_and_ewarp_reconnect() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3099);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    // 1. Issue #70: Interdiction Nullification skill (57319) scaling & hull class restriction
    let (dur_0, cd_0) =
        eve_server::services::tactical_modules::compute_nullifier_duration_and_cooldown(10.0, 150.0, 0);
    let (dur_5, cd_5) =
        eve_server::services::tactical_modules::compute_nullifier_duration_and_cooldown(10.0, 150.0, 5);
    assert!((dur_0 - 10.0).abs() < 1e-6 && (cd_0 - 150.0).abs() < 1e-6);
    assert!(
        (dur_5 - 15.0).abs() < 1e-6 && (cd_5 - 75.0).abs() < 1e-6,
        "Interdiction Nullification V must scale duration +50% and cooldown -50%"
    );

    let nullifier_id = 60_000_401;
    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 587; // Rifter (Frigate group 25 - NOT nullifier-capable)
        snap.items.insert(
            nullifier_id,
            make_item(nullifier_id, 58919, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let hull_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(nullifier_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        hull_err.code, "InvalidShipGroup",
        "Non-eligible hull must be rejected when activating Interdiction Nullifier"
    );

    // Switch hull to Ares (Interceptor group 831) -> allowed before warp, rejected mid-warp
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 11202; // Ares (group 831)
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys_id = sess.solar_system_id.unwrap();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut mid_warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = mid_warp_mode {
        *phase = eve_destiny::WarpPhase::Cruising;
    }
    sys_handle.set_movement_mode(ship_id, mid_warp_mode).await.unwrap();
    let in_warp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(nullifier_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        in_warp_err.code, "CannotActivateInWarp",
        "Interdiction Nullifier must reject activation after entering warp tunnel"
    );
    sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();

    // 2. Issue #74: Mobile Cyno Inhibitor uses live ballpark position
    let inhib_id = 60_000_402;
    let live_inhib_pos = DVec3::new(500_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            inhib_id,
            make_item(
                inhib_id,
                33476,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
                true,
            ),
        );
        // Put stale position at 0,0,0 in snapshot, while live ball is at +500 km
        snap.item_positions.insert(inhib_id, [0.0, 0.0, 0.0]);
    });
    sys_handle
        .spawn_ball(Ball {
            id: inhib_id,
            type_id: 33476,
            position: live_inhib_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .unwrap();
    let inhibited_near_live = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::is_cyno_inhibited(
            &ctx,
            snap,
            sys_id,
            Some(DVec3::new(550_000.0, 0.0, 0.0)),
        )
    });
    let inhibited_near_stale = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::is_cyno_inhibited(
            &ctx,
            snap,
            sys_id,
            Some(DVec3::new(10_000.0, 0.0, 0.0)),
        )
    });
    assert!(
        inhibited_near_live && !inhibited_near_stale,
        "Mobile Cyno Inhibitor must evaluate live ballpark coordinates over stale snapshot coordinates"
    );

    // 3. Issue #93: Disconnect emergency warp preserves pre-warp position and initiates return warp on reconnect
    let grid_origin = DVec3::new(250_000.0, -100_000.0, 50_000.0);
    sys_handle.set_ball_position(ship_id, grid_origin).await;
    let disc_entry = eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnect entry");
    assert_eq!(
        disc_entry.pre_warp_position,
        Some([grid_origin.x, grid_origin.y, grid_origin.z])
    );
    // Simulate ship arriving at deep-space emergency warp spot
    let ewarp_spot = DVec3::from_array(disc_entry.emergency_warp_dest.unwrap());
    sys_handle.set_ball_position(ship_id, ewarp_spot).await;
    sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    let _ = eve_server::sp6_lifecycle::reconnect_character_in_space_sync(&ctx, char_id)
        .expect("reconnect in space");
    let (_, reconnected_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination, .. } = reconnected_ball.mode else {
        panic!("Reconnected ship at deep-space e-warp spot must automatically initiate return warp to pre-disconnect grid spot");
    };
    assert!(
        destination.distance(grid_origin) < 1.0,
        "Return warp destination must match pre-disconnect grid position"
    );
}
