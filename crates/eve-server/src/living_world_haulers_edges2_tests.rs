//! Additional edge-case and branch-coverage tests for `living_world_haulers`.

#![forbid(unsafe_code)]

use super::*;
use crate::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use eve_db::clock::VirtualServerClock;
use eve_destiny::MovementMode;
use eve_sde::{SdeRegistry, StargateJump};
use glam::DVec3;
use std::sync::Arc;

#[test]
fn compute_trade_route_isolated_2cycle_selfloop_cargo_and_wreck_edges() {
    let mut sde = SdeRegistry::with_canonical_seed();

    // 1. Completely isolated system (no outgoing stargates) -> single-element route.
    let isolated_sys = 30_950_001_u32;
    assert_eq!(compute_trade_route(&sde, isolated_sys), vec![isolated_sys]);

    // 2. Self-loop stargate (from == to) -> route.len() == 1 fallback adds `to_solar_system_id`.
    let self_sys = 30_950_002_u32;
    sde.jumps.insert(
        50_950_001,
        StargateJump {
            stargate_id: 50_950_001,
            from_solar_system_id: self_sys,
            to_solar_system_id: self_sys,
            destination_stargate_id: 50_950_001,
            position: DVec3::ZERO,
        },
    );
    assert_eq!(compute_trade_route(&sde, self_sys), vec![self_sys, self_sys]);

    // 3. 2-system ping-pong cycle -> visits A, B, then stops at len 2.
    let cyc_a = 30_950_010_u32;
    let cyc_b = 30_950_011_u32;
    sde.jumps.insert(
        50_950_010,
        StargateJump {
            stargate_id: 50_950_010,
            from_solar_system_id: cyc_a,
            to_solar_system_id: cyc_b,
            destination_stargate_id: 50_950_011,
            position: DVec3::ZERO,
        },
    );
    sde.jumps.insert(
        50_950_011,
        StargateJump {
            stargate_id: 50_950_011,
            from_solar_system_id: cyc_b,
            to_solar_system_id: cyc_a,
            destination_stargate_id: 50_950_010,
            position: DVec3::ZERO,
        },
    );
    assert_eq!(compute_trade_route(&sde, cyc_a), vec![cyc_a, cyc_b]);

    // 4. Unknown hauler type_id -> default 5,000 m3 capacity; negative capacity -> clamped to 1,000 m3.
    assert_eq!(
        generate_hauler_sde_cargo_loot(&sde, 999_999_999),
        vec![
            (34, 200_000),
            (35, 125_000),
            (36, 75_000),
            (37, 40_000),
            (3828, 120),
        ]
    );
    let mut neg_t = sde.types.values().next().unwrap().clone();
    neg_t.type_id = 995_001;
    neg_t.capacity = -50.0;
    sde.types.insert(995_001, neg_t);
    assert_eq!(
        generate_hauler_sde_cargo_loot(&sde, 995_001),
        vec![
            (34, 40_000),
            (35, 25_000),
            (36, 15_000),
            (37, 8_000),
            (3828, 24),
        ]
    );

    // 5. hauler_wreck_type_id hit vs fallback (26557).
    sde.wrecks.insert(995_001, 26560);
    assert_eq!(hauler_wreck_type_id(&sde, 995_001), 26560);
    assert_eq!(hauler_wreck_type_id(&sde, 999_999_999), 26557);
}

#[tokio::test]
async fn step_hauler_convoy_lod1_non_materialized_warp_guard_and_gate_fallbacks() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let template_sys = sde.solar_systems[&30_000_142].clone();

    // Two systems: 30_960_001 -> 30_960_002, where 30_960_002 has NO return gate to 30_960_001,
    // only an outgoing gate to 30_960_003 (and a second higher-id gate) plus two stations.
    for (sid, st_id, st_pos) in [
        (30_960_001_u32, 60_960_001_i64, DVec3::new(100_000.0, 0.0, 0.0)),
        (30_960_002_u32, 60_960_002_i64, DVec3::new(200_000.0, 0.0, 0.0)),
    ] {
        let mut sys = template_sys.clone();
        sys.solar_system_id = sid;
        if let Some(st) = sys.stations.first_mut() {
            st.station_id = st_id;
            st.solar_system_id = sid;
            st.position = st_pos;
            sde.stations.insert(st_id, st.clone());
        }
        sde.solar_systems.insert(sid, sys);
    }
    sde.jumps.insert(
        50_960_102,
        StargateJump {
            stargate_id: 50_960_102,
            from_solar_system_id: 30_960_001,
            to_solar_system_id: 30_960_002,
            destination_stargate_id: 50_960_203,
            position: DVec3::new(50_000.0, 0.0, 0.0),
        },
    );
    // In 30_960_002, no gate back to 30_960_001; gates exist to 30_960_003 (id 50_960_203 vs 50_960_299).
    sde.jumps.insert(
        50_960_299,
        StargateJump {
            stargate_id: 50_960_299,
            from_solar_system_id: 30_960_002,
            to_solar_system_id: 30_960_004,
            destination_stargate_id: 50_960_402,
            position: DVec3::new(999_000.0, 0.0, 0.0),
        },
    );
    sde.jumps.insert(
        50_960_203,
        StargateJump {
            stargate_id: 50_960_203,
            from_solar_system_id: 30_960_002,
            to_solar_system_id: 30_960_003,
            destination_stargate_id: 50_960_302,
            position: DVec3::new(40_000.0, 100.0, 0.0),
        },
    );

    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000)));
    let systems = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let director = LivingWorldDirector::new();

    // Missing convoy -> error
    assert!(
        step_hauler_convoy_internal(&director, 999_999, &systems, &db, &sde)
            .await
            .is_err()
    );

    // LOD 1 convoy: traverses, jumps, and docks WITHOUT spawning or removing balls in SystemRegistry.
    let cid_lod1 = 800_960_001_i64;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: cid_lod1,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_960_001,
        current_system_id: 30_960_001,
        destination_system_id: 30_960_002,
        destination_station_id: 60_960_002,
        cargo_type_id: 34,
        cargo_quantity: 5_000,
        lod_level: 1,
        destroyed: false,
    });
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod1, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_960_001
        }
    );
    // Elapsed 1s (< transit_duration) stays TraversingSystem.
    db.clock().advance_secs(1);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod1, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_960_001
        }
    );
    // Advance past transit duration -> JumpedGate at LOD 1 (no ball spawned in 30_960_002).
    db.clock().advance_secs(600);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod1, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::JumpedGate {
            from_system: 30_960_001,
            to_system: 30_960_002
        }
    );
    assert!(systems
        .get_or_spawn(30_960_002)
        .get_ball_snapshot(cid_lod1)
        .await
        .unwrap()
        .is_none());

    // Advance past transit duration in destination system -> DockedAtDestination at LOD 1.
    db.clock().advance_secs(600);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod1, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::DockedAtDestination {
            station_id: 60_960_002,
            system_id: 30_960_002
        }
    );
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod1, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::AlreadyDocked
    );

    // LOD 2 convoy: test already-warping guard during Traverse, and fallback gate selection (50_960_203) on Jump.
    let cid_lod2 = 800_960_002_i64;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: cid_lod2,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_960_001,
        current_system_id: 30_960_001,
        destination_system_id: 30_960_002,
        destination_station_id: 60_999_999, // non-matching -> .or_else(|| sys.stations.first())
        cargo_type_id: 34,
        cargo_quantity: 5_000,
        lod_level: 2,
        destroyed: false,
    });
    let sys1 = systems.get_or_spawn(30_960_001);
    let (mut ball, combat) = sde_ball_and_combat(&sde, cid_lod2, 648, DVec3::ZERO);
    // Pre-set a Warp mode with a custom destination; Traverse must NOT overwrite an existing Warp.
    let custom_warp_dest = DVec3::new(123_456.0, 0.0, 0.0);
    ball.mode = MovementMode::new_warp(DVec3::ZERO, custom_warp_dest, 3.0);
    sys1.spawn_ball_with_combat(ball, combat).await.unwrap();

    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod2, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_960_001
        }
    );
    let (_, b_after) = sys1.get_ball_snapshot(cid_lod2).await.unwrap().unwrap();
    match b_after.mode {
        MovementMode::Warp { destination, .. } => assert_eq!(destination, custom_warp_dest),
        other => panic!("expected existing Warp mode preserved, got {other:?}"),
    }

    // Jump to 30_960_002 -> spawns at lowest stargate_id in 30_960_002 (50_960_203 at x=40_000, y=100) + (1_200, 0, 0).
    db.clock().advance_secs(600);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod2, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::JumpedGate {
            from_system: 30_960_001,
            to_system: 30_960_002
        }
    );
    let pos2 = systems.get_ball_position_sync(30_960_002, cid_lod2).unwrap();
    assert!((pos2.x - 41_200.0).abs() < 1e-6);
    assert!((pos2.y - 100.0).abs() < 1e-6);

    // Traverse in final destination system (next_sys is None) clears existing non-warp or initiates warp toward station.
    let sys2 = systems.get_or_spawn(30_960_002);
    db.clock().advance_secs(1);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod2, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_960_002
        }
    );
    let (_, b2) = sys2.get_ball_snapshot(cid_lod2).await.unwrap().unwrap();
    assert!(matches!(b2.mode, MovementMode::Warp { .. }));

    // Destroyed convoy returns ConvoyStepStatus::Destroyed.
    director.mark_convoy_destroyed(cid_lod2);
    assert_eq!(
        step_hauler_convoy_internal(&director, cid_lod2, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::Destroyed
    );
}
