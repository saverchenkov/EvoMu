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

fn make_space_drone(
    ball_id: i64,
    owner_char_id: i64,
    parent_ship_id: i64,
    solar_system_id: u32,
    is_abandoned: bool,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id,
        drone_item_id: ball_id,
        type_id: 2454,
        group_id: 100,
        owner_char_id: if is_abandoned { 0 } else { owner_char_id },
        original_owner_char_id: owner_char_id,
        parent_ship_id: if is_abandoned { 0 } else { parent_ship_id },
        solar_system_id,
        target_id: None,
        activity_state: 0,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 1_000.0,
        optimal_range: 5_000.0,
        damage_vector: DamageVector {
            em: 0.0,
            thermal: 40.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    }
}

struct TempSqliteGuard(std::path::PathBuf);
impl Drop for TempSqliteGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-shm"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_drones_checkpoint_persists_across_stop_and_sqlite_restart() {
    let tmp = std::env::temp_dir().join(format!(
        "evomu_active_drones_d2b_{}_{}.sqlite",
        std::process::id(),
        1
    ));
    let _guard = TempSqliteGuard(tmp.clone());
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("sqlite-shm"));

    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let jita_sys = 30_000_142u32;
    let perim_sys = 30_000_144u32;
    let drone_jita = 880_001i64;
    let drone_perim = 880_002i64;

    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(
            Arc::clone(&sde),
            Arc::clone(&db1),
        ));
        let sys_jita = reg1.get_or_spawn(jita_sys);
        let sys_perim = reg1.get_or_spawn(perim_sys);

        sys_jita
            .upsert_active_drone(make_space_drone(drone_jita, 100_001, 900_001, jita_sys, false))
            .await
            .unwrap();
        sys_perim
            .upsert_active_drone(make_space_drone(drone_perim, 100_002, 900_002, perim_sys, true))
            .await
            .unwrap();

        // Stop Jita actor -> flushes `KIND_ACTIVE_DRONES` checkpoint to SQLite under its own system scope.
        assert!(reg1.stop_system(jita_sys).await);
        let persisted_jita = db1
            .with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
                    conn,
                    jita_sys,
                    eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                )
            })
            .expect("load jita active drones");
        assert_eq!(persisted_jita.len(), 1);
        assert_eq!(persisted_jita[0].1.ball_id, drone_jita);
        assert_eq!(persisted_jita[0].1.solar_system_id, jita_sys);

        // Re-wake Jita -> drone restored only inside Jita, not Perimeter's drone.
        let sys_jita_re = reg1.get_or_spawn(jita_sys);
        assert!(sys_jita_re.active_drone(drone_jita).is_some());
        assert!(sys_jita_re.active_drone(drone_perim).is_none());

        // Removing the drone and stopping Jita clears stale SQLite checkpoint rows.
        assert!(sys_jita_re.remove_active_drone(drone_jita).await.unwrap().is_some());
        assert!(reg1.stop_system(jita_sys).await);
        let cleared_jita = db1
            .with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
                    conn,
                    jita_sys,
                    eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                )
            })
            .expect("load cleared jita active drones");
        assert!(cleared_jita.is_empty());

        // Re-seed Jita drone and stop both systems before reopening SQLite from disk.
        let sys_jita_re2 = reg1.get_or_spawn(jita_sys);
        sys_jita_re2
            .upsert_active_drone(make_space_drone(drone_jita, 100_001, 900_001, jita_sys, false))
            .await
            .unwrap();
        assert!(reg1.stop_system(jita_sys).await);
        assert!(reg1.stop_system(perim_sys).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(sde, db2));
    let sys_jita2 = reg2.get_or_spawn(jita_sys);
    let sys_perim2 = reg2.get_or_spawn(perim_sys);

    let restored_jita = sys_jita2
        .active_drone(drone_jita)
        .expect("jita drone survived sqlite restart");
    assert_eq!(restored_jita.solar_system_id, jita_sys);
    assert!(!restored_jita.is_abandoned);
    assert!(sys_jita2.active_drone(drone_perim).is_none());

    let restored_perim = sys_perim2
        .active_drone(drone_perim)
        .expect("perim drone survived sqlite restart");
    assert_eq!(restored_perim.solar_system_id, perim_sys);
    assert!(restored_perim.is_abandoned);
    assert!(sys_perim2.active_drone(drone_jita).is_none());

    assert!(reg2.stop_system(jita_sys).await);
    assert!(reg2.stop_system(perim_sys).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_extended_runtime_json_fallback_is_one_way_and_system_scoped() {
    let tmp = std::env::temp_dir().join(format!(
        "evomu_active_drones_legacy_d2b_{}_{}.sqlite",
        std::process::id(),
        2
    ));
    let _guard = TempSqliteGuard(tmp.clone());
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("sqlite-shm"));

    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite legacy"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let jita_sys = 30_000_142u32;
    let perim_sys = 30_000_144u32;
    let d1 = make_space_drone(880_101, 100_001, 900_001, jita_sys, false);
    let d2 = make_space_drone(880_102, 100_002, 900_002, perim_sys, true);

    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json = serde_json::json!({
            "active_drones_in_space": {
                "880101": d1,
                "880102": d2,
            }
        })
        .to_string();
    });

    let ctx = crate::router::ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    let sys_jita = ctx.systems.get_or_spawn(jita_sys);
    assert!(sys_jita.active_drone(880_101).is_some());
    assert!(sys_jita.active_drone(880_102).is_none());

    // Flushing combat runtime must NEVER write `active_drones_in_space` back into persisted combat payload.
    crate::services::combat_crimewatch::flush_combat_runtime_to_db(&ctx);
    let ext_after_flush = db.with_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json.clone()
    });
    assert!(
        !ext_after_flush.contains("active_drones_in_space"),
        "flush_combat_runtime_to_db must not persist active_drones_in_space: {ext_after_flush}"
    );

    // Stopping Jita flushes the migrated entry into SQLite `system_checkpoint` (`KIND_ACTIVE_DRONES`).
    assert!(ctx.systems.stop_system(jita_sys).await);
    let persisted = db
        .with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
                conn,
                jita_sys,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
            )
        })
        .expect("load migrated jita checkpoint");
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].1.ball_id, 880_101);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserve_finalize_mutate_and_hibernation_policy() {
    let reg = Arc::new(SystemRegistry::new());
    let sys_id = 30_000_142u32;
    let sys = reg.get_or_spawn(sys_id);

    // Partial-ok launch reservation enforces cap/bandwidth and keeps valid prefix.
    let res = sys
        .reserve_drone_launches(ReserveDroneLaunchesReq {
            ship_id: 900_001,
            max_drones_allowed: 1,
            max_bandwidth: 10.0,
            partial_ok: true,
            drones: vec![
                (1, make_space_drone(-200_001, 100_001, 900_001, sys_id, false)),
                (2, make_space_drone(-200_002, 100_001, 900_001, sys_id, false)),
            ],
        })
        .await
        .unwrap();
    assert_eq!(res.kept_indices, vec![0]);
    assert_eq!(res.reserved_ids, vec![-200_001]);
    assert_eq!(res.per_item_errors, vec![(2, "NoDroneManagementAbilitiesLeft")]);
    assert_eq!(res.rejected_error, None);

    // Rollback deletes pending reservation.
    sys.finalize_drone_launches(Vec::new(), res.reserved_ids)
        .await
        .unwrap();
    assert!(sys.active_drones_snapshot().is_empty());

    // Commit promotes reservation to persistent ball ID and keeps empty-ballpark system awake while controlled.
    let res2 = sys
        .reserve_drone_launches(ReserveDroneLaunchesReq {
            ship_id: 900_001,
            max_drones_allowed: 1,
            max_bandwidth: 10.0,
            partial_ok: false,
            drones: vec![(1, make_space_drone(-200_003, 100_001, 900_001, sys_id, false))],
        })
        .await
        .unwrap();
    assert_eq!(res2.rejected_error, None);
    let _ = sys
        .finalize_drone_launches(vec![(-200_003, 950_001, 950_001)], Vec::new())
        .await
        .unwrap();
    assert!(sys.active_drone(950_001).is_some());
    assert!(sys.active_drone(-200_003).is_none());
    assert!(!sys.is_hibernated());

    // Marking the drone abandoned and idle lets the zero-ball system hibernate cleanly.
    sys.mutate_active_drones(|drones, _, _| {
        let d = drones.get_mut(&950_001).unwrap();
        d.is_abandoned = true;
        d.owner_char_id = 0;
        d.parent_ship_id = 0;
        d.target_id = None;
        (true, ())
    })
    .await
    .unwrap();
    assert!(sys.is_hibernated());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_step_manual_drives_engaged_drones_in_system() {
    let ctx = crate::router::ServerContext::new();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let drone_id = 880_501i64;
    let target_id = 999_001i64;

    let _ = sys
        .spawn_ball_with_combat(make_drone_ball(drone_id), CombatState::default())
        .await
        .unwrap();
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                position: DVec3::new(2_000.0, 0.0, 0.0),
                radius: 50.0,
                ..make_drone_ball(target_id)
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                signature_radius: 100.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let mut d = make_space_drone(drone_id, 90_000_001, 100_001, sys_id, false);
    d.target_id = Some(target_id);
    d.activity_state = 1;
    sys.upsert_active_drone(d).await.unwrap();

    // Cooldown in future -> actor step_manual does NOT fire volley yet.
    let now = ctx.now_unix();
    assert!(sys.set_drone_next_ready_unix(drone_id, now + 60).await.unwrap());
    let _ = sys.step_manual(1.0).await.unwrap();
    let cs_before = sys.get_combat_state(target_id).await.unwrap().unwrap();
    assert!((cs_before.shield_hp - 500.0).abs() < 1e-6);

    // Cooldown ready -> SystemActor::step_manual executes step_engaged_drones_in_system via system_step_hook.
    assert!(sys.set_drone_next_ready_unix(drone_id, now).await.unwrap());
    let _ = sys.step_manual(1.0).await.unwrap();
    let cs_after = sys.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(
        cs_after.shield_hp < 500.0,
        "SystemActor::step_manual must fire engaged drone volley automatically"
    );
    assert!(
        sys.drone_next_ready_unix(drone_id).unwrap_or(0) > now,
        "step_engaged_drones_in_system must advance drone cooldown"
    );
}
