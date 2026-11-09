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
                entered_unix: now_unix.saturating_sub(1_200),
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
