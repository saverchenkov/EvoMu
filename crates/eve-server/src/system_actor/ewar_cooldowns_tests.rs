//! Unit tests for per-system actor EWAR state, `CooldownPersistGate`, `!db_read_safe` boot merge,
//! and TiDi module reactivation cooldown rebasing (`#1541`).

#![forbid(unsafe_code)]

use super::*;
use eve_db::sqlite_store::system_checkpoint::{
    ensure_system_checkpoint_schema, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
    KIND_MODULE_COOLDOWNS,
};
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, WorldRepository};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_sde::SdeRegistry;

fn test_sde() -> Arc<SdeRegistry> {
    SdeRegistry::open_shared(&SdeRegistry::default_db_path())
        .expect("default SDE must open for ewar_cooldowns tests")
}

fn test_registry_with_db() -> (Arc<SystemRegistry>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::new_empty());
    let sde = test_sde();
    let reg = SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db));
    (Arc::new(reg), db, sde)
}

#[tokio::test]
async fn ewar_state_is_actor_scoped_and_cleared_on_stop_restart() {
    let (reg, _db, sde) = test_registry_with_db();
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let h_jita = reg.get_or_spawn(jita);
    let h_amarr = reg.get_or_spawn(amarr);

    let mod_state = DogmaItemState::from_sde(700_001, 526, &sde);
    h_jita
        .upsert_projected_ewar(
            800_001,
            900_001,
            ProjectedEwarEffect {
                source_module_id: 700_001,
                source_module_type_id: 526,
                charge_type_id: None,
                distance: 5_000.0,
                overloaded: false,
                effective_module: mod_state,
            },
        )
        .await
        .expect("upsert projected EWAR");
    h_jita
        .record_active_ewar_start(800_001, 700_001, 900_001, "webify")
        .await
        .expect("record active EWAR start");

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
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let now_unix = db.clock().now_unix_secs() as f64;

    let h_jita = reg.get_or_spawn(jita);
    h_jita
        .record_module_cooldown(100_001, 200_001, 11_370, now_unix, 15.0, 1.0, false)
        .await
        .expect("record cooldown");
    h_jita
        .record_module_reload_sim_deadlines_sync(vec![200_002], 2.0, 12.0)
        .expect("record reload deadline");

    assert!(h_jita
        .module_reactivation_cooldown(&(100_001, 200_001, 11_370))
        .is_some());
    assert_eq!(
        reg.check_module_reload_sim_deadline(200_002),
        Some((jita, 2.0, 12.0))
    );

    let jita_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
    let amarr_rows = load_cold_system_module_cooldowns_from_db(&db, amarr);
    let cluster_rows = load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID);
    assert!(jita_rows.contains_key(&(100_001, 200_001, 11_370)));
    assert!(amarr_rows.is_empty());
    assert!(cluster_rows.is_empty());

    assert!(reg.stop_system(jita).await);
    assert!(reg.check_module_reload_sim_deadline(200_002).is_none());

    let h_jita_restarted = reg.get_or_spawn(jita);
    assert!(h_jita_restarted
        .module_reactivation_cooldown(&(100_001, 200_001, 11_370))
        .is_some());
}

#[test]
fn legacy_cluster_cooldown_checkpoint_migrates_one_way_to_system() {
    let (_reg, db, _sde) = test_registry_with_db();
    let ship_id = 300_001_i64;
    let mod_id = 300_002_i64;
    let mod_tid = 11_370_u32;
    let sys_id = 30_000_142_u32;
    let now_unix = db.clock().now_unix_secs() as f64;

    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let entry = ModuleCooldownEntry {
        recorded_unix_f64: now_unix,
        delay_s: 20.0,
        solar_system_id: None,
        recorded_sys_sim_s: 0.0,
    };
    let row_key = module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid);
    db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        replace_system_checkpoints_for_kind(
            conn,
            CLUSTER_SYSTEM_ID,
            KIND_MODULE_COOLDOWNS,
            &[(row_key, ((ship_id, mod_id, mod_tid), entry))],
            now_unix as u64,
        )
    })
    .expect("seed legacy cluster checkpoint");

    migrate_and_prune_module_cooldowns_in_db(&db, now_unix, |_| (1.0, false));

    assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());
    let migrated = load_cold_system_module_cooldowns_from_db(&db, sys_id);
    let migrated_entry = migrated
        .get(&(ship_id, mod_id, mod_tid))
        .expect("migrated into ship system");
    assert_eq!(migrated_entry.solar_system_id, Some(sys_id));
}

#[tokio::test]
async fn cooldown_persist_gate_rejects_stale_out_of_order_writes_and_keeps_dirty_until_commit() {
    let (reg, db, _sde) = test_registry_with_db();
    let jita = 30_000_142_u32;
    let now_unix = db.clock().now_unix_secs() as f64;
    let h_jita = reg.get_or_spawn(jita);
    let key = (500_001_i64, 600_001_i64, 11_370_u32);

    let (tx1, rx1) = tokio::sync::oneshot::channel();
    h_jita
        .cmd_sender()
        .send(SystemCommand::RecordModuleCooldown {
            ship_id: key.0,
            module_id: key.1,
            module_type_id: key.2,
            now_unix_f64: now_unix,
            delay_s: 30.0,
            tidi_factor: 1.0,
            has_tidi_history: false,
            reply: tx1,
        })
        .await
        .expect("send record");
    let payload_seq1 = rx1.await.expect("rx1").expect("record ok");

    // Snapshot while dirty MUST NOT clear `module_cooldown_checkpoint_dirty` before DB commit.
    let (snap_tx, snap_rx) = tokio::sync::oneshot::channel();
    h_jita
        .cmd_sender()
        .send(SystemCommand::SnapshotModuleCooldownCheckpoint {
            only_if_dirty: true,
            reply: snap_tx,
        })
        .await
        .expect("send snapshot");
    let snap_payload = snap_rx
        .await
        .expect("snap_rx")
        .expect("snap ok")
        .expect("still dirty before persist");
    assert_eq!(snap_payload.seq, payload_seq1.seq);
    assert!(
        h_jita
            .actor_state
            .read()
            .unwrap()
            .module_cooldown_checkpoint_dirty,
        "dirty flag must stay set until DB write commits"
    );

    // Now remove cooldown (higher seq = 2) and commit seq = 2 FIRST.
    let (tx2, rx2) = tokio::sync::oneshot::channel();
    h_jita
        .cmd_sender()
        .send(SystemCommand::RemoveModuleCooldown { key, reply: tx2 })
        .await
        .expect("send remove");
    let payload_seq2 = rx2.await.expect("rx2").expect("remove ok").expect("had entry");
    assert!(payload_seq2.seq > payload_seq1.seq);
    assert!(persist_cooldown_payload_with_gate(
        jita,
        &h_jita.cooldown_persist_gate,
        None,
        Some(&h_jita.actor_state),
        payload_seq2,
    ));
    assert!(
        !h_jita
            .actor_state
            .read()
            .unwrap()
            .module_cooldown_checkpoint_dirty
    );

    // Delayed out-of-order writer carrying seq = 1 MUST be rejected and NOT resurrect `key`.
    assert!(!persist_cooldown_payload_with_gate(
        jita,
        &h_jita.cooldown_persist_gate,
        None,
        Some(&h_jita.actor_state),
        payload_seq1,
    ));
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());
}

#[test]
fn boot_with_db_read_unsafe_merges_existing_cold_cooldowns_without_wiping() {
    let (reg, db, _sde) = test_registry_with_db();
    let jita = 30_000_142_u32;
    let now_unix = db.clock().now_unix_secs() as f64;
    let k_pre = (510_001_i64, 610_001_i64, 11_370_u32);
    let k_new = (510_002_i64, 610_002_i64, 11_370_u32);

    // Seed pre-existing cold checkpoint C1 in SQLite.
    reg.seed_cold_system_module_cooldown_in_db(
        &db,
        jita,
        k_pre,
        ModuleCooldownEntry {
            recorded_unix_f64: now_unix,
            delay_s: 25.0,
            solar_system_id: Some(jita),
            recorded_sys_sim_s: 0.0,
        },
    );

    // Simulate `!db_read_safe` boot state: in-memory map starts empty with `boot_db_pending_merge`.
    let gate = reg.cooldown_persist_gate(jita);
    let persist_epoch = gate.bump_epoch();
    let actor_state = Arc::new(RankedRwLock::new(SystemActorState {
        db_opt: Some(Arc::clone(&db)),
        module_cooldown_persist_epoch: persist_epoch,
        module_cooldowns: ActorModuleCooldownState {
            boot_db_pending_merge: true,
            ..Default::default()
        },
        ..Default::default()
    }));

    let payload = {
        let mut st = actor_state.write().unwrap();
        record_module_cooldown_in_actor_state(
            jita, &mut st, k_new.0, k_new.1, k_new.2, now_unix, 15.0, 1.0, false,
        )
        .expect("payload generated")
    };
    assert!(persist_cooldown_payload_with_gate(
        jita,
        &gate,
        None,
        Some(&actor_state),
        payload,
    ));

    // Both C1 (pre-existing cold) and C2 (newly recorded) exist in SQLite and live actor state!
    let db_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
    assert!(db_rows.contains_key(&k_pre));
    assert!(db_rows.contains_key(&k_new));
    let st_read = actor_state.read().unwrap();
    assert!(st_read.module_cooldowns.reactivation_cooldowns.contains_key(&k_pre));
    assert!(st_read.module_cooldowns.reactivation_cooldowns.contains_key(&k_new));
    assert!(!st_read.module_cooldowns.boot_db_pending_merge);
    assert!(!st_read.module_cooldown_checkpoint_dirty);
}

#[tokio::test]
async fn tidi_boot_hydration_and_eviction_reactivation_rebase_sim_clock() {
    let base_unix = 1_750_000_000_u64;
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
    let repo = WorldRepository::new_empty();
    repo.set_clock(clock.clone());
    let db = Arc::new(repo);
    let sde = test_sde();
    let jita = 30_000_142_u32;
    let key = (520_001_i64, 620_001_i64, 11_370_u32);

    let reg = Arc::new(SystemRegistry::with_sde_and_db(
        Arc::clone(&sde),
        Arc::clone(&db),
    ));
    reg.cluster_nodes
        .write()
        .unwrap()
        .manual_tidi_overrides
        .insert(jita, 0.25);

    let h1 = reg.get_or_spawn(jita);
    assert!((h1.tidi_factor() - 0.25).abs() < 1e-6);
    // Advance 200 wall seconds * 0.25 TiDi = 50 sim seconds.
    for _ in 0..200 {
        let _ = h1.step_manual(1.0).await.unwrap();
    }
    assert!((h1.sim_elapsed_secs() - 50.0).abs() < 1e-6);

    // Record 30s cooldown at sim = 50.0, then step 20 wall seconds = 5.0 sim seconds -> 25s rem.
    h1.record_module_cooldown(
        key.0,
        key.1,
        key.2,
        clock.now_unix_secs() as f64,
        30.0,
        0.25,
        true,
    )
    .await
    .expect("record cooldown");
    for _ in 0..20 {
        let _ = h1.step_manual(1.0).await.unwrap();
    }
    assert!((h1.sim_elapsed_secs() - 55.0).abs() < 1e-6);
    drop(h1);

    // Evict / stop actor cleanly -> must rebase persisted entry to (now, 25.0s, 0.0).
    assert!(reg.stop_system(jita).await);
    let cold_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
    let rebased = cold_rows.get(&key).expect("rebased row persisted on stop");
    assert!((rebased.recorded_sys_sim_s - 0.0).abs() < 1e-6);
    assert!((rebased.delay_s - 25.0).abs() < 1e-6);

    // Advance 20 wall seconds while cold under 0.25 TiDi (= 5.0 sim seconds elapsed -> 20.0s left).
    clock.advance_secs(20);

    // Reboot actor with `manual_tidi_overrides = 0.25`: initial_tidi MUST hydrate BEFORE
    // loading cooldowns and rebase again cleanly so sim stepping immediately advances it!
    let h2 = reg.get_or_spawn(jita);
    let booted_entry = h2
        .module_reactivation_cooldown(&key)
        .expect("cooldown hydrated at TiDi boot");
    assert!((booted_entry.recorded_sys_sim_s - 0.0).abs() < 1e-6);
    assert!((booted_entry.delay_s - 20.0).abs() < 1e-6);

    // Step 16 wall seconds * 0.25 = 4.0 sim seconds without advancing wall clock -> 16.0s left.
    for _ in 0..16 {
        let _ = h2.step_manual(1.0).await.unwrap();
    }
    let rem_after_step = remaining_cooldown_secs_with_tidi(
        &h2.module_reactivation_cooldown(&key).expect("still active"),
        clock.now_unix_secs() as f64,
        Some(jita),
        h2.sim_elapsed_secs(),
        0.25,
        true,
    );
    assert!(
        (rem_after_step - 16.0).abs() < 1e-6,
        "rebased TiDi cooldown must continue ticking on rebooted sim clock; got {rem_after_step}"
    );

    // Step 64 more wall seconds (= 16 sim seconds) -> pruned automatically on actor tick.
    for _ in 0..64 {
        let _ = h2.step_manual(1.0).await.unwrap();
    }
    assert!(h2.module_reactivation_cooldown(&key).is_none());
}

#[tokio::test]
async fn tidi_crash_recovery_unrebased_sim_clock_scales_and_advances_after_boot() {
    let base_unix = 1_750_100_000_u64;
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
    let repo = WorldRepository::new_empty();
    repo.set_clock(clock.clone());
    let db = Arc::new(repo);
    let sde = test_sde();
    let jita = 30_000_142_u32;
    let key = (530_001_i64, 630_001_i64, 11_370_u32);

    let reg = Arc::new(SystemRegistry::with_sde_and_db(
        Arc::clone(&sde),
        Arc::clone(&db),
    ));
    reg.cluster_nodes
        .write()
        .unwrap()
        .manual_tidi_overrides
        .insert(jita, 0.25);

    // Seed an un-rebased DB row simulating a mid-flight crash at sim = 100.0s under TiDi 0.25.
    reg.seed_cold_system_module_cooldown_in_db(
        &db,
        jita,
        key,
        ModuleCooldownEntry {
            recorded_unix_f64: base_unix as f64,
            delay_s: 30.0,
            solar_system_id: Some(jita),
            recorded_sys_sim_s: 100.0,
        },
    );
    clock.advance_secs(40); // 40 * 0.25 = 10s elapsed -> 20s remaining!

    let h = reg.get_or_spawn(jita);
    let entry = h
        .module_reactivation_cooldown(&key)
        .expect("crash-recovered TiDi entry loaded");
    assert!((entry.delay_s - 20.0).abs() < 1e-6);
    assert!((entry.recorded_sys_sim_s - 0.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_db_pending_merge_handle_view_same_key_no_resurrection_and_cold_epoch_safety() {
    let (reg, db, _sde) = test_registry_with_db();
    let jita = 30_000_142_u32;
    let now_unix = db.clock().now_unix_secs() as f64;
    let k_removed = (520_001_i64, 620_001_i64, 11_370_u32);
    let k_longer_db = (520_002_i64, 620_002_i64, 11_370_u32);
    let k_unmerged = (520_003_i64, 620_003_i64, 11_370_u32);

    let h = reg.get_or_spawn(jita);
    // Simulate `!db_read_safe` pre-merge state with cold rows waiting in SQLite.
    let cold_map = HashMap::from([
        (
            k_removed,
            ModuleCooldownEntry {
                recorded_unix_f64: now_unix,
                delay_s: 30.0,
                solar_system_id: Some(jita),
                recorded_sys_sim_s: 0.0,
            },
        ),
        (
            k_longer_db,
            ModuleCooldownEntry {
                recorded_unix_f64: now_unix,
                delay_s: 30.0,
                solar_system_id: Some(jita),
                recorded_sys_sim_s: 0.0,
            },
        ),
        (
            k_unmerged,
            ModuleCooldownEntry {
                recorded_unix_f64: now_unix,
                delay_s: 25.0,
                solar_system_id: Some(jita),
                recorded_sys_sim_s: 0.0,
            },
        ),
    ]);
    assert!(persist_system_module_cooldowns_in_db(
        &db,
        jita,
        &serialize_module_cooldown_rows(&cold_map),
    ));
    {
        let mut st = h.actor_state.write().unwrap();
        st.module_cooldowns.boot_db_pending_merge = true;
    }
    h.publish_view();
    assert!(h.view().module_cooldowns.boot_db_pending_merge);

    // Cold-DB helpers MUST skip live `jita` and NEVER bump CooldownPersistGate past `h`'s epoch.
    assert!(!reg.remove_cold_module_cooldown_key_across_systems(&db, None, k_unmerged));
    reg.clear_cold_ship_module_cooldowns_in_db(&db, k_unmerged.0);
    reg.clear_all_cold_module_cooldowns_in_db(&db);
    assert_eq!(load_cold_system_module_cooldowns_from_db(&db, jita).len(), 3);

    // Same-key `Remove` -> `Record(shorter)` before persist must NOT resurrect 30s from SQLite.
    let (tx_rm, rx_rm) = tokio::sync::oneshot::channel();
    h.cmd_sender()
        .send(SystemCommand::RemoveModuleCooldown {
            key: k_removed,
            reply: tx_rm,
        })
        .await
        .unwrap();
    let _ = rx_rm.await.unwrap().unwrap();

    let (tx_rec1, rx_rec1) = tokio::sync::oneshot::channel();
    h.cmd_sender()
        .send(SystemCommand::RecordModuleCooldown {
            ship_id: k_removed.0,
            module_id: k_removed.1,
            module_type_id: k_removed.2,
            now_unix_f64: now_unix,
            delay_s: 5.0,
            tidi_factor: 1.0,
            has_tidi_history: false,
            reply: tx_rec1,
        })
        .await
        .unwrap();
    let _ = rx_rec1.await.unwrap().unwrap();

    // Direct `Record(shorter)` without `Remove` must let longer DB entry (30s) converge in BOTH SQLite and live state/view.
    let (tx_rec2, rx_rec2) = tokio::sync::oneshot::channel();
    h.cmd_sender()
        .send(SystemCommand::RecordModuleCooldown {
            ship_id: k_longer_db.0,
            module_id: k_longer_db.1,
            module_type_id: k_longer_db.2,
            now_unix_f64: now_unix,
            delay_s: 10.0,
            tidi_factor: 1.0,
            has_tidi_history: false,
            reply: tx_rec2,
        })
        .await
        .unwrap();
    let payload = rx_rec2.await.unwrap().unwrap();
    assert!(h.persist_cooldown_payload(payload));

    // SystemView must publish immediately on merge, with exact convergence between memory & SQLite!
    let view = h.view();
    assert!(!view.module_cooldowns.boot_db_pending_merge);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_removed].delay_s - 5.0).abs() < 1e-6);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_longer_db].delay_s - 30.0).abs() < 1e-6);
    assert!((view.module_cooldowns.reactivation_cooldowns[&k_unmerged].delay_s - 25.0).abs() < 1e-6);
    let db_after = load_cold_system_module_cooldowns_from_db(&db, jita);
    assert!((db_after[&k_removed].delay_s - 5.0).abs() < 1e-6);
    assert!((db_after[&k_longer_db].delay_s - 30.0).abs() < 1e-6);
    assert!((db_after[&k_unmerged].delay_s - 25.0).abs() < 1e-6);

    // `ClearAllModuleCooldowns` must clear `pending_db_clear_all = false` on commit, and live actor persistence keeps working.
    h.clear_all_module_cooldowns_sync().unwrap();
    assert!(!h.actor_state.read().unwrap().module_cooldowns.pending_db_clear_all);
    assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());

    let k_after = (520_004_i64, 620_004_i64, 11_370_u32);
    h.record_module_cooldown(k_after.0, k_after.1, k_after.2, now_unix, 12.0, 1.0, false)
        .await
        .unwrap();
    assert!((load_cold_system_module_cooldowns_from_db(&db, jita)[&k_after].delay_s - 12.0).abs() < 1e-6);
}
