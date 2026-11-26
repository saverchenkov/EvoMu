//! Unit tests for `system_actor::weapon_effects` (`#1541` Track E1).

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    sync_combat_runtime_from_db, with_mut_combat_runtime, CombatRuntimeState,
};
use crate::services::combat_crimewatch::combat_missiles_aoe::{
    step_combat_system, step_missiles_and_breachers, step_system_weapon_effects_from_actor,
};
use crate::services::combat_crimewatch::combat_weapons_spool::{
    clear_entropic_spool, clear_entropic_spools_on_target_unlock, get_entropic_spool_multiplier,
};
use eve_db::sqlite_store::system_checkpoint::{load_system_checkpoints, KIND_WEAPON_EFFECTS};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::sync::atomic::Ordering;

struct TempSqliteGuard(std::path::PathBuf);

impl Drop for TempSqliteGuard {
    fn drop(&mut self) {
        for ext in ["sqlite", "sqlite-wal", "sqlite-shm"] {
            let _ = std::fs::remove_file(self.0.with_extension(ext));
        }
    }
}

fn temp_sqlite_db(tag: &str) -> (std::path::PathBuf, TempSqliteGuard) {
    let tmp = std::env::temp_dir().join(format!(
        "evomu_weapon_effects_{tag}_{}.sqlite",
        std::process::id()
    ));
    for ext in ["sqlite", "sqlite-wal", "sqlite-shm"] {
        let _ = std::fs::remove_file(tmp.with_extension(ext));
    }
    (tmp.clone(), TempSqliteGuard(tmp))
}

fn make_spool(sys_id: u32, ship_id: i64, module_id: i64, target_id: i64) -> EntropicSpoolState {
    EntropicSpoolState {
        target_id,
        solar_system_id: sys_id,
        attacker_char_id: 100_001,
        attacker_ship_id: ship_id,
        module_id,
        module_type_id: 47_240,
        charge_item_id: None,
        base_damage: DamageVector::new(0.0, 200.0, 0.0, 0.0),
        cap_need: 25.0,
        duration_s: 3.0,
        cycles: 2,
        elapsed_in_cycle: 0.5,
        per_cycle_bonus: 0.1,
        max_bonus: 1.5,
        current_multiplier: 1.2,
    }
}

fn make_missile(sys_id: u32, mid: i64, ship_id: i64, target_id: i64) -> InFlightMissileState {
    InFlightMissileState {
        missile_ball_id: mid,
        solar_system_id: sys_id,
        attacker_char_id: 100_001,
        attacker_ship_id: ship_id,
        launcher_module_id: 700_001,
        launcher_type_id: 2_410,
        missile_type_id: 209,
        target_ball_id: target_id,
        max_velocity: 2_000.0,
        flight_time_s: 6.0,
        remaining_flight_s: 4.5,
        hp: 75.0,
        max_hp: 75.0,
        base_damage: DamageVector::new(0.0, 0.0, 150.0, 0.0),
        explosion_radius: 40.0,
        explosion_velocity: 170.0,
        drf: 0.604,
        is_fof: true,
        is_breacher_pod: false,
        breacher_dot_duration_s: 0.0,
        breacher_flat_cap: 0.0,
        breacher_pct_cap: 0.0,
        destroyed: false,
        expired: false,
        impacted: false,
        applied_damage: 0.0,
    }
}

fn make_breacher(sys_id: u32, target_id: i64, ship_id: i64) -> ActiveBreacherDot {
    ActiveBreacherDot {
        target_ball_id: target_id,
        solar_system_id: sys_id,
        source_char_id: 100_001,
        source_ship_id: ship_id,
        weapon_type_id: 85_084,
        charge_type_id: 85_088,
        flat_cap_dps: 120.0,
        pct_cap_fraction: 0.02,
        remaining_s: 25.0,
        tick_damage: 120.0,
    }
}

fn make_ship_ball(id: i64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_system_isolation_and_sqlite_checkpoint_roundtrip() {
    let (tmp, _guard) = temp_sqlite_db("e1_roundtrip_1");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let (sys_a, sys_b) = (30_000_142u32, 30_000_144u32);
    let (ship_a, mod_a, mid_a, tgt_a) = (900_001i64, 710_001i64, 810_001i64, 300_001i64);
    let (ship_b, mod_b, mid_b, tgt_b) = (900_002i64, 710_002i64, 810_002i64, 300_002i64);

    {
        let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let ha = reg1.get_or_spawn(sys_a);
        let hb = reg1.get_or_spawn(sys_b);

        ha.upsert_entropic_spool_activation_sync(make_spool(sys_a, ship_a, mod_a, tgt_a), false)
            .unwrap();
        ha.upsert_in_flight_missile(make_missile(sys_a, mid_a, ship_a, tgt_a))
            .await
            .unwrap();
        ha.upsert_active_breacher_dot(make_breacher(sys_a, tgt_a, ship_a))
            .await
            .unwrap();

        hb.upsert_entropic_spool_activation_sync(make_spool(sys_b, ship_b, mod_b, tgt_b), false)
            .unwrap();
        hb.upsert_in_flight_missile(make_missile(sys_b, mid_b, ship_b, tgt_b))
            .await
            .unwrap();
        hb.upsert_active_breacher_dot(make_breacher(sys_b, tgt_b, ship_b))
            .await
            .unwrap();

        assert!(ha.entropic_spool(ship_b, mod_b).is_none());
        assert!(hb.in_flight_missile(mid_a).is_none());

        assert!(reg1.stop_system(sys_a).await);
        assert!(reg1.stop_system(sys_b).await);

        let rows_a = db1
            .with_sqlite_conn(|c| {
                load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_a, KIND_WEAPON_EFFECTS)
            })
            .expect("load sys_a checkpoints");
        let rows_b = db1
            .with_sqlite_conn(|c| {
                load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_b, KIND_WEAPON_EFFECTS)
            })
            .expect("load sys_b checkpoints");
        assert_eq!(rows_a.len(), 3);
        assert_eq!(rows_b.len(), 3);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db2)));
    let ha2 = reg2.get_or_spawn(sys_a);
    let hb2 = reg2.get_or_spawn(sys_b);

    assert!(ha2.entropic_spool(ship_a, mod_a).is_some());
    assert!(ha2.in_flight_missile(mid_a).is_some());
    assert!(ha2.active_breacher_dot(tgt_a).is_some());
    assert!(ha2.entropic_spool(ship_b, mod_b).is_none());
    assert!(hb2.entropic_spool(ship_b, mod_b).is_some());
    assert!(hb2.in_flight_missile(mid_b).is_some());
    assert!(hb2.active_breacher_dot(tgt_b).is_some());

    // Clearing all state on `sys_a` must delete SQLite checkpoint rows cleanly.
    assert!(ha2.remove_entropic_spool_sync(ship_a, mod_a).unwrap());
    assert!(ha2.remove_in_flight_missile_sync(mid_a).unwrap());
    assert!(ha2.remove_active_breacher_dot(tgt_a).await.unwrap());
    ha2.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(reg2.stop_system(sys_a).await);
    assert!(reg2.stop_system(sys_b).await);

    let cleared_a = db2
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_a, KIND_WEAPON_EFFECTS)
        })
        .expect("load cleared sys_a");
    assert!(cleared_a.is_empty(), "empty weapon effects state must clear SQLite rows");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_extended_runtime_json_migrates_once_and_never_rewrites() {
    let (tmp, _guard) = temp_sqlite_db("e1_legacy_2");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite legacy"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let (sys_a, sys_b) = (30_000_142u32, 30_000_144u32);
    let (ship_a, mod_a, mid_a, tgt_a) = (910_001i64, 720_001i64, 820_001i64, 310_001i64);
    let (ship_b, tgt_b) = (910_002i64, 310_002i64);

    let mut legacy_breacher_zero_sys = make_breacher(0, tgt_a, ship_a);
    legacy_breacher_zero_sys.solar_system_id = 0;
    let legacy_json = serde_json::json!({
        "entropic_spools": [[[ship_a, mod_a], make_spool(sys_a, ship_a, mod_a, tgt_a)]],
        "in_flight_missiles": { mid_a.to_string(): make_missile(sys_a, mid_a, ship_a, tgt_a) },
        "active_breacher_dots": {
            tgt_a.to_string(): legacy_breacher_zero_sys,
            tgt_b.to_string(): make_breacher(sys_b, tgt_b, ship_b)
        }
    })
    .to_string();

    db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            100_001,
            eve_db::CharacterRecord {
                character_id: 100_001,
                solar_system_id: sys_a,
                active_ship_id: tgt_a,
                ..Default::default()
            },
        );
        snap.combat_crimewatch_state.extended_runtime_json = legacy_json;
    });

    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    sync_combat_runtime_from_db(&ctx);
    let persisted_empty = with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_credited_balls.insert(55_555);
        rt.to_persisted()
    });
    assert!(
        !persisted_empty.extended_runtime_json.contains("entropic_spools")
            && !persisted_empty.extended_runtime_json.contains("in_flight_missiles")
            && !persisted_empty.extended_runtime_json.contains("active_breacher_dots"),
        "CombatRuntimeState::to_persisted must never emit migrated E1 maps: {}",
        persisted_empty.extended_runtime_json
    );

    let mut rt_legacy = CombatRuntimeState::default();
    let mut persisted_in = eve_db::PersistedCombatCrimewatchState::default();
    persisted_in.extended_runtime_json = db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.clone());
    rt_legacy.sync_from_persisted(&persisted_in);
    let legacy_reserialized = rt_legacy.to_persisted();
    assert!(!legacy_reserialized.extended_runtime_json.contains("entropic_spools"));
    assert!(!legacy_reserialized.extended_runtime_json.contains("in_flight_missiles"));
    assert!(!legacy_reserialized.extended_runtime_json.contains("active_breacher_dots"));

    assert!(ctx.systems.get_handle(sys_b).is_none());
    let ha = ctx.systems.get_or_spawn(sys_a);
    ha.flush_weapon_effects_checkpoint().await.unwrap();

    assert!(ha.entropic_spool(ship_a, mod_a).is_some());
    assert!(ha.in_flight_missile(mid_a).is_some());
    assert_eq!(
        ha.active_breacher_dot(tgt_a).map(|d| d.solar_system_id),
        Some(sys_a)
    );
    assert!(ha.active_breacher_dot(tgt_b).is_none());

    let rows_a = db
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_a, KIND_WEAPON_EFFECTS)
        })
        .expect("load migrated sys_a");
    assert_eq!(rows_a.len(), 3);
    db.with_snapshot(|snap| {
        let ext = &snap.combat_crimewatch_state.extended_runtime_json;
        assert!(!ext.contains("entropic_spools"));
        assert!(!ext.contains("in_flight_missiles"));
        assert!(ext.contains(&tgt_b.to_string()));
        assert!(!ext.contains(&tgt_a.to_string()));
    });
    assert!(ctx.systems.stop_system(sys_a).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_system_stays_cold_on_read_and_step() {
    let (tmp, _guard) = temp_sqlite_db("e1_cold_3");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite cold"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    let cold_sys = 30_000_142u32;

    assert!((get_entropic_spool_multiplier(&ctx, 920_001, 730_001) - 1.0).abs() < 1e-9);
    clear_entropic_spools_on_target_unlock(&ctx, 920_001, 100_001, Some(888_001));
    clear_entropic_spool(&ctx, 920_001, 730_001);
    assert!(step_combat_system(&ctx, cold_sys, 1.0).await.is_empty());
    assert!(step_missiles_and_breachers(&ctx, cold_sys, 1.0).await.is_empty());
    step_system_weapon_effects_from_actor(&ctx, cold_sys, 1.0).await;

    assert!(ctx.systems.get_handle(cold_sys).is_none());
    let rows = db
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, cold_sys, KIND_WEAPON_EFFECTS)
        })
        .expect("query cold sys");
    assert!(rows.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_unlock_fifo_ordering_and_shutdown_flush() {
    let (tmp, _guard) = temp_sqlite_db("e1_fifo_4");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite fifo"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let ship_id = 930_001i64;
    let mod_id = 740_001i64;
    let target_id = 330_001i64;

    let handle = reg.get_or_spawn(sys_id);
    handle
        .upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_id, target_id), false)
        .unwrap();
    let (cyc, mult) = handle.advance_entropic_spool_cycle_sync(ship_id, mod_id).unwrap();
    assert_eq!(cyc, 1);
    assert!((mult - 1.1).abs() < 1e-9);

    let (cyc2, mult2) = handle.advance_entropic_spool_cycle_sync(ship_id, mod_id).unwrap();
    assert_eq!(cyc2, 2);
    assert!((mult2 - 1.2).abs() < 1e-9);

    // Enqueue a snapshot item write on post-unlock FIFO and flush before stopping actor.
    handle.flush_weapon_effects_checkpoint().await.unwrap();
    let rows_mid = db
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS)
        })
        .expect("load fifo mid");
    assert_eq!(rows_mid.len(), 1);
    match &rows_mid[0].1 {
        WeaponEffectCheckpointEntry::Spool(s) => {
            assert_eq!(s.cycles, 2);
            assert!((s.current_multiplier - 1.2).abs() < 1e-9);
        }
        other => panic!("unexpected entry: {other:?}"),
    }

    // Mutate without manual flush and verify clean shutdown flushes the final state.
    let (cyc3, _) = handle.advance_entropic_spool_cycle_sync(ship_id, mod_id).unwrap();
    assert_eq!(cyc3, 3);
    assert!(reg.stop_system(sys_id).await);

    let rows_final = db
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS)
        })
        .expect("load fifo final");
    assert_eq!(rows_final.len(), 1);
    match &rows_final[0].1 {
        WeaponEffectCheckpointEntry::Spool(s) => assert_eq!(s.cycles, 3),
        other => panic!("unexpected final entry: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_fast_skip_and_pre_hydration_tombstone_prevent_resurrection() {
    let (tmp, _guard) = temp_sqlite_db("e1_tombstone_5");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let sys_id = 30_000_142u32;
    let (ship_id, mod_a, mod_b, mid_a, tgt_a) = (940_001i64, 750_001i64, 750_002i64, 850_001i64, 340_001i64);

    let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
    {
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let h1 = reg1.get_or_spawn(sys_id);
        h1.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_a, tgt_a), false)
            .unwrap();
        h1.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_b, tgt_a), false)
            .unwrap();
        h1.upsert_in_flight_missile(make_missile(sys_id, mid_a, ship_id, tgt_a))
            .await
            .unwrap();
        h1.upsert_active_breacher_dot(make_breacher(sys_id, tgt_a, ship_id))
            .await
            .unwrap();
        assert!(reg1.stop_system(sys_id).await);
    }

    // Reopen under held DB writer lock so boot skips blocking SQLite load (`needs_sqlite_hydration == true`).
    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db2)));
    let (lrx, rtx, writer_holder) = {
        let db_c = Arc::clone(&db2);
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_c.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx.await.unwrap();
    let h2 = reg2.get_or_spawn(sys_id);
    assert!(h2.view().weapon_effects_needs_sqlite_hydration);
    assert!(!h2.is_hibernated());

    // Remove/clear one spool, missile, and breacher BEFORE hydration runs -> records tombstones.
    assert!(h2.remove_entropic_spool_sync(ship_id, mod_a).unwrap());
    let _ = h2
        .spawn_ball_with_combat(make_ship_ball(tgt_a), CombatState::default())
        .await
        .unwrap();
    let _ = h2
        .spawn_ball_with_combat(make_ship_ball(mid_a), CombatState::default())
        .await
        .unwrap();
    assert!(h2.remove_ball(mid_a).await.is_some());
    assert!(h2.remove_ball(tgt_a).await.is_some());
    assert!(h2.view().weapon_effects_needs_sqlite_hydration);
    let _ = rtx.send(());
    writer_holder.await.unwrap();

    // Hydrate now: `mod_b` restores cleanly, while `mod_a`, `mid_a`, and `tgt_a` must stay deleted.
    h2.ensure_weapon_effects_hydrated().await;
    assert!(!h2.view().weapon_effects_needs_sqlite_hydration);
    assert!(h2.entropic_spool(ship_id, mod_a).is_none());
    assert!(h2.entropic_spool(ship_id, mod_b).is_some());
    assert!(h2.in_flight_missile(mid_a).is_none());
    assert!(h2.active_breacher_dot(tgt_a).is_none());

    // Clearing remaining `mod_b` makes `has_active_weapon_effects == false` and allows hibernation + fast-skip.
    assert!(h2.remove_entropic_spool_sync(ship_id, mod_b).unwrap());
    h2.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(!h2.view().has_active_weapon_effects);
    let _ = h2.step_manual(1.0).await.unwrap();
    assert!(h2.is_hibernated());
    assert!(reg2.stop_system(sys_id).await);
    let ctx2 = ServerContext::with_sde_and_repository(sde, Arc::clone(&db2));
    let h2_idle = ctx2.systems.get_or_spawn(sys_id);
    let coord2 = h2_idle.view().weapon_effects_coord.clone().unwrap();
    let seq_before_idle = coord2.seq.load(Ordering::Acquire);
    assert!(!h2_idle.view().has_active_weapon_effects);
    step_system_weapon_effects_from_actor(&ctx2, sys_id, 1.0).await;
    assert_eq!(coord2.seq.load(Ordering::Acquire), seq_before_idle);
    let _ = h2_idle
        .spawn_ball_with_combat(make_ship_ball(ship_id), CombatState::default())
        .await
        .unwrap();
    h2_idle
        .upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_a, tgt_a), false)
        .unwrap();
    h2_idle
        .upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_b, 999_999), false)
        .unwrap();
    clear_entropic_spools_on_target_unlock(&ctx2, ship_id, 100_001, Some(tgt_a));
    assert!(h2_idle.entropic_spool(ship_id, mod_a).is_none());
    assert!(h2_idle.entropic_spool(ship_id, mod_b).is_some());
    assert!(h2_idle.remove_ball(ship_id).await.is_some());
    assert!(h2_idle.entropic_spool(ship_id, mod_b).is_none());
    assert!(ctx2.systems.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_e1_01_failed_sqlite_write_preserves_legacy_json_until_retry_succeeds() {
    let (tmp, _guard) = temp_sqlite_db("bf_e1_01_sqlite_first");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite bf_e1_01"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let sys_id = 30_000_142u32;
    let (ship_id, mod_id, mid, tgt_id) = (960_001i64, 760_001i64, 860_001i64, 360_001i64);

    let legacy_json = serde_json::json!({
        "entropic_spools": [[[ship_id, mod_id], make_spool(sys_id, ship_id, mod_id, tgt_id)]],
        "in_flight_missiles": { mid.to_string(): make_missile(sys_id, mid, ship_id, tgt_id) },
        "active_breacher_dots": { tgt_id.to_string(): make_breacher(sys_id, tgt_id, ship_id) }
    })
    .to_string();
    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json = legacy_json;
    });
    let _ = db
        .with_sqlite_conn(|c| {
            load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS)
        })
        .unwrap();
    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER test_bf_e1_01_fail_ins BEFORE INSERT ON system_checkpoint \
             WHEN NEW.kind = 'weapon_effects' \
             BEGIN SELECT RAISE(ABORT, 'injected sqlite write failure'); END;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();

    let reg = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let ha = reg.get_or_spawn(sys_id);
    assert!(
        ha.flush_weapon_effects_checkpoint().await.is_err(),
        "failed SQLite migration commit must propagate Err"
    );
    let coord = ha.view().weapon_effects_coord.clone().unwrap();
    assert!(coord.retry_needed.load(Ordering::Acquire));
    assert!(!coord.migrated_legacy_systems.as_ref().unwrap().lock().unwrap().contains(&sys_id));
    db.with_snapshot(|snap| {
        let ext = &snap.combat_crimewatch_state.extended_runtime_json;
        assert!(ext.contains("entropic_spools"));
        assert!(ext.contains("in_flight_missiles"));
        assert!(ext.contains("active_breacher_dots"));
    });

    // Healing SQLite allows retry to commit SQLite rows and prune legacy JSON afterward.
    db.with_sqlite_conn(|conn| {
        conn.execute_batch("DROP TRIGGER test_bf_e1_01_fail_ins;")
            .map_err(|e| e.to_string())
    })
    .unwrap();
    ha.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(!coord.retry_needed.load(Ordering::Acquire));
    assert!(coord.migrated_legacy_systems.as_ref().unwrap().lock().unwrap().contains(&sys_id));
    let rows = db
        .with_sqlite_conn(|c| load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS))
        .unwrap();
    assert_eq!(rows.len(), 3);
    db.with_snapshot(|snap| {
        assert!(snap.combat_crimewatch_state.extended_runtime_json.is_empty());
    });
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_e1_02_failed_flush_retries_blocks_hibernation_and_recovers_on_heal() {
    let (tmp, _guard) = temp_sqlite_db("bf_e1_02_retry");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite bf_e1_02"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let (ship_id, mod_id, tgt_id) = (961_001i64, 761_001i64, 361_001i64);

    let ha = reg.get_or_spawn(sys_id);
    ha.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_id, tgt_id), false)
        .unwrap();
    ha.flush_weapon_effects_checkpoint().await.unwrap();

    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER test_bf_e1_02_fail_del BEFORE DELETE ON system_checkpoint \
             WHEN OLD.kind = 'weapon_effects' \
             BEGIN SELECT RAISE(ABORT, 'injected sqlite delete failure'); END;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();

    assert!(ha.remove_entropic_spool_sync(ship_id, mod_id).unwrap());
    assert!(ha.flush_weapon_effects_checkpoint().await.is_err());
    let coord = ha.view().weapon_effects_coord.clone().unwrap();
    assert!(coord.retry_needed.load(Ordering::Acquire));
    let _ = ha.step_manual(1.0).await.unwrap();
    assert!(!ha.is_hibernated(), "hibernation must stay blocked until checkpoint retry succeeds");

    db.with_sqlite_conn(|conn| {
        conn.execute_batch("DROP TRIGGER test_bf_e1_02_fail_del;")
            .map_err(|e| e.to_string())
    })
    .unwrap();
    ha.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(!coord.retry_needed.load(Ordering::Acquire));
    let _ = ha.step_manual(1.0).await.unwrap();
    assert!(ha.is_hibernated(), "hibernation must unblock once SQLite retry commits");
    let rows = db
        .with_sqlite_conn(|c| load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS))
        .unwrap();
    assert!(rows.is_empty());
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_e1_03_actor_failure_and_invalid_system_return_bridge_rpc_error() {
    struct FailingWakeProvider;
    impl crate::system_actor::SystemContentProvider for FailingWakeProvider {
        fn name(&self) -> &'static str {
            "bf_e1_03_failing_wake_provider"
        }

        fn seed(
            &self,
            _ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
        ) -> Result<(), String> {
            Err("simulated wake failure".to_string())
        }
    }

    let (tmp, _guard) = temp_sqlite_db("bf_e1_03_rpc_err");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite bf_e1_03"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let mod_type = sde.types.values().next().unwrap().clone();
    let dmg = DamageVector::new(0.0, 100.0, 0.0, 0.0);

    let ctx_fail = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db)));
    ctx_fail
        .systems
        .register_content_provider(Arc::new(FailingWakeProvider))
        .unwrap();
    let ctx_fail_c = Arc::clone(&ctx_fail);
    let mod_type_c = mod_type.clone();
    let wake_fail_res = tokio::task::spawn_blocking(move || {
        crate::services::combat_weapons_spool::handle_entropic_activation(
            &ctx_fail_c, 100_001, 962_001, 30_000_144, 762_001, 362_001, &mod_type_c, None, dmg, 10.0, 3.0,
        )
    })
    .await
    .unwrap();
    assert!(wake_fail_res.is_err(), "failed system wake must return Err(BridgeRpcError) without panicking");

    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let dead_handle = ctx.systems.get_or_spawn(sys_id);
    let (shut_tx, shut_rx) = tokio::sync::oneshot::channel();
    dead_handle
        .cmd_sender()
        .send(SystemCommand::ShutdownActor { reply: shut_tx })
        .await
        .unwrap();
    let _ = shut_rx.await;

    let dead_handle_c = dead_handle.clone();
    let mod_type_id = mod_type.type_id;
    let dead_spool_res = tokio::task::spawn_blocking(move || {
        crate::services::combat_weapons_spool::apply_entropic_activation_to_handle(
            &dead_handle_c, 100_001, 962_001, sys_id, 762_001, 362_001, mod_type_id, None, dmg, 10.0,
            3.0, 0.07, 2.125, false,
        )
    })
    .await
    .unwrap();
    assert!(
        dead_spool_res.is_err(),
        "entropic activation must propagate Err instead of synthetic Ok((0, 1.0)) on dead actor"
    );

    let mut fit = eve_dogma::ShipFittingEngine::from_sde(962_001, 587, &sde);
    let fitted = eve_dogma::FittedModule {
        item_id: 762_002,
        type_id: mod_type.type_id,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: Some(mod_type.type_id),
        charge_quantity: 10,
        dynamic_attribute_overrides: HashMap::new(),
    };
    fit.modules.insert(fitted.item_id, fitted.clone());
    let attacker = make_ship_ball(962_001);

    let wake_missile_fail = crate::services::combat_crimewatch::combat_missiles_aoe::activate_missile_or_breacher_launcher(
        &ctx_fail, 100_001, 962_001, 30_000_144, 1_700_000_000, 762_002, &fitted, &fit,
        mod_type.type_id, &mod_type, &attacker, 362_001, dmg, false, false, true,
    )
    .await;
    assert!(
        wake_missile_fail.is_err(),
        "activate_missile_or_breacher_launcher must return Err(BridgeRpcError) on wake failure"
    );

    let dead_missile_res = crate::services::combat_crimewatch::combat_missiles_aoe::activate_missile_or_breacher_launcher_on_handle(
        &ctx, &dead_handle, 100_001, 962_001, sys_id, 1_700_000_000, 762_002, &fitted, &fit,
        mod_type.type_id, &mod_type, &attacker, 362_001, dmg, false, false, true,
    )
    .await;
    assert!(
        dead_missile_res.is_err(),
        "missile launch must return Err(BridgeRpcError) cleanly on dead actor without panicking"
    );

    let live_sys = 30_000_144u32;
    let live_handle = ctx.systems.get_or_spawn(live_sys);
    let balls_before = live_handle.view().balls.len();
    live_handle
        .panic_on_command_countdown_for_test
        .store(3, Ordering::SeqCst);
    let upsert_fail_res = crate::services::combat_crimewatch::combat_missiles_aoe::activate_missile_or_breacher_launcher(
        &ctx, 100_001, 962_001, live_sys, 1_700_000_000, 762_002, &fitted, &fit,
        mod_type.type_id, &mod_type, &attacker, 362_001, dmg, false, false, true,
    )
    .await;
    assert!(
        upsert_fail_res.is_err(),
        "missile launch must fail when upsert_in_flight_missile fails"
    );
    assert_eq!(
        live_handle.view().balls.len(),
        balls_before,
        "failed upsert_in_flight_missile must roll back spawned missile ball"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_e1_04_pre_hydration_targeted_unlock_and_absent_ball_registry_queries() {
    let (tmp, _guard) = temp_sqlite_db("bf_e1_04_pre_hyd_absent");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let sys_id = 30_000_142u32;
    let (ship_id, mod_a, mod_b, mid_a, tgt_a, tgt_b) =
        (963_001i64, 763_001i64, 763_002i64, 863_001i64, 363_001i64, 363_002i64);

    let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 1"));
    {
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let h1 = reg1.get_or_spawn(sys_id);
        h1.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_a, tgt_a), false)
            .unwrap();
        h1.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_b, tgt_b), false)
            .unwrap();
        h1.upsert_in_flight_missile(make_missile(sys_id, mid_a, ship_id, tgt_a))
            .await
            .unwrap();
        h1.upsert_active_breacher_dot(make_breacher(sys_id, tgt_a, ship_id))
            .await
            .unwrap();
        assert!(reg1.stop_system(sys_id).await);
    }

    // Boot while writer held -> `needs_sqlite_hydration == true` and zero dynamic balls spawned.
    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 2"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db2)));
    let (lrx, rtx, holder) = {
        let db_c = Arc::clone(&db2);
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_c.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx.await.unwrap();
    let h2 = reg2.get_or_spawn(sys_id);
    assert!(h2.view().weapon_effects_needs_sqlite_hydration);
    assert!(!h2.view().balls.contains_key(&ship_id));
    // Pre-hydration targeted unlock of `Some(tgt_a)` must tombstone `mod_a` non-blockingly while keeping `mod_b`.
    let h2_c = h2.clone();
    tokio::task::spawn_blocking(move || {
        let _ = h2_c.collect_and_clear_entropic_spools_for_ship_sync(ship_id, Some(tgt_a));
    })
    .await
    .unwrap();
    assert!(h2.view().weapon_effects_needs_sqlite_hydration);
    let _ = rtx.send(());
    holder.await.unwrap();

    h2.ensure_weapon_effects_hydrated().await;
    assert!(h2.entropic_spool(ship_id, mod_a).is_none());
    assert!(h2.entropic_spool(ship_id, mod_b).is_some());
    assert!(reg2.stop_system(sys_id).await);

    // Reopen with deferred hydration + ship/missile/breacher absent from `view.balls` and test SystemRegistry APIs.
    let db3 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite 3"));
    let reg3 = Arc::new(SystemRegistry::with_sde_and_db(sde, db3));
    let (lrx3, rtx3, holder3) = {
        let h_db = reg3.db.as_ref().unwrap().clone();
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            h_db.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx3.await.unwrap();
    let h3 = reg3.get_or_spawn(sys_id);
    assert!(h3.view().weapon_effects_needs_sqlite_hydration);
    let _ = rtx3.send(());
    holder3.await.unwrap();

    let reg3_c = Arc::clone(&reg3);
    tokio::task::spawn_blocking(move || {
        assert!(reg3_c.active_entropic_spool(ship_id, mod_b).is_some());
        assert!(reg3_c.active_in_flight_missile(mid_a).is_some());
        assert!(reg3_c.active_breacher_dot(tgt_a).is_some());
        assert_eq!(reg3_c.active_breacher_dot_count(), 1);
        assert_eq!(reg3_c.active_entropic_spools_matching(|&(s, _), _| s == ship_id).len(), 1);
        assert_eq!(reg3_c.advance_entropic_spool_cycle_sync(ship_id, mod_b).0, 1);
    })
    .await
    .unwrap();
    assert!(reg3.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_01_clean_deferred_hydration_actor_self_hydrates_and_evicts() {
    let (tmp, _guard) = temp_sqlite_db("e1_01_clean_self_hydrate");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let sys_id = 30_000_142u32;
    let (ship_id, mod_id, tgt_id) = (964_001i64, 764_001i64, 364_001i64);

    // Part A: Persisted spool restores into direct SystemHandle getters via actor self-hydration.
    let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_01_a"));
    {
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let h1 = reg1.get_or_spawn(sys_id);
        h1.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_id, tgt_id), false)
            .unwrap();
        assert!(reg1.stop_system(sys_id).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_01_b"));
    let reg2 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db2)));
    let (lrx2, rtx2, holder2) = {
        let db_c = Arc::clone(&db2);
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_c.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx2.await.unwrap();
    let h2 = reg2.get_or_spawn(sys_id);
    assert!(h2.view().weapon_effects_needs_sqlite_hydration);
    assert!(!h2.is_hibernated());
    let _ = rtx2.send(());
    holder2.await.unwrap();

    // Unrelated normal command with zero weapon mutations triggers actor self-hydration.
    let _ = h2.get_ball_snapshot(999_999).await;
    assert!(!h2.view().weapon_effects_needs_sqlite_hydration);
    assert!(h2.entropic_spool(ship_id, mod_id).is_some());
    assert_eq!(h2.weapon_effects().entropic_spools.len(), 1);
    assert!(h2.remove_entropic_spool_sync(ship_id, mod_id).unwrap());
    h2.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(reg2.stop_system(sys_id).await);

    // Part B: Empty clean deferred-hydration actor self-hydrates, hibernates, and evicts cleanly.
    let db3 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_01_c"));
    let reg3 = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db3)));
    let (lrx3, rtx3, holder3) = {
        let db_c = Arc::clone(&db3);
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_c.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx3.await.unwrap();
    let h3 = reg3.get_or_spawn(sys_id);
    assert!(h3.view().weapon_effects_needs_sqlite_hydration);
    assert!(!h3.is_hibernated());
    let _ = rtx3.send(());
    holder3.await.unwrap();

    let _ = h3.get_ball_snapshot(999_999).await;
    assert!(!h3.view().weapon_effects_needs_sqlite_hydration);
    if h3.view().active_drones_needs_sqlite_hydration {
        let _ = h3.get_ball_snapshot(999_999).await;
    }
    assert!(h3.is_hibernated());
    drop(h3);
    assert_eq!(reg3.evict_idle_systems(0).await, vec![sys_id]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_p1_01_orphaned_breacher_dot_cleans_up_and_allows_hibernation() {
    let (tmp, _guard) = temp_sqlite_db("e1_p1_01_orphan_breacher");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let sys_id = 30_000_142u32;
    let (ship_id, tgt_id) = (965_001i64, 365_001i64);

    let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite p1_01_1"));
    {
        let reg1 = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db1)));
        let h1 = reg1.get_or_spawn(sys_id);
        h1.upsert_active_breacher_dot(make_breacher(sys_id, tgt_id, ship_id))
            .await
            .unwrap();
        assert!(reg1.stop_system(sys_id).await);
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite p1_01_2"));
    let ctx2 = ServerContext::with_sde_and_repository(sde, Arc::clone(&db2));
    let h2 = ctx2.systems.get_or_spawn(sys_id);
    assert!(h2.active_breacher_dot(tgt_id).is_some());
    assert!(h2.view().has_active_weapon_effects);
    assert!(!h2.is_hibernated());

    // Target ball is absent after restart -> step cleans up the orphan DoT.
    step_system_weapon_effects_from_actor(&ctx2, sys_id, 1.0).await;
    assert!(h2.active_breacher_dot(tgt_id).is_none());
    h2.flush_weapon_effects_checkpoint().await.unwrap();
    let _ = h2.step_manual(1.0).await.unwrap();
    assert!(!h2.view().has_active_weapon_effects);
    assert!(h2.is_hibernated());

    let coord = h2.view().weapon_effects_coord.clone().unwrap();
    let seq_idle = coord.seq.load(Ordering::Acquire);
    step_system_weapon_effects_from_actor(&ctx2, sys_id, 1.0).await;
    assert_eq!(coord.seq.load(Ordering::Acquire), seq_idle);
    let rows = db2
        .with_sqlite_conn(|c| load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS))
        .unwrap();
    assert!(rows.is_empty());
    assert!(ctx2.systems.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_p1_02_legacy_breacher_isolation_when_attacker_moved_and_deferred_npc_structure() {
    let (tmp, _guard) = temp_sqlite_db("e1_p1_02_legacy_breacher");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let (sys_a, sys_b) = (30_000_142u32, 30_000_144u32);
    let (attacker_char, tgt_ship, struct_id, npc_id) = (100_001i64, 366_001i64, 1_000_366_002i64, 8_000_366_003i64);

    let db1 = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite p1_02"));
    let dot_ship = make_breacher(0, tgt_ship, 966_001);
    let mut dot_struct = make_breacher(0, struct_id, 966_002);
    dot_struct.source_char_id = 0;
    let mut dot_npc = make_breacher(0, npc_id, 966_003);
    dot_npc.source_char_id = 0;

    let legacy_json = serde_json::json!({
        "active_breacher_dots": {
            tgt_ship.to_string(): dot_ship,
            struct_id.to_string(): dot_struct,
            npc_id.to_string(): dot_npc,
        }
    })
    .to_string();

    db1.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json = legacy_json;
        // Attacker moved to `sys_b`, while `tgt_ship` stays in `sys_a`.
        snap.characters.insert(
            attacker_char,
            eve_db::CharacterRecord {
                character_id: attacker_char,
                solar_system_id: sys_b,
                ..Default::default()
            },
        );
        snap.items.insert(
            tgt_ship,
            eve_db::InventoryItemRecord {
                item_id: tgt_ship,
                location_id: i64::from(sys_a),
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_a,
                ..Default::default()
            },
        );
    });

    let reg = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db1)));
    // Waking `sys_b` FIRST must NOT steal `sys_a`'s `tgt_ship` DoT even though attacker is in `sys_b`.
    let hb = reg.get_or_spawn(sys_b);
    hb.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(hb.active_breacher_dot(tgt_ship).is_none());
    db1.with_snapshot(|snap| {
        assert!(snap.combat_crimewatch_state.extended_runtime_json.contains(&tgt_ship.to_string()));
    });

    // Boot `sys_a` under held DB writer lock (`deferred hydration`), spawn `npc_id` live, then hydrate!
    let (lrx, rtx, holder) = {
        let db_c = Arc::clone(&db1);
        let (ltx, lrx) = tokio::sync::oneshot::channel::<()>();
        let (rtx, rrx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_c.with_mut_snapshot(|_| {
                let _ = ltx.send(());
                let _ = rrx.blocking_recv();
            });
        });
        (lrx, rtx, holder)
    };
    lrx.await.unwrap();
    let ha = reg.get_or_spawn(sys_a);
    assert!(ha.view().weapon_effects_needs_sqlite_hydration);
    let _ = ha
        .spawn_ball_with_combat(make_ship_ball(npc_id), CombatState::default())
        .await
        .unwrap();
    let _ = rtx.send(());
    holder.await.unwrap();

    ha.ensure_weapon_effects_hydrated().await;
    assert_eq!(ha.active_breacher_dot(tgt_ship).map(|d| d.solar_system_id), Some(sys_a));
    assert_eq!(ha.active_breacher_dot(struct_id).map(|d| d.solar_system_id), Some(sys_a));
    assert_eq!(ha.active_breacher_dot(npc_id).map(|d| d.solar_system_id), Some(sys_a));
    assert!(reg.stop_system(sys_a).await && reg.stop_system(sys_b).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_p1_03_try_remove_ball_for_db_flushes_weapon_effects_and_hibernates() {
    let (tmp, _guard) = temp_sqlite_db("e1_p1_03_db_remove_flush");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite p1_03"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let (ship_id, mod_id, tgt_id) = (967_001i64, 767_001i64, 367_001i64);

    let h = reg.get_or_spawn(sys_id);
    let _ = h
        .spawn_ball_with_combat(make_ship_ball(ship_id), CombatState::default())
        .await
        .unwrap();
    h.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_id, mod_id, tgt_id), false)
        .unwrap();
    h.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(!h.is_hibernated());

    // Remove via `try_remove_ball_for_db` without any manual handle flush.
    crate::system_actor::try_remove_ball_for_db(&db, sys_id, ship_id);
    assert!(h.entropic_spool(ship_id, mod_id).is_none());

    for _ in 0..100 {
        let pending = h
            .view()
            .weapon_effects_coord
            .as_ref()
            .is_some_and(|c| c.has_pending_work());
        if !pending && h.is_hibernated() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(h.is_hibernated(), "try_remove_ball_for_db must flush weapon_effects and unblock hibernation");
    let rows = db
        .with_sqlite_conn(|c| load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS))
        .unwrap();
    assert!(rows.is_empty(), "cleared spool checkpoint must be deleted from SQLite");
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_r1_01_legacy_prune_snapshot_failure_holds_flush_hibernation_and_recovers_on_retry() {
    let (tmp, _guard) = temp_sqlite_db("e1_r1_01_legacy_prune_retry");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_r1_01"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let (sys_id, sys_shutdown) = (30_000_142u32, 30_000_144u32);
    let (ship_id, mod_id, tgt_id) = (968_001i64, 768_001i64, 368_001i64);

    let legacy_json = serde_json::json!({
        "entropic_spools": [
            [[ship_id, mod_id], make_spool(sys_id, ship_id, mod_id, tgt_id)],
            [[ship_id + 1, mod_id + 1], make_spool(sys_shutdown, ship_id + 1, mod_id + 1, tgt_id + 1)]
        ]
    })
    .to_string();
    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json = legacy_json;
    });

    // Block only snapshot legacy JSON pruning (`domain_rows`), allowing SQLite `system_checkpoint` commits to succeed.
    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER test_e1_r1_01_fail_upd BEFORE UPDATE ON domain_rows \
             WHEN NEW.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention upd'); END; \
             CREATE TRIGGER test_e1_r1_01_fail_ins BEFORE INSERT ON domain_rows \
             WHEN NEW.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention ins'); END; \
             CREATE TRIGGER test_e1_r1_01_fail_del BEFORE DELETE ON domain_rows \
             WHEN OLD.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention del'); END;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();

    let reg = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let ha = reg.get_or_spawn(sys_id);
    let coord = ha.view().weapon_effects_coord.clone().unwrap();
    for _ in 0..100 {
        if coord.flushed_seq.load(Ordering::Acquire) >= 1 && coord.retry_needed.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(coord.flushed_seq.load(Ordering::Acquire) >= 1, "SQLite commit must happen before snapshot prune");
    assert!(coord.has_pending_legacy_prune.load(Ordering::Acquire));
    assert!(coord.has_pending_work());

    // Remove spool so weapon_effects & dyn_balls are both empty while legacy prune contention holds.
    assert!(ha.remove_entropic_spool_sync(ship_id, mod_id).unwrap());
    for _ in 0..100 {
        if coord.flushed_seq.load(Ordering::Acquire) >= coord.seq.load(Ordering::Acquire)
            && coord.retry_needed.load(Ordering::Acquire)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(coord.flushed_seq.load(Ordering::Acquire), coord.seq.load(Ordering::Acquire));
    assert!(coord.has_pending_legacy_prune.load(Ordering::Acquire));
    assert!(coord.has_pending_work(), "pending_legacy_prune must keep flush barrier open");
    assert!(
        ha.test_with_actor_state_async(|st| has_unflushed_weapon_effects_in_state(st)).await,
        "pending_legacy_prune must keep has_unflushed_weapon_effects_in_state true"
    );
    let _ = ha.step_manual(1.0).await.unwrap();
    assert!(!ha.is_hibernated(), "hibernation must stay blocked while legacy prune is pending");

    // Flush barrier must fail rather than returning Ok(()) while legacy prune fails.
    assert!(ha.flush_weapon_effects_checkpoint().await.is_err());
    assert!(coord.has_pending_legacy_prune.load(Ordering::Acquire));
    assert!(!ha.is_hibernated());

    // Releasing snapshot contention heals legacy prune on retry, clears flags, and unblocks hibernation.
    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "DROP TRIGGER test_e1_r1_01_fail_upd; \
             DROP TRIGGER test_e1_r1_01_fail_ins; \
             DROP TRIGGER test_e1_r1_01_fail_del;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();
    ha.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(!coord.has_pending_legacy_prune.load(Ordering::Acquire));
    assert!(!coord.dirty.load(Ordering::Acquire));
    assert!(!coord.retry_needed.load(Ordering::Acquire));
    assert!(!coord.has_pending_work());
    assert!(ha.is_hibernated(), "cleared weapon_effects after prune must hibernate cleanly");
    db.with_snapshot(|snap| {
        assert!(!snap.combat_crimewatch_state.extended_runtime_json.contains(&ship_id.to_string()));
    });

    // Also verify shutdown blocking flush completes a pending legacy prune even when flushed_seq == seq.
    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER test_e1_r1_01_fail_upd2 BEFORE UPDATE ON domain_rows \
             WHEN NEW.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention upd2'); END; \
             CREATE TRIGGER test_e1_r1_01_fail_ins2 BEFORE INSERT ON domain_rows \
             WHEN NEW.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention ins2'); END; \
             CREATE TRIGGER test_e1_r1_01_fail_del2 BEFORE DELETE ON domain_rows \
             WHEN OLD.key = 'combat_crimewatch_state.extended_runtime_json' \
             BEGIN SELECT RAISE(ABORT, 'injected legacy prune contention del2'); END;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();
    let hb = reg.get_or_spawn(sys_shutdown);
    let coord_b = hb.view().weapon_effects_coord.clone().unwrap();
    for _ in 0..100 {
        if coord_b.flushed_seq.load(Ordering::Acquire) >= 1 && coord_b.retry_needed.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(coord_b.has_pending_legacy_prune.load(Ordering::Acquire));
    db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "DROP TRIGGER test_e1_r1_01_fail_upd2; \
             DROP TRIGGER test_e1_r1_01_fail_ins2; \
             DROP TRIGGER test_e1_r1_01_fail_del2;",
        )
        .map_err(|e| e.to_string())
    })
    .unwrap();
    assert!(reg.stop_system(sys_shutdown).await);
    db.with_snapshot(|snap| {
        assert!(snap.combat_crimewatch_state.extended_runtime_json.is_empty());
    });
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_r1_02_actor_apply_damage_destruction_clears_weapon_effects_and_hibernates() {
    let (tmp, _guard) = temp_sqlite_db("e1_r1_02_damage_clear");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_r1_02"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(sde, Arc::clone(&db)));
    let sys_id = 30_000_142u32;
    let (ship_a, ship_b, mod_id) = (969_001i64, 969_002i64, 769_001i64);

    let fragile = CombatState {
        shield_hp: 0.0,
        max_shield_hp: 0.0,
        armor_hp: 0.0,
        max_armor_hp: 0.0,
        structure_hp: 10.0,
        max_structure_hp: 10.0,
        ..CombatState::default()
    };
    let h = reg.get_or_spawn(sys_id);
    let _ = h.spawn_ball_with_combat(make_ship_ball(ship_a), fragile.clone()).await.unwrap();
    let _ = h.spawn_ball_with_combat(make_ship_ball(ship_b), fragile).await.unwrap();
    h.upsert_entropic_spool_activation_sync(make_spool(sys_id, ship_a, mod_id, ship_b), false).unwrap();
    h.upsert_active_breacher_dot(make_breacher(sys_id, ship_b, ship_a)).await.unwrap();
    h.flush_weapon_effects_checkpoint().await.unwrap();
    assert!(h.view().has_active_weapon_effects);
    assert!(!h.is_hibernated());

    let (_, destroyed_b, _) = h
        .apply_damage(ship_b, DamageVector::new(500.0, 0.0, 0.0, 0.0))
        .await
        .unwrap()
        .expect("ship_b damage outcome");
    assert!(destroyed_b);
    assert!(h.active_breacher_dot(ship_b).is_none(), "apply_damage kill must clear breacher DoT immediately");

    let (_, destroyed_a, _) = h
        .apply_true_damage(ship_a, 500.0)
        .await
        .unwrap()
        .expect("ship_a true damage outcome");
    assert!(destroyed_a);
    assert!(h.entropic_spool(ship_a, mod_id).is_none(), "apply_true_damage kill must clear spool immediately");
    assert!(!h.view().has_active_weapon_effects);

    for _ in 0..100 {
        let pending = h.view().weapon_effects_coord.as_ref().is_some_and(|c| c.has_pending_work());
        if !pending && h.is_hibernated() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(h.is_hibernated(), "actor damage kills must flush weapon_effects and unblock hibernation");
    let rows = db
        .with_sqlite_conn(|c| load_system_checkpoints::<WeaponEffectCheckpointEntry>(c, sys_id, KIND_WEAPON_EFFECTS))
        .unwrap();
    assert!(rows.is_empty());
    assert!(reg.stop_system(sys_id).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e1_r1_03_zero_dt_actor_step_does_not_impact_point_blank_missile_or_dirty_seq() {
    let (tmp, _guard) = temp_sqlite_db("e1_r1_03_zero_dt");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&tmp).expect("open_sqlite e1_r1_03"));
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    let sys_id = 30_000_142u32;
    let (ship_id, tgt_id, mid) = (970_001i64, 370_001i64, 870_001i64);

    let full_hp = CombatState {
        shield_hp: 100.0,
        max_shield_hp: 100.0,
        armor_hp: 100.0,
        max_armor_hp: 100.0,
        structure_hp: 100.0,
        max_structure_hp: 100.0,
        ..CombatState::default()
    };
    let h = ctx.systems.get_or_spawn(sys_id);
    let _ = h.spawn_ball_with_combat(make_ship_ball(ship_id), full_hp.clone()).await.unwrap();
    let _ = h.spawn_ball_with_combat(make_ship_ball(tgt_id), full_hp).await.unwrap();
    let mut m_ball = make_ship_ball(mid);
    m_ball.position = DVec3::ZERO; // Point-blank (dist == 0.0) relative to target at DVec3::ZERO
    m_ball.max_velocity = 2_000.0;
    let _ = h.spawn_ball_with_combat(m_ball, CombatState::default()).await.unwrap();
    h.upsert_in_flight_missile(make_missile(sys_id, mid, ship_id, tgt_id)).await.unwrap();
    h.flush_weapon_effects_checkpoint().await.unwrap();

    let coord = h.view().weapon_effects_coord.clone().unwrap();
    let seq_before = coord.seq.load(Ordering::Acquire);
    step_system_weapon_effects_from_actor(&ctx, sys_id, 0.0).await;

    let after_m = h.in_flight_missile(mid).expect("missile must still exist after dt=0.0");
    assert!(!after_m.impacted && !after_m.destroyed, "dt=0.0 must not impact point-blank missile");
    let tgt_hp = h.get_combat_state(tgt_id).await.unwrap().expect("target alive");
    assert!((tgt_hp.shield_hp - 100.0).abs() < 1e-6);
    assert_eq!(coord.seq.load(Ordering::Acquire), seq_before, "dt=0.0 must not dirty weapon_effects seq");
    assert!(ctx.systems.stop_system(sys_id).await);
}
