//! Differential parity harness against live CCP `_destiny.so` trajectories (`CCP_BUNDLED_RUNTIME`).
//!
//! Verifies all 6 core movement modes (`Stop`, `Goto`, `Follow`, `Orbit`, `Warp`, `MicroJump`),
//! all 5 native flight regimes (`submarine_drag_accel`, `warp_alignment_threshold`,
//! `orbital_plane_stability`, `warp_three_phase_scurve`, `elastic_sphere_collision`),
//! binary destiny wire packet round-trips, and `generate_golden_fixtures.py --diff`.

use eve_destiny::wire::{decode_add_balls, encode_add_balls};
use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, WarpPhase};
use glam::DVec3;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[cfg(test)]
#[path = "t11_destiny_differential/flight_regimes_tests.rs"]
mod flight_regimes_tests;

pub(crate) const SYSTEM_ID: u32 = 30_000_142;
pub(crate) const SHIP_ID: BallId = 1_000_001;
pub(crate) const TARGET_ID: BallId = 2_000_001;
pub(crate) const MAX_POS_ERR_M: f64 = 1e-3;
pub(crate) const MAX_VEL_ERR_MPS: f64 = 1e-3;

pub(crate) fn v3(obj: &Value, key: &str) -> DVec3 {
    let a = obj[key].as_array().expect("vec3 array");
    DVec3::new(
        a[0].as_f64().unwrap(),
        a[1].as_f64().unwrap(),
        a[2].as_f64().unwrap(),
    )
}

pub(crate) fn opt_v3(obj: &Value, key: &str) -> Option<DVec3> {
    obj.get(key)
        .filter(|v| !v.is_null())
        .map(|a| DVec3::new(a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_f64().unwrap()))
}

pub(crate) fn f64_or(obj: &Value, key: &str, default: f64) -> f64 {
    obj.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

pub(crate) fn fixture_paths() -> [PathBuf; 2] {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        root.join("tests/fixtures/ccp_golden_vectors.json"),
        root.join("tests/golden_trajectories/ccp_golden_vectors.json"),
    ]
}

pub(crate) fn load_suite() -> (PathBuf, String, Value) {
    let [primary, mirror] = fixture_paths();
    let raw = std::fs::read_to_string(&primary).expect("read primary ccp_golden_vectors.json");
    let raw_mirror =
        std::fs::read_to_string(&mirror).expect("read mirror ccp_golden_vectors.json");
    assert_eq!(raw, raw_mirror, "Golden JSON copies out of sync");
    let val: Value = serde_json::from_str(&raw).expect("parse ccp_golden_vectors.json");
    (primary, raw, val)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ship(
    id: BallId,
    pos: DVec3,
    vel: DVec3,
    max_velocity: f64,
    inertia: f64,
    mass: f64,
    radius: f64,
    speed_fraction: f64,
    mode: MovementMode,
) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: vel,
        max_velocity,
        inertia,
        mass,
        radius,
        speed_fraction,
        mode,
    }
}

pub(crate) fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

pub(crate) fn assert_cp_1mm(label: &str, pos: DVec3, vel: DVec3, cp: &Value) {
    let tick = cp["tick"].as_u64().unwrap() as usize;
    let gp = v3(cp, "position");
    let gv = v3(cp, "velocity");
    let dp = (pos - gp).length();
    let dv = (vel - gv).length();
    assert!(
        dp <= MAX_POS_ERR_M,
        "{label} tick {tick} ||ΔP|| = {dp:.6e} m > 1e-3 (actual={pos:?}, golden={gp:?})"
    );
    assert!(
        dv <= MAX_VEL_ERR_MPS,
        "{label} tick {tick} ||ΔV|| = {dv:.6e} m/s > 1e-3 (actual={vel:?}, golden={gv:?})"
    );
}

pub(crate) fn assert_mode_parity(label: &str, tick: usize, mode: &MovementMode, info: &str) {
    let ok = match info {
        "Stop" => matches!(mode, MovementMode::Stop),
        "Warp(Phase=Aligning)" => {
            matches!(mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. })
        }
        "Warp(Phase=Accelerating)" => {
            matches!(mode, MovementMode::Warp { phase: WarpPhase::Accelerating, .. })
        }
        "Warp(Phase=Cruising)" => {
            matches!(mode, MovementMode::Warp { phase: WarpPhase::Cruising, .. })
        }
        "Warp(Phase=Decelerating)" => {
            matches!(mode, MovementMode::Warp { phase: WarpPhase::Decelerating, .. })
        }
        _ => true,
    };
    assert!(ok, "{label} tick {tick}: mode mismatch for {info}: got {mode:?}");
}

#[test]
fn test_t11_destiny_oracle_provenance_and_diff() {
    // parity: plan:T-11
    eve_testkit::attest(&["plan:T-11"]);

    let (_path, raw, root) = load_suite();
    assert!(
        !raw.contains("DERIVED_REFERENCE"),
        "Golden fixture must never contain DERIVED_REFERENCE provenance"
    );
    assert_eq!(root["fixture_type"].as_str(), Some("CCP_BUNDLED_RUNTIME"));
    assert_eq!(root["oracle_source"].as_str(), Some("CCP_BUNDLED_RUNTIME"));

    let ver = root["active_version"].as_str().expect("active_version");
    assert_eq!(ver, "V24.01-3624425");
    let v_suite = &root["versions"][ver];
    assert_eq!(v_suite["fixture_type"].as_str(), Some("CCP_BUNDLED_RUNTIME"));
    assert_eq!(v_suite["oracle_source"].as_str(), Some("CCP_BUNDLED_RUNTIME"));
    assert_eq!(
        v_suite["client_meta"]["oracle_source"].as_str(),
        Some("CCP_BUNDLED_RUNTIME")
    );
    assert_eq!(
        v_suite["client_meta"]["physics_engine"].as_str(),
        Some("CCP_NATIVE_DESTINY_SO")
    );

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let generator = repo_root.join("tools/generate_golden_fixtures.py");

    let ok_diff = Command::new("python3")
        .arg(&generator)
        .arg("--diff")
        .arg(ver)
        .arg(ver)
        .output()
        .expect("run generate_golden_fixtures.py --diff");
    assert!(
        ok_diff.status.success(),
        "--diff self-check failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&ok_diff.stdout),
        String::from_utf8_lossy(&ok_diff.stderr)
    );

    let bad_diff = Command::new("python3")
        .arg(&generator)
        .arg("--diff")
        .arg("nonexistent-version")
        .arg(ver)
        .output()
        .expect("run generate_golden_fixtures.py --diff missing");
    assert!(!bad_diff.status.success(), "--diff must fail for missing version");
}

#[tokio::test]
async fn test_t11_destiny_differential_trajectories_and_packets() {
    // parity: plan:T-11
    eve_testkit::attest(&["plan:T-11"]);

    let (path, _raw, root) = load_suite();
    let ver = root["active_version"].as_str().unwrap();
    let scenarios = root["versions"][ver]["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 6, "Expected all 6 core Destiny modes");

    for sc in scenarios {
        let name = sc["name"].as_str().unwrap();
        let mode_name = sc["mode_name"].as_str().unwrap();
        let init_pos = v3(sc, "initial_position");
        let init_vel = v3(sc, "initial_velocity");
        let max_v = f64_or(sc, "max_velocity", 365.0);
        let inertia = f64_or(sc, "inertia", 3.45);
        let mass = f64_or(sc, "mass", 1_067_000.0);
        let radius = f64_or(sc, "radius", 37.0);
        let mut park = Ballpark::new(SYSTEM_ID);

        match mode_name {
            "Stop" => {
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    0.0,
                    MovementMode::Stop,
                ));
            }
            "Goto" => {
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    1.0,
                    MovementMode::GotoDirection { direction: DVec3::X },
                ));
            }
            "Follow" => {
                let tpos = v3(sc, "target_position");
                let tvel = opt_v3(sc, "target_velocity").unwrap_or(DVec3::ZERO);
                let tdir = opt_v3(sc, "target_goto_direction").map(|d| d.normalize());
                park.add_ball(ship(
                    TARGET_ID,
                    tpos,
                    tvel,
                    f64_or(sc, "target_max_velocity", 300.0),
                    f64_or(sc, "target_inertia", 2.0),
                    f64_or(sc, "target_mass", 10_000_000.0),
                    f64_or(sc, "target_radius", 100.0),
                    if tdir.is_some() { 1.0 } else { 0.0 },
                    tdir.map_or(MovementMode::Stop, |direction| MovementMode::GotoDirection {
                        direction,
                    }),
                ));
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    1.0,
                    MovementMode::Follow {
                        target_id: TARGET_ID,
                        range: f64_or(sc, "follow_range", 1_000.0),
                    },
                ));
            }
            "Orbit" => {
                let tpos = v3(sc, "target_position");
                park.add_ball(ship(
                    TARGET_ID,
                    tpos,
                    DVec3::ZERO,
                    0.0,
                    0.0,
                    f64_or(sc, "target_mass", 10_000_000.0),
                    f64_or(sc, "target_radius", 500.0),
                    0.0,
                    MovementMode::Stop,
                ));
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    1.0,
                    MovementMode::Orbit {
                        target_id: TARGET_ID,
                        range: f64_or(sc, "orbit_range", 2_000.0),
                    },
                ));
            }
            "Warp" => {
                let dest = v3(sc, "target_position");
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    1.0,
                    MovementMode::new_warp(init_pos, dest, 3.0),
                ));
            }
            "MicroJump" => {
                park.add_ball(ship(
                    SHIP_ID,
                    init_pos,
                    init_vel,
                    max_v,
                    inertia,
                    mass,
                    radius,
                    1.0,
                    MovementMode::GotoDirection {
                        direction: DVec3::new(0.6, 0.8, 0.0),
                    },
                ));
            }
            other => panic!("Unknown mode {other}"),
        }

        let encoded = encode_add_balls(0, &[&park.balls[&SHIP_ID]]);
        assert_eq!(encoded, hex_decode(sc["wire_packet_hex"].as_str().unwrap()));
        let (pkt_type, stamp, decoded) = decode_add_balls(&encoded).expect("decode AddBalls");
        assert_eq!((pkt_type, stamp, decoded.len(), decoded[0].id), (0, 0, 1, SHIP_ID));
        assert!((decoded[0].position - init_pos).length() < 1e-4);

        let cps = sc["checkpoints"].as_array().unwrap();
        if mode_name == "MicroJump" {
            let b0 = &park.balls[&SHIP_ID];
            assert_cp_1mm(name, b0.position, b0.velocity, &cps[0]);
            let post_pos = park.micro_jump(SHIP_ID, 100_000.0).expect("micro_jump");
            assert_cp_1mm(name, post_pos, park.balls[&SHIP_ID].velocity, &cps[1]);
            assert!(park.pending_events.iter().any(|e| matches!(
                e,
                DestinyEvent::OnSpecialFX { ball_id, guid, is_active: true, .. }
                if *ball_id == SHIP_ID && guid == "effects.MicroJumpDrive"
            )));
        } else {
            for (idx, cp) in cps.iter().enumerate() {
                let b = &park.balls[&SHIP_ID];
                let tick = cp["tick"].as_u64().unwrap() as usize;
                assert_cp_1mm(name, b.position, b.velocity, cp);
                assert_mode_parity(name, tick, &b.mode, cp["mode_info"].as_str().unwrap());
                if idx + 1 < cps.len() {
                    park.step(1.0);
                }
            }
        }
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bridge_py = repo_root.join("client-bridge/evomu_bridge.py");
    if bridge_py.exists() {
        let out = Command::new("python3")
            .arg(&bridge_py)
            .arg("--verify-destiny-golden")
            .arg(&path)
            .output()
            .expect("run evomu_bridge.py --verify-destiny-golden");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let report: Value = serde_json::from_slice(&out.stdout).expect("parse report");
        assert_eq!(report["ok"].as_bool(), Some(true));
        assert_eq!(report["verified_scenarios"].as_u64(), Some(6));
    }
}
