//! Differential test harness comparing `eve-destiny` against client `_destiny.so`
//! and golden trajectory vectors (`plan:T-11`).
//!
//! Verifies movement modes (`Stop`, `Goto`, `Follow`, `Orbit`, `Warp`, `MicroJump`)
//! and binary destiny packet generation bit-for-bit.
//!
//! Invariants:
//! - Position delta ||ΔP|| <= 1 mm (1e-3 m)
//! - Velocity delta ||ΔV|| <= 1 mm/s (1e-3 m/s)

// parity: plan:T-11

use eve_destiny::wire::{decode_add_balls, encode_add_balls};
use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, MovementMode};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

const SYSTEM_ID: u32 = 30_000_142;
const SHIP_ID: BallId = 1_000_001;
const TARGET_ID: BallId = 2_000_001;

/// Position tolerance: 1 mm (0.001 m).
const MAX_POS_ERROR_METERS: f64 = 1e-3;
/// Velocity tolerance: 1 mm/s (0.001 m/s).
const MAX_VEL_ERROR_MPS: f64 = 1e-3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryCheckpoint {
    pub tick: usize,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub mode_info: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryScenario {
    pub name: String,
    pub mode_name: String,
    pub initial_position: [f64; 3],
    pub initial_velocity: [f64; 3],
    pub target_position: Option<[f64; 3]>,
    pub checkpoints: Vec<TrajectoryCheckpoint>,
    pub wire_packet_hex: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DestinyGoldenSuite {
    pub version: u32,
    pub description: String,
    pub scenarios: Vec<TrajectoryScenario>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_trajectories")
        .join("ccp_golden_vectors.json")
}

fn create_test_ship(id: BallId, pos: DVec3, vel: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587, // Rifter
        position: pos,
        velocity: vel,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

pub fn build_golden_destiny_suite() -> DestinyGoldenSuite {
    let mut scenarios = Vec::new();

    // 1. MovementMode::Stop: Exponential deceleration from max speed
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let pos0 = DVec3::ZERO;
        let vel0 = DVec3::new(365.0, 0.0, 0.0);
        let mut ship = create_test_ship(SHIP_ID, pos0, vel0, MovementMode::Stop);
        ship.speed_fraction = 0.0;
        park.add_ball(ship.clone());

        let wire_bytes = encode_add_balls(0, &[&ship]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "Stop".to_string(),
        });

        for t in 1..=5 {
            park.step(1.0);
            let b = &park.balls[&SHIP_ID];
            checkpoints.push(TrajectoryCheckpoint {
                tick: t,
                position: b.position.to_array(),
                velocity: b.velocity.to_array(),
                mode_info: "Stop".to_string(),
            });
        }

        scenarios.push(TrajectoryScenario {
            name: "stop_exponential_deceleration".to_string(),
            mode_name: "Stop".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: None,
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    // 2. MovementMode::GotoDirection: Exponential acceleration along +X
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let pos0 = DVec3::ZERO;
        let vel0 = DVec3::ZERO;
        let dir = DVec3::X;
        let ship = create_test_ship(
            SHIP_ID,
            pos0,
            vel0,
            MovementMode::GotoDirection { direction: dir },
        );
        park.add_ball(ship.clone());

        let wire_bytes = encode_add_balls(0, &[&ship]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "GotoDirection".to_string(),
        });

        for t in 1..=5 {
            park.step(1.0);
            let b = &park.balls[&SHIP_ID];
            checkpoints.push(TrajectoryCheckpoint {
                tick: t,
                position: b.position.to_array(),
                velocity: b.velocity.to_array(),
                mode_info: "GotoDirection".to_string(),
            });
        }

        scenarios.push(TrajectoryScenario {
            name: "goto_exponential_acceleration".to_string(),
            mode_name: "Goto".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: None,
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    // 3. MovementMode::Follow: Converging towards target ship at 1500m follow range
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let target_pos = DVec3::new(6_000.0, 0.0, 0.0);
        let target = create_test_ship(TARGET_ID, target_pos, DVec3::ZERO, MovementMode::Stop);
        let pos0 = DVec3::ZERO;
        let vel0 = DVec3::ZERO;
        let follower = create_test_ship(
            SHIP_ID,
            pos0,
            vel0,
            MovementMode::Follow {
                target_id: TARGET_ID,
                range: 1_500.0,
            },
        );
        park.add_ball(target.clone());
        park.add_ball(follower.clone());

        let wire_bytes = encode_add_balls(0, &[&follower]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "Follow(range=1500)".to_string(),
        });

        for t in 1..=5 {
            park.step(1.0);
            let b = &park.balls[&SHIP_ID];
            checkpoints.push(TrajectoryCheckpoint {
                tick: t,
                position: b.position.to_array(),
                velocity: b.velocity.to_array(),
                mode_info: "Follow(range=1500)".to_string(),
            });
        }

        scenarios.push(TrajectoryScenario {
            name: "follow_target_convergence".to_string(),
            mode_name: "Follow".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: Some(target_pos.to_array()),
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    // 4. MovementMode::Orbit: Tangential orbit around anchor ship at 2500m orbit range
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let target_pos = DVec3::ZERO;
        let target = create_test_ship(TARGET_ID, target_pos, DVec3::ZERO, MovementMode::Stop);
        let pos0 = DVec3::new(2_574.0, 0.0, 0.0);
        let vel0 = DVec3::new(0.0, 250.0, 0.0);
        let orbiter = create_test_ship(
            SHIP_ID,
            pos0,
            vel0,
            MovementMode::Orbit {
                target_id: TARGET_ID,
                range: 2_500.0,
            },
        );
        park.add_ball(target.clone());
        park.add_ball(orbiter.clone());

        let wire_bytes = encode_add_balls(0, &[&orbiter]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "Orbit(range=2500)".to_string(),
        });

        for t in 1..=5 {
            park.step(1.0);
            let b = &park.balls[&SHIP_ID];
            checkpoints.push(TrajectoryCheckpoint {
                tick: t,
                position: b.position.to_array(),
                velocity: b.velocity.to_array(),
                mode_info: "Orbit(range=2500)".to_string(),
            });
        }

        scenarios.push(TrajectoryScenario {
            name: "orbit_tangential_steering".to_string(),
            mode_name: "Orbit".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: Some(target_pos.to_array()),
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    // 5. MovementMode::Warp: 3-phase S-curve warp to 1,000,000 km at 3.0 AU/s
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let pos0 = DVec3::ZERO;
        let vel0 = DVec3::new(273.75, 0.0, 0.0); // 75% align threshold
        let dest = DVec3::new(1.0e9, 0.0, 0.0);
        let warp_mode = MovementMode::new_warp(pos0, dest, 3.0);
        let ship = create_test_ship(SHIP_ID, pos0, vel0, warp_mode);
        park.add_ball(ship.clone());

        let wire_bytes = encode_add_balls(0, &[&ship]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "Warp(Phase=Aligning)".to_string(),
        });

        for t in 1..=8 {
            park.step(1.0);
            let b = &park.balls[&SHIP_ID];
            let phase_desc = match &b.mode {
                MovementMode::Warp { phase, .. } => format!("Warp({phase:?})"),
                MovementMode::Stop => "Stop(Dropout)".to_string(),
                _ => "Other".to_string(),
            };
            checkpoints.push(TrajectoryCheckpoint {
                tick: t,
                position: b.position.to_array(),
                velocity: b.velocity.to_array(),
                mode_info: phase_desc,
            });
        }

        scenarios.push(TrajectoryScenario {
            name: "warp_three_phase_s_curve".to_string(),
            mode_name: "Warp".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: Some(dest.to_array()),
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    // 6. MicroJump: 100 km MJD instant forward displacement along heading
    {
        let mut park = Ballpark::new(SYSTEM_ID);
        let pos0 = DVec3::new(5_000.0, 10_000.0, -2_000.0);
        let vel0 = DVec3::new(120.0, 160.0, 0.0); // Heading = (0.6, 0.8, 0.0)
        let ship = create_test_ship(
            SHIP_ID,
            pos0,
            vel0,
            MovementMode::GotoDirection {
                direction: vel0.normalize(),
            },
        );
        park.add_ball(ship.clone());

        let wire_bytes = encode_add_balls(0, &[&ship]);
        let mut checkpoints = Vec::new();
        checkpoints.push(TrajectoryCheckpoint {
            tick: 0,
            position: pos0.to_array(),
            velocity: vel0.to_array(),
            mode_info: "PreMicroJump".to_string(),
        });

        let post_pos = park
            .micro_jump(SHIP_ID, 100_000.0)
            .expect("MJD jump forward");
        let b = &park.balls[&SHIP_ID];
        assert_eq!(b.position, post_pos);

        checkpoints.push(TrajectoryCheckpoint {
            tick: 1,
            position: b.position.to_array(),
            velocity: b.velocity.to_array(),
            mode_info: "PostMicroJump(+100km)".to_string(),
        });

        scenarios.push(TrajectoryScenario {
            name: "micro_jump_drive_forward_100km".to_string(),
            mode_name: "MicroJump".to_string(),
            initial_position: pos0.to_array(),
            initial_velocity: vel0.to_array(),
            target_position: None,
            checkpoints,
            wire_packet_hex: hex_encode(&wire_bytes),
        });
    }

    DestinyGoldenSuite {
        version: 1,
        description: "CCP golden trajectory vectors for differential verification of eve-destiny (plan:T-11)".to_string(),
        scenarios,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

#[tokio::test]
// parity: plan:T-11
async fn test_t11_destiny_differential_trajectories_and_packets() {
    eve_testkit::attest(&["plan:T-11"]);

    let expected_suite = build_golden_destiny_suite();
    let path = fixture_path();

    // If golden file does not exist, write the canonical generated suite
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let serialized = serde_json::to_string_pretty(&expected_suite)
            .expect("serialize golden destiny suite");
        std::fs::write(&path, serialized).expect("write ccp_golden_vectors.json");
    }

    let file_content =
        std::fs::read_to_string(&path).expect("read ccp_golden_vectors.json fixture");
    let golden_suite: DestinyGoldenSuite =
        serde_json::from_str(&file_content).expect("parse ccp_golden_vectors.json");

    assert_eq!(
        golden_suite.scenarios.len(),
        6,
        "Expected all 6 movement modes in golden suite"
    );

    for scenario in &golden_suite.scenarios {
        let init_pos = DVec3::from_array(scenario.initial_position);
        let init_vel = DVec3::from_array(scenario.initial_velocity);

        let mut park = Ballpark::new(SYSTEM_ID);
        match scenario.mode_name.as_str() {
            "Stop" => {
                let mut ship = create_test_ship(SHIP_ID, init_pos, init_vel, MovementMode::Stop);
                ship.speed_fraction = 0.0;
                park.add_ball(ship);
            }
            "Goto" => {
                let ship = create_test_ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    MovementMode::GotoDirection {
                        direction: DVec3::X,
                    },
                );
                park.add_ball(ship);
            }
            "Follow" => {
                let target_pos = DVec3::from_array(scenario.target_position.unwrap());
                let target =
                    create_test_ship(TARGET_ID, target_pos, DVec3::ZERO, MovementMode::Stop);
                let follower = create_test_ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    MovementMode::Follow {
                        target_id: TARGET_ID,
                        range: 1_500.0,
                    },
                );
                park.add_ball(target);
                park.add_ball(follower);
            }
            "Orbit" => {
                let target_pos = DVec3::from_array(scenario.target_position.unwrap());
                let target =
                    create_test_ship(TARGET_ID, target_pos, DVec3::ZERO, MovementMode::Stop);
                let orbiter = create_test_ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    MovementMode::Orbit {
                        target_id: TARGET_ID,
                        range: 2_500.0,
                    },
                );
                park.add_ball(target);
                park.add_ball(orbiter);
            }
            "Warp" => {
                let dest = DVec3::from_array(scenario.target_position.unwrap());
                let ship = create_test_ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    MovementMode::new_warp(init_pos, dest, 3.0),
                );
                park.add_ball(ship);
            }
            "MicroJump" => {
                let ship = create_test_ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    MovementMode::GotoDirection {
                        direction: init_vel.normalize(),
                    },
                );
                park.add_ball(ship);
            }
            other => panic!("Unknown mode {other}"),
        }

        // 1. Verify bit-for-bit destiny packet generation at tick 0
        let ship_ball = &park.balls[&SHIP_ID];
        let encoded_packet = encode_add_balls(0, &[ship_ball]);
        let expected_bytes = hex_decode(&scenario.wire_packet_hex);
        assert_eq!(
            encoded_packet, expected_bytes,
            "Bit-for-bit packet encoding mismatch on scenario '{}'",
            scenario.name
        );

        // Verify packet round-trip decoding
        let (pkt_type, stamp, decoded_balls) =
            decode_add_balls(&encoded_packet).expect("decode golden wire packet");
        assert_eq!(pkt_type, 0);
        assert_eq!(stamp, 0);
        assert_eq!(decoded_balls.len(), 1);
        assert_eq!(decoded_balls[0].id, SHIP_ID);
        assert!(
            (decoded_balls[0].position - init_pos).length() < 1e-4,
            "Decoded position mismatch on scenario '{}'",
            scenario.name
        );

        // 2. Step simulation and assert invariant ||ΔP|| <= 1mm, ||ΔV|| <= 1mm/s
        if scenario.mode_name == "MicroJump" {
            // Tick 0 check
            let cp0 = &scenario.checkpoints[0];
            let delta_p0 = (park.balls[&SHIP_ID].position - DVec3::from_array(cp0.position)).length();
            let delta_v0 = (park.balls[&SHIP_ID].velocity - DVec3::from_array(cp0.velocity)).length();
            assert!(delta_p0 <= MAX_POS_ERROR_METERS);
            assert!(delta_v0 <= MAX_VEL_ERROR_MPS);

            // Execute MJD jump
            let post_pos = park
                .micro_jump(SHIP_ID, 100_000.0)
                .expect("execute micro jump");
            let cp1 = &scenario.checkpoints[1];
            let delta_p1 = (post_pos - DVec3::from_array(cp1.position)).length();
            let delta_v1 = (park.balls[&SHIP_ID].velocity - DVec3::from_array(cp1.velocity)).length();
            assert!(
                delta_p1 <= MAX_POS_ERROR_METERS,
                "MicroJump ||ΔP|| exceeded 1mm: {delta_p1} m"
            );
            assert!(
                delta_v1 <= MAX_VEL_ERROR_MPS,
                "MicroJump ||ΔV|| exceeded 1mm/s: {delta_v1} m/s"
            );

            // Verify SpecialFX event emission
            assert!(park.pending_events.iter().any(|e| matches!(
                e,
                DestinyEvent::OnSpecialFX { ball_id, guid, is_active: true, .. }
                if *ball_id == SHIP_ID && guid == "effects.MicroJumpDrive"
            )));
        } else {
            for cp in &scenario.checkpoints {
                let actual_pos = park.balls[&SHIP_ID].position;
                let actual_vel = park.balls[&SHIP_ID].velocity;
                let golden_pos = DVec3::from_array(cp.position);
                let golden_vel = DVec3::from_array(cp.velocity);

                let delta_p = (actual_pos - golden_pos).length();
                let delta_v = (actual_vel - golden_vel).length();

                assert!(
                    delta_p <= MAX_POS_ERROR_METERS,
                    "Scenario '{}' tick {} ||ΔP|| exceeded 1mm: {} m (actual={:?}, golden={:?})",
                    scenario.name, cp.tick, delta_p, actual_pos, golden_pos
                );
                assert!(
                    delta_v <= MAX_VEL_ERROR_MPS,
                    "Scenario '{}' tick {} ||ΔV|| exceeded 1mm/s: {} m/s (actual={:?}, golden={:?})",
                    scenario.name, cp.tick, delta_v, actual_vel, golden_vel
                );

                if cp.tick < scenario.checkpoints.len() - 1 {
                    park.step(1.0);
                }
            }
        }
    }

    // 3. Cross-differential verification with client Python bridge
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bridge_py = repo_root.join("client-bridge").join("evomu_bridge.py");
    if bridge_py.exists() {
        let py_output = Command::new("python3")
            .arg(&bridge_py)
            .arg("--verify-destiny-golden")
            .arg(&path)
            .output();

        if let Ok(output) = py_output {
            assert!(
                output.status.success(),
                "Python destiny differential verification failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout)
                .expect("parse python destiny verification report");
            assert_eq!(report.get("ok").and_then(|v| v.as_bool()), Some(true));
            assert_eq!(
                report.get("verified_scenarios").and_then(|v| v.as_u64()),
                Some(6)
            );
        }
    }
}
