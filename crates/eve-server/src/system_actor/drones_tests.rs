//! Unit tests for `system_actor::drones` (`#1541`).

use super::*;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;

fn make_drone_ball(id: i64) -> Ball {
    Ball {
        id,
        type_id: 10246,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 5_000.0,
        radius: 5.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_miner_state(drone_id: i64, solar_system_id: u32, next_cycle_unix: u64) -> DroneMiningState {
    DroneMiningState {
        drone_id,
        asteroid_id: 40_000_123,
        ship_id: 900_001,
        owner_char_id: 100_001,
        solar_system_id,
        yield_units: 25,
        duration_secs: 60,
        next_cycle_unix,
        waste_prob: 0.0,
        ore_type_id: 1230,
        destination_flag: eve_db::InventoryFlag::Cargo,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_runtime_actor_lifecycle_and_ball_removal_cleanup() {
    let registry = SystemRegistry::new();
    let sys = registry.get_or_spawn(30_000_142);
    let drone_a = 800_001;
    let drone_b = 800_002;

    // `if_present` refuses setting cooldown when ball is absent from ballpark.
    assert!(!sys
        .set_drone_next_ready_unix_if_present(drone_a, 1_500)
        .await
        .unwrap());
    assert_eq!(sys.drone_next_ready_unix(drone_a), None);

    // Spawn ball `drone_a`; now `if_present` succeeds and updates published view.
    let _ = sys
        .spawn_ball_with_combat(make_drone_ball(drone_a), CombatState::default())
        .await
        .unwrap();
    assert!(sys
        .set_drone_next_ready_unix_if_present(drone_a, 1_500)
        .await
        .unwrap());
    assert_eq!(sys.drone_next_ready_unix(drone_a), Some(1_500));

    // Upsert & reschedule miner for `drone_a`, plus orphan miner `drone_b` without ball.
    sys.upsert_drone_miner(make_miner_state(drone_a, 30_000_142, 100))
        .await
        .unwrap();
    sys.upsert_drone_miner(make_miner_state(drone_b, 30_000_142, 200))
        .await
        .unwrap();
    assert_eq!(registry.active_drone_miners_count(), 2);
    assert!(sys.reschedule_drone_miner(drone_a, 1_000).await.unwrap());
    assert_eq!(
        sys.active_drone_miner(drone_a).map(|m| m.next_cycle_unix),
        Some(1_060)
    );

    // Removing ball `drone_a` automatically clears its miner and combat timer in the actor.
    assert!(sys.remove_ball(drone_a).await.is_some());
    assert_eq!(sys.active_drone_miner(drone_a), None);
    assert_eq!(sys.drone_next_ready_unix(drone_a), None);
    assert_eq!(registry.active_drone_miners_count(), 1);

    // Registry clear removes remaining `drone_b`.
    registry.clear_all_drone_miners().await;
    assert_eq!(registry.active_drone_miners_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_runtime_sync_and_try_remove_ball_clear_state() {
    let registry = SystemRegistry::new();
    let sys = registry.get_or_spawn(30_000_144);
    let drone_id = 800_101;
    let _ = sys
        .spawn_ball_with_combat(make_drone_ball(drone_id), CombatState::default())
        .await
        .unwrap();

    sys.upsert_drone_miner_sync(make_miner_state(drone_id, 30_000_144, 500))
        .unwrap();
    assert!(sys.set_drone_next_ready_unix_sync(drone_id, 999).unwrap());
    assert!(sys.active_drone_miner(drone_id).is_some());
    assert_eq!(sys.drone_next_ready_unix(drone_id), Some(999));

    sys.try_remove_ball(drone_id);
    assert_eq!(sys.active_drone_miner(drone_id), None);
    assert_eq!(sys.drone_next_ready_unix(drone_id), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_runtime_apply_damage_destruction_clears_state_and_allows_hibernation() {
    let registry = SystemRegistry::new();
    let sys = registry.get_or_spawn(30_000_145);
    let drone_id = 800_201;

    let _ = sys
        .spawn_ball_with_combat(
            make_drone_ball(drone_id),
            CombatState {
                shield_hp: 50.0,
                max_shield_hp: 50.0,
                armor_hp: 50.0,
                max_armor_hp: 50.0,
                structure_hp: 50.0,
                max_structure_hp: 50.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    sys.upsert_drone_miner(make_miner_state(drone_id, 30_000_145, 700))
        .await
        .unwrap();
    assert!(sys.set_drone_next_ready_unix(drone_id, 1_234).await.unwrap());
    assert!(!sys.is_hibernated());

    // Orphan miner (after ball removal) keeps system awake until active_drone_miners is empty;
    // lethal `apply_damage` removes the ball and clears both miner & cooldown, enabling hibernation.
    let lethal = DamageVector {
        em: 500.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    };
    let (_applied, destroyed, _) = sys.apply_damage(drone_id, lethal).await.unwrap().unwrap();
    assert!(destroyed);
    assert_eq!(sys.active_drone_miner(drone_id), None);
    assert_eq!(sys.drone_next_ready_unix(drone_id), None);
    assert_eq!(registry.active_drone_miners_count(), 0);
    assert!(sys.is_hibernated());
}
