//! Unit tests for `living_world_haulers` trade routing, SDE cargo loot generation, and convoy transit.

use super::*;
use crate::living_world::{LivingWorldConvoy, NpcArchetype};
use eve_db::clock::VirtualServerClock;
use eve_sde::{SdeRegistry, StargateJump};
use glam::DVec3;
use std::sync::Arc;

fn build_hauler_chain_fixture() -> (Arc<SdeRegistry>, Arc<WorldRepository>, Arc<SystemRegistry>) {
    let mut sde = SdeRegistry::with_canonical_seed();
    let template_sys = sde
        .solar_systems
        .get(&30_000_142)
        .expect("Jita in canonical SDE")
        .clone();

    // Build a 3-system linear chain: 30_800_001 <-> 30_800_002 <-> 30_800_003
    // with stations at 60_800_001 and 60_800_003 so warp -> jump -> dock transitions exercise all branches.
    let defs = [
        (30_800_001u32, 60_800_001i64, DVec3::ZERO),
        (
            30_800_002u32,
            60_800_002i64,
            DVec3::new(1_000_000.0, 0.0, 0.0),
        ),
        (
            30_800_003u32,
            60_800_003i64,
            DVec3::new(2_000_000.0, 0.0, 0.0),
        ),
    ];
    for (sys_id, st_id, st_pos) in defs {
        let mut sys = template_sys.clone();
        sys.solar_system_id = sys_id;
        if let Some(st) = sys.stations.first_mut() {
            st.station_id = st_id;
            st.solar_system_id = sys_id;
            st.position = st_pos;
            sde.stations.insert(st_id, st.clone());
        }
        sde.solar_systems.insert(sys_id, sys);
    }

    // Stargates connecting 30_800_001 <-> 30_800_002 and 30_800_002 <-> 30_800_003.
    let jumps = [
        StargateJump {
            stargate_id: 50_800_102,
            from_solar_system_id: 30_800_001,
            to_solar_system_id: 30_800_002,
            destination_stargate_id: 50_800_201,
            position: DVec3::new(500_000.0, 0.0, 0.0),
        },
        StargateJump {
            stargate_id: 50_800_201,
            from_solar_system_id: 30_800_002,
            to_solar_system_id: 30_800_001,
            destination_stargate_id: 50_800_102,
            position: DVec3::new(-500_000.0, 0.0, 0.0),
        },
        StargateJump {
            stargate_id: 50_800_203,
            from_solar_system_id: 30_800_002,
            to_solar_system_id: 30_800_003,
            destination_stargate_id: 50_800_302,
            position: DVec3::new(500_000.0, 0.0, 0.0),
        },
        StargateJump {
            stargate_id: 50_800_302,
            from_solar_system_id: 30_800_003,
            to_solar_system_id: 30_800_002,
            destination_stargate_id: 50_800_203,
            position: DVec3::new(-500_000.0, 0.0, 0.0),
        },
    ];
    for j in jumps {
        sde.jumps.insert(j.stargate_id, j);
    }

    // Seed an explicit wreck mapping for Badger (648) so hauler_wreck_type_id covers hit + fallback.
    sde.wrecks.insert(648, 26558);

    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_760_000_000)));
    let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), db.clone()));
    (sde, db, systems)
}

#[test]
fn compute_trade_route_cargo_loot_and_wreck_resolution() {
    let (mut sde_arc, _, _) = build_hauler_chain_fixture();
    let sde = Arc::make_mut(&mut sde_arc);

    // 3-system chain: 30_800_001 -> 30_800_002 -> 30_800_003
    let route = compute_trade_route(sde, 30_800_001);
    assert_eq!(route, vec![30_800_001, 30_800_002, 30_800_003]);

    // Dead-end system with single gate into an already-visited origin still returns 2 hops.
    sde.jumps.insert(
        50_800_991,
        StargateJump {
            stargate_id: 50_800_991,
            from_solar_system_id: 30_800_999,
            to_solar_system_id: 30_800_999,
            destination_stargate_id: 50_800_991,
            position: DVec3::ZERO,
        },
    );
    let self_loop = compute_trade_route(sde, 30_800_999);
    assert_eq!(self_loop, vec![30_800_999, 30_800_999]);

    // Isolated system with zero stargates returns `[origin]`.
    assert_eq!(compute_trade_route(sde, 30_800_777), vec![30_800_777]);

    // Known vs unknown hauler cargo loot generation.
    let known_loot = generate_hauler_sde_cargo_loot(sde, 648);
    assert_eq!(known_loot.len(), 5);
    assert_eq!(known_loot[0].0, 34); // Tritanium
    assert!(known_loot[0].1 >= 10_000);
    assert_eq!(known_loot[4], (3828, known_loot[4].1.max(20)));

    let fallback_loot = generate_hauler_sde_cargo_loot(sde, 999_999_999);
    assert_eq!(fallback_loot.len(), 5);
    // 5_000 m3 capacity -> Tritanium = 5000 * 0.40 / 0.01 = 200_000
    assert_eq!(fallback_loot[0], (34, 200_000));
    assert_eq!(fallback_loot[4], (3828, 120));

    // Wreck type resolution: mapped (648 -> 26558) vs default fallback (26557).
    assert_eq!(hauler_wreck_type_id(sde, 648), 26558);
    assert_eq!(hauler_wreck_type_id(sde, 999_999_999), 26557);
}

#[tokio::test]
async fn step_hauler_convoy_full_route_warp_jump_dock_and_errors() {
    let (sde, db, systems) = build_hauler_chain_fixture();
    let director = LivingWorldDirector::new();

    // Missing convoy returns Err.
    assert!(step_hauler_convoy_internal(&director, 999_999, &systems, &db, &sde)
        .await
        .is_err());

    // Destroyed convoy returns ConvoyStepStatus::Destroyed.
    director.register_convoy(LivingWorldConvoy {
        convoy_id: 800_500_000,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_800_001,
        current_system_id: 30_800_001,
        destination_system_id: 30_800_003,
        destination_station_id: 60_800_003,
        cargo_type_id: 34,
        cargo_quantity: 25_000,
        lod_level: 1,
        destroyed: true,
    });
    assert_eq!(
        step_hauler_convoy_internal(&director, 800_500_000, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::Destroyed
    );

    // LOD 2 multi-system hauler convoy along 30_800_001 -> 30_800_002 -> 30_800_003.
    let cid = 800_500_001;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_800_001,
        current_system_id: 30_800_001,
        destination_system_id: 30_800_003,
        destination_station_id: 60_800_003,
        cargo_type_id: 34,
        cargo_quantity: 50_000,
        lod_level: 2,
        destroyed: false,
    });
    director.set_convoy_route(cid, vec![30_800_001, 30_800_002, 30_800_003]);
    assert!(director.get_convoy_route(cid).is_some());
    assert!(!director.is_convoy_docked(cid));

    // Spawn ball at origin (500km from exit gate) so initial Traverse initiates warp.
    let sys1 = systems.get_or_spawn(30_800_001);
    let (ball, combat) = sde_ball_and_combat(&sde, cid, 648, DVec3::ZERO);
    sys1.spawn_ball_with_combat(ball, combat).await.unwrap();

    // Tick 1: initializes last_hop_unix & initiates warp toward exit gate.
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_800_001
        }
    );

    // Tick 2 (+1s): mid-transit still returns TraversingSystem.
    db.clock().advance_secs(1);
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_800_001
        }
    );

    // Tick 3 (+500s): jumps gate 30_800_001 -> 30_800_002, despawns in Sys1, spawns in Sys2.
    db.clock().advance_secs(500);
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::JumpedGate {
            from_system: 30_800_001,
            to_system: 30_800_002
        }
    );
    assert!(systems.get_ball_position_sync(30_800_001, cid).is_none());
    assert!(systems.get_ball_position_sync(30_800_002, cid).is_some());

    // Tick 4 (+500s): jumps gate 30_800_002 -> 30_800_003.
    db.clock().advance_secs(500);
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::JumpedGate {
            from_system: 30_800_002,
            to_system: 30_800_003
        }
    );
    assert!(systems.get_ball_position_sync(30_800_003, cid).is_some());

    // Tick 5 (+500s): arrives & docks at station 60_800_003, removes ball & lowers LOD to 1.
    db.clock().advance_secs(500);
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::DockedAtDestination {
            station_id: 60_800_003,
            system_id: 30_800_003
        }
    );
    assert!(director.is_convoy_docked(cid));
    assert_eq!(director.get_convoy(cid).unwrap().lod_level, 1);
    assert!(systems.get_ball_position_sync(30_800_003, cid).is_none());

    // Tick 6: subsequent step returns AlreadyDocked.
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::AlreadyDocked
    );

    // step_all_hauler_convoys skips docked convoys and steps active LOD-1 convoys.
    let cid2 = 800_500_002;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: cid2,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_800_001,
        current_system_id: 30_800_001,
        destination_system_id: 30_800_002,
        destination_station_id: 60_800_002,
        cargo_type_id: 34,
        cargo_quantity: 25_000,
        lod_level: 1,
        destroyed: false,
    });
    director.set_convoy_route(cid2, vec![30_800_001, 30_800_002]);
    let batch = director.step_all_hauler_convoys(&systems, &db, &sde).await;
    assert_eq!(batch.len(), 1);
    assert_eq!(
        batch[0],
        (
            cid2,
            Ok(ConvoyStepStatus::TraversingSystem {
                system_id: 30_800_001
            })
        )
    );
}
