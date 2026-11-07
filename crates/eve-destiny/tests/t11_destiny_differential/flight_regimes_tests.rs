//! Semantic unit tests split from `t11_destiny_differential`.

use super::*;
use eve_destiny::{BallId, Ballpark, MovementMode, WarpPhase};
use glam::DVec3;

#[test]
fn test_t11_destiny_all_five_flight_regimes() {
    // parity: plan:T-11
    eve_testkit::attest(&["plan:T-11"]);

    let (_path, _raw, root) = load_suite();
    let ver = root["active_version"].as_str().unwrap();
    let regimes = &root["versions"][ver]["flight_regimes"];

    // 1. Submarine drag & acceleration + stop decay
    let r1 = &regimes["submarine_drag_accel"];
    let mut p1 = Ballpark::new(SYSTEM_ID);
    p1.add_ball(ship(
        SHIP_ID,
        DVec3::ZERO,
        DVec3::ZERO,
        r1["max_velocity"].as_f64().unwrap(),
        r1["inertia"].as_f64().unwrap(),
        r1["mass"].as_f64().unwrap(),
        r1["radius"].as_f64().unwrap(),
        1.0,
        MovementMode::GotoDirection { direction: DVec3::X },
    ));
    let accel_ticks = r1["accel_ticks"].as_u64().unwrap() as usize;
    let cps1 = r1["checkpoints"].as_array().unwrap();
    for (idx, cp) in cps1.iter().enumerate() {
        let b = &p1.balls[&SHIP_ID];
        let tick = cp["tick"].as_u64().unwrap() as usize;
        assert_cp_1mm("submarine_drag_accel", b.position, b.velocity, cp);
        assert_mode_parity("submarine_drag_accel", tick, &b.mode, cp["mode_info"].as_str().unwrap());
        if idx + 1 < cps1.len() {
            if tick == accel_ticks {
                p1.set_mode(SHIP_ID, MovementMode::Stop)
                    .expect("set Stop mode");
            }
            p1.step(1.0);
        }
    }

    // 2. Warp alignment threshold (> 0.75 * maxVelocity)
    let r2 = &regimes["warp_alignment_threshold"];
    let mut p2 = Ballpark::new(SYSTEM_ID);
    p2.add_ball(ship(
        SHIP_ID,
        DVec3::ZERO,
        DVec3::ZERO,
        r2["max_velocity"].as_f64().unwrap(),
        r2["inertia"].as_f64().unwrap(),
        r2["mass"].as_f64().unwrap(),
        r2["radius"].as_f64().unwrap(),
        1.0,
        MovementMode::new_warp(
            DVec3::ZERO,
            v3(r2, "destination"),
            r2["warp_speed_au_s"].as_f64().unwrap(),
        ),
    ));
    let mut entry_tick = None;
    let cps2 = r2["checkpoints"].as_array().unwrap();
    for (idx, cp) in cps2.iter().enumerate() {
        let b = &p2.balls[&SHIP_ID];
        let tick = cp["tick"].as_u64().unwrap() as usize;
        assert_cp_1mm("warp_alignment_threshold", b.position, b.velocity, cp);
        assert_mode_parity(
            "warp_alignment_threshold",
            tick,
            &b.mode,
            cp["mode_info"].as_str().unwrap(),
        );
        if entry_tick.is_none()
            && matches!(
                b.mode,
                MovementMode::Warp {
                    phase: WarpPhase::Accelerating,
                    ..
                }
            )
        {
            entry_tick = Some(tick);
        }
        if idx + 1 < cps2.len() {
            p2.step(1.0);
        }
    }
    assert_eq!(entry_tick, r2["warp_entry_tick"].as_u64().map(|v| v as usize));

    // 3. Orbital plane stability (DSTBALL_ORBIT with enable_new_orbit)
    let r3 = &regimes["orbital_plane_stability"];
    let mut p3 = Ballpark::new(SYSTEM_ID);
    let tid3 = r3["target_id"].as_i64().unwrap() as BallId;
    p3.add_ball(ship(
        tid3,
        v3(r3, "target_position"),
        DVec3::ZERO,
        0.0,
        0.0,
        10_000_000.0,
        r3["target_radius"].as_f64().unwrap(),
        0.0,
        MovementMode::Stop,
    ));
    p3.add_ball(ship(
        SHIP_ID,
        v3(r3, "initial_position"),
        v3(r3, "initial_velocity"),
        r3["max_velocity"].as_f64().unwrap(),
        r3["inertia"].as_f64().unwrap(),
        r3["mass"].as_f64().unwrap(),
        r3["radius"].as_f64().unwrap(),
        1.0,
        MovementMode::Orbit {
            target_id: tid3,
            range: r3["orbit_range"].as_f64().unwrap(),
        },
    ));
    let cps3 = r3["checkpoints"].as_array().unwrap();
    for (idx, cp) in cps3.iter().enumerate() {
        let b = &p3.balls[&SHIP_ID];
        let tick = cp["tick"].as_u64().unwrap() as usize;
        assert_cp_1mm("orbital_plane_stability", b.position, b.velocity, cp);
        assert!(
            b.position.z.abs() <= 1e-12 && b.velocity.z.abs() <= 1e-12,
            "Out-of-plane drift at tick {tick}: z={}, vz={}",
            b.position.z,
            b.velocity.z
        );
        if idx + 1 < cps3.len() {
            p3.step(1.0);
        }
    }

    // 4. Warp 3-phase S-curve (accel -> cruise -> decel -> Stop exit)
    let r4 = &regimes["warp_three_phase_scurve"];
    let mut p4 = Ballpark::new(SYSTEM_ID);
    let init4 = v3(r4, "initial_position");
    p4.add_ball(ship(
        SHIP_ID,
        init4,
        v3(r4, "initial_velocity"),
        r4["max_velocity"].as_f64().unwrap(),
        r4["inertia"].as_f64().unwrap(),
        r4["mass"].as_f64().unwrap(),
        r4["radius"].as_f64().unwrap(),
        1.0,
        MovementMode::new_warp(
            init4,
            v3(r4, "destination"),
            r4["warp_speed_au_s"].as_f64().unwrap(),
        ),
    ));
    let cps4 = r4["checkpoints"].as_array().unwrap();
    let mut cur_tick4 = 0usize;
    let mut seen_phases = std::collections::BTreeSet::new();
    for cp in cps4 {
        let tick = cp["tick"].as_u64().unwrap() as usize;
        while cur_tick4 < tick {
            p4.step(1.0);
            cur_tick4 += 1;
        }
        let b = &p4.balls[&SHIP_ID];
        let info = cp["mode_info"].as_str().unwrap();
        assert_cp_1mm("warp_three_phase_scurve", b.position, b.velocity, cp);
        assert_mode_parity("warp_three_phase_scurve", tick, &b.mode, info);
        seen_phases.insert(info.to_string());
    }
    for expected in [
        "Warp(Phase=Aligning)",
        "Warp(Phase=Accelerating)",
        "Warp(Phase=Cruising)",
        "Warp(Phase=Decelerating)",
        "Stop",
    ] {
        assert!(seen_phases.contains(expected), "Missing warp S-curve phase {expected}: {seen_phases:?}");
    }

    // 5. Elastic sphere collisions (free-free & free-static DoCollision)
    let r5 = &regimes["elastic_sphere_collision"];
    for label in ["free_free", "free_static"] {
        let sub = &r5[label];
        let mut pc = Ballpark::new(SYSTEM_ID);
        for b in sub["balls"].as_array().unwrap() {
            let id = b["id"].as_i64().unwrap() as BallId;
            let is_free = b["is_free"].as_bool().unwrap();
            let mode = opt_v3(b, "goto_direction").map_or(MovementMode::Stop, |d| {
                MovementMode::GotoDirection {
                    direction: d.normalize(),
                }
            });
            let ball_obj = ship(
                id,
                v3(b, "position"),
                v3(b, "velocity"),
                b["max_velocity"].as_f64().unwrap(),
                b["inertia"].as_f64().unwrap(),
                b["mass"].as_f64().unwrap(),
                b["radius"].as_f64().unwrap(),
                if is_free { 1.0 } else { 0.0 },
                if is_free { mode } else { MovementMode::Rigid },
            );
            if is_free {
                pc.add_ball(ball_obj);
            } else {
                pc.add_static_ball(ball_obj, 15);
            }
        }
        let cps = sub["checkpoints"].as_array().unwrap();
        for (idx, cp) in cps.iter().enumerate() {
            let tick = cp["tick"].as_u64().unwrap() as usize;
            for bg in cp["balls"].as_array().unwrap() {
                let id = bg["id"].as_i64().unwrap() as BallId;
                let actual = &pc.balls[&id];
                let dp = (actual.position - v3(bg, "position")).length();
                let dv = (actual.velocity - v3(bg, "velocity")).length();
                assert!(
                    dp <= MAX_POS_ERR_M && dv <= MAX_VEL_ERR_MPS,
                    "elastic_sphere_collision({label}) ball {id} tick {tick}: dp={dp:.6e} dv={dv:.6e}"
                );
            }
            if idx + 1 < cps.len() {
                pc.step(1.0);
            }
        }
    }
}
