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

fn make_space_drone(ball_id: i64, owner_char_id: i64, parent_ship_id: i64, solar_system_id: u32, is_abandoned: bool) -> DroneSpaceState {
    DroneSpaceState {
        ball_id, drone_item_id: ball_id, type_id: 2454, group_id: 100,
        owner_char_id: if is_abandoned { 0 } else { owner_char_id }, original_owner_char_id: owner_char_id,
        parent_ship_id: if is_abandoned { 0 } else { parent_ship_id }, solar_system_id,
        target_id: None, activity_state: 0, assist_char_id: None, guard_char_id: None,
        is_abandoned, is_sentry: false, bandwidth_used: 5.0, max_velocity: 1_000.0, optimal_range: 5_000.0,
        damage_vector: DamageVector { em: 0.0, thermal: 40.0, kinetic: 0.0, explosive: 0.0 },
        ..Default::default()
    }
}

fn hold_writer(db: &Arc<eve_db::WorldRepository>) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (db_c, (ltx, lrx), (rtx, rrx)) = (Arc::clone(db), tokio::sync::oneshot::channel::<()>(), tokio::sync::oneshot::channel::<()>());
    let h = tokio::task::spawn_blocking(move || db_c.with_mut_snapshot(|_| { let _ = ltx.send(()); let _ = rrx.blocking_recv(); }));
    (lrx, rtx, h)
}

struct TempSqliteGuard(std::path::PathBuf);
impl Drop for TempSqliteGuard {
    fn drop(&mut self) { for e in ["sqlite", "sqlite-wal", "sqlite-shm"] { let _ = std::fs::remove_file(self.0.with_extension(e)); } }
}

fn temp_sqlite_db(tag: &str) -> (std::path::PathBuf, TempSqliteGuard) {
    let tmp = std::env::temp_dir().join(format!("evomu_active_drones_{tag}_{}.sqlite", std::process::id()));
    for e in ["sqlite", "sqlite-wal", "sqlite-shm"] { let _ = std::fs::remove_file(tmp.with_extension(e)); }
    (tmp.clone(), TempSqliteGuard(tmp))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_drones_checkpoint_persists_across_stop_and_sqlite_restart() {
    let (tmp, _guard) = temp_sqlite_db("d2b_1");
    let (sde, jita_sys, perim_sys, drone_jita, drone_perim) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32, 30_000_144u32, 880_001i64, 880_002i64);
    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let (sys_jita, sys_perim) = (reg1.get_or_spawn(jita_sys), reg1.get_or_spawn(perim_sys));
        sys_jita.upsert_active_drone(make_space_drone(drone_jita, 100_001, 900_001, jita_sys, false)).await.unwrap();
        sys_perim.upsert_active_drone(make_space_drone(drone_perim, 100_002, 900_002, perim_sys, true)).await.unwrap();

        assert!(reg1.stop_system(jita_sys).await);
        let pj = db1.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(c, jita_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES)).expect("load jita active drones");
        assert_eq!((pj.len(), pj[0].1.ball_id, pj[0].1.solar_system_id), (1, drone_jita, jita_sys));

        let sys_jita_re = reg1.get_or_spawn(jita_sys);
        assert!(sys_jita_re.active_drone(drone_jita).is_some() && sys_jita_re.active_drone(drone_perim).is_none());
        assert!(sys_jita_re.remove_active_drone(drone_jita).await.unwrap().is_some() && reg1.stop_system(jita_sys).await);
        assert!(db1.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(c, jita_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES)).expect("load cleared jita").is_empty());

        reg1.get_or_spawn(jita_sys).upsert_active_drone(make_space_drone(drone_jita, 100_001, 900_001, jita_sys, false)).await.unwrap();
        assert!(reg1.stop_system(jita_sys).await && reg1.stop_system(perim_sys).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(sde, db2));
    let (sys_jita2, sys_perim2) = (reg2.get_or_spawn(jita_sys), reg2.get_or_spawn(perim_sys));
    let (rj, rp) = (sys_jita2.active_drone(drone_jita).expect("jita survived"), sys_perim2.active_drone(drone_perim).expect("perim survived"));
    assert_eq!((rj.solar_system_id, rj.is_abandoned, rp.solar_system_id, rp.is_abandoned), (jita_sys, false, perim_sys, true));
    assert!(sys_jita2.active_drone(drone_perim).is_none() && sys_perim2.active_drone(drone_jita).is_none());
    assert!(reg2.stop_system(jita_sys).await && reg2.stop_system(perim_sys).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_extended_runtime_json_fallback_is_one_way_and_system_scoped() {
    let (tmp, _guard) = temp_sqlite_db("legacy_d2b_2");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite legacy"));
    let (sde, jita_sys, perim_sys) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32, 30_000_144u32);
    let (d1, d2) = (make_space_drone(880_101, 100_001, 900_001, jita_sys, false), make_space_drone(880_102, 100_002, 900_002, perim_sys, true));
    db.with_mut_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json = serde_json::json!({"active_drones_in_space": {"880101": d1, "880102": d2}}).to_string());

    let ctx = crate::router::ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    assert!(ctx.systems.get_handle(perim_sys).is_none(), "upfront legacy migration must not wake cold {perim_sys}");
    let sys_jita = ctx.systems.get_or_spawn(jita_sys);
    assert!(sys_jita.active_drone(880_101).is_some() && sys_jita.active_drone(880_102).is_none());
    crate::services::combat_crimewatch::flush_combat_runtime_to_db(&ctx);
    assert!(db.with_snapshot(|s| !s.combat_crimewatch_state.extended_runtime_json.contains("active_drones_in_space")));
    assert!(ctx.systems.stop_system(jita_sys).await);
    let persisted = db.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(c, jita_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES)).expect("load migrated jita checkpoint");
    assert_eq!((persisted.len(), persisted[0].1.ball_id), (1, 880_101));

    let sys_perim = ctx.systems.get_or_spawn(perim_sys);
    let restored_perim = sys_perim.active_drone(880_102).expect("cold perim_sys must restore migrated 880_102");
    assert_eq!((restored_perim.solar_system_id, restored_perim.is_abandoned), (perim_sys, true));
    assert!(ctx.systems.stop_system(perim_sys).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserve_finalize_mutate_and_hibernation_policy() {
    let reg = Arc::new(SystemRegistry::new());
    let sys_id = 30_000_142u32;
    let sys = reg.get_or_spawn(sys_id);

    let res = sys.reserve_drone_launches(ReserveDroneLaunchesReq {
        ship_id: 900_001, max_drones_allowed: 1, max_bandwidth: 10.0, partial_ok: true,
        drones: vec![(1, make_space_drone(-200_001, 100_001, 900_001, sys_id, false)), (2, make_space_drone(-200_002, 100_001, 900_001, sys_id, false))],
    }).await.unwrap();
    assert_eq!((res.kept_indices, res.reserved_ids.clone(), res.per_item_errors, res.rejected_error), (vec![0], vec![-200_001], vec![(2, "NoDroneManagementAbilitiesLeft")], None));
    assert!(sys.active_drones_snapshot().is_empty() && reg.all_active_drones().is_empty() && reg.find_active_drone_by_item(-200_001).is_none() && !sys.is_hibernated());
    assert_eq!(sys.reserve_drone_launches(ReserveDroneLaunchesReq {
        ship_id: 900_001, max_drones_allowed: 1, max_bandwidth: 10.0, partial_ok: false,
        drones: vec![(3, make_space_drone(-200_009, 100_001, 900_001, sys_id, false))],
    }).await.unwrap().rejected_error, Some("NoDroneManagementAbilitiesLeft"));

    sys.finalize_drone_launches(Vec::new(), res.reserved_ids).await.unwrap();
    assert!(sys.active_drones_snapshot().is_empty());

    let res2 = sys.reserve_drone_launches(ReserveDroneLaunchesReq {
        ship_id: 900_001, max_drones_allowed: 1, max_bandwidth: 10.0, partial_ok: false,
        drones: vec![(1, make_space_drone(-200_003, 100_001, 900_001, sys_id, false))],
    }).await.unwrap();
    assert_eq!(res2.rejected_error, None);
    let _ = sys.finalize_drone_launches(vec![(-200_003, 950_001, 950_001)], Vec::new()).await.unwrap();
    assert!(sys.active_drone(950_001).is_some() && sys.active_drone(-200_003).is_none());
    assert!(!sys.is_hibernated());

    // Marking the drone abandoned and idle lets the zero-ball system hibernate cleanly.
    sys.mutate_active_drones(|drones, _, _| {
        let d = drones.get_mut(&950_001).unwrap();
        (d.is_abandoned, d.owner_char_id, d.parent_ship_id, d.target_id) = (true, 0, 0, None);
        (true, ())
    }).await.unwrap();
    assert!(sys.is_hibernated());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_step_manual_drives_engaged_drones_in_system() {
    let ctx = crate::router::ServerContext::new();
    let (sys_id, drone_id, target_id) = (30_000_142u32, 880_501i64, 999_001i64);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball_with_combat(make_drone_ball(drone_id), CombatState::default()).await.unwrap();
    let _ = sys.spawn_ball_with_combat(
        Ball { position: DVec3::new(2_000.0, 0.0, 0.0), radius: 50.0, ..make_drone_ball(target_id) },
        CombatState { shield_hp: 500.0, max_shield_hp: 500.0, armor_hp: 500.0, max_armor_hp: 500.0, structure_hp: 500.0, max_structure_hp: 500.0, signature_radius: 100.0, ..CombatState::default() },
    ).await.unwrap();
    let mut d = make_space_drone(drone_id, 90_000_001, 100_001, sys_id, false);
    (d.target_id, d.activity_state) = (Some(target_id), 1);
    sys.upsert_active_drone(d).await.unwrap();

    let now = ctx.now_unix();
    assert!(sys.set_drone_next_ready_unix(drone_id, now + 60).await.unwrap());
    let _ = sys.step_manual(1.0).await.unwrap();
    assert!((sys.get_combat_state(target_id).await.unwrap().unwrap().shield_hp - 500.0).abs() < 1e-6);

    assert!(sys.set_drone_next_ready_unix(drone_id, now).await.unwrap());
    let _ = sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(target_id).await.unwrap().unwrap().shield_hp < 500.0, "SystemActor::step_manual must fire engaged drone volley");
    assert!(sys.drone_next_ready_unix(drone_id).unwrap_or(0) > now, "step_engaged_drones_in_system must advance drone cooldown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upfront_legacy_migration_clears_disk_json_without_flush_preventing_resurrection() {
    let (tmp, _guard) = temp_sqlite_db("legacy_clear_3");
    let (sde, jita_sys, perim_sys) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32, 30_000_144u32);
    let (d1, d2) = (make_space_drone(880_201, 100_001, 900_001, jita_sys, false), make_space_drone(880_202, 100_002, 900_002, perim_sys, true));
    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        db1.with_mut_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json = serde_json::json!({"active_drones_in_space": {"880201": d1, "880202": d2}}).to_string());
        let ctx1 = crate::router::ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db1));
        assert!(db1.with_snapshot(|s| !s.combat_crimewatch_state.extended_runtime_json.contains("active_drones_in_space")));
        let (sys_jita, sys_perim) = (ctx1.systems.get_or_spawn(jita_sys), ctx1.systems.get_or_spawn(perim_sys));
        assert!(sys_jita.active_drone(880_201).is_some() && sys_perim.active_drone(880_202).is_some());
        assert!(sys_jita.remove_active_drone(880_201).await.unwrap().is_some() && sys_perim.remove_active_drone(880_202).await.unwrap().is_some());
        assert!(ctx1.systems.stop_system(jita_sys).await && ctx1.systems.stop_system(perim_sys).await);
    }
    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    assert!(db2.with_snapshot(|s| !s.combat_crimewatch_state.extended_runtime_json.contains("active_drones_in_space")));
    let ctx2 = crate::router::ServerContext::with_sde_and_repository(sde, db2);
    assert!(ctx2.systems.get_or_spawn(jita_sys).active_drone(880_201).is_none() && ctx2.systems.get_or_spawn(perim_sys).active_drone(880_202).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dirty_active_drone_checkpoint_prevents_hibernation_until_flushed() {
    let (tmp, _guard) = temp_sqlite_db("dirty_hib_4");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite dirty hib"));
    let reg = Arc::new(SystemRegistry::with_sde_and_db(Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let sys = reg.get_or_spawn(sys_id);
    let (lrx, rtx, holder) = hold_writer(&db);
    lrx.await.unwrap();
    sys.upsert_active_drone(make_space_drone(950_777, 100_001, 900_001, sys_id, true)).await.unwrap();
    let _ = sys.step_manual(1.0).await.unwrap();
    assert!(!sys.is_hibernated(), "actor with un-flushed dirty checkpoint must not hibernate while writer contended");
    let _ = rtx.send(()); holder.await.unwrap();
    let _ = sys.step_manual(1.0).await.unwrap();
    let _ = sys.step_manual(1.0).await.unwrap();
    assert!(sys.is_hibernated(), "actor with only idle abandoned drone must hibernate once dirty checkpoint flushes");
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_sqlite_hydration_contended_and_unsafe_read_preserves_view_hibernation_and_capacity() {
    let (tmp, _guard) = temp_sqlite_db("deferred_hyd_5");
    let (sde, jita_sys, perim_sys, ren_sys) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32, 30_000_144u32, 30_000_145u32);
    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let (jita, perim, ren) = (reg1.get_or_spawn(jita_sys), reg1.get_or_spawn(perim_sys), reg1.get_or_spawn(ren_sys));
        jita.upsert_active_drones(vec![make_space_drone(951_101, 100_001, 900_001, jita_sys, false), make_space_drone(951_102, 100_001, 900_001, jita_sys, false)], false).await.unwrap();
        perim.upsert_active_drone(make_space_drone(951_201, 100_002, 900_002, perim_sys, true)).await.unwrap();
        ren.upsert_active_drone(make_space_drone(951_301, 100_003, 900_003, ren_sys, true)).await.unwrap();
        assert!(reg1.stop_system(jita_sys).await && reg1.stop_system(perim_sys).await && reg1.stop_system(ren_sys).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db2)));
    // (a) Contended `db.writer` wake stays non-hibernated, never overwrites SQLite, republishes SystemView, and enforces capacity.
    let (lrx1, rtx1, h1) = hold_writer(&db2);
    lrx1.await.unwrap();
    let jita = reg2.get_or_spawn(jita_sys);
    assert!(!jita.is_hibernated(), "unhydrated DB-backed actor must not hibernate while db.writer is contended");
    let _ = jita.step_manual(1.0).await.unwrap();
    assert!(!jita.is_hibernated());
    let _ = rtx1.send(()); h1.await.unwrap();
    let _ = jita.step_manual(1.0).await.unwrap();
    let _ = jita.step_manual(1.0).await.unwrap();
    assert!(jita.active_drone(951_101).is_some() && jita.active_drone(951_102).is_some(), "SystemView must expose hydrated drones immediately");
    assert_eq!(jita.reserve_drone_launches(ReserveDroneLaunchesReq {
        ship_id: 900_001, max_drones_allowed: 2, max_bandwidth: 25.0, partial_ok: false,
        drones: vec![(1, make_space_drone(-200_501, 100_001, 900_001, jita_sys, false))],
    }).await.unwrap().rejected_error, Some("NoDroneManagementAbilitiesLeft"));

    // (b) `!db_read_safe` wake marks `needs_sqlite_hydration`, preserves persisted drones, updates hibernation & SystemView.
    let (lrx2, rtx2, h2) = hold_writer(&db2);
    lrx2.await.unwrap();
    let perim = db2.with_snapshot(|_| reg2.get_or_spawn(perim_sys));
    assert!(!perim.is_hibernated(), "!db_read_safe unhydrated actor must not hibernate");
    let _ = rtx2.send(()); h2.await.unwrap();
    let saw_persisted = perim.mutate_active_drones(move |drones, _, _| {
        let had = drones.contains_key(&951_201);
        drones.insert(951_202, make_space_drone(951_202, 100_002, 900_002, perim_sys, true));
        (true, had)
    }).await.unwrap();
    assert!(saw_persisted, "command after !db_read_safe wake must hydrate SQLite first without overwriting");
    assert!(perim.active_drone(951_201).is_some() && perim.active_drone(951_202).is_some() && reg2.stop_system(perim_sys).await);
    let saved_perim = db2.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
        c, perim_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
    )).unwrap();
    assert_eq!(saved_perim.len(), 2, "persisted drone 951_201 must survive !db_read_safe mutation");

    // Idle abandoned `!db_read_safe` system with free writer exposes SystemView and hibernates once hydrated.
    let ren = db2.with_snapshot(|_| reg2.get_or_spawn(ren_sys));
    let _ = ren.flush_condition_checkpoint().await.unwrap();
    assert!(ren.active_drone(951_301).is_some() && ren.is_hibernated());
    assert!(reg2.stop_system(jita_sys).await && reg2.stop_system(ren_sys).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_hydration_removed_drone_dirties_checkpoint_and_never_resurrects() {
    let (tmp, _guard) = temp_sqlite_db("pre_hyd_remove_6");
    let (sde, jita_sys, perim_sys) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32, 30_000_144u32);
    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let (jita, perim) = (reg1.get_or_spawn(jita_sys), reg1.get_or_spawn(perim_sys));
        jita.upsert_active_drones(vec![make_space_drone(951_401, 100_001, 900_001, jita_sys, true), make_space_drone(951_402, 100_001, 900_001, jita_sys, true)], false).await.unwrap();
        perim.upsert_active_drones(vec![make_space_drone(951_501, 100_002, 900_002, perim_sys, true), make_space_drone(951_502, 100_002, 900_002, perim_sys, true)], false).await.unwrap();
        assert!(reg1.stop_system(jita_sys).await && reg1.stop_system(perim_sys).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db2)));
    // (a) Remove ball while `db.writer` held (pre-hydration) -> hydration dirties checkpoint & SQLite purges on flush/stop.
    let (lrx, rtx, h) = hold_writer(&db2); lrx.await.unwrap();
    let jita2 = reg2.get_or_spawn(jita_sys);
    let _ = jita2.spawn_ball_with_combat(make_drone_ball(951_401), CombatState::default()).await.unwrap();
    assert!(jita2.remove_ball(951_401).await.is_some() && jita2.view().active_drones_needs_sqlite_hydration);
    let _ = rtx.send(()); h.await.unwrap();
    let _ = jita2.step_manual(1.0).await.unwrap();
    assert!(jita2.active_drone(951_401).is_none() && jita2.active_drone(951_402).is_some() && reg2.stop_system(jita_sys).await);

    // (b) Remove ball after `!db_read_safe` wake before hydration -> dirties checkpoint and flushes cleanly.
    let (lrx2, rtx2, h2) = hold_writer(&db2); lrx2.await.unwrap();
    let perim2 = db2.with_snapshot(|_| reg2.get_or_spawn(perim_sys));
    let _ = perim2.spawn_ball_with_combat(make_drone_ball(951_501), CombatState::default()).await.unwrap();
    assert!(perim2.remove_ball(951_501).await.is_some() && perim2.view().active_drones_needs_sqlite_hydration);
    let _ = rtx2.send(()); h2.await.unwrap();
    let _ = perim2.flush_condition_checkpoint().await.unwrap();
    assert!(perim2.is_hibernated() && reg2.stop_system(perim_sys).await);

    let db3 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 3"));
    let reg3 = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db3)));
    let (jita3, perim3) = (reg3.get_or_spawn(jita_sys), reg3.get_or_spawn(perim_sys));
    assert!(jita3.active_drone(951_401).is_none() && jita3.active_drone(951_402).is_some() && perim3.active_drone(951_501).is_none() && perim3.active_drone(951_502).is_some());
    let rows_j = db3.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(c, jita_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES)).unwrap();
    let rows_p = db3.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(c, perim_sys, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES)).unwrap();
    assert_eq!((rows_j.len(), rows_j[0].1.ball_id, rows_p.len(), rows_p[0].1.ball_id), (1, 951_402, 1, 951_502));
    assert!(reg3.stop_system(jita_sys).await && reg3.stop_system(perim_sys).await);
}

#[test]
fn stale_seq_active_drone_flush_does_not_overwrite_newer_sqlite_checkpoint() {
    let (tmp, _guard) = temp_sqlite_db("stale_seq_7");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite stale seq"));
    let sys_id = 30_000_142u32;
    let shared = drones::ActiveDronesSharedCtx { db_opt: Some(Arc::clone(&db)), ..Default::default() };
    let (d1, d2) = (make_space_drone(951_601, 100_001, 900_001, sys_id, true), make_space_drone(951_602, 100_001, 900_001, sys_id, true));
    assert!(!drones::write_sqlite_for_shared(&shared, sys_id, 0, &[(d1.ball_id, d1.clone())], true));
    assert!(drones::write_sqlite_for_shared(&shared, sys_id, 2, &[(d2.ball_id, d2)], true));
    assert!(!drones::write_sqlite_for_shared(&shared, sys_id, 1, &[(d1.ball_id, d1)], true));
    let rows = db.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
        c, sys_id, eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
    )).unwrap();
    assert_eq!((rows.len(), rows[0].1.ball_id), (1, 951_602));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_deferred_hydration_preserves_legacy_json_drones_until_commit() {
    let (tmp, _guard) = temp_sqlite_db("conc_leg_hyd_8");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite conc legacy"));
    let (sde, sys_id) = (Arc::new(eve_sde::SdeRegistry::with_canonical_seed()), 30_000_142u32);
    let d_leg = make_space_drone(951_701, 100_001, 900_001, sys_id, true);
    db.with_mut_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json = serde_json::json!({"active_drones_in_space": {"951701": d_leg}}).to_string());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let shared = drones::ActiveDronesSharedCtx {
        db_opt: Some(Arc::clone(&db)), migrated_opt: Some(Arc::clone(&reg.drone_rt_state.migrated_legacy_drone_systems)),
        needs_hydration: Arc::new(std::sync::atomic::AtomicBool::new(true)), ..Default::default()
    };
    let ((r1, ld1), (r2, ld2)) = (drones::read_sqlite_for_shared(&shared, sys_id, true).unwrap(), drones::read_sqlite_for_shared(&shared, sys_id, true).unwrap());
    assert_eq!((r1.len(), ld1, r2.len(), ld2), (1, true, 1, true));
    assert!(!reg.drone_rt_state.migrated_legacy_drone_systems.lock().unwrap().contains(&sys_id));
    let sys = db.with_snapshot(|_| reg.get_or_spawn(sys_id));
    let _ = sys.flush_condition_checkpoint().await.unwrap();
    assert!(sys.active_drone(951_701).is_some() && reg.drone_rt_state.migrated_legacy_drone_systems.lock().unwrap().contains(&sys_id));
    assert!(sys.remove_active_drone(951_701).await.unwrap().is_some() && reg.stop_system(sys_id).await);
    assert!(reg.get_or_spawn(sys_id).active_drone(951_701).is_none() && reg.stop_system(sys_id).await);
}
