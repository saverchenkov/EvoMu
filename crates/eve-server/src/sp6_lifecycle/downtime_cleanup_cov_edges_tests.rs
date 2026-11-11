//! Edge-case unit tests for `cluster_tick_downtime` (`recover_scanner_probes_on_downtime`,
//! `flush_pending_bounty_ticks_on_downtime`, `disconnect_safe_logoff_sessions`, `clear_safe_logoff`,
//! and non-combat Accepted mission reset).

use super::super::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime, clear_safe_logoff,
    disconnect_safe_logoff_sessions, flush_pending_bounty_ticks_on_downtime,
    recover_scanner_probes_on_downtime,
};
use super::test_ctx;
use crate::router::{ActiveScannerProbe, BoundObjectEntry};
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::sp6_lifecycle::{with_mut_sp6_state, with_sp6_state, SafeLogoffEntry};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downtime_recover_probes_bounties_safe_logoff_and_courier_mission() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let user_id = 1u64;
    let now_unix = ctx.now_unix();

    let ship_ok_id = 97_100_100i64;
    let launcher_ok_id = 97_100_101i64;
    let ship_full_id = 97_100_200i64;
    let launcher_full_id = 97_100_201i64;
    let existing_clip_id = 97_100_202i64;
    let cargo_filler_id = 97_100_203i64;
    let missing_ship_id = 97_100_999i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = user_id;
            ch.corporation_id = 1_000_009;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            ship_ok_id,
            InventoryItemRecord {
                item_id: ship_ok_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::CAPACITY, 50.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            launcher_ok_id,
            InventoryItemRecord {
                item_id: launcher_ok_id,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_ok_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_full_id,
            InventoryItemRecord {
                item_id: ship_full_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::CAPACITY, 0.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            launcher_full_id,
            InventoryItemRecord {
                item_id: launcher_full_id,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_full_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            existing_clip_id,
            InventoryItemRecord {
                item_id: existing_clip_id,
                type_id: 30_013,
                owner_id: char_id,
                location_id: ship_full_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_filler_id,
            InventoryItemRecord {
                item_id: cargo_filler_id,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_full_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Non-combat Accepted mission (has_combat_wave == false)
        snap.active_missions.insert(
            (char_id, 5555),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 5555,
                agent_id: 3008416,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                npcs_remaining: 0,
                current_room_id: 3,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let make_probe = |pid: i64, sid: i64, lid: i64, state: i32, launched: u64, mft: f64| ActiveScannerProbe {
        probe_id: pid,
        probe_type_id: 30_013,
        owner_char_id: char_id,
        ship_id: sid,
        launcher_module_id: lid,
        solar_system_id: sys_id,
        position: [1_000.0, 0.0, 0.0],
        destination: [1_000.0, 0.0, 0.0],
        range_step: 4,
        scan_range_m: 1_000_000.0,
        base_sensor_strength: 40.0,
        max_flight_time_s: mft,
        launched_at_unix: launched,
        state,
        warp_end_ft: Some(12345),
    };
    let p_expired_state = 97_110_001i64;
    let p_expired_time = 97_110_002i64;
    let p_missing_ship = 97_110_003i64;
    let p_full_ship = 97_110_004i64;
    let mut ok_probe_ids = Vec::new();
    for idx in 0..9 {
        ok_probe_ids.push(97_110_100 + idx);
    }

    for &pid in [p_expired_state, p_expired_time].iter().chain(ok_probe_ids.iter()) {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: pid,
                type_id: 30_013,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 100.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(p_expired_state, make_probe(p_expired_state, ship_ok_id, launcher_ok_id, -1, now_unix, 3600.0));
        map.insert(p_expired_time, make_probe(p_expired_time, ship_ok_id, launcher_ok_id, 0, now_unix.saturating_sub(4000), 3600.0));
        map.insert(p_missing_ship, make_probe(p_missing_ship, missing_ship_id, launcher_ok_id, 1, now_unix, 3600.0));
        map.insert(p_full_ship, make_probe(p_full_ship, ship_full_id, launcher_full_id, 1, now_unix, 3600.0));
        for &pid in &ok_probe_ids {
            map.insert(pid, make_probe(pid, ship_ok_id, launcher_ok_id, 0, now_unix, 3600.0));
        }
    }

    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 25_000.0);
        rt.pending_bounty_ticks.insert((sys_id, 90_000_002), 0.0);
    });
    let flushed = flush_pending_bounty_ticks_on_downtime(&ctx);
    assert_eq!(flushed, 1);

    let (recovered, purged) = recover_scanner_probes_on_downtime(&ctx);
    assert_eq!(recovered, 9);
    assert_eq!(purged, 2);

    ctx.db.with_snapshot(|snap| {
        let hi0_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_ok_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_ok_id)
            .map(|i| i.quantity)
            .sum();
        let cargo_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_ok_id && i.flag == InventoryFlag::Cargo)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(hi0_qty, 8);
        assert_eq!(cargo_qty, 1);
    });
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&p_missing_ship));
        assert!(map.contains_key(&p_full_ship));
        assert_eq!(map[&p_full_ship].state, 0);
        assert!(map[&p_full_ship].warp_end_ft.is_none());
    }
    assert!(sys_handle.get_ball_snapshot(p_expired_state).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(ok_probe_ids[0]).await.unwrap().is_none());

    let (_wrecks, missions_reset) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(missions_reset, 0);
    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, 5555)];
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 0);
    });
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.missions_awaiting_downtime_respawn.contains(&(char_id, 5555)));
    });

    let _ = ctx
        .db
        .acquire_online_lock(user_id, 4242, Some(char_id), true, now_unix);
    ctx.bound_objects.write().unwrap().insert(
        999_u64,
        BoundObjectEntry {
            bound_id: 999,
            session_id: 4242,
            user_id,
            character_id: char_id,
            service: "ship".into(),
            object_id: ship_ok_id,
            oid_str: "bound-safe-test".into(),
            ref_id: 1,
        },
    );
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id: ship_ok_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 30,
                completed: false,
            },
        );
    });
    assert!(ctx.db.get_online_session(user_id).is_some());
    disconnect_safe_logoff_sessions(&ctx, char_id);
    assert!(ctx.db.get_online_session(user_id).is_none());
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&999_u64));

    clear_safe_logoff(&ctx.db, char_id);
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.safe_logoffs.contains_key(&char_id));
    });
}
