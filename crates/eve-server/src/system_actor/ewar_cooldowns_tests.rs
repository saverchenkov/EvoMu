//! Unit tests for per-system actor EWAR state, `CooldownPersistGate`, `!db_read_safe` boot merge,
//! lifecycle transition journaling, and TiDi reactivation cooldown rebasing (`#1541`).

#![forbid(unsafe_code)]

use super::*;
use eve_db::sqlite_store::system_checkpoint::{
    ensure_system_checkpoint_schema, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
    KIND_MODULE_COOLDOWNS,
};
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, VirtualServerClock, WorldRepository};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_sde::SdeRegistry;

fn test_sde() -> Arc<SdeRegistry> {
    SdeRegistry::open_shared(&SdeRegistry::default_db_path())
        .expect("default SDE must open for ewar_cooldowns tests")
}

fn test_registry_with_db() -> (Arc<SystemRegistry>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::new_empty());
    let sde = test_sde();
    let reg = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    (reg, db, sde)
}

fn test_tidi_reg(
    base_unix: u64,
    sys_opt: Option<(u32, f64)>,
) -> (Arc<SystemRegistry>, Arc<WorldRepository>, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    let repo = WorldRepository::new_empty();
    repo.set_clock(clock.clone());
    let db = Arc::new(repo);
    let reg = Arc::new(SystemRegistry::with_sde_and_db(test_sde(), Arc::clone(&db)));
    if let Some((sys, tidi)) = sys_opt {
        reg.cluster_nodes.write().unwrap().manual_tidi_overrides.insert(sys, tidi);
    }
    (reg, db, clock)
}

fn cd_entry(now: f64, delay_s: f64, sid: u32, sim_s: f64) -> ModuleCooldownEntry {
    ModuleCooldownEntry {
        recorded_unix_f64: now,
        delay_s,
        solar_system_id: Some(sid),
        recorded_sys_sim_s: sim_s,
    }
}

async fn send_rec(
    h: &SystemHandle,
    key: (i64, i64, u32),
    now: f64,
    delay_s: f64,
) -> ModuleCooldownPersistPayload {
    let (tx, rx) = tokio::sync::oneshot::channel();
    h.cmd_sender()
        .send(SystemCommand::RecordModuleCooldown {
            ship_id: key.0,
            module_id: key.1,
            module_type_id: key.2,
            now_unix_f64: now,
            delay_s,
            tidi_factor: 1.0,
            has_tidi_history: false,
            reply: tx,
        })
        .await
        .expect("send record");
    rx.await.expect("rx").expect("record ok")
}

async fn send_rm(h: &SystemHandle, key: (i64, i64, u32)) -> Option<ModuleCooldownPersistPayload> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    h.cmd_sender()
        .send(SystemCommand::RemoveModuleCooldown { key, reply: tx })
        .await
        .expect("send remove");
    rx.await.expect("rx").expect("remove ok")
}

async fn step_n(h: &SystemHandle, count: usize) {
    for _ in 0..count {
        let _ = h.step_manual(1.0).await.unwrap();
    }
}

#[tokio::test]
async fn ewar_state_is_actor_scoped_and_cleared_on_stop_restart() {
    let (reg, _db, sde) = test_registry_with_db();
    let (jita, amarr) = (30_000_142_u32, 30_002_187_u32);
    let (h_jita, h_amarr) = (reg.get_or_spawn(jita), reg.get_or_spawn(amarr));

    let mod_state = DogmaItemState::from_sde(700_001, 526, &sde);
    let eff = ProjectedEwarEffect {
        source_module_id: 700_001,
        source_module_type_id: 526,
        charge_type_id: None,
        distance: 5_000.0,
        overloaded: false,
        effective_module: mod_state,
    };
    h_jita.upsert_projected_ewar(800_001, 900_001, eff).await.expect("upsert");
    h_jita.record_active_ewar_start(800_001, 700_001, 900_001, "webify").await.expect("start");

    assert_eq!(h_jita.projected_ewar_for(900_001).len(), 1);
    assert!(h_amarr.projected_ewar_for(900_001).is_empty());
    assert_eq!(reg.ship_ewar_projections(900_001).len(), 1);

    assert!(reg.stop_system(jita).await);
    assert!(reg.ship_ewar_projections(900_001).is_empty());

    let h_jita_restarted = reg.get_or_spawn(jita);
    assert!(h_jita_restarted.projected_ewar_for(900_001).is_empty());
    assert!(h_jita_restarted.ewar_state_snapshot().is_empty());
}

#[tokio::test]
async fn cooldowns_persist_per_system_and_reload_sim_deadlines_live_in_actor() {
    let (reg, db, _sde) = test_registry_with_db();
    let (jita, amarr) = (30_000_142_u32, 30_002_187_u32);
    let now = db.clock().now_unix_secs() as f64;
    let h_jita = reg.get_or_spawn(jita);
    h_jita.record_module_cooldown(100_001, 200_001, 11_370, now, 15.0, 1.0, false).await.unwrap();
    h_jita.record_module_reload_sim_deadlines_sync(vec![200_002], 2.0, 12.0).unwrap();

    assert!(h_jita.module_reactivation_cooldown(&(100_001, 200_001, 11_370)).is_some());
    assert_eq!(reg.check_module_reload_sim_deadline(200_002), Some((jita, 2.0, 12.0)));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).contains_key(&(100_001, 200_001, 11_370)));
    assert!(load_cold_system_module_cooldowns_from_db(&db, amarr).is_empty());
    assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());

    assert!(reg.stop_system(jita).await);
    assert!(reg.check_module_reload_sim_deadline(200_002).is_none());
    let h_jita_restarted = reg.get_or_spawn(jita);
    assert!(h_jita_restarted.module_reactivation_cooldown(&(100_001, 200_001, 11_370)).is_some());
}

#[test]
fn legacy_cluster_cooldown_checkpoint_migrates_one_way_to_system() {
    let (reg, db, _sde) = test_registry_with_db();
    let (ship_id, mod_id, mod_tid, sys_id) = (300_001_i64, 300_002_i64, 11_370_u32, 30_000_142_u32);
    let now = db.clock().now_unix_secs() as f64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: 90_000_001, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let entry = ModuleCooldownEntry { recorded_unix_f64: now, delay_s: 20.0, solar_system_id: None, recorded_sys_sim_s: 0.0 };
    let row_key = module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid);
    db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        replace_system_checkpoints_for_kind(
            conn, CLUSTER_SYSTEM_ID, KIND_MODULE_COOLDOWNS, &[(row_key, ((ship_id, mod_id, mod_tid), entry))], now as u64,
        )
    }).unwrap();

    migrate_and_prune_module_cooldowns_in_db(&reg, &db, now);
    assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());
    let migrated = load_cold_system_module_cooldowns_from_db(&db, sys_id);
    assert_eq!(migrated.get(&(ship_id, mod_id, mod_tid)).expect("migrated").solar_system_id, Some(sys_id));
}

#[tokio::test]
async fn cooldown_persist_gate_rejects_stale_out_of_order_writes_and_keeps_dirty_until_commit() {
    let (reg, db, _sde) = test_registry_with_db();
    let (jita, now, key) = (30_000_142_u32, db.clock().now_unix_secs() as f64, (500_001_i64, 600_001_i64, 11_370_u32));
    let h_jita = reg.get_or_spawn(jita);

    let payload_seq1 = send_rec(&h_jita, key, now, 30.0).await;
    let (snap_tx, snap_rx) = tokio::sync::oneshot::channel();
    h_jita.cmd_sender().send(SystemCommand::SnapshotModuleCooldownCheckpoint { only_if_dirty: true, reply: snap_tx }).await.unwrap();
    let snap_payload = snap_rx.await.unwrap().unwrap().expect("still dirty before persist");
    assert_eq!(snap_payload.seq, payload_seq1.seq);
    assert!(h_jita.actor_state.read().unwrap().module_cooldown_checkpoint_dirty);

    let payload_seq2 = send_rm(&h_jita, key).await.expect("had entry");
    assert!(payload_seq2.seq > payload_seq1.seq);
    assert!(persist_cooldown_payload_with_gate(jita, &h_jita.cooldown_persist_gate, None, Some(&h_jita.actor_state), payload_seq2));
    assert!(!h_jita.actor_state.read().unwrap().module_cooldown_checkpoint_dirty);
    assert!(!persist_cooldown_payload_with_gate(jita, &h_jita.cooldown_persist_gate, None, Some(&h_jita.actor_state), payload_seq1));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());
}

#[test]
fn boot_with_db_read_unsafe_merges_existing_cold_cooldowns_without_wiping() {
    let (reg, db, _sde) = test_registry_with_db();
    let (jita, now) = (30_000_142_u32, db.clock().now_unix_secs() as f64);
    let (k_pre, k_new) = ((510_001_i64, 610_001_i64, 11_370_u32), (510_002_i64, 610_002_i64, 11_370_u32));

    reg.seed_cold_system_module_cooldown_in_db(&db, jita, k_pre, cd_entry(now, 25.0, jita, 0.0));
    let gate = reg.cooldown_persist_gate(jita);
    let actor_state = Arc::new(RankedRwLock::new(SystemActorState {
        db_opt: Some(Arc::clone(&db)),
        module_cooldown_persist_epoch: gate.bump_epoch(),
        module_cooldowns: ActorModuleCooldownState { boot_db_pending_merge: true, ..Default::default() },
        ..Default::default()
    }));
    let payload = record_module_cooldown_in_actor_state(jita, &mut actor_state.write().unwrap(), k_new.0, k_new.1, k_new.2, now, 15.0, 1.0, false).expect("payload generated");
    assert!(persist_cooldown_payload_with_gate(jita, &gate, None, Some(&actor_state), payload));

    let db_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
    assert!(db_rows.contains_key(&k_pre) && db_rows.contains_key(&k_new));
    let st = actor_state.read().unwrap();
    assert!(st.module_cooldowns.reactivation_cooldowns.contains_key(&k_pre) && st.module_cooldowns.reactivation_cooldowns.contains_key(&k_new));
    assert!(!st.module_cooldowns.boot_db_pending_merge && !st.module_cooldown_checkpoint_dirty);
}

#[tokio::test]
async fn tidi_boot_hydration_and_eviction_reactivation_rebase_sim_clock() {
    let (jita, key) = (30_000_142_u32, (520_001_i64, 620_001_i64, 11_370_u32));
    let (reg, db, clock) = test_tidi_reg(1_750_000_000, Some((jita, 0.25)));

    let h1 = reg.get_or_spawn(jita);
    assert!((h1.tidi_factor() - 0.25).abs() < 1e-6);
    step_n(&h1, 200).await;
    assert!((h1.sim_elapsed_secs() - 50.0).abs() < 1e-6);
    h1.record_module_cooldown(key.0, key.1, key.2, clock.now_unix_secs() as f64, 30.0, 0.25, true).await.unwrap();
    step_n(&h1, 20).await;
    assert!((h1.sim_elapsed_secs() - 55.0).abs() < 1e-6);
    drop(h1);

    assert!(reg.stop_system(jita).await);
    let rebased = load_cold_system_module_cooldowns_from_db(&db, jita).get(&key).copied().expect("rebased row persisted on stop");
    assert!((rebased.recorded_sys_sim_s - 0.0).abs() < 1e-6 && (rebased.delay_s - 25.0).abs() < 1e-6);

    clock.advance_secs(20);
    let h2 = reg.get_or_spawn(jita);
    let booted = h2.module_reactivation_cooldown(&key).expect("cooldown hydrated at TiDi boot");
    assert!((booted.recorded_sys_sim_s - 0.0).abs() < 1e-6 && (booted.delay_s - 20.0).abs() < 1e-6);

    step_n(&h2, 16).await;
    let rem = remaining_cooldown_secs_with_tidi(
        &h2.module_reactivation_cooldown(&key).expect("still active"),
        clock.now_unix_secs() as f64, Some(jita), h2.sim_elapsed_secs(), 0.25, true,
    );
    assert!((rem - 16.0).abs() < 1e-6, "expected 16s remaining, got {rem}");
    step_n(&h2, 64).await;
    assert!(h2.module_reactivation_cooldown(&key).is_none());
}

#[tokio::test]
async fn tidi_crash_recovery_unrebased_sim_clock_scales_and_advances_after_boot() {
    let (jita, key, base_unix) = (30_000_142_u32, (530_001_i64, 630_001_i64, 11_370_u32), 1_750_100_000_u64);
    let (reg, db, clock) = test_tidi_reg(base_unix, Some((jita, 0.25)));
    reg.seed_cold_system_module_cooldown_in_db(&db, jita, key, cd_entry(base_unix as f64, 30.0, jita, 100.0));
    clock.advance_secs(40);
    let h = reg.get_or_spawn(jita);
    let entry = h.module_reactivation_cooldown(&key).expect("crash-recovered TiDi entry loaded");
    assert!((entry.delay_s - 20.0).abs() < 1e-6 && (entry.recorded_sys_sim_s - 0.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_db_pending_merge_handle_view_same_key_no_resurrection_and_cold_epoch_safety() {
    let (reg, db, _sde) = test_registry_with_db();
    let jita = 30_000_142_u32;
    let now = db.clock().now_unix_secs() as f64;
    let (k_rm, k_db, k_un) = (
        (520_001_i64, 620_001_i64, 11_370_u32),
        (520_002_i64, 620_002_i64, 11_370_u32),
        (520_003_i64, 620_003_i64, 11_370_u32),
    );
    let h = reg.get_or_spawn(jita);
    let cold_map = HashMap::from([
        (k_rm, cd_entry(now, 30.0, jita, 0.0)),
        (k_db, cd_entry(now, 30.0, jita, 0.0)),
        (k_un, cd_entry(now, 25.0, jita, 0.0)),
    ]);
    assert!(persist_system_module_cooldowns_in_db(&db, jita, &serialize_module_cooldown_rows(&cold_map)));
    h.actor_state.write().unwrap().module_cooldowns.boot_db_pending_merge = true;
    h.publish_view();
    assert!(h.view().module_cooldowns.boot_db_pending_merge);

    assert!(!reg.remove_cold_module_cooldown_key_across_systems(&db, None, k_un));
    reg.clear_cold_ship_module_cooldowns_in_db(&db, k_un.0);
    reg.clear_all_cold_module_cooldowns_in_db(&db);
    assert_eq!(load_cold_system_module_cooldowns_from_db(&db, jita).len(), 3);

    let _ = send_rm(&h, k_rm).await;
    let _ = send_rec(&h, k_rm, now, 5.0).await;
    let payload = send_rec(&h, k_db, now, 10.0).await;
    assert!(h.persist_cooldown_payload(payload));

    let view = h.view();
    assert!(!view.module_cooldowns.boot_db_pending_merge);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_rm].delay_s - 5.0).abs() < 1e-6);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_db].delay_s - 30.0).abs() < 1e-6);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_un].delay_s - 25.0).abs() < 1e-6);
    let db_after = load_cold_system_module_cooldowns_from_db(&db, jita);
    assert!((db_after[&k_rm].delay_s - 5.0).abs() < 1e-6);
    assert!((db_after[&k_db].delay_s - 30.0).abs() < 1e-6);
    assert!((db_after[&k_un].delay_s - 25.0).abs() < 1e-6);

    h.clear_all_module_cooldowns_sync().unwrap();
    assert!(!h.actor_state.read().unwrap().module_cooldowns.pending_db_clear_all);
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());

    let k_after = (520_004_i64, 620_004_i64, 11_370_u32);
    h.record_module_cooldown(k_after.0, k_after.1, k_after.2, now, 12.0, 1.0, false).await.unwrap();
    assert!((load_cold_system_module_cooldowns_from_db(&db, jita)[&k_after].delay_s - 12.0).abs() < 1e-6);
}

#[tokio::test]
async fn live_set_tidi_factor_raw_stop_rebase_at_zero_and_nonzero_sim_clock() {
    let (jita, amarr) = (30_000_142_u32, 30_002_187_u32);
    let (k_zero_sim, k_cleared) = (
        (540_001_i64, 640_001_i64, 11_370_u32),
        (540_002_i64, 640_002_i64, 11_370_u32),
    );
    let (reg, db, clock) = test_tidi_reg(1_750_200_000, None);

    // Case A (`G1A-R3-P1-01`): live `set_tidi_factor_raw(0.25)` + stop at `cur_sys_sim_s == 0.0`.
    let h1 = reg.get_or_spawn(jita);
    let _ = h1.set_tidi_factor_raw(0.25);
    h1.record_module_cooldown(k_zero_sim.0, k_zero_sim.1, k_zero_sim.2, clock.now_unix_secs() as f64, 30.0, 0.25, true)
        .await
        .unwrap();
    drop(h1);
    clock.advance_secs(20); // 20s wall * 0.25 TiDi = 5s elapsed -> 25.0s left.
    assert!(reg.stop_system(jita).await);
    let r1 = load_cold_system_module_cooldowns_from_db(&db, jita)[&k_zero_sim];
    assert!((r1.delay_s - 25.0).abs() < 1e-6 && r1.recorded_sys_sim_s.abs() < 1e-6);

    // Case B (`G1A-R3-P1-01`): live `set_tidi_factor_raw(0.25)` -> step 5s sim -> reset `1.0` -> stop.
    let h2 = reg.get_or_spawn(amarr);
    let _ = h2.set_tidi_factor_raw(0.25);
    h2.record_module_cooldown(k_cleared.0, k_cleared.1, k_cleared.2, clock.now_unix_secs() as f64, 30.0, 0.25, true)
        .await
        .unwrap();
    step_n(&h2, 20).await;
    assert!((h2.sim_elapsed_secs() - 5.0).abs() < 1e-6);
    let _ = h2.set_tidi_factor_raw(1.0);
    drop(h2);
    clock.advance_secs(40); // If `has_tidi_history` dropped, wall=40s would delete the 30s entry!
    assert!(reg.stop_system(amarr).await);
    let r2 = load_cold_system_module_cooldowns_from_db(&db, amarr)[&k_cleared];
    assert!((r2.delay_s - 25.0).abs() < 1e-6 && r2.recorded_sys_sim_s.abs() < 1e-6);
}

#[test]
fn resolve_effective_tidi_mode_under_held_svc_and_world_ranks() {
    let (reg, db, _clk) = test_tidi_reg(1_750_300_000, Some((30_000_142, 0.25)));
    reg.cooldown_persist_gate(30_000_142).mark_tidi_history();
    let _svc_guard = reg.cluster_nodes.read().unwrap();
    let (tf, hist) = db.with_snapshot(|_| reg.resolve_effective_tidi_mode(30_000_142, false));
    assert!((tf - 0.25).abs() < 1e-6 && hist);
}

#[tokio::test]
async fn tidi_history_synced_from_gate_before_step_prune_after_reset_to_one() {
    let (jita, key) = (30_000_142_u32, (541_001_i64, 641_001_i64, 11_370_u32));
    let (reg, _db, clock) = test_tidi_reg(1_750_310_000, None);
    let h = reg.get_or_spawn(jita);
    let _ = h.set_tidi_factor_raw(0.25);
    h.record_module_cooldown(key.0, key.1, key.2, clock.now_unix_secs() as f64, 30.0, 0.25, false).await.unwrap();
    clock.advance_secs(40); // If gate tidi history weren't synced prior to step prune, wall=40>30 deletes `key`.
    let _ = h.set_tidi_factor_raw(1.0);
    step_n(&h, 1).await;
    assert!(h.module_reactivation_cooldown(&key).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rehydrate_active_cooldowns_on_ticked_actor_and_migration_max_merge_prune() {
    let (jita, amarr, cloak_tid) = (30_000_142_u32, 30_002_187_u32, 11_370_u32);
    let (s_live, s_cold, s_exp, m_live, m_cold, m_exp) = (542_001, 542_002, 542_003, 642_001, 642_002, 642_003);
    let (reg, db, clock) = test_tidi_reg(1_750_320_000, None);
    let now = clock.now_unix_secs() as f64;
    db.with_mut_snapshot(|snap| {
        for (s, sys) in [(s_live, jita), (s_cold, amarr), (s_exp, jita)] {
            snap.items.insert(s, InventoryItemRecord {
                item_id: s, type_id: 587, owner_id: 90_000_001, location_id: sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let h = reg.get_or_spawn(jita);
    step_n(&h, 25).await;
    assert!((h.sim_elapsed_secs() - 25.0).abs() < 1e-6);
    reg.seed_cold_system_module_cooldown_in_db(&db, jita, (s_live, m_live, cloak_tid), cd_entry(now, 12.0, jita, 0.0));
    reg.rehydrate_active_system_cooldowns_from_db();
    let rehyd = h.module_reactivation_cooldown(&(s_live, m_live, cloak_tid)).expect("anchored at live sim clock");
    assert!((rehyd.recorded_sys_sim_s - 25.0).abs() < 1e-6 && (rehyd.delay_s - 12.0).abs() < 1e-6);

    h.record_module_cooldown(s_live, m_live, cloak_tid, now, 35.0, 1.0, false).await.unwrap();
    reg.seed_cold_system_module_cooldown_in_db(&db, amarr, (s_cold, m_cold, cloak_tid), cd_entry(now, 40.0, amarr, 0.0));
    let cluster_rows = [
        (module_cooldown_checkpoint_key(s_live, m_live, cloak_tid), ((s_live, m_live, cloak_tid), cd_entry(now, 10.0, CLUSTER_SYSTEM_ID, 0.0))),
        (module_cooldown_checkpoint_key(s_cold, m_cold, cloak_tid), ((s_cold, m_cold, cloak_tid), cd_entry(now, 10.0, CLUSTER_SYSTEM_ID, 0.0))),
        (module_cooldown_checkpoint_key(s_exp, m_exp, cloak_tid), ((s_exp, m_exp, cloak_tid), cd_entry(now - 50.0, 5.0, jita, 0.0))),
    ];
    db.with_sqlite_conn(|c| {
        ensure_system_checkpoint_schema(c)?;
        replace_system_checkpoints_for_kind(c, CLUSTER_SYSTEM_ID, KIND_MODULE_COOLDOWNS, &cluster_rows[..2], now as u64)?;
        replace_system_checkpoints_for_kind(c, jita, KIND_MODULE_COOLDOWNS, &cluster_rows[2..], now as u64)
    }).unwrap();
    migrate_and_prune_module_cooldowns_in_db(&reg, &db, now);
    assert!(h.module_reactivation_cooldown(&(s_live, m_live, cloak_tid)).is_some_and(|e| e.delay_s >= 34.0));
    assert!(load_cold_system_module_cooldowns_from_db(&db, amarr)[&(s_cold, m_cold, cloak_tid)].delay_s >= 39.0);
    assert!(!load_cold_system_module_cooldowns_from_db(&db, jita).contains_key(&(s_exp, m_exp, cloak_tid)));
}

#[test]
fn unresolved_cluster_cooldown_rows_are_removed_cleared_and_purged() {
    let (reg, db, _sde) = test_registry_with_db();
    let now = db.clock().now_unix_secs() as f64;
    let (k1, k2, k3) = ((543_001, 643_001, 11_370), (543_001, 643_002, 11_370), (543_002, 643_003, 11_370));
    let rows: Vec<_> = [k1, k2, k3].into_iter()
        .map(|k| (module_cooldown_checkpoint_key(k.0, k.1, k.2), (k, ModuleCooldownEntry { recorded_unix_f64: now, delay_s: 30.0, solar_system_id: None, recorded_sys_sim_s: 0.0 })))
        .collect();
    db.with_sqlite_conn(|c| { ensure_system_checkpoint_schema(c)?; replace_system_checkpoints_for_kind(c, CLUSTER_SYSTEM_ID, KIND_MODULE_COOLDOWNS, &rows, now as u64) }).unwrap();
    assert!(reg.remove_cold_module_cooldown_key_across_systems(&db, None, k1));
    assert_eq!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).len(), 2);
    reg.clear_cold_ship_module_cooldowns_in_db(&db, k2.0);
    assert_eq!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).len(), 1);
    reg.clear_all_cold_module_cooldowns_in_db(&db);
    assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());
}

#[tokio::test]
async fn stopping_to_cold_with_empty_pending_bumps_epoch_and_rejects_stale_persist() {
    let (reg, db, _sde) = test_registry_with_db();
    let (jita, now, k) = (30_000_142_u32, db.clock().now_unix_secs() as f64, (544_001, 644_001, 11_370));
    let h = reg.get_or_spawn(jita);
    let stale_payload = send_rec(&h, k, now, 30.0).await;
    h.replace_module_cooldowns_sync(HashMap::new()).unwrap();
    drop(h);
    assert!(reg.stop_system(jita).await);
    assert!(!persist_cooldown_payload_with_gate(jita, &reg.cooldown_persist_gate(jita), None, None, stale_payload));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());
}

fn seed_ships_in_db(db: &WorldRepository, sys: u32, ship_ids: &[i64]) {
    db.with_mut_snapshot(|snap| {
        for &s in ship_ids {
            snap.items.insert(s, InventoryItemRecord {
                item_id: s, type_id: 587, owner_id: 90_000_001, location_id: sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncommitted_live_cooldown_and_in_flight_post_unlock_survive_rehydrate() {
    let (reg, db, _sde) = test_registry_with_db();
    let (jita, cloak_tid, now) = (30_000_142_u32, 11_370_u32, db.clock().now_unix_secs() as f64);
    let (k_live, k_stale) = ((545_001_i64, 645_001_i64, cloak_tid), (545_002_i64, 645_002_i64, cloak_tid));
    seed_ships_in_db(&db, jita, &[k_live.0, k_stale.0]);
    let h = reg.get_or_spawn(jita);
    reg.seed_cold_system_module_cooldown_in_db(&db, jita, k_stale, cd_entry(now, 10.0, jita, 0.0));
    let _unsent = send_rec(&h, k_live, now, 35.0).await;
    assert!(h.actor_state.read().unwrap().module_cooldown_checkpoint_dirty);

    let stale_token = {
        let gate = reg.cooldown_persist_gate(jita);
        let cg = gate.inner.lock().unwrap();
        (gate.current_epoch(), cg.last_persisted)
    };
    assert_eq!(h.replace_module_cooldowns_checked_sync(HashMap::new(), Some(stale_token)), Err("STALE_REHYDRATE_TOKEN".to_string()));
    assert!(h.module_reactivation_cooldown(&k_live).is_some());

    let in_flight = Arc::clone(&h.post_unlock_in_flight);
    in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let release = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    });
    let reg_c = Arc::clone(&reg);
    tokio::task::spawn_blocking(move || reg_c.rehydrate_active_system_cooldowns_from_db()).await.unwrap();
    release.await.unwrap();

    assert!(h.module_reactivation_cooldown(&k_live).is_some_and(|e| e.delay_s >= 34.0));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).contains_key(&k_live));
    assert!(!h.actor_state.read().unwrap().module_cooldown_checkpoint_dirty);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_system_migration_prunes_stale_sqlite_rows_when_cluster_migrated_is_empty() {
    let (reg, db, clock) = test_tidi_reg(1_750_330_000, None);
    let (jita, cloak_tid, now) = (30_000_142_u32, 11_370_u32, clock.now_unix_secs() as f64);
    let (s_live, s_exp, m_live, m_exp) = (546_001_i64, 546_002_i64, 646_001_i64, 646_002_i64);
    seed_ships_in_db(&db, jita, &[s_live, s_exp]);
    let h = reg.get_or_spawn(jita);
    let live_entry = ModuleCooldownEntry { recorded_unix_f64: now, delay_s: 30.0, solar_system_id: None, recorded_sys_sim_s: 0.0 };
    h.replace_module_cooldowns_sync(HashMap::from([((s_live, m_live, cloak_tid), live_entry)])).unwrap();
    let stale_rows = vec![
        (module_cooldown_checkpoint_key(s_live, m_live, cloak_tid), ((s_live, m_live, cloak_tid), live_entry)),
        (module_cooldown_checkpoint_key(s_exp, m_exp, cloak_tid), ((s_exp, m_exp, cloak_tid), cd_entry(now - 50.0, 5.0, jita, 0.0))),
    ];
    db.with_sqlite_conn(|c| {
        ensure_system_checkpoint_schema(c)?;
        replace_system_checkpoints_for_kind(c, jita, KIND_MODULE_COOLDOWNS, &stale_rows, now as u64)
    }).unwrap();
    assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());

    migrate_and_prune_module_cooldowns_in_db(&reg, &db, now);

    let after = load_cold_system_module_cooldowns_from_db(&db, jita);
    assert!(!after.contains_key(&(s_exp, m_exp, cloak_tid)));
    assert_eq!(after.get(&(s_live, m_live, cloak_tid)).and_then(|e| e.solar_system_id), Some(jita));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rehydrate_clean_seq_zero_actor_after_stop_restart_preserves_seeded_sqlite_cooldown() {
    let (reg, db, clock) = test_tidi_reg(1_750_340_000, None);
    let (jita, cloak_tid, now) = (30_000_142_u32, 11_370_u32, clock.now_unix_secs() as f64);
    let k = (547_001_i64, 647_001_i64, cloak_tid);
    seed_ships_in_db(&db, jita, &[k.0]);
    drop(reg.get_or_spawn(jita));
    assert!(reg.stop_system(jita).await);
    let h = reg.get_or_spawn(jita);
    assert_eq!(h.actor_state.read().unwrap().module_cooldown_checkpoint_seq, 0);
    reg.seed_cold_system_module_cooldown_in_db(&db, jita, k, cd_entry(now, 25.0, jita, 0.0));
    let r = Arc::clone(&reg);
    tokio::task::spawn_blocking(move || r.rehydrate_active_system_cooldowns_from_db()).await.unwrap();
    assert!(h.module_reactivation_cooldown(&k).is_some_and(|e| e.delay_s >= 24.0));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).contains_key(&k));
}
