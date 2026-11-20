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

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.sp6_lifecycle.as_inner_mut();
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
                last_velocity: [0.0, 0.0, 0.0],
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
                last_velocity: [0.0, 0.0, 0.0],
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
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.sp6_lifecycle.as_inner_mut();
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
    with_mut_sp6_state(&ctx.db, |st| {
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
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.sp6_lifecycle.as_inner_mut();
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
    let sde_path = eve_sde::SdeRegistry::default_db_path();
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
    let sde_path = eve_sde::SdeRegistry::default_db_path();
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
    let db_path = dir.join(format!("evomu_idle_cluster_tick_{}.sqlite", std::process::id()));
    let db_wal = std::path::PathBuf::from(format!("{}-wal", db_path.display()));
    let db_shm = std::path::PathBuf::from(format!("{}-shm", db_path.display()));
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&db_wal);
    let _ = std::fs::remove_file(&db_shm);

    let repo = Arc::new(
        eve_db::WorldRepository::open_sqlite(&db_path)
            .expect("init sqlite repo"),
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

    // Run 1 initial cold-boot tick to initialize once-per-boot / once-per-day fields before measuring idle ticks.
    tick_background_cluster(&ctx).await;

    let commits_after_warmup = ctx.db.tx_commit_count();
    let seq_after_warmup = ctx.db.with_snapshot(|s| s.wal_seq);
    let epoch_after_warmup = ctx.db.state_epoch();

    // Advance 10 consecutive 1 Hz ticks: zero SQLite tx commits, zero `wal_seq` bumps, zero `state_epoch` bumps.
    for _ in 0..10 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }

    assert_eq!(
        ctx.db.tx_commit_count(),
        commits_after_warmup,
        "idle 1 Hz cluster ticks must not commit empty or no-op SQLite transactions"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.wal_seq), seq_after_warmup);
    assert_eq!(ctx.db.state_epoch(), epoch_after_warmup);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&db_wal);
    let _ = std::fs::remove_file(&db_shm);
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
    dedicated_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = disc_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = 100_000_001;
        }
    });
    with_mut_sp6_state(&dedicated_ctx.db, |st| {
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
                last_velocity: [0.0, 0.0, 0.0],
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

    let init_cs = eve_destiny::CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        shield_recharge_time_ms: 0.0,
        armor_hp: 400.0,
        max_armor_hp: 400.0,
        structure_hp: 350.0,
        max_structure_hp: 350.0,
        capacitor: 250.0,
        max_capacitor: 250.0,
        capacitor_recharge_time_ms: 0.0,
        ..Default::default()
    };
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
        crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            Some(&init_cs),
            Some(sys_id),
            Some([1_000.0, 2_000.0, 3_000.0]),
        )
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .spawn_ball_with_combat(
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
            init_cs.clone(),
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
        crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            Some(&init_cs),
            Some(sys_id),
            Some([1_000.0, 2_000.0, 3_000.0]),
        )
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
    let probe = eve_db::PersistedScannerProbe {
        probe_id,
        probe_type_id: 30013,
        owner_char_id: char_id,
        ship_id: 100_000_001,
        launcher_module_id: 100_000_002,
        solar_system_id: sys_id,
        position: [1_000.0, 0.0, 0.0],
        destination: [1_000.0, 0.0, 0.0],
        range_step: 4,
        scan_range_m: 4.0 * 149_597_870_700.0,
        base_sensor_strength: 40.0,
        max_flight_time_s: 3_600.0,
        launched_at_unix: 1_700_000_000,
        state: 1,
        warp_end_ft: None,
    };
    ctx.active_scanner_probes
        .write()
        .unwrap()
        .insert(probe_id, probe.clone());
    with_mut_sp6_state(&ctx.db, |st| {
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
            crate::combat::npc_ai::NpcCombatEntity {
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
                damage_vector: eve_destiny::DamageVector::new(2.5, 2.5, 2.5, 2.5),
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
        destroyed_lease: None,
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
    ctx.clock().advance_secs(600);
    let evicted = ctx.systems.evict_idle_systems(300).await;
    assert!(
        evicted.contains(&idle_sys),
        "#919/#866: evict_idle_systems must evict hibernated empty system past threshold"
    );
    assert!(ctx.systems.get_handle(idle_sys).is_none());

    // 5. GH-869: npc_spawns entry for an active system is updated in place without remove->await->insert,
    // including under concurrent handle_npc_destroyed / map mutations while planning/executing belt rats.
    let active_sys = 30_000_142_u32;
    let _ = ctx.systems.get_or_spawn(active_sys);
    let concurrent_sys = 31_000_001_u32;
    let _ = ctx.systems.get_or_spawn(concurrent_sys);
    let now_unix = ctx.now_unix();
    let concurrent_npc_ball_id = 9_990_001_i64;
    let concurrent_spawn_group_id = 77_777_u32;
    let concurrent_bounty_npc = crate::combat::npc_ai::NpcCombatEntity {
        ball_id: concurrent_npc_ball_id,
        type_id: 587,
        faction_id: 500_001,
        state: crate::combat::npc_ai::NpcAiState::Idle,
        spawn_group_id: concurrent_spawn_group_id,
        spawn_pos: glam::DVec3::ZERO,
        target_id: None,
        bounty_isk: 1_000.0,
        optimal_range: 5_000.0,
        orbit_range: 3_000.0,
        base_dps: 10.0,
        damage_vector: Default::default(),
        last_attack_tick: 0,
        has_shield_rep: false,
        has_armor_rep: false,
    };
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        let mgr_active = spawns
            .entry(active_sys)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        mgr_active
            .respawn_timers
            .insert((active_sys, 42_424), 9_999_999_999);

        let mgr_wh = spawns
            .entry(concurrent_sys)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        mgr_wh
            .respawn_timers
            .insert((concurrent_sys, 42_424), 9_999_999_999);
        mgr_wh
            .entities
            .insert(concurrent_npc_ball_id, concurrent_bounty_npc);
    }

    // Deterministically suspend execute_belt_rat_plan_detached_with_start_id across its actor await
    // and inject a concurrent handle_npc_destroyed mutation on ctx.npc_spawns before merge.
    let (mut planned_groups, special_specs) = {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        spawns
            .get_mut(&concurrent_sys)
            .expect("concurrent_sys must remain present in npc_spawns")
            .plan_belt_rats_for_system(&ctx, concurrent_sys, false, None, false, false, false)
    };
    if planned_groups.is_empty() {
        let Some((&sg_id, sg)) = ctx.sde.npc_spawn_groups.iter().next() else {
            panic!("SDE must contain at least one npc_spawn_group");
        };
        planned_groups.push((sg_id, sg.clone(), glam::DVec3::ZERO));
    }
    let mut built_specialized = Vec::new();
    let mut concurrent_kill_injected = false;
    let exec_fut =
        crate::combat::npc_ai::NpcSpawnManager::execute_belt_rat_plan_detached_with_start_id(
            &ctx,
            concurrent_sys,
            planned_groups,
            special_specs,
            88_000_100,
            &mut built_specialized,
        );
    tokio::pin!(exec_fut);
    let (_spawned_ids, spawned_entities) = loop {
        tokio::select! {
            biased;
            res = &mut exec_fut => break res,
            _ = std::future::ready(()), if !concurrent_kill_injected => {
                concurrent_kill_injected = true;
                let mut spawns = ctx.npc_spawns.write().unwrap();
                let mgr = spawns
                    .get_mut(&concurrent_sys)
                    .expect("GH-869: concurrent_sys entry must still exist in npc_spawns mid-await");
                mgr.handle_npc_destroyed(
                    &ctx,
                    concurrent_sys,
                    concurrent_npc_ball_id,
                    90_000_001,
                    now_unix,
                )
                .expect("concurrent handle_npc_destroyed succeeds");
            }
        }
    };
    assert!(
        concurrent_kill_injected,
        "execute_belt_rat_plan_detached_with_start_id must yield across actor spawn await"
    );
    assert!(
        !spawned_entities.is_empty(),
        "detached belt rat execution must yield spawned entities to merge"
    );
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        let mgr = spawns
            .entry(concurrent_sys)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        for (ball_id, entity) in &spawned_entities {
            mgr.entities.insert(*ball_id, entity.clone());
        }
    }

    // Also drive tick_background_cluster concurrently with a spawned writer task on npc_spawns.
    ctx.clock().advance_secs(1);
    let ctx_bg = ctx.clone();
    let mut concurrent_writer = tokio::spawn(async move {
        tokio::task::yield_now().await;
        let mut spawns = ctx_bg.npc_spawns.write().unwrap();
        let mgr = spawns
            .entry(active_sys)
            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new);
        mgr.respawn_timers.insert((active_sys, 55_555), 8_888_888_888);
    });
    tokio::join!(tick_background_cluster(&ctx), async {
        let _ = (&mut concurrent_writer).await;
    });

    let spawns_guard = ctx.npc_spawns.read().unwrap();
    let active_mgr = spawns_guard
        .get(&active_sys)
        .expect("active_sys present in npc_spawns");
    assert_eq!(
        active_mgr.respawn_timers.get(&(active_sys, 42_424)).copied(),
        Some(9_999_999_999),
        "GH-869: pre-existing respawn timer must not be dropped across cluster tick awaits"
    );
    assert_eq!(
        active_mgr.respawn_timers.get(&(active_sys, 55_555)).copied(),
        Some(8_888_888_888),
        "GH-869: concurrent mutation injected during cluster tick must survive"
    );

    let wh_mgr = spawns_guard
        .get(&concurrent_sys)
        .expect("concurrent_sys present in npc_spawns");
    assert_eq!(
        wh_mgr.respawn_timers.get(&(concurrent_sys, 42_424)).copied(),
        Some(9_999_999_999),
        "GH-869: existing respawn timer must survive mid-await mutation + merge"
    );
    assert_eq!(
        wh_mgr
            .respawn_timers
            .get(&(concurrent_sys, concurrent_spawn_group_id))
            .copied(),
        Some(now_unix + 1_200),
        "GH-869: concurrent handle_npc_destroyed cooldown must survive post-await merge"
    );
    assert!(
        !wh_mgr.entities.contains_key(&concurrent_npc_ball_id),
        "GH-869: concurrently destroyed NPC must remain removed after post-await merge"
    );
    for (ball_id, _) in &spawned_entities {
        assert!(
            wh_mgr.entities.contains_key(ball_id),
            "GH-869: newly spawned belt rat {ball_id} must be merged after concurrent kill"
        );
    }
}

#[tokio::test]
async fn tick_background_cluster_evicts_idle_system_after_60_idle_ticks_919_866() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let idle_sys = 30_002_187_u32; // Amarr (no characters in demo seed)
    let held_sys = 30_002_659_u32; // Dodixie (no demo chars; externally held handle)
    let held_handle = ctx.systems.get_or_spawn(held_sys);
    assert!(held_handle.is_hibernated());
    {
        let idle_handle = ctx.systems.get_or_spawn(idle_sys);
        assert!(idle_handle.is_hibernated());
    }
    // One idle physics tick marks `idle_ticks >= 1` so cluster_tick does not treat the newly
    // spawned empty system as a cold-boot NPC spawn target on its very first pass.
    crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 1).await;

    // Drive 60 idle background ticks with 1s steps (< 300s hibernation threshold):
    // handle must stay loaded until both idle tick count and hibernation age are met.
    for _ in 0..60 {
        ctx.clock().advance_secs(1);
        tick_background_cluster(&ctx).await;
    }
    let handle_before_age = ctx
        .systems
        .get_handle(idle_sys)
        .expect("system must remain loaded before 300s hibernation timeout expires");
    assert!(handle_before_age.is_hibernated());
    assert!(handle_before_age.idle_ticks() >= 60);
    drop(handle_before_age);

    // Once hibernation age passes 300s and idle_ticks >= 60, tick_background_cluster itself evicts it,
    // while keeping another hibernated system that has < 60 idle_ticks or an externally held SystemHandle.
    let young_sys = 30_002_510_u32; // Rens
    drop(ctx.systems.get_or_spawn(young_sys));

    ctx.clock().advance_secs(300);
    tick_background_cluster(&ctx).await;
    assert!(
        ctx.systems.get_handle(idle_sys).is_none(),
        "W5C3-02: tick_background_cluster must await evict_idle_systems_with_min_ticks(300, 60) and drop idle system"
    );
    assert!(
        ctx.systems.get_handle(held_sys).is_some(),
        "W5C3R-02: system with externally held SystemHandle (strong_count > 3) must not be evicted"
    );
    assert!(
        ctx.systems.get_handle(young_sys).is_some(),
        "W5C3R-02: system with < 60 idle_ticks must not be evicted when another system triggers eviction"
    );
    drop(held_handle);
}

#[test]
fn server_context_weak_upgrade_shares_live_config_and_catalog_arcs_without_clones_839() {
    let mut cfg = crate::config::ServerConfig::default();
    cfg.storage.backend = "memory".to_string();
    cfg.feature_flags.enable_living_world = false;
    cfg.feature_flags.starter_plex_balance = 777;
    cfg.feature_flags.free_fast_checkout_plex = true;
    cfg.esi_port = 26_199;
    cfg.xmpp_port = 25_199;

    let ctx = ServerContext::from_config(&cfg).expect("from_config succeeds");
    let weak = ctx.downgrade();

    // Live updates to operator config must be observed immediately by subsequent Weak upgrades.
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .starter_plex_balance = 888;
    ctx.set_esi_port(26_299);
    ctx.set_xmpp_port(25_299);

    let upgraded1 = weak.upgrade().expect("weak upgrades while ServerContext lives");
    let upgraded2 = weak.upgrade().expect("weak upgrades again");

    assert!(Arc::ptr_eq(&ctx.cataloged_services, &upgraded1.cataloged_services));
    assert!(Arc::ptr_eq(&ctx.quasar_domains, &upgraded1.quasar_domains));
    assert!(Arc::ptr_eq(&ctx.feature_flags, &upgraded1.feature_flags));
    assert!(Arc::ptr_eq(
        &upgraded1.cataloged_services,
        &upgraded2.cataloged_services
    ));
    assert!(Arc::ptr_eq(&upgraded1.quasar_domains, &upgraded2.quasar_domains));

    let flags = upgraded1.feature_flags_snapshot();
    assert!(!flags.enable_living_world);
    assert!(flags.free_fast_checkout_plex);
    assert_eq!(flags.starter_plex_balance, 888);
    assert_eq!(upgraded1.esi_port(), 26_299);
    assert_eq!(upgraded1.xmpp_port(), 25_299);

    drop(upgraded1);
    drop(upgraded2);
    drop(ctx);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn tick_delta_restart_gm_advance_overrun_and_backward_clock_avoid_double_skill_credit() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_tick_delta_skill_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("world.sqlite");
    let sde_path = eve_sde::SdeRegistry::default_db_path();
    let sde = Arc::new(eve_sde::SdeRegistry::open(&sde_path).unwrap_or_default());

    let base_now = 1_800_000_000_u64;
    let char_id = 90_000_001_i64;
    let skill_tid = 3402_u32;

    {
        let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
        let ctx1 = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
        ctx1.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now)));
        ctx1.seed_demo_world_fixtures();
        ctx1.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 2;
                ch.skill_points = 0.0;
            }
            snap.skills.entry(char_id).or_default().insert(
                skill_tid,
                eve_db::TrainedSkillRecord {
                    type_id: skill_tid,
                    level: 0,
                    skill_points: 0.0,
                },
            );
            snap.skill_queues.insert(
                char_id,
                vec![eve_db::SkillQueueEntry {
                    type_id: skill_tid,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
                    started_at_unix: Some(base_now),
                }],
            );
        });

        // 1. Initial tick advances 1s -> 0.5 SP and persists cluster_tick_last_unix = base_now.
        tick_background_cluster(&ctx1).await;
        let sp_t0 = ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
        assert!((sp_t0 - 0.5).abs() < 1e-4, "expected 0.5 SP after init tick, got {sp_t0}");

        // 2. Duplicate tick within same second (tick overrun -> 0s delta) adds 0 SP.
        tick_background_cluster(&ctx1).await;
        let sp_overrun = ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
        assert!(
            (sp_overrun - sp_t0).abs() < 1e-6,
            "same-second tick overrun must add 0 SP, got {sp_overrun}"
        );

        // 3. GM clock advance (+60s) updates offset & cluster_tick_last_unix atomically,
        //    and a subsequent +1s wall tick adds only +0.5 SP (not +30.5 SP).
        ctx1.advance_secs(60);
        let sp_after_gm = ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
        assert!(
            (sp_after_gm - 30.5).abs() < 1e-4,
            "expected 30.5 SP after GM +60s, got {sp_after_gm}"
        );
        ctx1.clock().advance_secs(1);
        tick_background_cluster(&ctx1).await;
        let sp_after_wall1 = ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
        assert!(
            (sp_after_wall1 - 31.0).abs() < 1e-4,
            "wall tick after GM advance must add only 1s (0.5 SP), got {sp_after_wall1}"
        );

        // 4. Backward wall-clock jump + catching back up must not rewind or double-credit.
        ctx1.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base_and_offset(
            base_now, 30,
        )));
        tick_background_cluster(&ctx1).await;
        assert!(
            (ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp) - 31.0).abs() < 1e-6
        );
        ctx1.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base_and_offset(
            base_now, 61,
        )));
        tick_background_cluster(&ctx1).await;
        assert!(
            (ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp) - 31.0).abs() < 1e-6
        );

        // 5. Advance 1s to +62 (< 60s flush interval since +60) so SQLite cluster_tick_last_unix
        //    stays at base_now + 60, while skill_queue_last_applied_unix is persisted at base_now + 62.
        ctx1.clock().advance_secs(1);
        tick_background_cluster(&ctx1).await;
        let sp_before_restart = ctx1.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
        assert!(
            (sp_before_restart - 31.5).abs() < 1e-4,
            "expected 31.5 SP before restart, got {sp_before_restart}"
        );
    }

    // 6. Restart: clock_offset_secs = 60 restored from world_meta; ticking at +70
    //    must only apply +8s (+62..+70 -> +4.0 SP = 35.5 SP total), not +10s (+60..+70).
    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite"));
    assert_eq!(db2.clock().offset_secs(), 60);
    let ctx2 = ServerContext::with_sde_and_repository(sde, db2);
    let restored_off = ctx2.clock().offset_secs();
    assert_eq!(restored_off, 60);
    ctx2.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base_and_offset(
        base_now,
        restored_off + 10,
    )));
    tick_background_cluster(&ctx2).await;
    let sp_after_restart_tick = ctx2.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
    assert!(
        (sp_after_restart_tick - 35.5).abs() < 1e-4,
        "restart tick must not double-credit 60..62s; expected 35.5 SP, got {sp_after_restart_tick}"
    );

    drop(ctx2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn clean_shutdown_final_checkpoint_persists_ship_condition_and_clock_offset() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_clean_shutdown_ckpt_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("world.sqlite");
    let sde_path = eve_sde::SdeRegistry::default_db_path();
    let sde = Arc::new(eve_sde::SdeRegistry::open(&sde_path).unwrap_or_default());

    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;

    {
        let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
        ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
        ctx.seed_demo_world_fixtures();

        let init_cs = eve_destiny::CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 275.0,
            max_armor_hp: 400.0,
            structure_hp: 350.0,
            max_structure_hp: 350.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            ..Default::default()
        };
        let pos = [1_234.0, 2_345.0, 3_456.0];
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            if let Some(item) = snap.items.get_mut(&ship_id) {
                item.location_id = sys_id as i64;
            }
            crate::sp6_lifecycle::leave_space_in_snap(snap, ship_id, Some(&init_cs), Some(sys_id), Some(pos))
        });

        let handle = ctx.systems.get_or_spawn(sys_id);
        handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: ship_id,
                    type_id: 587,
                    position: glam::DVec3::from_array(pos),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 2.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                init_cs,
            )
            .await
            .expect("spawn_ball_with_combat");

        // Advance clock directly (not yet persisted to world_meta) & populate CrashLost map.
        ctx.clock().advance_secs(42);
        ctx.active_warps.lock().unwrap().insert(ship_id, 60_003_760);

        let flushed = crate::sp6_lifecycle::flush_all_systems_final_checkpoints(&ctx).await;
        assert_eq!(flushed, 1, "expected 1 active system flushed on shutdown");
        ctx.persist_server_runtime_to_db();
        ctx.db
            .with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::set_clean_shutdown_marker(conn, true)
            })
            .expect("set_clean_shutdown_marker");
    }

    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite"));
    assert_eq!(db2.clock().offset_secs(), 42);
    let was_clean = db2
        .with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::load_clean_shutdown_marker(conn))
        .expect("load_clean_shutdown_marker");
    assert!(was_clean);

    let ckpts = crate::sp6_lifecycle::load_system_condition_checkpoints_sqlite(&db2, sys_id);
    let ship_ckpt = ckpts
        .iter()
        .find(|c| c.ball_id == ship_id)
        .expect("ship_condition checkpoint persisted even when snapshot already matched");
    assert_eq!(ship_ckpt.armor_hp, Some(275.0));

    let ctx2 = ServerContext::with_sde_and_repository(sde, db2);
    ctx2.hydrate_server_runtime_from_db();
    assert!(ctx2.active_warps.lock().unwrap().is_empty());

    drop(ctx2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cluster_tick_npc_ai_ship_death_flushes_projected_ewar_without_lock_rank_violation() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let violations_before = eve_db::lock_rank::unratcheted_violation_count();

    let sys_a = 30_000_142_u32;
    let sys_b = 30_002_187_u32;

    let char_a1 = 90_000_001_i64;
    let ship_a1 = 100_000_001_i64;
    let char_a2 = 90_000_702_i64;
    let ship_a2 = 100_000_702_i64;
    let mod_a1 = 100_000_901_i64;
    let npc_a1 = 100_400_901_i64;

    let char_b1 = 90_000_703_i64;
    let ship_b1 = 100_000_703_i64;
    let char_b2 = 90_000_704_i64;
    let ship_b2 = 100_000_704_i64;
    let mod_b1 = 100_000_902_i64;
    let npc_b1 = 100_400_902_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&char_a1].clone();
        if let Some(ch) = snap.characters.get_mut(&char_a1) {
            ch.solar_system_id = sys_a;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_a1;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a1) {
            sh.location_id = sys_a as i64;
            sh.flag = InventoryFlag::None;
        }
        for (cid, sid, sys_id, uid, name) in [
            (char_a2, ship_a2, sys_a, 702_u64, "Pilot A2"),
            (char_b1, ship_b1, sys_b, 703_u64, "Pilot B1"),
            (char_b2, ship_b2, sys_b, 704_u64, "Pilot B2"),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    user_id: uid,
                    name: name.into(),
                    solar_system_id: sys_id,
                    station_id: None,
                    structure_id: None,
                    active_ship_id: sid,
                    ..base_char.clone()
                },
            );
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        for (mid, owner, loc) in [(mod_a1, char_a1, ship_a1), (mod_b1, char_b1, ship_b1)] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 447,
                    owner_id: owner,
                    location_id: loc,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let spawn_ship = |handle: &crate::system_actor::SystemHandle,
                      ship_id: i64,
                      pos: glam::DVec3,
                      structure_hp: f64| {
        let ball = eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let cs = eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 100.0,
            armor_hp: 0.0,
            max_armor_hp: 100.0,
            structure_hp,
            max_structure_hp: 300.0,
            ..Default::default()
        };
        handle.try_spawn_ball_with_combat(ball, cs);
    };
    let spawn_npc = |handle: &crate::system_actor::SystemHandle, npc_id: i64, pos: glam::DVec3| {
        handle.try_spawn_ball_with_combat(
            eve_destiny::Ball {
                id: npc_id,
                type_id: 597,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 320.0,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 45.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_type_combat_state(597),
        );
        crate::combat::npc_ai::NpcCombatEntity {
            ball_id: npc_id,
            type_id: 597,
            faction_id: 500_010,
            state: crate::combat::npc_ai::NpcAiState::Idle,
            spawn_group_id: 1,
            spawn_pos: pos,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 10_000.0,
            orbit_range: 5_000.0,
            base_dps: 500.0,
            damage_vector: eve_destiny::DamageVector::new(250.0, 250.0, 250.0, 250.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        }
    };

    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let handle_b = ctx.systems.get_or_spawn(sys_b);
    spawn_ship(&handle_a, ship_a1, glam::DVec3::ZERO, 1.0);
    spawn_ship(&handle_a, ship_a2, glam::DVec3::new(100_000.0, 0.0, 0.0), 300.0);
    spawn_ship(&handle_b, ship_b1, glam::DVec3::ZERO, 1.0);
    spawn_ship(&handle_b, ship_b2, glam::DVec3::new(100_000.0, 0.0, 0.0), 300.0);

    let npc_ent_a1 = spawn_npc(&handle_a, npc_a1, glam::DVec3::new(1_000.0, 0.0, 0.0));
    let npc_ent_b1 = spawn_npc(&handle_b, npc_b1, glam::DVec3::new(1_000.0, 0.0, 0.0));
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        spawns.entry(sys_a).or_default().entities.insert(npc_a1, npc_ent_a1);
        spawns.entry(sys_b).or_default().entities.insert(npc_b1, npc_ent_b1);
    }

    let mut eff_a = eve_dogma::DogmaItemState::from_sde(mod_a1, 447, &ctx.sde);
    eff_a.base_attributes.insert(103, 2.0);
    ctx.upsert_projected_ewar_on_ship(
        ship_a1,
        ship_a2,
        eve_dogma::ProjectedEwarEffect {
            source_module_id: mod_a1,
            source_module_type_id: 447,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: eff_a,
        },
    );
    let mut eff_b = eve_dogma::DogmaItemState::from_sde(mod_b1, 447, &ctx.sde);
    eff_b.base_attributes.insert(103, 2.0);
    ctx.upsert_projected_ewar_on_ship(
        ship_b1,
        ship_b2,
        eve_dogma::ProjectedEwarEffect {
            source_module_id: mod_b1,
            source_module_type_id: 447,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: eff_b,
        },
    );

    // Ensure current_tick >= last_attack_tick + 2 so both NPCs attack immediately.
    handle_a.ballpark.write().unwrap().current_tick = 10;
    handle_b.ballpark.write().unwrap().current_tick = 10;
    // Drain initial actor commands before testing cluster_tick paths.
    let _ = handle_a.get_ball_snapshot(ship_a2).await;
    let _ = handle_b.get_ball_snapshot(ship_b2).await;

    // 1. Exercise `on_advance_secs` (`cluster_tick.rs` sync NPC AI path):
    //    `npc_a1` kills `ship_a1` -> `death_pipeline` flushes EWAR onto `ship_a2`
    //    -> `try_send_fit_projection` -> `cmd_sender()` must see zero held locks.
    let now_unix = ctx.now_unix();
    on_advance_secs(&ctx, now_unix, 1);
    assert_eq!(
        eve_db::lock_rank::unratcheted_violation_count(),
        violations_before,
        "on_advance_secs NPC AI ship kill must not trigger lock-rank violations"
    );
    let _ = handle_a.get_ball_snapshot(ship_a2).await;
    assert!(
        ctx.projected_ewar
            .read()
            .unwrap()
            .get(&ship_a2)
            .is_none_or(|v| v.is_empty()),
        "ship_a1 death in on_advance_secs must flush projected EWAR on ship_a2"
    );
    assert!(
        handle_a.latest_fit_projection(ship_a2).is_some(),
        "flushed projected EWAR must push FitProjection to sys_a actor"
    );

    // 2. Exercise `step_active_systems_physics_tick` (`step_single_system_physics_step` path)
    //    with both `sys_a` and `sys_b` active concurrently:
    //    `npc_b1` kills `ship_b1` -> flushes EWAR onto `ship_b2` -> pushes FitProjection
    //    while `npc_a1` remains tracked in `sys_a`'s `ctx.npc_spawns`.
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(
        eve_db::lock_rank::unratcheted_violation_count(),
        violations_before,
        "step_active_systems_physics_tick NPC AI ship kill must not trigger lock-rank violations"
    );
    let _ = handle_b.get_ball_snapshot(ship_b2).await;
    assert!(
        ctx.projected_ewar
            .read()
            .unwrap()
            .get(&ship_b2)
            .is_none_or(|v| v.is_empty()),
        "ship_b1 death in step_active_systems_physics_tick must flush projected EWAR on ship_b2"
    );
    assert!(
        handle_b.latest_fit_projection(ship_b2).is_some(),
        "flushed projected EWAR must push FitProjection to sys_b actor"
    );
    assert!(
        ctx.npc_spawns
            .read()
            .unwrap()
            .get(&sys_a)
            .is_some_and(|mgr| mgr.entities.contains_key(&npc_a1)),
        "sibling system npc_spawns entity must survive concurrent physics tick"
    );
}

#[tokio::test]
async fn physics_tick_subsec_accumulation_overrun_lateness_and_isolation_1207_1208() {
    use std::time::Duration;

    // 1. Sub-second remainder preserves 0.6s jitter across ticks so catch-up replays on tick 2 (#1207).
    let (steps_1, rem_1) =
        accumulate_physics_ticker_steps(Duration::from_millis(1_600), Duration::ZERO);
    assert_eq!(steps_1, 1);
    assert_eq!(rem_1, Duration::from_millis(600));
    let (steps_2, rem_2) =
        accumulate_physics_ticker_steps(Duration::from_millis(1_600), rem_1);
    assert_eq!(steps_2, 2);
    assert_eq!(rem_2, Duration::from_millis(200));
    // Sub-1s wakes preserve remainder without early stepping (#1169).
    let (steps_sub, rem_sub) =
        accumulate_physics_ticker_steps(Duration::from_millis(400), Duration::from_millis(300));
    assert_eq!(steps_sub, 0);
    assert_eq!(rem_sub, Duration::from_millis(700));
    let (steps_sub_done, rem_sub_done) =
        accumulate_physics_ticker_steps(Duration::from_millis(350), rem_sub);
    assert_eq!(steps_sub_done, 1);
    assert_eq!(rem_sub_done, Duration::from_millis(50));

    let (steps_cap, rem_cap) =
        accumulate_physics_ticker_steps(Duration::from_secs(10), Duration::from_millis(500));
    assert_eq!(steps_cap, MAX_PHYSICS_BACKLOG_STEPS);
    assert_eq!(rem_cap, Duration::from_millis(500));

    // 2. Per-system step duration inside the 1,000 ms tick window reports 0 ms lateness (#1207).
    assert_eq!(compute_physics_step_lateness_ms(0.0, 270.0, true), 0.0);
    assert!(
        (compute_physics_step_lateness_ms(0.0, 1_270.0, true) - 270.0).abs() < 1e-9,
        "step_ms beyond 1000 ms must report exact overrun lateness"
    );
    assert!((compute_physics_step_lateness_ms(50.0, 2_000.0, false) - 50.0).abs() < 1e-9);

    // 3. Budgeted physics tick with healthy Jita + wedged wormhole isolates Jita at 1.0x TiDi (#1208).
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.dedicated_physics_ticker_active
        .store(true, std::sync::atomic::Ordering::Release);
    let sys_jita = 30_000_142_u32;
    let sys_wh = 31_000_005_u32;
    let h_jita = ctx.systems.get_or_spawn(sys_jita);
    let h_wh_orig = ctx.systems.get_or_spawn(sys_wh);

    // Replace sys_wh cmd_tx with an unresponsive sink task that ignores commands so
    // step_manual_tick_only stalls until SYSTEM_STEP_ASK_TIMEOUT (2s).
    let (stall_tx, mut stall_rx) = tokio::sync::mpsc::channel(8);
    let _stall_sink = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(cmd) = stall_rx.recv().await {
            held.push(cmd);
        }
    });
    {
        let mut reg = ctx
            .systems
            .systems
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut wedged = h_wh_orig.clone();
        wedged.cmd_tx = stall_tx;
        reg.insert(sys_wh, wedged);
    }

    let jita_sim_0 = h_jita.sim_elapsed_secs();
    let t0 = std::time::Instant::now();
    step_active_systems_physics_tick_with_budget(&ctx, 1, 0.0, Duration::from_millis(200)).await;
    let elapsed_tick_1 = t0.elapsed();
    assert!(
        elapsed_tick_1 < Duration::from_millis(1_000),
        "budgeted physics tick must return near 200ms budget without blocking on wedged sys_wh (got {:?})",
        elapsed_tick_1
    );
    assert!(
        (h_jita.sim_elapsed_secs() - (jita_sim_0 + 1.0)).abs() < 1e-6,
        "healthy sys_jita must advance 1.0s during first tick despite wedged sys_wh"
    );
    assert!(
        (h_jita.tidi_factor() - 1.0).abs() < 1e-6,
        "healthy sys_jita must remain at 1.0x TiDi"
    );

    // Second tick immediately after skips the still-in-flight wedged sys_wh in <200ms while advancing sys_jita cleanly.
    let t1 = std::time::Instant::now();
    step_active_systems_physics_tick_with_budget(&ctx, 1, 0.0, Duration::from_millis(200)).await;
    let elapsed_tick_2 = t1.elapsed();
    assert!(
        elapsed_tick_2 < Duration::from_millis(150),
        "in-flight wedged sys_wh must be skipped without blocking second tick (got {:?})",
        elapsed_tick_2
    );
    assert!(
        (h_jita.sim_elapsed_secs() - (jita_sim_0 + 2.0)).abs() < 1e-6,
        "healthy sys_jita must advance another 1.0s cleanly"
    );
}

#[cfg(test)]
fn seed_combat_ship_1168(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: glam::DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut ch = snap
                .characters
                .get(&90_000_001)
                .cloned()
                .expect("demo seed character 90_000_001 exists");
            ch.character_id = char_id;
            ch.user_id = char_id as u64;
            ch.name = format!("Pilot-{char_id}");
            snap.characters.insert(char_id, ch);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: std::collections::HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    let slim = ctx.build_ship_slim_meta(char_id, type_id);
    sys_handle.try_spawn_ball_with_meta(
        eve_destiny::Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        cs,
        slim,
    );
}

#[cfg(test)]
fn make_lethal_cycle_1168(
    attacker_ship_id: i64,
    attacker_char_id: i64,
    module_id: i64,
    target_ship_id: i64,
    elapsed_s: f64,
    duration_s: f64,
    linked_turrets: Vec<i64>,
) -> crate::system_actor::ActiveModuleCycle {
    crate::system_actor::ActiveModuleCycle {
        ship_id: attacker_ship_id,
        module_id,
        module_type_id: 486,
        owner_char_id: Some(attacker_char_id),
        effect_name: "projectileFired".to_string(),
        target_id: Some(target_ship_id),
        repeat: 0,
        duration_s,
        elapsed_s,
        linked_turrets,
        damage_vector: eve_destiny::DamageVector::new(100_000.0, 0.0, 0.0, 0.0),
        ..Default::default()
    }
}

#[cfg(test)]
fn count_system_wrecks_for_type_1168(ctx: &ServerContext, sys_id: u32, ship_type_id: u32) -> usize {
    let wreck_tid = ctx.sde.resolve_wreck_type_id(ship_type_id, None).unwrap_or(26_557);
    ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.type_id == wreck_tid && it.location_id == sys_id as i64)
            .count()
    })
}

#[cfg(test)]
fn assert_victim_death_committed_1168(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    ship_type_id: u32,
) {
    assert!(
        !handle.ballpark.read().unwrap().balls.contains_key(&victim_ship_id),
        "victim ship ball {victim_ship_id} must be removed from ballpark"
    );
    let capsule_id = ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&victim_ship_id),
            "victim ship item {victim_ship_id} must be removed from DB"
        );
        let ch = snap
            .characters
            .get(&victim_char_id)
            .expect("victim character exists");
        assert_ne!(
            ch.active_ship_id, victim_ship_id,
            "victim character {victim_char_id} must no longer reference destroyed ship"
        );
        let cap = snap
            .items
            .get(&ch.active_ship_id)
            .expect("victim capsule item must exist in DB");
        assert_eq!(cap.type_id, 670, "victim must transition to Capsule (670)");
        assert_eq!(cap.location_id, sys_id as i64);

        let kms = snap
            .killmails
            .values()
            .filter(|km| {
                km.victim_character_id == victim_char_id
                    && km.ship_type_id == ship_type_id
                    && km.solar_system_id == sys_id
            })
            .count();
        assert_eq!(
            kms, 1,
            "victim {victim_char_id} must have exactly one killmail"
        );
        ch.active_ship_id
    });
    assert!(
        handle
            .ballpark
            .read()
            .unwrap()
            .balls
            .get(&capsule_id)
            .is_some_and(|b| b.type_id == 670),
        "victim capsule {capsule_id} must exist in ballpark"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destroyed_balls_survive_step_timeout_and_apply_outside_ask_timeout_1168() {
    use std::time::Duration;
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let attacker_char = 90_100_000_i64;
    let attacker_ship = 950_000_001_i64;
    let victim_1_char = 90_100_001_i64;
    let victim_1_ship = 950_000_002_i64;
    let ship_type_id = 587_u32;

    seed_combat_ship_1168(
        &ctx,
        sys_id,
        attacker_char,
        attacker_ship,
        ship_type_id,
        glam::DVec3::ZERO,
    );
    seed_combat_ship_1168(
        &ctx,
        sys_id,
        victim_1_char,
        victim_1_ship,
        ship_type_id,
        glam::DVec3::new(1_000.0, 0.0, 0.0),
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let wrecks_0 = count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id);

    // Part A: hold WorldRepository::writer past SYSTEM_STEP_ASK_TIMEOUT (2 s) so SystemActor blocks
    // after destroying victim_1_ship (`cycles.rs:1245`), causing ask_step_tick_only to time out.
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            950_000_101,
            victim_1_ship,
            4.0,
            5.0,
            vec![950_000_101],
        ))
        .await
        .unwrap();

    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let db_hold_a = Arc::clone(&ctx.db);
    let writer_thread = std::thread::spawn(move || {
        db_hold_a.with_mut_snapshot(|_| {
            let _ = locked_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
    });
    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer thread acquired lock");

    let t0 = std::time::Instant::now();
    step_active_systems_physics_tick(&ctx, 1).await;
    let timeout_elapsed = t0.elapsed();
    assert!(
        timeout_elapsed >= SYSTEM_STEP_ASK_TIMEOUT.saturating_sub(Duration::from_millis(100)),
        "tick must hit SYSTEM_STEP_ASK_TIMEOUT while writer is held (got {:?})",
        timeout_elapsed
    );

    let _ = release_tx.send(());
    writer_thread.join().expect("writer thread joined cleanly");
    // Synchronize on the actor mailbox so the timed-out step finishes and drops its unsent reply lease.
    let _ = handle.list_balls().await.unwrap();

    assert!(
        !handle.ballpark.read().unwrap().balls.contains_key(&victim_1_ship),
        "victim ship must already be removed from ballpark when actor step timed out"
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (1, 0),
        "timed-out destroyed_balls entry must be rolled back into pending outbox"
    );
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0,
        "wreck must not exist before retained outbox drains"
    );

    // Run next tick with writer free: outbox drains and commits wreck, killmail, and capsule.
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_1_char, victim_1_ship, ship_type_id);
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + 1
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 0)
    );
    assert!(!handle.pending_destroyed_balls.is_non_empty());

    // Part B: hold WorldRepository::writer > 2 s right as StepOutput is returned so `apply_step_output`
    // waits outside SYSTEM_STEP_ASK_TIMEOUT and commits without dropping or timing out.
    let victim_2_char = 90_100_002_i64;
    let victim_2_ship = 950_000_012_i64;
    seed_combat_ship_1168(
        &ctx,
        sys_id,
        victim_2_char,
        victim_2_ship,
        ship_type_id,
        glam::DVec3::new(1_500.0, 0.0, 0.0),
    );
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            950_000_102,
            victim_2_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();

    let writer_released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_released_flag = Arc::clone(&writer_released);
    let db_hold_b = Arc::clone(&ctx.db);
    *handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let (locked_b_tx, locked_b_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            db_hold_b.with_mut_snapshot(|_| {
                let _ = locked_b_tx.send(());
                std::thread::sleep(Duration::from_millis(2_250));
                writer_released_flag.store(true, std::sync::atomic::Ordering::Release);
            });
        });
        let _ = locked_b_rx.recv_timeout(Duration::from_secs(5));
    }));

    let t1 = std::time::Instant::now();
    step_active_systems_physics_tick(&ctx, 1).await;
    let apply_elapsed = t1.elapsed();
    assert!(
        apply_elapsed >= Duration::from_millis(2_200),
        "apply_step_output must wait past SYSTEM_STEP_ASK_TIMEOUT on writer without cancelling (got {:?})",
        apply_elapsed
    );
    assert!(
        writer_released.load(std::sync::atomic::Ordering::Acquire),
        "writer lock must have been held and released during apply_step_output"
    );
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_2_char, victim_2_ship, ship_type_id);
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + 2
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 0)
    );
    // An extra follow-up tick must produce zero duplicate wrecks or killmails.
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + 2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destroyed_balls_recover_across_dropped_reply_receiver_and_post_destruction_panic_1168() {
    use std::time::Duration;
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let attacker_char = 90_100_010_i64;
    let attacker_ship = 951_000_001_i64;
    let victim_a_char = 90_100_011_i64;
    let victim_a_ship = 951_000_002_i64;
    let victim_b_char = 90_100_003_i64;
    let victim_b_ship = 951_000_003_i64;
    let ship_type_id = 587_u32;

    seed_combat_ship_1168(
        &ctx,
        sys_id,
        attacker_char,
        attacker_ship,
        ship_type_id,
        glam::DVec3::ZERO,
    );
    seed_combat_ship_1168(
        &ctx,
        sys_id,
        victim_a_char,
        victim_a_ship,
        ship_type_id,
        glam::DVec3::new(1_000.0, 0.0, 0.0),
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let wrecks_0 = count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id);

    // Part A: drop step oneshot::Receiver before SystemActor sends StepOutput.
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            951_000_101,
            victim_a_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();

    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    *handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = arrived_tx.send(());
        let _ = proceed_rx.recv_timeout(Duration::from_secs(5));
    }));

    let rx = handle.start_step_manual_tick_only(1.0).await.unwrap();
    arrived_rx.await.unwrap();
    drop(rx);
    let _ = proceed_tx.send(());
    let _ = handle.list_balls().await.unwrap();

    assert!(
        !handle.ballpark.read().unwrap().balls.contains_key(&victim_a_ship),
        "victim A ship ball must already be removed before reply send"
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (1, 0),
        "dropped oneshot::Receiver must restore victim A into pending outbox"
    );
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0
    );

    // Next step drains & commits cleanly with no duplicates.
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_a_char, victim_a_ship, ship_type_id);
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + 1
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 0)
    );

    // Part B: inject a post-destruction SystemActor panic after ball destruction & checkout.
    seed_combat_ship_1168(
        &ctx,
        sys_id,
        victim_b_char,
        victim_b_ship,
        ship_type_id,
        glam::DVec3::new(1_200.0, 0.0, 0.0),
    );
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            951_000_102,
            victim_b_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();

    handle
        .panic_after_destruction_for_test
        .store(true, std::sync::atomic::Ordering::SeqCst);
    step_active_systems_physics_tick(&ctx, 1).await;
    let _ = handle.list_balls().await.unwrap();

    assert!(
        handle
            .command_panics
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1,
        "post-destruction command panic must be observed by SystemActor supervisor loop"
    );
    assert!(
        !handle.ballpark.read().unwrap().balls.contains_key(&victim_b_ship),
        "victim B ship ball must have been removed prior to injected panic"
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (1, 0),
        "post-destruction panic must unwind lease back into pending outbox"
    );

    step_active_systems_physics_tick(&ctx, 1).await;
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_b_char, victim_b_ship, ship_type_id);
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + 2
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 0)
    );
    assert_eq!(handle.pending_destroyed_balls.totals(), (2, 2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeded_multi_ship_combat_terminal_destruction_matches_committed_deaths_1168() {
    use std::time::Duration;
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let attacker_a_char = 90_200_091_i64;
    let attacker_a_ship = 952_000_001_i64;
    let attacker_b_char = 90_200_092_i64;
    let attacker_b_ship = 952_000_002_i64;
    let ship_type_id = 587_u32;

    seed_combat_ship_1168(
        &ctx,
        sys_id,
        attacker_a_char,
        attacker_a_ship,
        ship_type_id,
        glam::DVec3::ZERO,
    );
    seed_combat_ship_1168(
        &ctx,
        sys_id,
        attacker_b_char,
        attacker_b_ship,
        ship_type_id,
        glam::DVec3::new(500.0, 0.0, 0.0),
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let wrecks_0 = count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id);
    let kms_0 = ctx.db.with_snapshot(|s| s.killmails.len());

    // Deterministic seeded schedule across 6 victim ships under mixed timeouts, dropped receivers, and panics.
    let mut rng = 0x1168_CAFE_BABE_u64;
    let mut victims = Vec::new();
    for i in 0..6_i64 {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let victim_char = 90_200_001 + i;
        let victim_ship = 952_000_100 + i;
        let offset = 1_000.0 + ((rng >> 48) as f64) * 0.1;
        seed_combat_ship_1168(
            &ctx,
            sys_id,
            victim_char,
            victim_ship,
            ship_type_id,
            glam::DVec3::new(offset, (i as f64) * 200.0, 0.0),
        );
        let (atk_ship, atk_char, turrets) = if i % 2 == 0 {
            (attacker_a_ship, attacker_a_char, Vec::new())
        } else {
            (attacker_b_ship, attacker_b_char, vec![952_000_500 + i])
        };
        let elapsed_s = match i {
            0 | 1 => 4.0,
            2 | 3 => 3.0,
            _ => 2.0,
        };
        handle
            .add_module_cycle(make_lethal_cycle_1168(
                atk_ship,
                atk_char,
                952_000_500 + i,
                victim_ship,
                elapsed_s,
                5.0,
                turrets,
            ))
            .await
            .unwrap();
        victims.push((victim_char, victim_ship));
    }

    // Tick 1 (victims 0 & 1 die): drop reply receiver before reply send.
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    *handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = arrived_tx.send(());
        let _ = proceed_rx.recv_timeout(Duration::from_secs(5));
    }));
    let rx1 = handle.start_step_manual_tick_only(1.0).await.unwrap();
    arrived_rx.await.unwrap();
    drop(rx1);
    let _ = proceed_tx.send(());
    let _ = handle.list_balls().await.unwrap();

    // Tick 2 (victims 2 & 3 die while retained victims 0 & 1 await drain): inject post-destruction panic.
    handle
        .panic_after_destruction_for_test
        .store(true, std::sync::atomic::Ordering::SeqCst);
    step_active_systems_physics_tick(&ctx, 1).await;
    let _ = handle.list_balls().await.unwrap();

    // Tick 3 (victims 4 & 5 die while victims 0..4 await drain): drain via clean physics ticks.
    for _ in 0..3 {
        step_active_systems_physics_tick(&ctx, 1).await;
    }

    let (recorded, acked) = handle.pending_destroyed_balls.totals();
    assert_eq!(
        recorded,
        victims.len() as u64,
        "all 6 ships removed via destroy_ball_with_terminal_explosion must enter durable outbox once"
    );
    assert_eq!(
        acked, recorded,
        "total committed on_ball_destroyed_sync completions must equal total terminal explosions"
    );
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 0)
    );
    assert!(!handle.pending_destroyed_balls.is_non_empty());

    for &(victim_char, victim_ship) in &victims {
        assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_char, victim_ship, ship_type_id);
    }
    assert_eq!(
        count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id),
        wrecks_0 + victims.len()
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.killmails.len()),
        kms_0 + victims.len()
    );
}

#[cfg(test)]
fn install_gated_step_hook_1168(
    handle: &crate::system_actor::SystemHandle,
    orig: crate::system_actor::SystemStepHook,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let entered_slot = Arc::new(std::sync::Mutex::new(Some(entered_tx)));
    let release_slot = Arc::new(std::sync::Mutex::new(Some(release_rx)));
    *handle.system_step_hook.write().unwrap() = Some(Arc::new(move |sid, dt, destroyed| {
        let entered = entered_slot.clone();
        let release = release_slot.clone();
        let orig = orig.clone();
        Box::pin(async move {
            if !destroyed.is_empty() {
                let tx_opt = entered.lock().unwrap().take();
                if let Some(tx) = tx_opt {
                    let _ = tx.send(());
                }
                let rx_opt = release.lock().unwrap().take();
                if let Some(rx) = rx_opt {
                    let _ = rx.await;
                }
            }
            orig(sid, dt, destroyed).await;
        })
    }));
    (entered_rx, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn try_step_full_tick_rolls_back_without_runtime_and_holds_lease_until_step_hook_finishes_1168() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let attacker_char = 90_300_001_i64;
    let attacker_ship = 953_000_001_i64;
    let victim_a_char = 90_300_002_i64;
    let victim_a_ship = 953_000_002_i64;
    let victim_b_char = 90_300_003_i64;
    let victim_b_ship = 953_000_003_i64;
    let victim_c_char = 90_300_004_i64;
    let victim_c_ship = 953_000_004_i64;
    let ship_type_id = 587_u32;

    seed_combat_ship_1168(&ctx, sys_id, attacker_char, attacker_ship, ship_type_id, glam::DVec3::ZERO);
    seed_combat_ship_1168(&ctx, sys_id, victim_a_char, victim_a_ship, ship_type_id, glam::DVec3::new(1_000.0, 0.0, 0.0));
    seed_combat_ship_1168(&ctx, sys_id, victim_b_char, victim_b_ship, ship_type_id, glam::DVec3::new(1_100.0, 0.0, 0.0));
    seed_combat_ship_1168(&ctx, sys_id, victim_c_char, victim_c_ship, ship_type_id, glam::DVec3::new(1_200.0, 0.0, 0.0));

    let handle = ctx.systems.get_or_spawn(sys_id);
    let wrecks_0 = count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id);
    let kms_0 = ctx.db.with_snapshot(|s| s.killmails.len());

    // Part A: invoke try_step_full_tick from an OS thread without a Tokio runtime.
    // The lease must drop un-acked so records roll back into `pending` instead of acking early.
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            953_000_101,
            victim_a_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    let h_clone = handle.clone();
    std::thread::spawn(move || h_clone.try_step_full_tick(1.0))
        .join()
        .expect("no-runtime try_step_full_tick thread completed");

    assert!(!handle.ballpark.read().unwrap().balls.contains_key(&victim_a_ship));
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (1, 0));
    assert!(handle.pending_destroyed_balls.is_non_empty());
    assert_eq!(count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id), wrecks_0);

    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_a_char, victim_a_ship, ship_type_id);

    // Part B: with active runtime, both `npc_death_hook` (`!death_futs.is_empty()`) and
    // `system_step_hook` (`step_fut`) must complete sequentially before `lease.ack()`.
    let orig_step_hook = handle
        .system_step_hook
        .read()
        .unwrap()
        .clone()
        .expect("ServerContext attaches system_step_hook");
    let orig_npc_hook = handle.npc_death_hook.read().unwrap().clone();
    let npc_hook_done = Arc::new(AtomicBool::new(false));
    let npc_hook_done_clone = npc_hook_done.clone();
    let orig_npc_for_wrap = orig_npc_hook.clone();
    *handle.npc_death_hook.write().unwrap() = Some(Arc::new(move |sid, bid, att| {
        let done = npc_hook_done_clone.clone();
        let orig = orig_npc_for_wrap.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            if let Some(orig_fn) = orig {
                orig_fn(sid, bid, att).await;
            }
            done.store(true, Ordering::SeqCst);
        })
    }));

    let (entered_rx_b, release_tx_b) =
        install_gated_step_hook_1168(&handle, orig_step_hook.clone());

    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            953_000_102,
            victim_b_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    let _ = handle.try_step_full_tick(1.0);
    tokio::time::timeout(Duration::from_secs(5), entered_rx_b)
        .await
        .expect("try_step_full_tick reached system_step_hook")
        .unwrap();
    assert!(npc_hook_done.load(Ordering::SeqCst), "death_futs must complete before step_fut");
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 1),
        "lease must remain in_flight while system_step_hook is still executing"
    );
    let _ = release_tx_b.send(());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while handle.pending_destroyed_balls.is_non_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_b_char, victim_b_ship, ship_type_id);

    // Part C: with `npc_death_hook = None` (`death_futs.is_empty()`), lease must STILL stay
    // `in_flight` until `step_fut` finishes instead of acking synchronously.
    *handle.npc_death_hook.write().unwrap() = None;
    let (entered_rx_c, release_tx_c) =
        install_gated_step_hook_1168(&handle, orig_step_hook.clone());

    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            953_000_103,
            victim_c_ship,
            4.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    let _ = handle.try_step_full_tick(1.0);
    tokio::time::timeout(Duration::from_secs(5), entered_rx_c)
        .await
        .expect("empty-death_futs try_step_full_tick reached system_step_hook")
        .unwrap();
    assert_eq!(
        handle.pending_destroyed_balls.pending_and_in_flight_counts(),
        (0, 1),
        "lease must NOT be acked synchronously when death_futs.is_empty()"
    );
    let _ = release_tx_c.send(());
    let deadline_c = tokio::time::Instant::now() + Duration::from_secs(5);
    while handle.pending_destroyed_balls.is_non_empty() && tokio::time::Instant::now() < deadline_c {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    *handle.system_step_hook.write().unwrap() = Some(orig_step_hook);
    *handle.npc_death_hook.write().unwrap() = orig_npc_hook;

    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_eq!(handle.pending_destroyed_balls.totals(), (3, 3));
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victim_c_char, victim_c_ship, ship_type_id);
    assert_eq!(count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id), wrecks_0 + 3);
    assert_eq!(ctx.db.with_snapshot(|s| s.killmails.len()), kms_0 + 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_weapon_cycle_volley_outbox_lease_rollback_and_ack_1168() {
    use std::time::Duration;

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let attacker_char = 90_400_001_i64;
    let attacker_ship = 954_000_001_i64;
    let victims: [(i64, i64, i64); 4] = [
        (90_400_002, 954_000_002, 954_000_101),
        (90_400_003, 954_000_003, 954_000_102),
        (90_400_004, 954_000_004, 954_000_103),
        (90_400_005, 954_000_005, 954_000_104),
    ];
    let ship_type_id = 587_u32;

    seed_combat_ship_1168(&ctx, sys_id, attacker_char, attacker_ship, ship_type_id, glam::DVec3::ZERO);
    for (idx, &(v_char, v_ship, _)) in victims.iter().enumerate() {
        seed_combat_ship_1168(
            &ctx,
            sys_id,
            v_char,
            v_ship,
            ship_type_id,
            glam::DVec3::new(1_000.0 + (idx as f64) * 100.0, 0.0, 0.0),
        );
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let wrecks_0 = count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id);
    let kms_0 = ctx.db.with_snapshot(|s| s.killmails.len());

    // Subcase A: dropped reply receiver after locks drop before reply.send rolls volley lease back.
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            victims[0].2,
            victims[0].1,
            0.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    *handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = arrived_tx.send(());
        let _ = proceed_rx.recv_timeout(Duration::from_secs(5));
    }));
    let rx_a = handle
        .start_fire_weapon_cycle_volley(attacker_ship, victims[0].2)
        .await
        .unwrap();
    arrived_rx.await.unwrap();
    // Write guards must already be dropped before reply/hook delivery points.
    assert!(handle.ballpark.try_read().is_ok(), "ballpark write guard leaked past lock scope");
    assert!(handle.actor_state.try_read().is_ok(), "actor_state write guard leaked past lock scope");
    drop(rx_a);
    let _ = proceed_tx.send(());
    let _ = handle.list_balls().await.unwrap();

    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (1, 0));
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victims[0].0, victims[0].1, ship_type_id);

    // Subcase B: post-destruction panic inside actor rolls volley lease back and keeps actor alive.
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            victims[1].2,
            victims[1].1,
            0.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    handle
        .panic_after_destruction_for_test
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        handle
            .fire_weapon_cycle_volley(attacker_ship, victims[1].2)
            .await
            .is_err()
    );
    let _ = handle.list_balls().await.unwrap();
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (1, 0));
    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victims[1].0, victims[1].1, ship_type_id);

    // Subcase C: caller future cancelled mid-`system_step_hook` rolls victim 2 back to `pending`;
    // subsequent volley on victim 3 checks out and acks ONLY victim 3 while victim 2 stays in
    // `pending` until the next physics tick drains it cleanly.
    let orig_step_hook = handle
        .system_step_hook
        .read()
        .unwrap()
        .clone()
        .expect("ServerContext attaches system_step_hook");
    let (hook_entered_rx, _never_release_tx) =
        install_gated_step_hook_1168(&handle, orig_step_hook.clone());

    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            victims[2].2,
            victims[2].1,
            0.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    {
        let volley_fut = handle.fire_weapon_cycle_volley(attacker_ship, victims[2].2);
        tokio::pin!(volley_fut);
        tokio::select! {
            _ = &mut volley_fut => panic!("blocked volley future should not finish before hook gate"),
            res = hook_entered_rx => {
                res.unwrap();
                assert_eq!(
                    handle.pending_destroyed_balls.pending_and_in_flight_counts(),
                    (0, 1),
                    "volley lease must stay in_flight until hooks complete"
                );
            }
        }
    }
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (1, 0));

    *handle.system_step_hook.write().unwrap() = Some(orig_step_hook);
    handle
        .add_module_cycle(make_lethal_cycle_1168(
            attacker_ship,
            attacker_char,
            victims[3].2,
            victims[3].1,
            0.0,
            5.0,
            Vec::new(),
        ))
        .await
        .unwrap();
    let (destroyed_3, _) = handle
        .fire_weapon_cycle_volley(attacker_ship, victims[3].2)
        .await
        .unwrap();
    assert_eq!(destroyed_3, vec![(victims[3].1, Some(attacker_char))]);
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victims[3].0, victims[3].1, ship_type_id);
    // Victim 3 is acked while unrelated victim 2 remains queued in `pending`.
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (1, 0));

    step_active_systems_physics_tick(&ctx, 1).await;
    assert_eq!(handle.pending_destroyed_balls.pending_and_in_flight_counts(), (0, 0));
    assert_eq!(handle.pending_destroyed_balls.totals(), (4, 4));
    assert!(!handle.pending_destroyed_balls.is_non_empty());
    assert_victim_death_committed_1168(&ctx, &handle, sys_id, victims[2].0, victims[2].1, ship_type_id);
    assert_eq!(count_system_wrecks_for_type_1168(&ctx, sys_id, ship_type_id), wrecks_0 + 4);
    assert_eq!(ctx.db.with_snapshot(|s| s.killmails.len()), kms_0 + 4);
}

#[test]
fn populate_persisted_space_objects_populates_sde_type_name_on_wreck_slim_meta() {
    let mut ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142u32;
    let now_unix = ctx.now_unix();
    let named_wreck_id = 880_101i64;
    let empty_name_wreck_id = 880_102i64;
    let empty_name_tid = 999_880_102u32;

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        26_557,
        eve_sde::InvType {
            type_id: 26_557,
            group_id: 186,
            category_id: 2,
            name: "Frigate Wreck".into(),
            mass: 10_000.0,
            volume: 10_000.0,
            capacity: 250.0,
            radius: 14.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: Some(3117),
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: Default::default(),
        },
    );
    sde.types.insert(
        empty_name_tid,
        eve_sde::InvType {
            type_id: empty_name_tid,
            group_id: 186,
            category_id: 2,
            name: String::new(),
            mass: 10_000.0,
            volume: 10_000.0,
            capacity: 250.0,
            radius: 14.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: Default::default(),
        },
    );
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, pos) in [
            (named_wreck_id, 26_557, [1_200.0, 0.0, 0.0]),
            (empty_name_wreck_id, empty_name_tid, [1_500.0, 0.0, 0.0]),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 1,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(iid, pos);
            snap.sp6_lifecycle
                .as_inner_mut()
                .space_object_meta
                .insert(
                    iid,
                    PersistedSpaceObjectMeta {
                        created_unix: Some(now_unix),
                        ..Default::default()
                    },
                );
        }
    });

    let mut ballpark = Ballpark::new(sys_id);
    let _ = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);

    let named_meta = ballpark
        .ball_slim_meta(named_wreck_id)
        .expect("persisted named wreck slim meta");
    assert_eq!(named_meta.name.as_deref(), Some("Frigate Wreck"));
    let named_ball = ballpark.balls.get(&named_wreck_id).expect("named wreck ball");
    let named_slim = eve_destiny::wire::slim_item_dict(named_ball, named_meta);
    assert_eq!(
        named_slim.dict_get("name"),
        Some(&eve_proto::PyRep::String("Frigate Wreck".into()))
    );

    let unnamed_meta = ballpark
        .ball_slim_meta(empty_name_wreck_id)
        .expect("persisted empty-name wreck slim meta");
    assert_eq!(unnamed_meta.name, None);
    let unnamed_ball = ballpark
        .balls
        .get(&empty_name_wreck_id)
        .expect("empty-name wreck ball");
    let unnamed_slim = eve_destiny::wire::slim_item_dict(unnamed_ball, unnamed_meta);
    assert_eq!(
        unnamed_slim.dict_get("name"),
        Some(&eve_proto::PyRep::String(format!("Item {empty_name_wreck_id}")))
    );
}

#[tokio::test]
async fn self_destruct_abort_before_tick_persist_not_resurrected_by_cluster_tick_gh_1174() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let base_now = 1_770_000_000u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now)));
    let sys_id = 30_000_142u32;
    let char_id = 90_000_002i64;
    let ship_id = 830_901i64;

    let km_before = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let ch = snap
            .characters
            .get_mut(&char_id)
            .expect("demo character 90_000_002 present");
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.killmails.len()
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..eve_destiny::CombatState::default()
            },
        )
        .await
        .unwrap();

    // 1. Start countdown, snapshot `rt_self_destructs` as the tick does, abort, then run persistence step.
    let detonate_1 = base_now + 120;
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx, ship_id, char_id, sys_id, base_now, detonate_1,
    );
    let rt_snapshot = crate::sp6_lifecycle::snapshot_rt_self_destructs(&ctx, None);
    assert_eq!(rt_snapshot.len(), 1);
    assert_eq!(rt_snapshot[0].entry.ship_id, ship_id);
    assert!(crate::sp6_lifecycle::abort_self_destruct_for_ship(
        &ctx, ship_id
    ));

    // Persistence step with stale snapshot must NOT resurrect the aborted countdown.
    crate::sp6_lifecycle::persist_rt_self_destructs_snapshot(&ctx, &rt_snapshot);
    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.len()),
        0
    );
    assert_eq!(
        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.self_destruct_countdowns.len()
        }),
        0
    );

    // 2. Advance clock past `detonate_at_unix` and run cluster lifecycle ticks: ship survives intact.
    ctx.advance_secs(125);
    on_advance_secs(&ctx, ctx.now_unix(), 5);
    tick_background_cluster(&ctx).await;

    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.len()),
        0
    );
    assert_eq!(
        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.self_destruct_countdowns.len()
        }),
        0
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items.get(&ship_id).map(|i| (i.owner_id, i.location_id)),
            Some((char_id, i64::from(sys_id)))
        );
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.active_ship_id, ship_id);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.solar_system_id, sys_id);
        assert_eq!(snap.killmails.len(), km_before);
    });
    assert!(sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .is_some());

    // 3. Abort-then-restart race: stale snapshot from old countdown must not overwrite new `detonate_at_unix`.
    let t2 = ctx.now_unix();
    let det2 = t2 + 120;
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx, ship_id, char_id, sys_id, t2, det2,
    );
    let stale_snap2 = crate::sp6_lifecycle::snapshot_rt_self_destructs(&ctx, None);
    assert!(crate::sp6_lifecycle::abort_self_destruct_for_ship(
        &ctx, ship_id
    ));
    let t3 = t2 + 15;
    let det3 = t3 + 120;
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx, ship_id, char_id, sys_id, t3, det3,
    );
    crate::sp6_lifecycle::persist_rt_self_destructs_snapshot(&ctx, &stale_snap2);

    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
            st.self_destructs.get(&ship_id).map(|e| e.detonate_at_unix)
        }),
        Some(det3)
    );
    ctx.advance_secs(121);
    assert_eq!(ctx.now_unix(), det2 + 1);
    on_advance_secs(&ctx, ctx.now_unix(), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].active_ship_id, ship_id);
        assert_eq!(snap.killmails.len(), km_before);
        assert_eq!(
            snap.sp6_lifecycle
                .self_destructs
                .get(&ship_id)
                .map(|e| e.detonate_at_unix),
            Some(det3)
        );
    });
}

#[tokio::test]
async fn self_destruct_due_countdown_aborts_cleanly_if_docked_or_unowned_gh_1174() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let base_now = 1_770_000_000u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now)));
    let sys_id = 30_000_142u32;
    let char_id = 90_000_002i64;
    let ship_id = 830_902i64;
    let station_id = 60_003_760i64;

    let km_before = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let ch = snap
            .characters
            .get_mut(&char_id)
            .expect("demo character 90_000_002 present");
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.killmails.len()
    });

    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx,
        ship_id,
        char_id,
        sys_id,
        base_now,
        base_now + 120,
    );
    ctx.clock().advance_secs(125);
    let now_due = ctx.now_unix();

    // Pilot docks in station concurrently before due detonation executes.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = station_id;
        }
    });

    let guarded = crate::combat::death_pipeline::execute_guarded_self_destruct_sync(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        glam::DVec3::ZERO,
        now_due,
        None,
        true,
    )
    .expect("guarded self-destruct does not error on docked ship");
    assert!(guarded.is_none());

    on_advance_secs(&ctx, now_due, 1);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items.get(&ship_id).map(|i| (i.owner_id, i.location_id)),
            Some((char_id, station_id))
        );
        assert_eq!(snap.characters[&char_id].active_ship_id, ship_id);
        assert_eq!(snap.killmails.len(), km_before);
        assert_eq!(snap.sp6_lifecycle.self_destructs.len(), 0);
    });
    assert_eq!(
        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.self_destruct_countdowns.len()
        }),
        0
    );
}

#[tokio::test]
async fn overrun_physics_tick_emits_sim_clock_rebase_for_skipped_secs_1169() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142_u32;
    let char_space = 90_000_001_i64;
    let char_docked = 90_000_002_i64;

    let (u_space, u_docked) = ctx.db.with_mut_snapshot(|snap| {
        let c1 = snap.characters.get_mut(&char_space).expect("char_space");
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.structure_id = None;
        let u1 = c1.user_id;

        let c2 = snap.characters.get_mut(&char_docked).expect("char_docked");
        c2.solar_system_id = sys_id;
        c2.station_id = Some(60_003_760);
        c2.structure_id = None;
        let u2 = c2.user_id;
        (u1, u2)
    });
    let _ = ctx
        .db
        .acquire_online_lock(u_space, 11_169, Some(char_space), false, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(u_docked, 11_170, Some(char_docked), false, ctx.now_unix());

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let tick_0 = sys_h.ballpark.read().unwrap().current_tick;
    let _ = ctx.drain_notifications_for_char(char_space);
    let _ = ctx.drain_notifications_for_char(char_docked);

    // 0-second tick is a no-op once subsec remainder has not reached 1.0s.
    step_active_systems_physics_tick(&ctx, 0).await;
    assert_eq!(sys_h.ballpark.read().unwrap().current_tick, tick_0);
    assert!(ctx.drain_notifications_for_char(char_space).is_empty());

    // 10s overrun steps MAX_PHYSICS_BACKLOG_STEPS (3) ticks and reconciles the 7 skipped seconds
    // via DoSimClockRebase for online in-space pilots only (not docked pilots).
    step_active_systems_physics_tick(&ctx, 10).await;
    assert_eq!(
        sys_h.ballpark.read().unwrap().current_tick,
        tick_0 + MAX_PHYSICS_BACKLOG_STEPS
    );

    let space_notes = ctx.drain_notifications_for_char(char_space);
    let rebase = space_notes
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("in-space pilot must receive DoSimClockRebase on multi-second overrun");
    let times = match rebase.args.first() {
        Some(eve_proto::PyRep::Tuple(items)) | Some(eve_proto::PyRep::List(items)) => items,
        other => panic!("expected DoSimClockRebase tuple arg, got {other:?}"),
    };
    let old_ft = times[0].as_i64().expect("old_ft integer");
    let new_ft = times[1].as_i64().expect("new_ft integer");
    let expected_skipped_ft = (10 - MAX_PHYSICS_BACKLOG_STEPS) as i64 * FILETIME_TICKS_PER_SEC;
    assert_eq!(
        old_ft - new_ft,
        expected_skipped_ft,
        "overrun DoSimClockRebase delta must equal -7s skipped wall-clock time"
    );

    assert!(
        !ctx.drain_notifications_for_char(char_docked)
            .iter()
            .any(|n| n.event == "DoSimClockRebase"),
        "docked pilots must not receive space overrun DoSimClockRebase"
    );
}

#[tokio::test]
async fn cluster_tick_prunes_disconnected_ships_on_system_mismatch_or_online_character() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let now_unix = ctx.now_unix();

    let char_moved = 90_000_001_i64;
    let char_online = 90_000_002_i64;
    let ship_moved = 100_000_001_i64;
    let ship_online = 100_000_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&char_moved) {
            c1.solar_system_id = sys_b;
            c1.station_id = None;
            c1.structure_id = None;
            c1.active_ship_id = ship_moved;
        }
        let mut c2 = snap.characters[&char_moved].clone();
        c2.character_id = char_online;
        c2.user_id = 2;
        c2.solar_system_id = sys_a;
        c2.station_id = None;
        c2.structure_id = None;
        c2.active_ship_id = ship_online;
        snap.characters.insert(char_online, c2);

        let mut base_ship = snap.items[&ship_moved].clone();
        base_ship.item_id = ship_online;
        base_ship.owner_id = char_online;
        base_ship.location_id = i64::from(sys_a);
        snap.items.insert(ship_online, base_ship);

        snap.accounts
            .entry("demo2".into())
            .or_insert_with(|| eve_db::AccountRecord {
                user_id: 2,
                username: "demo2".into(),
                password_token: "secret".into(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            });
    });

    ctx.db
        .acquire_online_lock(2, 55_002, Some(char_online), true, now_unix)
        .expect("online lock for char_online");

    let stale_warp_dest = [1_000_000.0, 0.0, 0.0];
    with_mut_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.insert(
            char_moved,
            DisconnectedShipEntry {
                char_id: char_moved,
                ship_id: ship_moved,
                ship_type_id: 606,
                solar_system_id: sys_a,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                last_position: [1_000.0, 0.0, 0.0],
                pre_warp_position: Some([1_000.0, 0.0, 0.0]),
                emergency_warp_dest: Some(stale_warp_dest),
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
            },
        );
        st.disconnected_ships.insert(
            char_online,
            DisconnectedShipEntry {
                char_id: char_online,
                ship_id: ship_online,
                ship_type_id: 606,
                solar_system_id: sys_a,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                last_position: [2_000.0, 0.0, 0.0],
                pre_warp_position: Some([2_000.0, 0.0, 0.0]),
                emergency_warp_dest: Some(stale_warp_dest),
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
            },
        );
    });

    tick_background_cluster_with_step(&ctx, 1).await;

    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(
            !st.disconnected_ships.contains_key(&char_moved),
            "disconnected_ships must prune entries whose solar_system_id no longer matches character"
        );
        assert!(
            !st.disconnected_ships.contains_key(&char_online),
            "disconnected_ships must prune entries for currently online characters"
        );
    });

    let sys_a_handle = ctx.systems.get_or_spawn(sys_a);
    assert!(
        sys_a_handle
            .get_ball_snapshot(ship_moved)
            .await
            .unwrap()
            .is_none(),
        "stale sys_a disconnected_ships entry must not synthesize ship_moved ball into sys_a"
    );
    assert!(
        sys_a_handle
            .get_ball_snapshot(ship_online)
            .await
            .unwrap()
            .is_none(),
        "pruned online disconnected_ships entry must not synthesize ship_online ball into sys_a"
    );
}

#[tokio::test]
async fn cluster_tick_preserves_prelogoff_hold_until_expiry_or_new_session() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let user_id = 1_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = user_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = char_id;
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 606,
        position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .expect("spawn ball");

    // Simulate direct `handle_character_logoff_or_disconnect` while an active pre-logoff
    // session owns `char_id` (as in direct IT disconnect tests and pre-release RPC paths).
    ctx.db
        .acquire_online_lock(user_id, 9201, Some(char_id), true, ctx.now_unix())
        .expect("acquire initial session lock");
    let entry = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnect hold recorded");
    assert!(!entry.removed_from_space);

    // Mid-hold tick must NOT prune the recorded hold while only the matching pre-logoff session remains.
    ctx.advance_secs(10);
    tick_background_cluster_with_step(&ctx, 10).await;
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id));
    assert!(sys.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // After hold expiry, finalize_expired_disconnected_ship must despawn the ball cleanly.
    ctx.advance_secs(60);
    tick_background_cluster_with_step(&ctx, 60).await;
    assert!(
        sys.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "prelogoff hold must expire and despawn ball cleanly"
    );
    assert_eq!(
        ctx.prelogoff_hold_session(char_id),
        None,
        "expired prelogoff hold entry must be cleared from ContextGraceStore"
    );

    // Conversely, if a DIFFERENT replacement session claims `char_id`, any stale hold is pruned immediately.
    let prior_pair = ctx
        .db
        .get_online_session(user_id)
        .filter(|os| os.session_id == 9201)
        .map(|os| (os.session_id, os.connected_at_unix))
        .expect("session 9201 lock active");
    ctx.record_prelogoff_hold_session(char_id, Some(prior_pair));
    assert_eq!(
        ctx.prelogoff_hold_session(char_id),
        Some(prior_pair),
        "prelogoff hold must be populated before replacement claim so cluster_tick pruning is exercised"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.sp6_lifecycle.disconnected_ships.get_mut(&char_id) {
            st.removed_from_space = false;
            st.remove_at_unix = ctx.now_unix() + 60;
        }
    });
    ctx.db
        .acquire_online_lock(user_id, 9202, Some(char_id), true, ctx.now_unix())
        .expect("replacement session claims char_id");
    ctx.advance_secs(1);
    tick_background_cluster_with_step(&ctx, 1).await;
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "new session claiming char_id must invalidate prelogoff grace and prune disconnected_ships"
    );
    assert_eq!(
        ctx.prelogoff_hold_session(char_id),
        None,
        "invalidated prelogoff hold entry must be cleared from ContextGraceStore"
    );
}

#[tokio::test]
async fn finalize_expired_disconnected_ship_aborts_when_consumed_or_held_for_reconnect() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let pos = eve_destiny::DVec3::new(150_000.0, 25_000.0, -10_000.0);
    let now = ctx.now_unix();

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.user_id = user_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let sh_id = ch.active_ship_id;
        if let Some(sh) = snap.items.get_mut(&sh_id) {
            sh.location_id = i64::from(sys_id);
            sh.flag = InventoryFlag::None;
        }
        sh_id
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .expect("spawn ball");

    let disc = DisconnectedShipEntry {
        char_id,
        ship_id,
        ship_type_id: 587,
        solar_system_id: sys_id,
        disconnected_at_unix: now.saturating_sub(60),
        last_position: [pos.x, pos.y, pos.z],
        emergency_warp_dest: None,
        pre_warp_position: Some([pos.x, pos.y, pos.z]),
        last_velocity: [0.0, 0.0, 0.0],
        remove_at_unix: now.saturating_sub(1),
        removed_from_space: true,
    };
    with_mut_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.insert(char_id, disc.clone());
    });

    // Case 1: ReconnectHoldGuard active while `disc` is still present -> aborts despawn and clears `removed_from_space`.
    let reconnect_guard = crate::router::binding::ReconnectHoldGuard::acquire(&ctx, char_id, 92_101);
    ctx.db
        .acquire_online_lock(user_id, 92_101, Some(char_id), true, now)
        .expect("S2 acquires char_id lock");
    super::super::disconnect_hold::finalize_expired_disconnected_ship(&ctx, disc.clone());
    assert!(
        sys.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "live ship ball must not be despawned while ReconnectHoldGuard is held"
    );
    let still_armed = ctx.db.with_snapshot(|snap| {
        snap.sp6_lifecycle
            .disconnected_ships
            .get(&char_id)
            .is_some_and(|d| !d.removed_from_space)
    });
    assert!(
        still_armed,
        "aborted finalization under ReconnectHoldGuard must reset removed_from_space = false"
    );

    // Case 2: `reconnect_character_in_space_sync` has already consumed `disconnected_ships[char_id]`
    // while `ReconnectHoldGuard` is still held -> finalization must abort before touching the live ball or `leave_space_in_snap`.
    crate::sp6_lifecycle::reconnect_character_in_space_sync(&ctx, char_id)
        .expect("reconnect succeeds");
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.remove(&ship_id);
    });
    super::super::disconnect_hold::finalize_expired_disconnected_ship(&ctx, disc);
    drop(reconnect_guard);
    assert!(
        sys.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "finalization after disconnected_ships consumption must never despawn reconnected ball"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.item_positions.contains_key(&ship_id)
                && snap
                    .sp6_lifecycle
                    .ship_combat_hp
                    .get(&(-ship_id))
                    .is_none_or(|m| m.armor_hp >= 0.0),
            "finalization after consumption must not invoke leave_space_in_snap"
        );
        assert_eq!(snap.items[&ship_id].location_id, i64::from(sys_id));
    });
}

#[tokio::test]
async fn is_disconnected_ship_valid_queries_reconnect_hold_post_session_read() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.user_id = user_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let sh_id = ch.active_ship_id;
        if let Some(sh) = snap.items.get_mut(&sh_id) {
            sh.location_id = i64::from(sys_id);
            sh.flag = InventoryFlag::None;
        }
        sh_id
    });

    let disc = DisconnectedShipEntry {
        char_id,
        ship_id,
        ship_type_id: 587,
        solar_system_id: sys_id,
        disconnected_at_unix: now.saturating_sub(30),
        last_position: [150_000.0, 0.0, 0.0],
        emergency_warp_dest: None,
        pre_warp_position: Some([150_000.0, 0.0, 0.0]),
        last_velocity: [0.0, 0.0, 0.0],
        remove_at_unix: now + 60,
        removed_from_space: false,
    };
    with_mut_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.insert(char_id, disc.clone());
    });

    // Simulate SelectCharacterID order: ReconnectHoldGuard acquired, then online lock acquired
    // AFTER a stale caller would have passed `reconnecting_session: None`.
    let reconnect_guard = crate::router::binding::ReconnectHoldGuard::acquire(&ctx, char_id, 92_102);
    ctx.db
        .acquire_online_lock(user_id, 92_102, Some(char_id), true, now)
        .expect("S2 acquires char_id lock");

    let (valid_with_runtime, valid_tx_only) = ctx.db.with_snapshot(|snap| {
        let r = super::super::disconnect_hold::is_disconnected_ship_valid(
            snap.items.contains_key(&ship_id),
            snap.characters.get(&char_id),
            &*snap.sessions,
            Some(&ctx),
            None,
            None,
            Some(&ctx),
            char_id,
            &disc,
            true,
        );
        let tx = super::super::disconnect_hold::is_disconnected_ship_valid(
            snap.items.contains_key(&ship_id),
            snap.characters.get(&char_id),
            &*snap.sessions,
            None,
            None,
            None,
            Some(&ctx),
            char_id,
            &disc,
            true,
        );
        (r, tx)
    });
    assert!(
        valid_with_runtime && valid_tx_only,
        "post-read reconnect_hold_ctx check must preserve disconnected ship despite None pre-snapshot"
    );

    ctx.advance_secs(1);
    tick_background_cluster_with_step(&ctx, 1).await;
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "cluster_tick must retain disconnected_ships while ReconnectHoldGuard is held"
    );

    drop(reconnect_guard);
    ctx.advance_secs(1);
    tick_background_cluster_with_step(&ctx, 1).await;
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "cluster_tick must prune disconnected_ships once ReconnectHoldGuard drops while S2 holds online lock"
    );
}

#[test]
fn populate_persisted_space_objects_preserves_bonused_ship_hp_and_fitted_bonuses_after_restart() {
    let ctx = ServerContext::new();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let venture_type_id = 32_880_u32;
    let now_unix = ctx.db.clock().now_unix_secs();

    let ship_a = 8_814_530_001_i64;
    let ship_b = 8_814_530_002_i64;
    let abandoned_ship = 8_814_530_003_i64;
    let extender_b = 8_814_530_101_i64;
    let plate_b = 8_814_530_102_i64;
    let extender_ab = 8_814_530_201_i64;
    let plate_ab = 8_814_530_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.clone_grade = 2; // Omega: un-capped level 5 HP skills
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
            }
            for skill_id in [3419_u32, 3394, 3392] {
                snap.skills.entry(cid).or_default().insert(
                    skill_id,
                    eve_db::TrainedSkillRecord {
                        type_id: skill_id,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.characters.get_mut(&char_a).unwrap().active_ship_id = ship_a;
        snap.characters.get_mut(&char_b).unwrap().active_ship_id = ship_b;

        for (item_id, owner_id, location_id, type_id, flag) in [
            (ship_a, char_a, sys_id as i64, venture_type_id, InventoryFlag::None),
            (ship_b, char_b, sys_id as i64, venture_type_id, InventoryFlag::None),
            (abandoned_ship, char_a, sys_id as i64, venture_type_id, InventoryFlag::None),
            (extender_b, char_b, ship_b, 377_u32, InventoryFlag::MedSlot0),
            (plate_b, char_b, ship_b, 11_295_u32, InventoryFlag::LoSlot0),
            (extender_ab, char_a, abandoned_ship, 377_u32, InventoryFlag::MedSlot0),
            (plate_ab, char_a, abandoned_ship, 11_295_u32, InventoryFlag::LoSlot0),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for mod_id in [extender_b, plate_b, extender_ab, plate_ab] {
            snap.module_online_states.insert(mod_id, true);
        }
        snap.item_positions.insert(abandoned_ship, [25_000.0, 0.0, 0.0]);

        let st = snap.sp6_lifecycle.as_inner_mut();
        st.disconnected_ships.insert(
            char_a,
            DisconnectedShipEntry {
                char_id: char_a,
                ship_id: ship_a,
                ship_type_id: venture_type_id,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                emergency_warp_dest: None,
                last_position: [10_000.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        st.disconnected_ships.insert(
            char_b,
            DisconnectedShipEntry {
                char_id: char_b,
                ship_id: ship_b,
                ship_type_id: venture_type_id,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                emergency_warp_dest: None,
                last_position: [15_000.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        // Venture base SDE: 225 / 175 / 200; level 5 skills (1.25x): 281.25 / 218.75 / 250.0.
        st.ship_combat_hp.insert(
            ship_a,
            PersistedCombatHp {
                shield_hp: 281.25,
                armor_hp: 218.75,
                hull_hp: 250.0,
            },
        );
        st.ship_combat_hp.insert(
            -ship_a,
            PersistedCombatHp {
                shield_hp: 1.0 + 180.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
            },
        );
        // Ship B has Small Shield Extender I (+400) + 200mm Steel Plates I (+450) -> 781.25 / 781.25 / 250.0.
        st.ship_combat_hp.insert(
            ship_b,
            PersistedCombatHp {
                shield_hp: 500.0,
                armor_hp: 999_999.0, // must clamp at bonused 781.25, NOT unbonused 175.0
                hull_hp: 240.0,
            },
        );
        // Abandoned ship (unpiloted -> module bonuses only: 625.0 / 625.0 / 200.0).
        st.ship_combat_hp.insert(
            abandoned_ship,
            PersistedCombatHp {
                shield_hp: 625.0,
                armor_hp: 500.0,
                hull_hp: 200.0,
            },
        );
    });

    let mut ballpark = Ballpark::new(sys_id);
    let _ = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);

    let cs_a = ballpark.combat_states.get(&ship_a).expect("ship_a restored");
    assert!((cs_a.max_shield_hp - 281.25).abs() < 1e-6, "got {}", cs_a.max_shield_hp);
    assert!((cs_a.shield_hp - 281.25).abs() < 1e-6, "full bonused shield must not clamp to 225.0 (#1453)");
    assert!((cs_a.max_armor_hp - 218.75).abs() < 1e-6, "got {}", cs_a.max_armor_hp);
    assert!((cs_a.armor_hp - 218.75).abs() < 1e-6, "full bonused armor must not clamp to 175.0 (#1453)");
    assert!((cs_a.max_structure_hp - 250.0).abs() < 1e-6, "got {}", cs_a.max_structure_hp);
    assert!((cs_a.structure_hp - 250.0).abs() < 1e-6, "full bonused hull must not clamp to 200.0 (#1453)");
    assert!((cs_a.capacitor - 180.0).abs() < 1e-6, "persisted capacitor metadata must restore");

    let cs_b = ballpark.combat_states.get(&ship_b).expect("ship_b restored");
    assert!((cs_b.max_shield_hp - 781.25).abs() < 1e-6, "got {}", cs_b.max_shield_hp);
    assert!((cs_b.shield_hp - 500.0).abs() < 1e-6, "damaged bonused shield preserved");
    assert!((cs_b.max_armor_hp - 781.25).abs() < 1e-6, "got {}", cs_b.max_armor_hp);
    assert!((cs_b.armor_hp - 781.25).abs() < 1e-6, "overcap clamps to bonused armor max");
    assert!((cs_b.max_structure_hp - 250.0).abs() < 1e-6);
    assert!((cs_b.structure_hp - 240.0).abs() < 1e-6);

    let cs_ab = ballpark
        .combat_states
        .get(&abandoned_ship)
        .expect("abandoned ship restored");
    assert!((cs_ab.max_shield_hp - 625.0).abs() < 1e-6, "got {}", cs_ab.max_shield_hp);
    assert!((cs_ab.shield_hp - 625.0).abs() < 1e-6);
    assert!((cs_ab.max_armor_hp - 625.0).abs() < 1e-6, "got {}", cs_ab.max_armor_hp);
    assert!((cs_ab.armor_hp - 500.0).abs() < 1e-6);
    assert!((cs_ab.max_structure_hp - 200.0).abs() < 1e-6);
}

#[test]
fn populate_persisted_space_objects_reconstructs_mid_warp_phases_and_preserves_aligning_fixtures() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;
    let venture_type_id = 32_880_u32; // warp speed = 5.0 AU/s (k_a = 5.0, k_d = 5.0/3.0), max_vel = 335.0
    let now_unix = ctx.db.clock().now_unix_secs();

    let cruise_char = 90_145_401_i64;
    let cruise_ship = 8_814_540_001_i64;
    let decel_char = 90_145_402_i64;
    let decel_ship = 8_814_540_002_i64;
    let accel_char = 90_145_403_i64;
    let accel_ship = 8_814_540_003_i64;
    let align_char = 90_145_404_i64;
    let align_ship = 8_814_540_004_i64;

    let origin = DVec3::new(0.0, 0.0, 0.0);
    let dest = DVec3::new(50.0 * AU_METERS, 0.0, 0.0);
    let dir = DVec3::X;

    let v_cruise = 5.0 * AU_METERS;
    let cruise_pos = origin + dir * (10.0 * AU_METERS);
    // Final-second deceleration window: k_d = 5/3, rem_dist = 150.0 -> rem_plus_1 * k_d = 251.6667 m/s < max_vel (335.0)
    let decel_pos = dest - dir * 150.0;
    let decel_speed = 151.0 * (5.0 / 3.0);
    // Accelerating phase: speed well above max_vel (50_000 m/s) and far from decel threshold
    let accel_pos = origin + dir * 20_000.0;
    let align_pos = origin + dir * 500_000.0;

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.sp6_lifecycle.as_inner_mut();
        for (cid, sid, pos, vel, pre_warp) in [
            (cruise_char, cruise_ship, cruise_pos, [v_cruise, 0.0, 0.0], None),
            (decel_char, decel_ship, decel_pos, [decel_speed, 0.0, 0.0], Some(origin.to_array())),
            (accel_char, accel_ship, accel_pos, [50_000.0, 0.0, 0.0], Some(origin.to_array())),
            (align_char, align_ship, align_pos, [0.0, 0.0, 0.0], Some(origin.to_array())),
        ] {
            st.disconnected_ships.insert(
                cid,
                DisconnectedShipEntry {
                    char_id: cid,
                    ship_id: sid,
                    ship_type_id: venture_type_id,
                    solar_system_id: sys_id,
                    disconnected_at_unix: now_unix,
                    remove_at_unix: now_unix + 60,
                    emergency_warp_dest: Some(dest.to_array()),
                    last_position: pos.to_array(),
                    last_velocity: vel,
                    removed_from_space: false,
                    pre_warp_position: pre_warp,
                },
            );
        }
    });

    let mut ballpark = Ballpark::new(sys_id);
    let _ = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);

    let b_cruise = ballpark.balls.get(&cruise_ship).expect("cruise ship restored").clone();
    assert!(
        matches!(b_cruise.mode, MovementMode::Warp { phase: WarpPhase::Cruising, .. }),
        "expected Cruising, got {:?}",
        b_cruise.mode
    );
    assert!((b_cruise.velocity.x - v_cruise).abs() < 1.0);
    assert!((b_cruise.speed_fraction - 1.0).abs() < 1e-6);
    assert!(!ballpark.combat_states[&cruise_ship].is_emergency_warping);

    let b_decel = ballpark.balls.get(&decel_ship).expect("decel ship restored").clone();
    assert!(
        matches!(b_decel.mode, MovementMode::Warp { phase: WarpPhase::Decelerating, .. }),
        "expected Decelerating even when speed <= max_vel in final second, got {:?}",
        b_decel.mode
    );
    assert!((b_decel.velocity.x - decel_speed).abs() < 1e-3);
    assert!((b_decel.speed_fraction - 1.0).abs() < 1e-6);
    assert!(ballpark.combat_states[&decel_ship].is_emergency_warping);

    let b_accel = ballpark.balls.get(&accel_ship).expect("accel ship restored").clone();
    assert!(
        matches!(b_accel.mode, MovementMode::Warp { phase: WarpPhase::Accelerating, .. }),
        "expected Accelerating, got {:?}",
        b_accel.mode
    );
    assert!((b_accel.velocity.x - 50_000.0).abs() < 1e-3);
    assert!((b_accel.speed_fraction - 1.0).abs() < 1e-6);

    let b_align = ballpark.balls.get(&align_ship).expect("align fixture preserved").clone();
    assert!(
        matches!(b_align.mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. }),
        "zero last_velocity fixture must still restore Aligning, got {:?}",
        b_align.mode
    );
    assert_eq!(b_align.velocity, DVec3::ZERO);

    // Stepping ballpark 1s advances cruise ship smoothly without resetting to Aligning and
    // lands the decelerating ship cleanly with CCP subwarp coast decaying below 0.5 m/s.
    ballpark.step(1.0);
    let post_cruise = &ballpark.balls[&cruise_ship];
    assert!(matches!(post_cruise.mode, MovementMode::Warp { phase: WarpPhase::Cruising, .. }));
    assert!(
        post_cruise.position.x > cruise_pos.x,
        "cruising warp must continue advancing after restart"
    );

    let mut decel_landed = false;
    for _ in 0..5 {
        if matches!(ballpark.balls[&decel_ship].mode, MovementMode::Stop) {
            decel_landed = true;
            break;
        }
        ballpark.step(1.0);
    }
    assert!(decel_landed, "decelerating restored warp must complete cleanly");
    let exit_ball = &ballpark.balls[&decel_ship];
    let mut prev_speed = exit_ball.velocity.length();
    assert!(
        prev_speed <= exit_ball.max_velocity + 1e-6,
        "warp exit must clamp coast speed to max_velocity, got {prev_speed}"
    );
    assert_eq!(exit_ball.speed_fraction, 0.0);

    for _ in 0..120 {
        ballpark.step(1.0);
        let speed = ballpark.balls[&decel_ship].velocity.length();
        if prev_speed > 1e-6 {
            assert!(
                speed < prev_speed,
                "post-warp coast velocity must decay monotonically: prev={prev_speed}, cur={speed}"
            );
        }
        prev_speed = speed;
        if prev_speed < 0.5 {
            break;
        }
    }
    assert!(
        prev_speed < 0.5,
        "post-warp coast must decay below 0.5 m/s, got {prev_speed}"
    );
}

