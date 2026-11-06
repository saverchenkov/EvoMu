//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle::cluster_tick`
//! and `sp6_lifecycle::cluster_tick_downtime`.

use super::*;
use crate::router::ServerContext;
use crate::services::mobile_deployables::MobileDeployableRecord;
use crate::sp6_lifecycle::{
    record_anchor_meta_in_snap, with_mut_sp6_state, with_sp6_state, DisconnectedShipEntry,
    PersistedSpaceObjectMeta, SafeLogoffEntry, MOBILE_WARP_DISRUPTOR_DECAY_SECS,
    SECURE_CONTAINER_DECAY_SECS,
};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::Ballpark;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn deep_downtime_npc_wreck_purge_and_mission_wave_respawn() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let agent_accepted = 3_019_101u32;
    let agent_offered = 3_019_102u32;
    let npc_wreck_id = 950_000_001i64;
    let child_cargo_id = 950_000_002i64;
    let player_wreck_id = 950_000_003i64;
    let stale_npc_id = 950_000_010i64;
    let now_unix = ctx.now_unix();

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.active_missions.clear();
        snap.items.insert(
            npc_wreck_id,
            InventoryItemRecord {
                item_id: npc_wreck_id,
                type_id: 26557,
                owner_id: 1_000_125,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions
            .insert(npc_wreck_id, [10_000.0, 0.0, 0.0]);
        st.space_object_meta.insert(
            npc_wreck_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix),
                is_npc_wreck: true,
                ..Default::default()
            },
        );

        snap.items.insert(
            child_cargo_id,
            InventoryItemRecord {
                item_id: child_cargo_id,
                type_id: 34,
                owner_id: 1_000_125,
                location_id: npc_wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        snap.items.insert(
            player_wreck_id,
            InventoryItemRecord {
                item_id: player_wreck_id,
                type_id: 26557,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions
            .insert(player_wreck_id, [20_000.0, 0.0, 0.0]);
        st.space_object_meta.insert(
            player_wreck_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now_unix),
                is_npc_wreck: false,
                ..Default::default()
            },
        );

        snap.active_missions.insert(
            (char_id, agent_accepted),
            ActiveMissionRecord {
                mission_id: 701,
                character_id: char_id,
                agent_id: agent_accepted as i64,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 4,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![stale_npc_id],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, agent_offered),
            ActiveMissionRecord {
                mission_id: 702,
                character_id: char_id,
                agent_id: agent_offered as i64,
                solar_system_id: sys_id,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 3,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:701"), 3);
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:702"), 9);
    });

    let (wrecks_purged, missions_reset) =
        crate::sp6_lifecycle::cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(wrecks_purged, 1);
    assert_eq!(missions_reset, 1);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&npc_wreck_id));
        assert!(!snap.items.contains_key(&child_cargo_id));
        assert!(snap.items.contains_key(&player_wreck_id));
        let m = snap
            .active_missions
            .get(&(char_id, agent_accepted))
            .expect("accepted mission retained");
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 3);
        assert!(m.spawned_npc_ball_ids.is_empty());
        assert!(!snap
            .sp6_lifecycle
            .mission_initial_npc_counts
            .contains_key(&format!("{char_id}:702")));
    });

    // Wrong solar system / wrong character does not spawn wave
    assert_eq!(
        crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
            &ctx,
            char_id,
            30_000_144,
            DVec3::new(50_000.0, 0.0, 0.0),
        ),
        0
    );

    // Matching pocket entry spawns 3 pirate NPCs and clears pending respawn marker
    let spawned = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
        &ctx,
        char_id,
        sys_id,
        DVec3::new(50_000.0, 0.0, 0.0),
    );
    assert_eq!(spawned, 3);
    assert_eq!(
        crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
            &ctx,
            char_id,
            sys_id,
            DVec3::new(50_000.0, 0.0, 0.0),
        ),
        0
    );
}

#[tokio::test]
async fn deep_space_object_anchoring_and_expiry_lifecycle() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now_unix = ctx.now_unix();

    let mwd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12198);
    let container_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340)
        .map(|t| t.type_id)
        .unwrap_or(17366);
    let deployable_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1246)
        .map(|t| t.type_id)
        .unwrap_or(33475);

    let mwd_id = 920_001;
    let container_id = 920_002;
    let deployable_id = 920_003;
    let planet_launch_id = 920_004;

    with_mut_sp6_state(&ctx.db, |st, snap| {
        for (iid, tid, pos) in [
            (mwd_id, mwd_tid, [15_000.0, 0.0, 0.0]),
            (container_id, container_tid, [25_000.0, 0.0, 0.0]),
            (deployable_id, deployable_tid, [35_000.0, 0.0, 0.0]),
            (planet_launch_id, 23, [45_000.0, 0.0, 0.0]),
        ] {
            snap.items.insert(
                iid,
                make_item(iid, tid, 90_000_001, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.item_positions.insert(iid, pos);
        }

        // Completed anchor timestamp for bubble, secure container, and mobile deployable
        st.space_object_meta.insert(
            mwd_id,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                anchor_start_unix: Some(now_unix.saturating_sub(20)),
                anchoring_delay_secs: 10.0,
                bubble_radius_m: Some(25_000.0),
                anchor_completed: false,
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            container_id,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                anchor_start_unix: Some(now_unix.saturating_sub(20)),
                anchoring_delay_secs: 10.0,
                anchor_completed: false,
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            deployable_id,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                anchor_start_unix: Some(now_unix.saturating_sub(20)),
                anchoring_delay_secs: 10.0,
                anchor_completed: false,
                ..Default::default()
            },
        );
        crate::services::mobile_deployables::write_deployable_state_snap(snap, |dst| {
            dst.deployables.insert(
                deployable_id,
                MobileDeployableRecord {
                    item_id: deployable_id,
                    type_id: deployable_tid,
                    group_id: 1246,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: now_unix.saturating_sub(20),
                    anchor_duration_secs: 10,
                    anchor_completed: false,
                    is_unanchored: false,
                    expires_at_unix: now_unix + 3_600,
                    last_interaction_unix: now_unix.saturating_sub(20),
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [35_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
        st.space_object_meta.insert(
            planet_launch_id,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                created_unix: Some(now_unix.saturating_sub(PLANETARY_LAUNCH_DECAY_SECS + 10)),
                planet_id: Some(40_009_086),
                ..Default::default()
            },
        );
    });

    on_advance_secs(&ctx, now_unix, 1);

    with_sp6_state(&ctx.db, |st| {
        assert!(st.space_object_meta[&mwd_id].anchor_completed);
        assert!(st.space_object_meta[&container_id].anchor_completed);
        assert!(st.space_object_meta[&deployable_id].anchor_completed);
    });
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&planet_launch_id)));

    // Expire container and mobile warp disruptor by moving anchor_start_unix far into the past
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        if let Some(m) = st.space_object_meta.get_mut(&container_id) {
            m.anchor_start_unix = Some(now_unix.saturating_sub(SECURE_CONTAINER_DECAY_SECS + 5));
        }
        if let Some(m) = st.space_object_meta.get_mut(&mwd_id) {
            m.anchor_start_unix =
                Some(now_unix.saturating_sub(MOBILE_WARP_DISRUPTOR_DECAY_SECS + 1_000_000));
        }
    });
    on_advance_secs(&ctx, now_unix, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&container_id));
        assert!(!snap.items.contains_key(&mwd_id));
    });
}

#[tokio::test]
async fn deep_disconnected_warp_and_safe_logoff_abort_transitions() {
    let ctx = test_ctx();
    let now_unix = ctx.now_unix();
    let sys_id = 30_000_142;
    let cid_warp = 90_000_001;
    let sid_warp = 100_000_001;
    let cid_abort = 90_000_002;
    let sid_abort = 925_002;

    // 1. Disconnected ship mid-emergency-warp steps forward on on_advance_secs and later expires
    let start_pos = DVec3::new(100_000.0, 0.0, 0.0);
    let dest_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    with_mut_sp6_state(&ctx.db, |st, snap| {
        if let Some(ch) = snap.characters.get_mut(&cid_warp) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid_warp;
        }
        snap.items.insert(
            sid_warp,
            make_item(sid_warp, 587, cid_warp, sys_id as i64, InventoryFlag::None, 1),
        );
        st.disconnected_ships.insert(
            cid_warp,
            DisconnectedShipEntry {
                char_id: cid_warp,
                ship_id: sid_warp,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                emergency_warp_dest: Some([dest_pos.x, dest_pos.y, dest_pos.z]),
                last_position: [start_pos.x, start_pos.y, start_pos.z],
                removed_from_space: false,
                pre_warp_position: Some([start_pos.x, start_pos.y, start_pos.z]),
            },
        );

        // 2. Pending safe logoff whose pilot gains an active weapon timer -> aborted on tick
        st.safe_logoffs.insert(
            cid_abort,
            SafeLogoffEntry {
                char_id: cid_abort,
                ship_id: sid_abort,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 25,
                completed: false,
            },
        );
        st.weapon_timers_until.insert(cid_abort, now_unix + 50);
    });

    on_advance_secs(&ctx, now_unix, 5);

    with_sp6_state(&ctx.db, |st| {
        assert!(!st.safe_logoffs.contains_key(&cid_abort));
        let disc = st.disconnected_ships.get(&cid_warp).expect("still in space");
        assert!(!disc.removed_from_space);
    });

    // Trigger populate_persisted_space_objects with pending anchoring disruptor & expired disconnect
    let mut bp = Ballpark::new(sys_id);
    let pend_bub_id = 900_200_100;
    let mwd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12198);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pend_bub_id,
            make_item(pend_bub_id, mwd_tid, cid_warp, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions
            .insert(pend_bub_id, [12_000.0, 0.0, 0.0]);
        record_anchor_meta_in_snap(snap, pend_bub_id, now_unix, 300.0, Some(22_000.0));
        if let Some(d) = snap.sp6_lifecycle.disconnected_ships.get_mut(&cid_warp) {
            d.remove_at_unix = now_unix;
        }
    });
    let pending_disruptors = populate_persisted_space_objects(&mut bp, sys_id, &ctx.sde, &ctx.db);
    assert_eq!(pending_disruptors.len(), 1);
    assert!(with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships
            .get(&cid_warp)
            .is_some_and(|d| d.removed_from_space)
    }));
}
