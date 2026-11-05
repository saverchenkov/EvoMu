//! Additional unit and mutation-killing tests for `living_world_haulers`.

use super::*;
use crate::living_world::{LivingWorldConvoy, NpcArchetype};
use eve_db::clock::VirtualServerClock;
use eve_sde::{SdeRegistry, StargateJump};
use glam::DVec3;
use std::sync::Arc;

#[test]
fn compute_trade_route_max_hops_min_gate_and_cargo_loot_all_tiers() {
    let mut sde = SdeRegistry::with_canonical_seed();

    // 5-system chain with competing stargate IDs to test min_by_key and 3-hop cap (4 systems total)
    for (gid, from_sys, to_sys) in [
        (50_900_010i64, 30_900_001u32, 30_900_099u32), // higher stargate_id -> ignored
        (50_900_005, 30_900_001, 30_900_002),          // lower stargate_id -> chosen
        (50_900_020, 30_900_002, 30_900_003),
        (50_900_030, 30_900_003, 30_900_004),
        (50_900_040, 30_900_004, 30_900_005),
    ] {
        sde.jumps.insert(
            gid,
            StargateJump {
                stargate_id: gid,
                from_solar_system_id: from_sys,
                to_solar_system_id: to_sys,
                destination_stargate_id: gid + 1,
                position: DVec3::ZERO,
            },
        );
    }

    let route = compute_trade_route(&sde, 30_900_001);
    assert_eq!(
        route,
        vec![30_900_001, 30_900_002, 30_900_003, 30_900_004]
    );

    // Custom hauler types with capacity = 10_000.0 and capacity = 100.0 (clamped to 1_000.0)
    let base_t = sde.types.values().next().unwrap().clone();
    let mut big_h = base_t.clone();
    big_h.type_id = 990_001;
    big_h.capacity = 10_000.0;
    sde.types.insert(big_h.type_id, big_h);

    let mut tiny_h = base_t;
    tiny_h.type_id = 990_002;
    tiny_h.capacity = 100.0;
    sde.types.insert(tiny_h.type_id, tiny_h);

    assert_eq!(
        generate_hauler_sde_cargo_loot(&sde, 990_001),
        vec![
            (34, 400_000),
            (35, 250_000),
            (36, 150_000),
            (37, 80_000),
            (3828, 240),
        ]
    );
    assert_eq!(
        generate_hauler_sde_cargo_loot(&sde, 990_002),
        vec![
            (34, 40_000),
            (35, 25_000),
            (36, 15_000),
            (37, 8_000),
            (3828, 24),
        ]
    );
}

#[tokio::test]
async fn step_hauler_convoy_default_route_warp_distance_boundary_and_fallbacks() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let template_sys = sde.solar_systems[&30_000_142].clone();

    // System 30_910_001 has no stargates and no stations -> exercises target_pos fallback (20_000, 0, 0)
    // System 30_910_002 has no stargates -> exercises gate_pos fallback (10_000, 0, 0)
    for sid in [30_910_001u32, 30_910_002] {
        let mut sys = template_sys.clone();
        sys.solar_system_id = sid;
        sys.stations.clear();
        sde.solar_systems.insert(sid, sys);
    }

    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let systems = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let director = LivingWorldDirector::new();

    // Register convoy WITHOUT calling set_convoy_route -> initializes default [origin, destination] route
    let cid = 800_600_001;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_910_001,
        current_system_id: 30_910_001,
        destination_system_id: 30_910_002,
        destination_station_id: 60_999_999,
        cargo_type_id: 34,
        cargo_quantity: 10_000,
        lod_level: 2,
        destroyed: false,
    });

    // Spawn ball at (15_000, 0, 0) -> exact 5_000.0 m from fallback target_pos (20_000, 0, 0) -> no warp!
    let sys1 = systems.get_or_spawn(30_910_001);
    let (ball, combat) = sde_ball_and_combat(&sde, cid, 648, DVec3::new(15_000.0, 0.0, 0.0));
    sys1.spawn_ball_with_combat(ball, combat).await.unwrap();

    let st1 = step_hauler_convoy_internal(&director, cid, &systems, &db, &sde)
        .await
        .unwrap();
    assert_eq!(
        st1,
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_910_001
        }
    );
    let (_, snap_5000) = sys1.get_ball_snapshot(cid).await.unwrap().unwrap();
    assert!(!matches!(snap_5000.mode, MovementMode::Warp { .. }));

    // Move ball to (14_999, 0, 0) -> dist 5_001.0 m (> 5_000.0 m) -> initiates warp!
    assert!(
        sys1.set_ball_position(cid, DVec3::new(14_999.0, 0.0, 0.0))
            .await
    );
    db.clock().advance_secs(1);
    let st2 = step_hauler_convoy_internal(&director, cid, &systems, &db, &sde)
        .await
        .unwrap();
    assert_eq!(
        st2,
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_910_001
        }
    );
    let (_, snap_5001) = sys1.get_ball_snapshot(cid).await.unwrap().unwrap();
    assert!(matches!(snap_5001.mode, MovementMode::Warp { .. }));

    // Advance past transit duration -> jumps to 30_910_002 using fallback gate_pos (10_000, 0, 0) + (1_200, 0, 0)
    db.clock().advance_secs(600);
    let st3 = step_hauler_convoy_internal(&director, cid, &systems, &db, &sde)
        .await
        .unwrap();
    assert_eq!(
        st3,
        ConvoyStepStatus::JumpedGate {
            from_system: 30_910_001,
            to_system: 30_910_002
        }
    );
    let spawned_pos = systems.get_ball_position_sync(30_910_002, cid).unwrap();
    assert!((spawned_pos.x - 11_200.0).abs() < 1e-6);
}
