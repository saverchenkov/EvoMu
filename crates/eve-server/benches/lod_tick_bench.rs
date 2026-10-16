//! Benchmark harness for the 3-Tier LOD Living World Ecology (`living_world::LivingWorldDirector`)
//! and 1.0 Hz Destiny `Ballpark` simulation tick.

#![forbid(unsafe_code)]

use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_server::living_world::LivingWorldDirector;
use glam::DVec3;
use std::time::Instant;

fn main() {
    let ecology = LivingWorldDirector::new_seeded();

    let t0 = Instant::now();
    for _ in 0..100 {
        let _ = ecology.list_convoys();
    }
    let ecology_elapsed = t0.elapsed();

    let mut park = Ballpark::new(30000142);
    for i in 0..250 {
        park.add_ball(Ball {
            id: 900_000_000 + i,
            type_id: 587,
            position: DVec3::new((i as f64) * 250.0, 0.0, 0.0),
            velocity: DVec3::new(150.0, 0.0, 0.0),
            max_velocity: 385.0,
            inertia: 3.08,
            mass: 1_067_000.0,
            radius: 33.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection {
                direction: DVec3::X,
            },
        });
    }

    let t1 = Instant::now();
    for _ in 0..100 {
        let _ = park.step(1.0);
    }
    let park_elapsed = t1.elapsed();

    println!(
        "lod_tick_bench: 100 LivingWorld queries = {:?}, 100 Ballpark(250 balls) ticks = {:?}",
        ecology_elapsed, park_elapsed
    );
}
