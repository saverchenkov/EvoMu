//! Unit and mutation-killing tests for `sp6_lifecycle::cluster_tick`
//! (`populate_persisted_space_objects`, `seed_persisted_abandoned_drones`,
//! `collapse_stranded_abyssal_if_needed_sync`, `on_advance_secs`, and `tick_background_cluster`).

use super::*;
use crate::router::ServerContext;
use crate::sp6_lifecycle::{
    with_mut_sp6_state, AbyssalPersistedSession, DisconnectedShipEntry, PersistedCombatHp,
    PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::Ballpark;
use std::sync::Arc;

#[test]
fn populate_persisted_space_objects_materializes_and_prunes_expired_entities() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142;
    let now_unix = ctx.now_unix();

    let st_active_id = 1_000_000_900_001;
    let st_destroyed_id = 1_000_000_900_002;
    let wreck_valid_id = 810_001;
    let wreck_expired_id = 810_002;
    let jetcan_expired_id = 810_003;
    let pi_expired_id = 810_004;
    let bubble_anchoring_id = 810_005;
    let bubble_anchored_id = 810_006;
    let disc_active_ship = 810_007;
    let disc_expired_ship = 810_008;

    let mwd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12198);

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.structures.insert(
            st_active_id,
            UpwellStructureRecord {
                structure_id: st_active_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Jita Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 0.0, 0.0],
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                quantum_core_installed: true,
                fuel_blocks: 1000,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            st_destroyed_id,
            UpwellStructureRecord {
                structure_id: st_destroyed_id,
                state: "Destroyed".into(),
                ..snap.structures[&st_active_id].clone()
            },
        );

        for (iid, tid, pos) in [
            (wreck_valid_id, 26557, [1_000.0, 0.0, 0.0]),
            (wreck_expired_id, 26557, [2_000.0, 0.0, 0.0]),
            (jetcan_expired_id, 23, [3_000.0, 0.0, 0.0]),
            (pi_expired_id, 23, [4_000.0, 0.0, 0.0]),
            (bubble_anchoring_id, mwd_tid, [5_000.0, 0.0, 0.0]),
            (bubble_anchored_id, mwd_tid, [25_000.0, 0.0, 0.0]),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(iid, pos);
        }

        st.space_object_meta.insert(
            wreck_valid_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix),
                ..Default::default()
            },
        );
        st.ship_combat_hp.insert(
            wreck_valid_id,
            PersistedCombatHp {
                shield_hp: 10.0,
                armor_hp: 20.0,
                hull_hp: 30.0,
            },
        );
        st.space_object_meta.insert(
            wreck_expired_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix.saturating_sub(WRECK_DECAY_LIFETIME_SECS + 10)),
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            jetcan_expired_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix.saturating_sub(JETCAN_DECAY_LIFETIME_SECS + 10)),
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            pi_expired_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix.saturating_sub(PLANETARY_LAUNCH_DECAY_SECS + 10)),
                planet_id: Some(40_009_082),
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            bubble_anchoring_id,
            PersistedSpaceObjectMeta {
                anchor_start_unix: Some(now_unix),
                anchoring_delay_secs: 60.0,
                anchor_completed: false,
                bubble_radius_m: Some(20_000.0),
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            bubble_anchored_id,
            PersistedSpaceObjectMeta {
                anchor_start_unix: Some(now_unix.saturating_sub(120)),
                anchoring_delay_secs: 60.0,
                anchor_completed: true,
                bubble_radius_m: Some(20_000.0),
                ..Default::default()
            },
        );

        st.disconnected_ships.insert(
            90_000_001,
            DisconnectedShipEntry {
                char_id: 90_000_001,
                ship_id: disc_active_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                last_position: [8_000.0, 0.0, 0.0],
                pre_warp_position: Some([8_000.0, 0.0, 0.0]),
                emergency_warp_dest: Some([100_000.0, 0.0, 0.0]),
                removed_from_space: false,
            },
        );
        st.disconnected_ships.insert(
            90_000_002,
            DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: disc_expired_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix.saturating_sub(120),
                remove_at_unix: now_unix.saturating_sub(10),
                last_position: [9_000.0, 0.0, 0.0],
                pre_warp_position: Some([9_000.0, 0.0, 0.0]),
                emergency_warp_dest: Some([50_000.0, 0.0, 0.0]),
                removed_from_space: false,
            },
        );
    });

    let mut ballpark = Ballpark::new(sys_id);
    let pending = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, bubble_anchoring_id);

    assert!(ballpark.balls.contains_key(&st_active_id));
    assert!(!ballpark.balls.contains_key(&st_destroyed_id));
    assert!(ballpark.balls.contains_key(&wreck_valid_id));
    assert!(!ballpark.balls.contains_key(&wreck_expired_id));
    assert!(!ballpark.balls.contains_key(&jetcan_expired_id));
    assert!(!ballpark.balls.contains_key(&pi_expired_id));
    assert!(ballpark.balls.contains_key(&bubble_anchored_id));
    assert!(ballpark.balls.contains_key(&disc_active_ship));
    assert!(!ballpark.balls.contains_key(&disc_expired_ship));
}

#[test]
fn seed_persisted_abandoned_drones_populates_drones_and_fighters() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let drone_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 18)
        .map(|t| t.type_id)
        .unwrap_or(2454);
    let fighter_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 87)
        .map(|t| t.type_id)
        .unwrap_or(40556);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            820_001,
            InventoryItemRecord {
                item_id: 820_001,
                type_id: drone_tid,
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            820_002,
            InventoryItemRecord {
                item_id: 820_002,
                type_id: fighter_tid,
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 3,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    seed_persisted_abandoned_drones(&ctx);
    let has_drone = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.contains_key(&820_001));
    assert!(has_drone);
    assert!(ctx.active_fighters.read().unwrap().contains_key(&820_002));
}

#[tokio::test]
async fn on_advance_secs_collapse_abyssal_and_tick_background_cluster() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let now_unix = ctx.now_unix();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;

    // 1. Safe logoff completion and Zarzakh gate lock expiry via on_advance_secs
    with_mut_sp6_state(&ctx.db, |st, snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix.saturating_sub(35),
                complete_at_unix: now_unix.saturating_sub(5),
                completed: false,
            },
        );
        snap.zarzakh_gate_locks.insert(char_id, (50_000_001, now_unix));
    });

    on_advance_secs(&ctx, now_unix, 1);

    let completed = super::super::with_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.get(&char_id).map(|e| e.completed).unwrap_or(false)
    });
    assert!(completed);
    let lock_cleared = ctx.db.with_snapshot(|s| !s.zarzakh_gate_locks.contains_key(&char_id));
    assert!(lock_cleared);

    // 2. collapse_stranded_abyssal_if_needed_sync on Void Space session
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.abyssal_sessions.insert(
            char_id,
            AbyssalPersistedSession {
                char_id,
                ship_id,
                pocket_system_id: crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID,
                origin_system_id: sys_id,
                entered_unix: now_unix.saturating_sub(
                    crate::services::abyssal_void_mgr::VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS,
                ),
                collapse_at_unix: now_unix,
            },
        );
    });
    collapse_stranded_abyssal_if_needed_sync(&ctx, char_id, now_unix);
    let has_ab = super::super::with_sp6_state(&ctx.db, |st| st.abyssal_sessions.contains_key(&char_id));
    assert!(!has_ab);

    // 3. Self-destruct execution on due entry
    let target_ship_id = 830_001;
    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = None;
            ch2.solar_system_id = sys_id;
            ch2.active_ship_id = target_ship_id;
        }
        st.self_destructs.insert(
            target_ship_id,
            SelfDestructPersistedEntry {
                ship_id: target_ship_id,
                char_id: 90_000_002,
                solar_system_id: sys_id,
                started_unix: now_unix.saturating_sub(120),
                detonate_at_unix: now_unix,
            },
        );
    });
    on_advance_secs(&ctx, now_unix, 1);
    let sd_left = super::super::with_sp6_state(&ctx.db, |st| st.self_destructs.contains_key(&target_ship_id));
    assert!(!sd_left);

    // 4. Full background cluster tick
    tick_background_cluster(&ctx).await;
    let tick_recorded = ctx
        .db
        .with_snapshot(|s| s.service_state_blobs.contains_key("cluster_tick_last_unix"));
    assert!(tick_recorded);
}

#[tokio::test]
async fn cold_boot_inside_downtime_window_does_not_shutdown_but_day_crossing_does() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    // 2026-01-01 00:00:00 UTC + downtime window offset + 120s (11:02 UTC)
    let base_day_start = 1_767_225_600_u64;
    let in_window_unix = base_day_start + dt_sec + 120;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        in_window_unix,
    )));
    assert!(crate::services::serverops_cluster::is_scheduled_downtime_enabled(&ctx));

    let char_id = 90_000_001_i64;
    let _ = ctx.db.acquire_online_lock(1, 9_001, Some(char_id), false, 0);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Cold boot: last_downtime_day_idx starts as None even inside 11:00..11:15 UTC.
    let initial_day_idx = super::super::with_sp6_state(&ctx.db, |st| st.last_downtime_day_idx);
    assert_eq!(initial_day_idx, None);

    tick_background_cluster(&ctx).await;

    let expected_day_idx = in_window_unix.saturating_sub(dt_sec) / 86_400;
    let after_cold_boot_idx =
        super::super::with_sp6_state(&ctx.db, |st| st.last_downtime_day_idx);
    assert_eq!(after_cold_boot_idx, Some(expected_day_idx));
    assert!(
        !ctx.is_shutting_down(),
        "cold boot inside 11:00..11:15 UTC must NOT trigger downtime shutdown"
    );
    assert!(
        !ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnClusterShutdownInitiated"),
        "cold boot inside 11:00..11:15 UTC must NOT emit OnClusterShutdownInitiated"
    );

    // Advance 24h to next day's 11:02 UTC -> crossing day boundary triggers scheduled downtime.
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;

    let after_crossing_idx =
        super::super::with_sp6_state(&ctx.db, |st| st.last_downtime_day_idx);
    assert_eq!(after_crossing_idx, Some(expected_day_idx + 1));
    assert!(
        ctx.is_shutting_down(),
        "day crossing into 11:00..11:15 UTC must trigger start_daily_downtime_window"
    );
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnClusterShutdownInitiated"),
        "day crossing into 11:00..11:15 UTC must emit OnClusterShutdownInitiated"
    );
}

#[tokio::test]
async fn idle_cluster_tick_avoids_per_tick_sqlite_wal_writes() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_idle_tick_wal_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("world.sqlite");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    let sde_path = std::env::var("EVOMU_SDE_PATH").unwrap_or_else(|_| "data/evomu_sde.sqlite".into());
    let sde = Arc::new(eve_sde::SdeRegistry::open(&sde_path).unwrap_or_default());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_770_000_010)));
    ctx.seed_demo_world_fixtures();
    // Place demo char in space in Maurasi (30000140) with active Venture (32880) like Batch P
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_000_140;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    let _jita = ctx.systems.get_or_spawn(30_000_142);
    let maurasi = ctx.systems.get_or_spawn(30_000_140);
    let _ = maurasi
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 32880,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 3.6,
            mass: 1_200_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    // Warm up initial ambient spawns / daily timers across two ticks
    for _ in 0..2 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }

    let wal_before = ctx.db.with_snapshot(|s| s.wal_seq);
    for _ in 0..5 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }
    let wal_after = ctx.db.with_snapshot(|s| s.wal_seq);
    assert_eq!(
        wal_after.saturating_sub(wal_before),
        0,
        "steady-state idle cluster ticks must not perform SQLite WAL writes every second"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cluster_tick_catches_up_multi_second_warp_and_respects_dedicated_physics_ticker() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // 1. When dedicated_physics_ticker_active is true, tick_background_cluster skips physics
    // and step_active_systems_physics_tick owns simulation advancement.
    let tick_initial = sys_h.ballpark.read().unwrap().current_tick;
    ctx.dedicated_physics_ticker_active
        .store(true, std::sync::atomic::Ordering::Release);
    ctx.clock().advance_secs(5);
    tick_background_cluster(&ctx).await;
    let tick_after_bg = sys_h.ballpark.read().unwrap().current_tick;
    assert_eq!(
        tick_after_bg, tick_initial,
        "tick_background_cluster must not step physics when dedicated physics ticker is active"
    );
    crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(sys_h.ballpark.read().unwrap().current_tick, tick_initial + 1);

    // 2. step_active_systems_physics_tick catches up multi-second wall-clock deltas so an
    // in-flight warp finishes strictly on schedule independent of background cluster ticks.
    let warp_dest = glam::DVec3::new(150_000_000.0, 0.0, 0.0);
    let warp_mode = eve_destiny::MovementMode::new_warp(glam::DVec3::ZERO, warp_dest, 3.0);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: warp_mode,
        })
        .await
        .unwrap();

    ctx.clock().advance_secs(35);
    crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 35).await;

    let tick_after_first_burst = sys_h.ballpark.read().unwrap().current_tick;
    assert_eq!(
        tick_after_first_burst,
        tick_initial + 1 + crate::sp6_lifecycle::MAX_PHYSICS_BACKLOG_STEPS,
        "step_active_systems_physics_tick must cap backlog replay at MAX_PHYSICS_BACKLOG_STEPS (#870)"
    );
    for _ in 0..11 {
        ctx.clock().advance_secs(3);
        crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 3).await;
    }

    let tick_after_catchup = sys_h.ballpark.read().unwrap().current_tick;
    assert!(
        tick_after_catchup >= tick_initial + 36,
        "expected multi-second warp catchup ticks >= {}, got {tick_after_catchup}",
        tick_initial + 36
    );
    let ball = sys_h
        .ballpark
        .read()
        .unwrap()
        .balls
        .get(&ship_id)
        .cloned()
        .expect("ship ball exists");
    assert!(
        matches!(ball.mode, eve_destiny::MovementMode::Stop),
        "ship should have completed warp after 35s catchup, mode={:?}",
        ball.mode
    );
}

#[tokio::test]
async fn sov_claimed_without_sov_hubs_entry_stays_idle_clean_and_advances_indices() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_sov_tick_wal_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("world.sqlite");
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    let sde_path = std::env::var("EVOMU_SDE_PATH").unwrap_or_else(|_| "data/evomu_sde.sqlite".into());
    let sde = Arc::new(eve_sde::SdeRegistry::open(&sde_path).unwrap_or_default());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    // Freeze wall-clock drift and keep the 2 warmup + 5 measured 1 Hz ticks clear of a `% 60 == 0` periodic NPC-mining DB sync.
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_770_000_010)));
    ctx.seed_demo_world_fixtures();

    let claimed_sys = 30_004_759_u32;
    let unclaimed_stale_sys = 30_004_760_u32;
    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        // Positive setup: claimed sovereignty system without any `sov_hubs` Equinox upgrade map entry.
        let st = snap.system_states.entry(claimed_sys).or_default();
        st.sov_alliance_id = Some(99_000_001);
        st.strategic_index = 0;
        st.military_index = 2;
        snap.sov_runtime.sov_npc_kills.insert(claimed_sys, 15);
        snap.sov_runtime.sov_claimed_since_unix.insert(claimed_sys, now0);

        // Negative setup: stale `sov_claimed_since_unix` on an unclaimed system (`sov_alliance_id == None`)
        // must neither advance strategic_index nor trigger per-tick SQLite WAL writes.
        let st_unclaimed = snap.system_states.entry(unclaimed_stale_sys).or_default();
        st_unclaimed.sov_alliance_id = None;
        st_unclaimed.strategic_index = 0;
        snap.sov_runtime
            .sov_claimed_since_unix
            .insert(unclaimed_stale_sys, now0.saturating_sub(30 * 86_400));
    });

    // Warm up initial decay timestamp & daily markers across 2 ticks.
    for _ in 0..2 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }

    // Steady-state 1 Hz ticks with claimed sovereignty must still perform 0 SQLite WAL writes.
    let wal_before = ctx.db.with_snapshot(|s| s.wal_seq);
    for _ in 0..5 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }
    let wal_after = ctx.db.with_snapshot(|s| s.wal_seq);
    assert_eq!(
        wal_after.saturating_sub(wal_before),
        0,
        "steady-state claimed sov without due thresholds must not perform per-tick SQLite WAL writes"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&unclaimed_stale_sys].strategic_index),
        0,
        "unclaimed system must not gain strategic_index from stale sov_claimed_since_unix"
    );

    // Advancing past 7 days must advance `strategic_index` (0 -> 1) and decay inactive military kills (2 -> 0).
    ctx.clock().advance_secs(7 * 86_400 + 10);
    tick_background_cluster(&ctx).await;
    let (strat_after, mil_after, unclaimed_strat) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&claimed_sys].strategic_index,
            s.system_states[&claimed_sys].military_index,
            s.system_states[&unclaimed_stale_sys].strategic_index,
        )
    });
    assert_eq!(strat_after, 1, "claimed sov must advance strategic_index to 1 after 7 days");
    assert_eq!(mil_after, 0, "inactive military kills must decay over 7 days");
    assert_eq!(unclaimed_strat, 0, "unclaimed system must still remain at strategic_index 0");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn idle_1hz_cluster_ticks_do_not_append_wal_snapshots() {
    let dir = std::env::temp_dir();
    let snap_path = dir.join(format!("evomu_idle_cluster_tick_{}.json", std::process::id()));
    let wal_path = snap_path.with_extension("json.wal");
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);

    let repo = Arc::new(
        eve_db::WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone()))
            .expect("init json repo"),
    );
    let ctx = ServerContext::with_repository(repo);
    let base_unix = 1_770_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix)));

    // Seed structures (including ACL profile state, online Metenox Moon Drill, active Moon Extraction,
    // Sovereignty Hub), an active industry job, and an active non-expired safe-logoff timer.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_000_101,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_000_101,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Idle Astrahus".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_000_000_000_102,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_000_102,
                type_id: 81826,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_002_537,
                name: "Idle Metenox".into(),
                position: [2_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                ..Default::default()
            },
        );
        snap.auto_moon_miners.insert(
            1_000_000_000_102,
            eve_db::AutoMoonMinerRecord {
                structure_id: 1_000_000_000_102,
                fuel_units_remaining: 500,
                magmatic_gas_remaining: 5_000,
                efficiency: 0.4,
                cycle_duration_seconds: 3_600,
                last_cycle_unix: base_unix,
                ore_type_id: 45490,
                hourly_yield: 1_000,
                status: "Active".into(),
            },
        );
        snap.moon_extractions.insert(
            1_000_000_000_101,
            eve_db::MoonExtractionRecord {
                structure_id: 1_000_000_000_101,
                moon_id: 40_009_083,
                extraction_start_unix: base_unix,
                chunk_arrival_unix: base_unix + 86_400,
                natural_decay_unix: base_unix + 172_800,
                status: "Started".into(),
                auto_fracture: true,
                event_id: 1,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: Vec::new(),
            },
        );
        let mut ext = crate::services::station_structure::structure_upwell_ops::StructureExtendedState::default();
        let mut prof = crate::services::station_structure::structure_upwell_ops::StructureAccessProfile::default();
        prof.profile_id = 1;
        prof.allowed_characters.extend([90_000_001, 90_000_002, 90_000_003]);
        prof.blocked_characters.extend([90_000_010, 90_000_011]);
        ext.access_profiles.insert(1_000_000_000_101, prof);
        ext.lifecycle.insert(
            1_000_000_000_101,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                last_fuel_tick_unix: Some(base_unix),
                ..Default::default()
            },
        );
        ext.lifecycle.insert(
            1_000_000_000_102,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                last_fuel_tick_unix: Some(base_unix),
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);

        snap.industry_jobs.insert(
            610_500,
            eve_db::IndustryJobRecord {
                job_id: 610_500,
                installer_id: 90_000_001,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_500,
                blueprint_type_id: 683,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix: base_unix,
                ..Default::default()
            },
        );
        snap.sp6_lifecycle.abyssal_sessions.insert(
            90_000_001,
            AbyssalPersistedSession {
                char_id: 90_000_001,
                ship_id: 100_000_001,
                pocket_system_id: 32_000_001,
                origin_system_id: 30_000_142,
                entered_unix: base_unix,
                collapse_at_unix: base_unix + 1_200,
            },
        );
    });

    // Run 1 initial cold-boot tick to initialize once-per-boot / once-per-day fields, then checkpoint.
    tick_background_cluster(&ctx).await;
    ctx.db.save_snapshot_to_file(&snap_path).expect("checkpoint after cold-boot tick");
    assert!(!wal_path.exists());

    let seq_after_warmup = ctx.db.with_snapshot(|s| s.wal_seq);
    let epoch_after_warmup = ctx.db.state_epoch();

    // Advance 10 consecutive 1 Hz ticks: zero `.json.wal` writes, zero `wal_seq` bumps, zero `state_epoch` bumps.
    for _ in 0..10 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }

    assert!(
        !wal_path.exists(),
        "idle 1 Hz cluster ticks must not append full-snapshot or no-op records to .json.wal"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.wal_seq), seq_after_warmup);
    assert_eq!(ctx.db.state_epoch(), epoch_after_warmup);

    drop(ctx);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}

#[tokio::test]
async fn sweep_expired_cosmic_anomalies_and_execute_due_abyssal_collapse_sync_branches() {
    use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord};

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let now_unix = 2_000_000_u64;

    // 0-step background tick is a no-op.
    tick_background_cluster_with_step(&ctx, 0).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-EXPIRED".into(),
                    solar_system_id: sys_id,
                    name: "Expired Hideaway".into(),
                    anomaly_type: "Combat".into(),
                    position: [1_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-ICE".into(),
                    solar_system_id: sys_id,
                    name: "Ice Belt".into(),
                    anomaly_type: "IceBelt".into(),
                    position: [2_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-ORE".into(),
                    solar_system_id: sys_id,
                    name: "Ore Belt".into(),
                    anomaly_type: "OreAnomaly".into(),
                    position: [3_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-INF".into(),
                    solar_system_id: sys_id,
                    name: "Permanent".into(),
                    anomaly_type: "Combat".into(),
                    position: [4_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_000,
                    lifetime_seconds: 0,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-FRESH".into(),
                    solar_system_id: sys_id,
                    name: "Fresh".into(),
                    anomaly_type: "Combat".into(),
                    position: [5_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: now_unix - 100,
                    lifetime_seconds: 500,
                    cleared: false,
                },
            ],
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: "SIG-EXPIRED".into(),
                    solar_system_id: sys_id,
                    sig_group: "Relic".into(),
                    type_name: "Ruins".into(),
                    position: [1_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-WH".into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "K162".into(),
                    position: [2_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-GAS".into(),
                    solar_system_id: sys_id,
                    sig_group: "Gas".into(),
                    type_name: "Gas Cloud".into(),
                    position: [3_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1_000,
                    lifetime_seconds: 500,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );
    });

    sweep_expired_cosmic_anomalies_and_signatures(&ctx, now_unix);
    ctx.db.with_snapshot(|snap| {
        let anoms: Vec<&str> = snap.cosmic_anomalies[&sys_id]
            .iter()
            .map(|a| a.anomaly_id.as_str())
            .collect();
        assert_eq!(anoms, vec!["ANO-ICE", "ANO-ORE", "ANO-INF", "ANO-FRESH"]);
        let sigs: Vec<&str> = snap.cosmic_signatures[&sys_id]
            .iter()
            .map(|s| s.signature_id.as_str())
            .collect();
        assert_eq!(sigs, vec!["SIG-WH", "SIG-GAS"]);
    });

    // `execute_due_abyssal_collapse_sync` Void Space branch vs regular Abyssal branch.
    let void_sess = AbyssalPersistedSession {
        char_id: 90_000_001,
        ship_id: 100_000_001,
        pocket_system_id: crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID,
        origin_system_id: sys_id,
        entered_unix: 1_000,
        collapse_at_unix: 1_500,
    };
    execute_due_abyssal_collapse_sync(&ctx, &void_sess, now_unix);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&90_000_001].solar_system_id),
        sys_id
    );
}

#[tokio::test]
async fn engine_supervision_catch_unwind_system_actor_recovery_and_backlog_cap_841_870_909() {
    use std::sync::atomic::Ordering;

    // 1. `catch_unwind_future` catches panics across async poll steps (#841).
    let ok_res = catch_unwind_future(async { 42_u32 }).await;
    assert_eq!(ok_res.unwrap(), 42);
    let panic_res = catch_unwind_future(async {
        tokio::task::yield_now().await;
        panic!("simulated tick loop panic");
    })
    .await;
    assert!(panic_res
        .unwrap_err()
        .contains("simulated tick loop panic"));

    // 2. `clamp_physics_ticker_backlog` caps backlog replay at MAX_PHYSICS_BACKLOG_STEPS = 3 (#870).
    assert_eq!(clamp_physics_ticker_backlog(0), 1);
    assert_eq!(clamp_physics_ticker_backlog(1), 1);
    assert_eq!(clamp_physics_ticker_backlog(2), 2);
    assert_eq!(clamp_physics_ticker_backlog(3), 3);
    assert_eq!(clamp_physics_ticker_backlog(10), 3);

    // 3. `SystemActor` catches per-command panics and stays alive for subsequent commands (#841).
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let handle = ctx.systems.get_or_spawn(sys_id);
    assert_eq!(handle.command_panics.load(Ordering::SeqCst), 0);
    handle
        .panic_next_command_for_test
        .store(true, Ordering::SeqCst);
    // Trigger a command that will panic inside the actor's command handler.
    let _ = handle.step_manual(0.1).await;
    assert_eq!(handle.command_panics.load(Ordering::SeqCst), 1);
    assert!(!handle.cmd_tx.is_closed());
    // Subsequent command succeeds on the same actor!
    let step_after = handle.step_manual(0.5).await.expect("actor recovers after command panic");
    assert!(step_after.tick > 0);

    // 4. `SystemRegistry::get_or_spawn` replaces a closed `SystemHandle` (#841).
    let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
    drop(closed_rx);
    {
        let mut reg = ctx
            .systems
            .systems
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut dead_handle = handle.clone();
        dead_handle.cmd_tx = closed_tx;
        reg.insert(sys_id, dead_handle);
    }
    assert!(ctx.systems.get_handle(sys_id).is_none());
    let respawned = ctx.systems.get_or_spawn(sys_id);
    assert!(ctx.systems.get_handle(sys_id).is_some());
    assert!(!respawned.cmd_tx.is_closed());
    let step_respawned = respawned
        .step_manual(0.5)
        .await
        .expect("respawned actor handles commands");
    assert!(step_respawned.tick > 0);

    // 5. When `dedicated_physics_ticker_active` is true, `cluster_tick` does NOT double-step
    // `disconnected_ships` systems (#870).
    let dedicated_ctx =
        ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    dedicated_ctx
        .dedicated_physics_ticker_active
        .store(true, Ordering::Relaxed);
    let disc_sys = 30_002_187_u32;
    let _disc_handle = dedicated_ctx.systems.get_or_spawn(disc_sys);
    let sim_before = dedicated_ctx.systems.get_system_sim_elapsed(disc_sys);
    let now_unix = dedicated_ctx.now_unix();
    with_mut_sp6_state(&dedicated_ctx.db, |st, _snap| {
        st.disconnected_ships.insert(
            90_000_001,
            DisconnectedShipEntry {
                char_id: 90_000_001,
                ship_id: 100_000_001,
                ship_type_id: 606,
                solar_system_id: disc_sys,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                last_position: [1_000.0, 0.0, 0.0],
                pre_warp_position: Some([1_000.0, 0.0, 0.0]),
                emergency_warp_dest: None,
                removed_from_space: false,
            },
        );
    });
    tick_background_cluster_with_step(&dedicated_ctx, 1).await;
    assert_eq!(
        dedicated_ctx.systems.get_system_sim_elapsed(disc_sys),
        sim_before,
        "cluster_tick must not double-step disconnected_systems when dedicated_physics_ticker_active is true"
    );
}

#[tokio::test]
async fn committed_warp_mirror_non_blocking_under_ballpark_write_lock_831_909() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let handle = ctx.systems.get_or_spawn(sys_id);

    handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::ZERO,
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: eve_destiny::MovementMode::Stop,
            speed_fraction: 1.0,
        })
        .await
        .unwrap();
    // Start warp to a distant point (>150 km) in Accelerating phase (committed warp).
    let dest = eve_destiny::DVec3::new(10_000_000.0, 0.0, 0.0);
    handle
        .set_movement_mode(
            ship_id,
            eve_destiny::MovementMode::Warp {
                origin: eve_destiny::DVec3::ZERO,
                destination: dest,
                warp_speed_au_s: 3.0,
                total_distance: 10_000_000.0,
                phase: eve_destiny::WarpPhase::Accelerating,
                elapsed_warp_time: 1.0,
            },
        )
        .await
        .unwrap();
    let _ = handle.step_manual(0.5).await.unwrap();
    assert!(ctx.systems.is_ball_in_committed_warp_sync(sys_id, ship_id));

    // Hold `ballpark.write()` on the current thread and verify `is_ball_in_committed_warp_sync`
    // and `committed_warp_destination_sync` read the leaf mirror lock without deadlocking (#831)!
    let bp_write_guard = handle.ballpark.write().unwrap();
    assert!(ctx.systems.is_ball_in_committed_warp_sync(sys_id, ship_id));
    assert_eq!(
        handle.committed_warp_destination_sync(ship_id),
        Some(dest)
    );
    drop(bp_write_guard);
}

#[tokio::test]
async fn skill_queue_invalidation_only_on_level_completion_and_single_system_sync_840_909() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let char_sys = 30_000_142_u32;
    let other_sys = 30_002_187_u32;

    // Put character in space in `char_sys` and spawn a second unrelated system `other_sys`.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = char_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: 3300, // Gunnery (rank 1, L1 = 250 SP)
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 600.0,
                started_at_unix: Some(1_700_000_000),
            }],
        );
        snap.skills.entry(char_id).or_default().insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id);
    assert!(ctx
        .ship_fittings
        .read()
        .unwrap()
        .contains_key(&ship_id));

    // Partial SP advance (1 second -> < 250 SP): must NOT invalidate `ship_fittings` (#840).
    ctx.advance_skill_queues(1.0);
    assert!(
        ctx.ship_fittings
            .read()
            .unwrap()
            .contains_key(&ship_id),
        "partial SP tick must not invalidate cached ship fitting"
    );

    // Spawn `other_sys` with a dummy ball of the same ID to verify `sync_ship_ballpark_stats`
    // only updates `char_sys` and does NOT touch `other_sys` (#840).
    let char_sys_handle = ctx.systems.get_or_spawn(char_sys);
    let other_sys_handle = ctx.systems.get_or_spawn(other_sys);
    char_sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::ZERO,
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 111.0,
            inertia: 3.0,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: eve_destiny::MovementMode::Stop,
            speed_fraction: 1.0,
        })
        .await
        .unwrap();
    other_sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::ZERO,
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 999.0,
            inertia: 3.0,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: eve_destiny::MovementMode::Stop,
            speed_fraction: 1.0,
        })
        .await
        .unwrap();

    // Complete the skill level: invalidates `ship_fittings` and syncs only `char_sys`.
    ctx.advance_skill_queues(3_600.0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.skills[&char_id][&3300].level),
        1
    );
    ctx.sync_ship_ballpark_stats(char_id, ship_id);
    let other_max_speed = other_sys_handle
        .ballpark
        .read()
        .unwrap()
        .balls
        .get(&ship_id)
        .unwrap()
        .max_velocity;
    assert_eq!(
        other_max_speed, 999.0,
        "sync_ship_ballpark_stats must only sync the character's active solar system"
    );
}

#[test]
fn social_extras_leaf_lock_and_no_nested_world_snapshots_825_838_891_909() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let char_id = 90_000_001_i64;
    let wh_sys = 31_000_005_u32;
    let wh_chan = format!("solarsystemid2:{wh_sys}");

    // 1. `#825`: `collect_wh_local_witness_speakers_to_drop_in_snap` mutates `wh_local_witnesses`
    // without locking `ctx.social_extras` (even when `ctx.social_extras` is held on the current thread!).
    ctx.social_extras
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .delayed_local_speakers
        .entry(wh_chan.clone())
        .or_default()
        .insert(90_000_002);
    let candidates =
        crate::services::fleet_social::fleet_social_chat::get_delayed_local_speakers(&ctx, &wh_chan);
    assert_eq!(candidates, vec![90_000_002]);

    let social_guard = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
    let dropped = ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops
            .wh_local_witnesses
            .insert(format!("{wh_chan}:90000002"), std::collections::HashSet::from([char_id]));
        crate::services::fleet_social::fleet_social_chat::collect_wh_local_witness_speakers_to_drop_in_snap(
            snap,
            &wh_chan,
            char_id,
            &std::collections::HashSet::new(),
            &candidates,
        )
    });
    drop(social_guard);
    assert_eq!(dropped, vec![90_000_002]);
    crate::services::fleet_social::fleet_social_chat::remove_delayed_local_speakers(
        &ctx, &wh_chan, &dropped,
    );
    assert!(
        crate::services::fleet_social::fleet_social_chat::get_delayed_local_speakers(&ctx, &wh_chan)
            .is_empty()
    );

    // 2. `#838` (server half): `prune_offline_fleets_and_channels` and `resolve_notification_recipients`
    // complete cleanly and prune offline fleet/channel members.
    crate::services::fleet_social::fleet_social_notify::prune_offline_fleets_and_channels(&ctx);
    let recips = ctx.resolve_notification_recipients(&crate::router::NotificationTarget::All);
    assert!(recips.is_empty());

    // 3. `#891`: `resolve_agent_and_corp_for_system_with_preferred_station` does not call
    // `ctx.db.get_character` inside `try_with_mut_snapshot`, so calling it while holding a
    // mutable snapshot does not self-deadlock.
    let preferred_station = ctx
        .db
        .get_character(char_id)
        .ok()
        .and_then(|c| c.station_id);
    let resolved = ctx
        .db
        .try_with_mut_snapshot(|_snap| {
            Ok::<_, std::convert::Infallible>(
                crate::services::agents_missions::resolve_agent_and_corp_for_system_with_preferred_station(
                    &ctx,
                    30_000_142,
                    preferred_station,
                    false,
                ),
            )
        })
        .unwrap();
    assert!(resolved.0 > 0);
}

#[tokio::test]
async fn step_output_condition_checkpoint_epoch_guard_and_dirty_only_919_839_866() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    let epoch_1 = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(item) = snap.items.get_mut(&ship_id) {
            item.location_id = sys_id as i64;
        }
        let ep = crate::sp6_lifecycle::bump_ship_space_entry_epoch_in_snap(snap, ship_id, sys_id);
        crate::sp6_lifecycle::write_ship_condition_in_snap(
            snap,
            ship_id,
            crate::sp6_lifecycle::PersistedShipCondition {
                shield_hp: 500.0,
                armor_hp: 400.0,
                hull_hp: 350.0,
                capacitor: Some(250.0),
                in_space_system: Some(sys_id),
                space_entry_epoch: ep,
            },
        );
        ep
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .spawn_ball_with_combat_state(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::new(1_000.0, 2_000.0, 3_000.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState {
                shield_hp: 500.0,
                max_shield: 500.0,
                shield_recharge_rate_ms: 0.0,
                armor_hp: 400.0,
                max_armor: 400.0,
                structure_hp: 350.0,
                max_structure: 350.0,
                capacitor: 250.0,
                max_capacitor: 250.0,
                capacitor_recharge_rate_ms: 0.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 1. T-5: Step through 10 ticks with zero HP/cap/position changes; the staggered checkpoint tick
    // must emit `Some(vec![])` (0 dirty rows).
    let mut saw_checkpoint_tick = false;
    for _ in 0..10 {
        let out = handle.step_with_output(1.0).await.unwrap();
        if crate::system_actor::is_condition_checkpoint_tick(sys_id, out.tick) {
            saw_checkpoint_tick = true;
            assert_eq!(
                out.condition_checkpoint.as_ref().map(Vec::len),
                Some(0),
                "T-5: unchanged ball must produce 0 dirty checkpoint entries on checkpoint tick"
            );
        } else {
            assert!(
                out.condition_checkpoint.is_none(),
                "non-checkpoint tick without armor/hull change must not emit condition_checkpoint"
            );
        }
    }
    assert!(saw_checkpoint_tick);

    // 2. T-4: Armor damage on a non-10-tick step commits immediately via StepOutput.
    handle
        .modify_combat_state(ship_id, |cs| {
            cs.armor_hp = 275.0;
        })
        .await
        .unwrap();
    let out_armor = handle.step_with_output(1.0).await.unwrap();
    assert!(
        out_armor
            .condition_checkpoint
            .as_ref()
            .is_some_and(|v| v.len() == 1 && v[0].ball_id == ship_id),
        "T-4: armor HP change must emit immediate condition_checkpoint entry"
    );
    let persisted_armor = ctx
        .db
        .with_snapshot(|snap| crate::sp6_lifecycle::read_ship_condition_in_snap(snap, ship_id))
        .unwrap();
    assert!((persisted_armor.armor_hp - 275.0).abs() < 1e-6);

    // 3. T-2: Stale space_entry_epoch or wrong solar_system_id is rejected by apply_condition_checkpoint_for_system_in_db.
    let epoch_2 = ctx.db.with_mut_snapshot(|snap| {
        let ep = crate::sp6_lifecycle::bump_ship_space_entry_epoch_in_snap(snap, ship_id, sys_id);
        crate::sp6_lifecycle::write_ship_condition_in_snap(
            snap,
            ship_id,
            crate::sp6_lifecycle::PersistedShipCondition {
                shield_hp: 500.0,
                armor_hp: 400.0,
                hull_hp: 350.0,
                capacitor: Some(250.0),
                in_space_system: Some(sys_id),
                space_entry_epoch: ep,
            },
        );
        ep
    });
    assert!(epoch_2 > epoch_1);

    let stale_entry = crate::system_actor::ConditionCheckpointEntry {
        ball_id: ship_id,
        space_entry_epoch: epoch_1, // Stale epoch!
        combat_state: Some(eve_destiny::CombatState {
            shield_hp: 10.0,
            armor_hp: 10.0,
            structure_hp: 10.0,
            capacitor: 10.0,
            ..Default::default()
        }),
        position: [99_999.0, 0.0, 0.0],
    };
    let written_stale = crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
        &ctx.db,
        sys_id,
        &[stale_entry.clone()],
    );
    assert_eq!(
        written_stale, 0,
        "T-2: checkpoint entry with stale space_entry_epoch must be rejected"
    );
    let written_wrong_sys = crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
        &ctx.db,
        30_002_187,
        &[crate::system_actor::ConditionCheckpointEntry {
            space_entry_epoch: epoch_2,
            ..stale_entry
        }],
    );
    assert_eq!(
        written_wrong_sys, 0,
        "T-2: checkpoint entry for wrong solar_system_id must be rejected"
    );
    let cond_after = ctx
        .db
        .with_snapshot(|snap| crate::sp6_lifecycle::read_ship_condition_in_snap(snap, ship_id))
        .unwrap();
    assert!((cond_after.armor_hp - 400.0).abs() < 1e-6);
}

#[tokio::test]
async fn system_step_hook_and_npc_death_hook_use_live_server_context_839() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let probe_id = 990_001_111_i64;
    let npc_ball_id = -88_001_i64;
    let spawn_group_id = 777_u32;

    // 1. Seed a live scanner probe in `ctx.active_scanner_probes` and `sp6_lifecycle.active_scanner_probes`.
    let probe = crate::sp6_lifecycle::PersistedScannerProbe {
        probe_id,
        owner_char_id: char_id,
        ship_id: 100_000_001,
        launcher_item_id: 100_000_002,
        probe_type_id: 30013,
        solar_system_id: sys_id,
        position: [1_000.0, 0.0, 0.0],
        destination: [1_000.0, 0.0, 0.0],
        scan_range_au: 4.0,
        state: 1,
        launched_at_unix: 1_700_000_000,
        expires_at_unix: 1_900_000_000,
    };
    ctx.active_scanner_probes
        .write()
        .unwrap()
        .insert(probe_id, probe.clone());
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.active_scanner_probes.insert(probe_id, probe);
    });

    // 2. Seed an NPC entity in `ctx.npc_spawns` for `sys_id`.
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        let mgr = spawns
            .entry(sys_id)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        mgr.entities.insert(
            npc_ball_id,
            crate::combat::npc_ai::NpcEntity {
                ball_id: npc_ball_id,
                type_id: 10001,
                faction_id: 500010,
                state: crate::combat::npc_ai::NpcAiState::Idle,
                spawn_group_id,
                spawn_pos: glam::DVec3::ZERO,
                target_id: None,
                bounty_isk: 1_500.0,
                optimal_range: 5_000.0,
                orbit_range: 2_500.0,
                base_dps: 10.0,
                damage_vector: [2.5; 4],
                last_attack_tick: 0,
                has_shield_rep: false,
                has_armor_rep: false,
            },
        );
    }

    // 3. Apply a StepOutput that destroys `npc_ball_id` and runs `npc_death_hook` + `system_step_hook`.
    let handle = ctx.systems.get_or_spawn(sys_id);
    let step_out = crate::system_actor::StepOutput {
        solar_system_id: sys_id,
        tick: 1,
        dt_sim: 1.0,
        landed_warps: Vec::new(),
        destroyed_balls: vec![(npc_ball_id, Some(char_id))],
        lock_completed_events: Vec::new(),
        target_lost_events: Vec::new(),
        cycle_actions: Vec::new(),
        condition_checkpoint: None,
        broadcast: None,
    };
    handle.apply_step_output(&step_out).await;

    // 4. Verify `ctx.npc_spawns` recorded the respawn timer on the real `ServerContext` (#839)
    // and `active_scanner_probes` in `ctx.db` was NOT wiped by a throwaway shadow ServerContext (#839).
    {
        let spawns = ctx.npc_spawns.read().unwrap();
        let mgr = spawns.get(&sys_id).expect("npc_spawns entry exists");
        assert!(
            !mgr.entities.contains_key(&npc_ball_id),
            "#839: destroyed NPC ball must be removed from live ctx.npc_spawns"
        );
        assert!(
            mgr.respawn_timers.contains_key(&(sys_id, spawn_group_id)),
            "#839: live ctx.npc_spawns must record respawn timer when NPC dies in step hook"
        );
    }
    ctx.persist_server_runtime_to_db();
    let probe_still_persisted =
        super::super::with_sp6_state(&ctx.db, |st| st.active_scanner_probes.contains_key(&probe_id));
    assert!(
        probe_still_persisted,
        "#839: persisted scanner probes must not be clobbered after system_step_hook"
    );
}

#[tokio::test]
async fn system_hibernation_zero_idle_tick_work_and_eviction_919_866_869() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let idle_sys = 30_002_187_u32; // Amarr (no characters in demo seed)
    let idle_handle = ctx.systems.get_or_spawn(idle_sys);

    // 1. Empty system with no characters, no dynamic balls, and no subscribers starts hibernated (#919, #866).
    assert!(
        idle_handle.is_hibernated(),
        "empty system with no dynamic balls or sessions must hibernate"
    );
    assert!(idle_handle.hibernated_since_unix() > 0);
    let tick_before = idle_handle.ballpark.read().unwrap().current_tick;

    // 2. Physics tick skips hibernated empty systems (0 per-tick physics work) and records idle ticks.
    crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(
        idle_handle.ballpark.read().unwrap().current_tick,
        tick_before,
        "#919: hibernated empty system must do zero per-tick physics work"
    );
    assert!(idle_handle.idle_ticks() >= 1);

    // 3. Spawning a dynamic ball wakes the system out of hibernation immediately.
    let temp_ball_id = 777_888_999_i64;
    idle_handle
        .spawn_ball(eve_destiny::Ball {
            id: temp_ball_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    assert!(
        !idle_handle.is_hibernated(),
        "#919: spawning a dynamic ball must wake the system from hibernation"
    );
    crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(
        idle_handle.ballpark.read().unwrap().current_tick,
        tick_before + 1,
        "awake system must advance physics tick"
    );

    // 4. Removing the last dynamic ball returns the system to hibernation, and evict_idle_systems evicts it.
    idle_handle.remove_ball(temp_ball_id).await.unwrap();
    assert!(
        idle_handle.is_hibernated(),
        "removing the last dynamic ball must return empty system to hibernation"
    );
    drop(idle_handle);
    let evicted = ctx
        .systems
        .evict_idle_systems(&ctx.db, ctx.now_unix().saturating_add(600), 300);
    assert!(
        evicted.contains(&idle_sys),
        "#919/#866: evict_idle_systems must evict hibernated empty system past threshold"
    );
    assert!(ctx.systems.get_handle(idle_sys).is_none());

    // 5. #869: npc_spawns entry for an active system is updated in place without remove->await->insert.
    let active_sys = 30_000_142_u32;
    let _ = ctx.systems.get_or_spawn(active_sys);
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        let mgr = spawns
            .entry(active_sys)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        mgr.respawn_timers.insert((active_sys, 42_424), 9_999_999_999);
    }
    ctx.clock().advance_secs(1);
    tick_background_cluster(&ctx).await;
    let preserved_timer = ctx
        .npc_spawns
        .read()
        .unwrap()
        .get(&active_sys)
        .and_then(|mgr| mgr.respawn_timers.get(&(active_sys, 42_424)).copied());
    assert_eq!(
        preserved_timer,
        Some(9_999_999_999),
        "#869: npc_spawns state must never be dropped or overwritten across cluster tick awaits"
    );
}
