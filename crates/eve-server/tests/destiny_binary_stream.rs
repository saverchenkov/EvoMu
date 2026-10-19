//! Integration tests for Destiny Binary Stream Completeness (`SP-6`, `Z-3`)
//! and Continuous Warp Physics curves, visual handoff, bubble interception, and scrambling.

#![forbid(unsafe_code)]

// parity: plan:Z-3
// parity: feat:navigation.warp-speed-formula
// parity: ballmode:DSTBALL_MISSILE
// parity: ballmode:DSTBALL_MUSHROOM
// parity: ballmode:DSTBALL_TROLL
// parity: ballmode:DSTBALL_FORMATION

use eve_destiny::wire::{
    decode_add_balls, encode_add_balls, DST_BALL_FORMATION, DST_BALL_MISSILE, DST_BALL_MUSHROOM,
    DST_BALL_TROLL,
};
use eve_destiny::{
    Ball, Ballpark, DestinyEvent, MovementMode, WarpPhase, AU_METERS,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn sample_test_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1000.0, 2000.0, 3000.0),
        velocity: DVec3::new(10.0, 20.0, 30.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

// parity: plan:Z-3
// parity: ballmode:DSTBALL_MISSILE
// parity: ballmode:DSTBALL_MUSHROOM
// parity: ballmode:DSTBALL_TROLL
// parity: ballmode:DSTBALL_FORMATION
#[test]
fn test_destiny_binary_mode_missile_mushroom_troll_formation() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Missile mode: MISSILE_Struct (48 bytes payload: target ID, speed, explosion params, flight time)
    // Total mode sector: 1 byte formationID + 48 bytes payload = 49 bytes. Total ball = 148 bytes.
    let missile_ball = sample_test_ball(101, MovementMode::Missile { target_id: 9001 });

    // 2. Mushroom mode: MUSHROOM_Struct (24 bytes payload)
    // Total mode sector: 1 byte formationID + 24 bytes payload = 25 bytes. Total ball = 124 bytes.
    let mushroom_ball = sample_test_ball(102, MovementMode::Mushroom);

    // 3. Troll mode: TROLL_Struct (4 bytes payload)
    // Total mode sector: 1 byte formationID + 4 bytes payload = 5 bytes. Total ball = 104 bytes.
    let troll_ball = sample_test_ball(103, MovementMode::Troll);

    // 4. Formation mode: FORMATION_Struct (16 bytes payload: formation ID, layout parameters)
    // Total mode sector: 1 byte formationID + 16 bytes payload = 17 bytes. Total ball = 116 bytes.
    let formation_ball = sample_test_ball(104, MovementMode::Formation);

    park.add_ball(missile_ball.clone());
    park.add_ball(mushroom_ball.clone());
    park.add_ball(troll_ball.clone());
    park.add_ball(formation_ball.clone());

    let balls = [&missile_ball, &mushroom_ball, &troll_ball, &formation_ball];
    let stamp = 42u32;
    let encoded = encode_add_balls(stamp, &balls);

    // Verify 5-byte header: packet_type = 0, stamp = 42
    assert_eq!(encoded[0], 0);
    assert_eq!(u32::from_le_bytes(encoded[1..5].try_into().unwrap()), 42);

    // Total length: 5 header + 148 (Missile) + 124 (Mushroom) + 104 (Troll) + 116 (Formation) = 497 bytes
    assert_eq!(encoded.len(), 5 + 148 + 124 + 104 + 116);

    // Verify slice bounds for each mode matching DestinyStructs.h exactly
    let missile_slice = &encoded[5..5 + 148];
    assert_eq!(missile_slice[8], DST_BALL_MISSILE);
    // Payload targetID at offset 100..108 (after 38 header + 25 mass + 36 data + 1 formationID = 100)
    assert_eq!(
        i64::from_le_bytes(missile_slice[100..108].try_into().unwrap()),
        9001
    );

    let mushroom_slice = &encoded[5 + 148..5 + 148 + 124];
    assert_eq!(mushroom_slice[8], DST_BALL_MUSHROOM);
    assert_eq!(mushroom_slice.len(), 124);

    let troll_slice = &encoded[5 + 148 + 124..5 + 148 + 124 + 104];
    assert_eq!(troll_slice[8], DST_BALL_TROLL);
    assert_eq!(troll_slice.len(), 104);

    let form_slice = &encoded[5 + 148 + 124 + 104..];
    assert_eq!(form_slice[8], DST_BALL_FORMATION);
    assert_eq!(form_slice.len(), 116);

    // Decode and verify all fields roundtrip
    let (pkt_type, dec_stamp, decoded) = decode_add_balls(&encoded).expect("decode_add_balls");
    assert_eq!(pkt_type, 0);
    assert_eq!(dec_stamp, 42);
    assert_eq!(decoded.len(), 4);

    // Missile
    let dec_missile = &decoded[0];
    assert_eq!(dec_missile.id, 101);
    assert_eq!(dec_missile.mode, DST_BALL_MISSILE);
    assert_eq!(dec_missile.target_id, Some(9001));
    assert_eq!(dec_missile.owner_id, Some(1));
    assert_eq!(dec_missile.follow_range, Some(0.0));
    assert_eq!(dec_missile.effect_stamp, Some(42));
    assert_eq!(dec_missile.direction, Some(DVec3::new(10.0, 20.0, 30.0)));

    // Mushroom
    let dec_mushroom = &decoded[1];
    assert_eq!(dec_mushroom.id, 102);
    assert_eq!(dec_mushroom.mode, DST_BALL_MUSHROOM);
    assert_eq!(dec_mushroom.follow_range, Some(37.0));
    assert_eq!(dec_mushroom.owner_id, Some(1));
    assert_eq!(dec_mushroom.effect_stamp, Some(42));

    // Troll
    let dec_troll = &decoded[2];
    assert_eq!(dec_troll.id, 103);
    assert_eq!(dec_troll.mode, DST_BALL_TROLL);
    assert_eq!(dec_troll.effect_stamp, Some(42));

    // Formation
    let dec_form = &decoded[3];
    assert_eq!(dec_form.id, 104);
    assert_eq!(dec_form.mode, DST_BALL_FORMATION);
    assert_eq!(dec_form.target_id, Some(0));
    assert_eq!(dec_form.follow_range, Some(0.0));
    assert_eq!(dec_form.effect_stamp, Some(42));
}

// parity: plan:Z-3
// parity: feat:navigation.warp-speed-formula
#[test]
fn test_warp_continuous_physics_curves_and_visual_handoff() {
    let mut park = Ballpark::new(30_000_142);
    let warp_speed_au = 3.0;
    let origin = DVec3::ZERO;
    let distance = 20.0 * AU_METERS; // 20 AU long warp
    let destination = DVec3::new(distance, 0.0, 0.0);

    let ship = Ball {
        id: 2001,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);

    // Initiate warp
    park.set_mode(
        2001,
        MovementMode::new_warp(origin, destination, warp_speed_au),
    )
    .expect("set warp mode");

    // Check tunnel visual handoff events emitted on warp initiation
    let init_events = park.drain_all_observer_events();
    let has_warp_to = init_events
        .values()
        .flatten()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 2001, .. }));
    assert!(has_warp_to, "Must emit DestinyEvent::WarpTo");

    let has_act_warp = park.pending_events.iter().any(|e| {
        matches!(e, DestinyEvent::OnActivatingWarp { ball_id: 2001 })
    });
    assert!(has_act_warp, "Must emit DestinyEvent::OnActivatingWarp");

    let has_sfx_start = park.pending_events.iter().any(|e| {
        matches!(
            e,
            DestinyEvent::OnSpecialFX {
                ball_id: 2001,
                guid,
                is_active: true,
                ..
            } if guid == "effects.WarpDrive"
        )
    });
    assert!(has_sfx_start, "Must emit OnSpecialFX for WarpDrive activation");

    // Clear pending events for step monitoring
    park.pending_events.clear();

    // Step physics and monitor continuous acceleration, cruise, deceleration
    let mut saw_acceleration = false;
    let mut saw_cruise = false;
    let mut saw_deceleration = false;
    let mut prev_velocity = 0.0f64;
    let mut max_speed = 0.0f64;

    for tick in 1..=80 {
        let events = park.step(1.0);
        let ball = park.balls.get(&2001).unwrap();
        let speed = ball.velocity.length();
        max_speed = max_speed.max(speed);

        match &ball.mode {
            MovementMode::Warp { phase, .. } => match phase {
                WarpPhase::Aligning => {}
                WarpPhase::Accelerating => {
                    saw_acceleration = true;
                    if prev_velocity > 0.0 && speed > 1000.0 {
                        // In exponential acceleration, speed must strictly increase
                        assert!(
                            speed >= prev_velocity,
                            "Speed must monotonically increase during acceleration (tick {tick}: {speed} >= {prev_velocity})"
                        );
                    }
                }
                WarpPhase::Cruising => {
                    saw_cruise = true;
                    let cruise_expected = warp_speed_au * AU_METERS;
                    let diff_ratio = (speed - cruise_expected).abs() / cruise_expected;
                    assert!(
                        diff_ratio < 0.05,
                        "Cruising speed must match warp_speed_au_s * AU_METERS: got {speed}, expected {cruise_expected}"
                    );
                }
                WarpPhase::Decelerating => {
                    saw_deceleration = true;
                }
            },
            MovementMode::Stop => {
                // Landed at destination
                let dist_from_dest = (ball.position - destination).length();
                assert!(
                    dist_from_dest <= 2_500.0,
                    "Ship overshot destination: remaining distance {dist_from_dest} m (must be <= 2,500m)"
                );

                // Verify visual handoff on deactivating warp
                let has_deact = events
                    .iter()
                    .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 2001 }));
                assert!(has_deact, "Must emit OnDeactivatingWarp on dropping out of warp");

                let has_sfx_end = events.iter().any(|e| {
                    matches!(
                        e,
                        DestinyEvent::OnSpecialFX {
                            ball_id: 2001,
                            guid,
                            is_active: false,
                            ..
                        } if guid == "effects.WarpDrive"
                    )
                });
                assert!(has_sfx_end, "Must emit OnSpecialFX with is_active = false on warp exit");
                break;
            }
            _ => {}
        }
        prev_velocity = speed;
    }

    assert!(saw_acceleration, "Warp must exhibit continuous exponential acceleration phase");
    assert!(saw_cruise, "Long warp must exhibit cruise phase at ship warp speed");
    assert!(saw_deceleration, "Warp must exhibit deceleration phase without overshoot");
    assert!(max_speed > 1.0e9, "Warp must reach relativistic AU/s velocities");
}

// parity: plan:Z-3
// parity: feat:modules.warp-disruptor
#[tokio::test]
async fn test_warp_scrambler_disruptor_blocking() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9901);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");

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

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock in Jita");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!() };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);

    // Perimeter stargate position
    let dest_x = 50_001_249i64;

    // 1. Unscrambled warp attempt: CmdWarpTo succeeds
    let ok_warp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpTo",
            &[PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await;
    assert!(ok_warp.is_ok(), "Unscrambled ship can warp");

    // Stop the ship
    let _ = jita_handle.set_movement_mode(ship_id, MovementMode::Stop).await;

    // 2. Apply warp scrambler: strength 1 vs warp core strength 1
    jita_handle
        .set_warp_scramble_strength(ship_id, 1)
        .await
        .expect("set_warp_scramble_strength");

    let scrambled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpTo",
            &[PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        scrambled_err.code, "WarpScrambled",
        "Warp scrambler must reject CmdWarpTo with WarpScrambled"
    );

    // Also verify CmdWarpToPoint is blocked
    let p_gate_pos = ctx
        .sde
        .jumps
        .get(&dest_x)
        .map(|j| j.position)
        .expect("stargate pos");
    let scrambled_point_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(p_gate_pos.x),
                PyRep::Real(p_gate_pos.y),
                PyRep::Real(p_gate_pos.z),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        scrambled_point_err.code, "WarpScrambled",
        "Warp scrambler must reject CmdWarpToPoint with WarpScrambled"
    );

    // 3. Equip warp core stabilizer (increase warp_core_strength to 2 > scramble strength 1)
    jita_handle
        .set_warp_core_strength(ship_id, 2)
        .await
        .expect("set_warp_core_strength");

    let stabilized_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpTo",
            &[PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await;
    assert!(
        stabilized_ok.is_ok(),
        "Ship with 2 warp core strength can warp through 1 disruptor strength"
    );

    // Stop again
    let _ = jita_handle.set_movement_mode(ship_id, MovementMode::Stop).await;

    // 4. Heavy warp scrambler with strength 2 (strength 2 >= core strength 2)
    jita_handle
        .set_warp_scramble_strength(ship_id, 2)
        .await
        .expect("set_warp_scramble_strength 2");

    let heavy_scrambled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpTo",
            &[PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(heavy_scrambled_err.code, "WarpScrambled");
}

// parity: plan:Z-3
// parity: feat:modules.warp-disruption-field
#[test]
fn test_warp_bubble_interception_stop_and_drag() {
    let mut park = Ballpark::new(30_000_142);
    let ship_id = 3001;

    let origin = DVec3::ZERO;
    let target_destination = DVec3::new(10_000_000.0, 0.0, 0.0); // 10,000 km

    // Test 1: Stop bubble placed at x = 5,000,000 m (5,000 km), radius = 20,000 m (20 km)
    let stop_bubble_id = 7001;
    let stop_bubble_pos = DVec3::new(5_000_000.0, 0.0, 0.0);
    let bubble_radius = 20_000.0;
    park.add_warp_bubble(stop_bubble_id, stop_bubble_pos, bubble_radius);

    let ship = Ball {
        id: ship_id,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);

    // Initiating warp towards 10,000 km must be intercepted by the stop bubble at 4,980,000 m
    park.set_mode(
        ship_id,
        MovementMode::new_warp(origin, target_destination, 3.0),
    )
    .expect("set warp mode");

    let ball_after_init = park.balls.get(&ship_id).unwrap();
    if let MovementMode::Warp { destination, .. } = ball_after_init.mode {
        assert_eq!(
            destination.x, 4_980_000.0,
            "Stop bubble must pull warp destination to the bubble perimeter entry point"
        );
    } else {
        panic!("Ship must be in Warp mode");
    }

    // Step physics to completion
    for _ in 0..40 {
        park.step(1.0);
    }
    let landed = park.balls.get(&ship_id).unwrap();
    assert_eq!(landed.mode, MovementMode::Stop);
    let dist_to_perimeter = (landed.position - DVec3::new(4_980_000.0, 0.0, 0.0)).length();
    assert!(
        dist_to_perimeter <= 2_500.0,
        "Ship must land at stop bubble entry perimeter within 2,500 m, got {dist_to_perimeter}"
    );

    // Test 2: Drag bubble placed at x = 10,100,000 m (100 km behind destination), radius = 20,000 m
    park.remove_warp_bubble(stop_bubble_id);
    let drag_bubble_id = 7002;
    let drag_bubble_pos = DVec3::new(10_100_000.0, 0.0, 0.0);
    park.add_warp_bubble(drag_bubble_id, drag_bubble_pos, bubble_radius);

    park.set_ball_position(ship_id, origin);
    park.set_mode(
        ship_id,
        MovementMode::new_warp(origin, target_destination, 3.0),
    )
    .expect("set warp with drag bubble");

    let ball_after_drag_init = park.balls.get(&ship_id).unwrap();
    if let MovementMode::Warp { destination, .. } = ball_after_drag_init.mode {
        assert_eq!(
            destination.x, 10_080_000.0,
            "Drag bubble inline behind destination must drag warp landing point to bubble perimeter"
        );
    } else {
        panic!("Ship must be in Warp mode");
    }

    // Step physics to completion
    for _ in 0..40 {
        park.step(1.0);
    }
    let landed_drag = park.balls.get(&ship_id).unwrap();
    assert_eq!(landed_drag.mode, MovementMode::Stop);
    let dist_to_drag = (landed_drag.position - DVec3::new(10_080_000.0, 0.0, 0.0)).length();
    assert!(
        dist_to_drag <= 2_500.0,
        "Ship must land at drag bubble perimeter within 2,500 m, got {dist_to_drag}"
    );

    // Test 3: Starting warp while already inside a bubble must fail with WarpScrambled
    park.set_ball_position(ship_id, drag_bubble_pos); // inside drag bubble
    let inside_err = park.set_mode(
        ship_id,
        MovementMode::new_warp(drag_bubble_pos, origin, 3.0),
    );
    assert!(inside_err.is_err(), "Warping from inside a bubble must be rejected");
    assert!(
        inside_err.unwrap_err().contains("WarpScrambled"),
        "Must return WarpScrambled error"
    );

    // Test 4: In-flight bubble interception
    park.remove_warp_bubble(drag_bubble_id);
    park.set_ball_position(ship_id, origin);
    park.set_mode(
        ship_id,
        MovementMode::new_warp(origin, target_destination, 3.0),
    )
    .expect("clean warp");

    // Advance 5 seconds into warp flight
    for _ in 0..5 {
        park.step(1.0);
    }
    let mid_flight_pos = park.balls.get(&ship_id).unwrap().position;

    // Deploy an interdiction bubble right at the ship's current position
    let dynamic_bubble_id = 7003;
    park.add_warp_bubble(dynamic_bubble_id, mid_flight_pos, 25_000.0);

    // Next step must immediately pull the ship out of warp
    let evs = park.step(1.0);
    let stopped_ball = park.balls.get(&ship_id).unwrap();
    assert_eq!(
        stopped_ball.mode,
        MovementMode::Stop,
        "Ship entering a warp bubble mid-flight must immediately drop out of warp"
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 3001 })),
        "Must emit OnDeactivatingWarp when stopped by in-flight bubble"
    );
}
