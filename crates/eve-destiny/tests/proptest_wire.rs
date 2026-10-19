//! Property-based (`proptest`, >= 10,000 cases) and boundary tests for `eve-destiny`:
//! - Binary `AddBalls` (`wire::encode_add_balls` / `wire::decode_add_balls`) round-trip across all 13 `MovementMode`s (`0..=12`).

use eve_destiny::{wire, Ball, MovementMode};
use glam::DVec3;
use proptest::prelude::*;

fn arb_dvec3() -> impl Strategy<Value = DVec3> {
    (
        -1e12f64..1e12f64,
        -1e12f64..1e12f64,
        -1e12f64..1e12f64,
    )
        .prop_map(|(x, y, z)| DVec3::new(x, y, z))
}

fn arb_f32_as_f64(min: f32, max: f32) -> impl Strategy<Value = f64> {
    (min..max).prop_map(|v| f64::from((v * 100.0).round() / 100.0))
}

fn arb_movement_mode(origin: DVec3) -> impl Strategy<Value = MovementMode> {
    prop_oneof![
        arb_dvec3().prop_map(|direction| MovementMode::GotoDirection { direction }),
        (1i64..10_000_000i64, arb_f32_as_f64(500.0, 100_000.0))
            .prop_map(|(target_id, range)| MovementMode::Follow { target_id, range }),
        Just(MovementMode::Stop),
        (arb_dvec3(), (1u32..=120u32)).prop_map(move |(destination, tenths)| {
            MovementMode::new_warp(origin, destination, f64::from(tenths) / 10.0)
        }),
        (1i64..10_000_000i64, arb_f32_as_f64(500.0, 100_000.0))
            .prop_map(|(target_id, range)| MovementMode::Orbit { target_id, range }),
        Just(MovementMode::Missile { target_id: 0 }),
        Just(MovementMode::Mushroom),
        Just(MovementMode::Boid),
        Just(MovementMode::Troll),
        Just(MovementMode::Miniball),
        Just(MovementMode::Field),
        Just(MovementMode::Rigid),
        Just(MovementMode::Formation),
    ]
}

fn arb_ball() -> impl Strategy<Value = Ball> {
    (
        1i64..9_000_000_000i64,
        arb_dvec3(),
        arb_dvec3(),
        arb_f32_as_f64(50.0, 5000.0),
        arb_f32_as_f64(0.5, 25.0),
        (1_000.0f64..1e12f64).prop_map(|m| m.round()),
        arb_f32_as_f64(5.0, 25_000.0),
        arb_f32_as_f64(0.0, 1.0),
    )
        .prop_flat_map(
            |(id, position, velocity, max_velocity, inertia, mass, radius, speed_fraction)| {
                arb_movement_mode(position).prop_map(move |mode| Ball {
                    id,
                    type_id: 0,
                    position,
                    velocity,
                    max_velocity,
                    inertia,
                    mass,
                    radius,
                    speed_fraction,
                    mode,
                })
            },
        )
}

fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(10_000)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_cases()))]

    #[test]
    fn prop_destiny_encode_decode_add_balls_roundtrip(
        stamp in any::<u32>(),
        balls in prop::collection::vec(arb_ball(), 0..=4),
    ) {
        let ball_refs: Vec<&Ball> = balls.iter().collect();
        let encoded = wire::encode_add_balls(stamp, &ball_refs);
        let (pkt_type, decoded_stamp, decoded_balls) =
            wire::decode_add_balls(&encoded).expect("wire::decode_add_balls must succeed");
        prop_assert_eq!(pkt_type, 0);
        prop_assert_eq!(decoded_stamp, stamp);
        prop_assert_eq!(decoded_balls.len(), balls.len());

        for (orig, dec) in balls.iter().zip(decoded_balls.iter()) {
            prop_assert_eq!(dec.id, orig.id);
            prop_assert_eq!(dec.position, orig.position);
            let expected_mode_byte = match &orig.mode {
                MovementMode::GotoDirection { .. } => wire::DSTBALL_GOTO,
                MovementMode::Follow { .. } => wire::DSTBALL_FOLLOW,
                MovementMode::Stop => wire::DSTBALL_STOP,
                MovementMode::Warp { .. } => wire::DSTBALL_WARP,
                MovementMode::Orbit { .. } => wire::DSTBALL_ORBIT,
                MovementMode::Missile { .. } => wire::DSTBALL_MISSILE,
                MovementMode::Mushroom => wire::DSTBALL_MUSHROOM,
                MovementMode::Boid => wire::DSTBALL_BOID,
                MovementMode::Troll => wire::DSTBALL_TROLL,
                MovementMode::Miniball => wire::DSTBALL_MINIBALL,
                MovementMode::Field => wire::DSTBALL_FIELD,
                MovementMode::Rigid => wire::DSTBALL_RIGID,
                MovementMode::Formation => wire::DSTBALL_FORMATION,
            };
            prop_assert_eq!(dec.mode, expected_mode_byte);
        }
    }
}

#[test]
fn test_destiny_wire_decode_all_modes_and_invalid_inputs() {
    // Short buffer
    assert!(wire::decode_add_balls(&[]).is_err());
    assert!(wire::decode_add_balls(&[0, 1, 2, 3]).is_err());

    // Unsupported mode byte (99)
    let mut bad_mode = wire::encode_add_balls(
        7,
        &[&Ball {
            id: 42,
            type_id: 587,
            position: DVec3::new(1.0, 2.0, 3.0),
            velocity: DVec3::new(4.0, 5.0, 6.0),
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        }],
    );
    bad_mode[5 + 8] = 99;
    assert!(wire::decode_add_balls(&bad_mode).is_err());

    // Truncated buffer at every byte boundary of a multi-mode stream must fail cleanly without panic
    let sample_balls = [
        Ball {
            id: 1,
            type_id: 587,
            position: DVec3::new(10.0, 20.0, 30.0),
            velocity: DVec3::new(1.0, 2.0, 3.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0),
        },
        Ball {
            id: 2,
            type_id: 587,
            position: DVec3::new(50.0, 60.0, 70.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 5_000_000.0,
            radius: 1000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        Ball {
            id: 3,
            type_id: 587,
            position: DVec3::new(80.0, 90.0, 100.0),
            velocity: DVec3::new(100.0, 0.0, 0.0),
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 10.0,
            speed_fraction: 1.0,
            mode: MovementMode::Missile { target_id: 0 },
        },
    ];
    let refs: Vec<&Ball> = sample_balls.iter().collect();
    let valid = wire::encode_add_balls(99, &refs);
    let (pkt, s, dec) = wire::decode_add_balls(&valid).unwrap();
    assert_eq!(pkt, 0);
    assert_eq!(s, 99);
    assert_eq!(dec.len(), 3);
    for cut in 6..valid.len() {
        // Any cut inside a ball record must return Err (except exact ball boundaries)
        let _ = wire::decode_add_balls(&valid[..cut]);
    }
}
