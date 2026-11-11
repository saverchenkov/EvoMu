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
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord, ServerClock, WorldRepository};
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

#[tokio::test]
async fn advance_secs_sim_clock_rebase_suppress_vs_emit_and_abyssal_trace_zarzakh_expiry() {
    let ctx = test_ctx();
    let base_unix = 1_800_000_000u64;
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());

    let sys_id = 30_000_142u32;
    let (cid, uid) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().expect("seed char");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        (ch.character_id, ch.user_id)
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 9_500_001, Some(cid), true, base_unix);
    let _ = ctx.drain_notifications_for_char(cid);

    let tid_exp = 900_901i64;
    let tid_live = 900_902i64;
    let tid_zero = 900_903i64;
    let sys = ctx.systems.get_or_spawn(sys_id);
    for tid in [tid_exp, tid_live, tid_zero] {
        sys.spawn_ball_with_meta(
            eve_destiny::Ball {
                id: tid,
                type_id: 47961,
                position: DVec3::new((tid - 900_900) as f64 * 1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta::default(),
        )
        .await
        .unwrap();
    }

    ctx.db.with_mut_snapshot(|snap| {
        for (tid, exp) in [
            (tid_exp, base_unix + 10),  // exact boundary == now_unix after +10s
            (tid_live, base_unix + 11), // 1s later -> kept
            (tid_zero, 0),              // 0 -> never expires
        ] {
            snap.abyssal_state.traces.insert(
                tid,
                eve_db::service_domains::AbyssalTraceRecord {
                    trace_ball_id: tid,
                    char_id: cid,
                    filament_type_id: 47757,
                    origin_solar_system_id: sys_id,
                    position: [0.0, 0.0, 0.0],
                    created_unix: base_unix,
                    expires_at_sec: exp,
                    max_ships: 1,
                    entered_char_ids: vec![cid],
                    pocket_id: None,
                    fleet_id: None,
                },
            );
        }
        snap.zarzakh_gate_locks.insert(1001, (50_000_001, base_unix + 10));
        snap.zarzakh_gate_locks.insert(1002, (50_000_002, base_unix + 11));
    });

    // Advance 10s with emit_sim_clock_rebase = false -> suppresses DoSimClockRebase, still expires trace & gate lock
    clock.advance_secs(10);
    on_advance_secs_with_sim_clock_rebase(&ctx, base_unix, 10, false);

    let notes_suppressed = ctx.drain_notifications_for_char(cid);
    assert!(
        !notes_suppressed.iter().any(|n| n.event == "DoSimClockRebase"),
        "expected DoSimClockRebase to be suppressed when emit_sim_clock_rebase == false"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.abyssal_state.traces.contains_key(&tid_exp));
        assert!(snap.abyssal_state.traces.contains_key(&tid_live));
        assert!(snap.abyssal_state.traces.contains_key(&tid_zero));
        assert!(!snap.zarzakh_gate_locks.contains_key(&1001));
        assert!(snap.zarzakh_gate_locks.contains_key(&1002));
    });
    assert!(sys.get_ball_snapshot(tid_exp).await.unwrap().is_none());
    assert!(sys.get_ball_snapshot(tid_live).await.unwrap().is_some());

    // Advance 5s with on_advance_secs (emit == true) -> emits DoSimClockRebase
    clock.advance_secs(5);
    on_advance_secs(&ctx, base_unix + 10, 5);
    let notes_emitted = ctx.drain_notifications_for_char(cid);
    assert!(
        notes_emitted.iter().any(|n| n.event == "DoSimClockRebase"),
        "expected DoSimClockRebase notification when on_advance_secs advances clock"
    );
}

#[test]
fn downtime_cleanup_syncs_runtime_to_db() {
    let ctx = test_ctx();
    {
        let mut ex = ctx.fleet_extras.lock().unwrap_or_else(|e| e.into_inner());
        ex.fleet_names.insert(999_001, "DowntimeSyncFleet".into());
    }
    assert!(
        !ctx.db.with_snapshot(|s| s
            .cluster_and_runtime
            .server_runtime_json
            .as_deref()
            .is_some_and(|j| j.contains("DowntimeSyncFleet")))
    );

    let _ = crate::sp6_lifecycle::cluster_tick_downtime::cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert!(
        ctx.db.with_snapshot(|s| s
            .cluster_and_runtime
            .server_runtime_json
            .as_deref()
            .is_some_and(|j| j.contains("DowntimeSyncFleet")))
    );
}

#[tokio::test]
async fn tick_background_cluster_cosmic_expiry_wormholes_metenox_and_bounty_ticks() {
    let ctx = test_ctx();
    let now_unix = 1_800_000_000u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now_unix)));

    let sys_id = 30_000_142u32;
    let char_a = 90_700_001i64;
    let char_b = 90_700_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for cid in [char_a, char_b] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.solar_system_id = sys_id;
            snap.characters.insert(cid, ch);
        }

        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                eve_db::CosmicAnomalyRecord {
                    anomaly_id: "anom-exp".into(),
                    solar_system_id: sys_id,
                    name: "Expired Anom".into(),
                    anomaly_type: "Combat".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_unix - 100,
                    lifetime_seconds: 100, // exact expiry boundary now_unix == created + lifetime
                    cleared: false,
                },
                eve_db::CosmicAnomalyRecord {
                    anomaly_id: "anom-live".into(),
                    solar_system_id: sys_id,
                    name: "Live Anom".into(),
                    anomaly_type: "Combat".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_unix - 99,
                    lifetime_seconds: 100,
                    cleared: false,
                },
                eve_db::CosmicAnomalyRecord {
                    anomaly_id: "anom-perm".into(),
                    solar_system_id: sys_id,
                    name: "Immortal Anom".into(),
                    anomaly_type: "Ore".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_unix - 500,
                    lifetime_seconds: 0,
                    cleared: false,
                },
            ],
        );

        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                eve_db::CosmicSignatureRecord {
                    signature_id: "SIG-EXP".into(),
                    solar_system_id: sys_id,
                    sig_group: "Data".into(),
                    type_name: "Expired Data".into(),
                    position: [0.0; 3],
                    signal_strength: 0.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix - 100,
                    lifetime_seconds: 100,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                eve_db::CosmicSignatureRecord {
                    signature_id: "SIG-WH".into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(), // case-insensitive wormhole exemption!
                    type_name: "Wormhole K162".into(),
                    position: [0.0; 3],
                    signal_strength: 0.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix - 500,
                    lifetime_seconds: 100,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                eve_db::CosmicSignatureRecord {
                    signature_id: "SIG-LIVE".into(),
                    solar_system_id: sys_id,
                    sig_group: "Relic".into(),
                    type_name: "Live Relic".into(),
                    position: [0.0; 3],
                    signal_strength: 0.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix - 50,
                    lifetime_seconds: 100,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );

        // If seed SDE includes Metenox Moon Drill (group 4744), exercise the prev_metenox_tick > 0 branch
        let corp_id = snap.corporations.keys().copied().next().unwrap_or(1_000_009);
        if let Some(met_type) = ctx.sde.types.values().find(|t| t.group_id == 4744) {
            let met_sys_id = ctx
                .sde
                .solar_systems
                .values()
                .find(|s| {
                    (s.security_status < 0.45 || s.solar_system_id >= 31_000_000)
                        && ctx
                            .sde
                            .celestials_in_system(s.solar_system_id)
                            .iter()
                            .any(|c| c.group_id == 8)
                })
                .map(|s| s.solar_system_id)
                .unwrap_or(30_000_144);
            snap.mining_and_ess_state.metenox_last_tick_unix = now_unix - 3_600;
            snap.structures.insert(
                1_088_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 1_088_000_001,
                    type_id: met_type.type_id,
                    owner_corp_id: corp_id,
                    solar_system_id: met_sys_id,
                    name: "Test Metenox".into(),
                    position: [10_000.0, 0.0, 0.0],
                    state: "Operating".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
    });

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_a), 1_000.0); // no next_flush yet -> scheduled for +1200
        rt.pending_bounty_ticks.insert((sys_id, char_b), 2_000.0);
        rt.bounty_tick_next_flush_unix.insert((sys_id, char_b), now_unix); // due immediately (`now >= next_flush`)
    });

    tick_background_cluster(&ctx).await;

    ctx.db.with_snapshot(|snap| {
        let anoms: Vec<&str> = snap.cosmic_anomalies[&sys_id]
            .iter()
            .map(|a| a.anomaly_id.as_str())
            .collect();
        assert_eq!(anoms, vec!["anom-live", "anom-perm"]);

        let sigs: Vec<&str> = snap.cosmic_signatures[&sys_id]
            .iter()
            .map(|s| s.signature_id.as_str())
            .collect();
        assert!(!sigs.contains(&"SIG-EXP"));
        assert!(sigs.contains(&"SIG-WH"));
        assert!(sigs.contains(&"SIG-LIVE"));

        if ctx.sde.types.values().any(|t| t.group_id == 4744) {
            assert_eq!(snap.mining_and_ess_state.metenox_last_tick_unix, now_unix);
        }
    });

    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert_eq!(
            rt.bounty_tick_next_flush_unix.get(&(sys_id, char_a)).copied(),
            Some(now_unix + 1_200)
        );
        assert!(!rt.bounty_tick_next_flush_unix.contains_key(&(sys_id, char_b)));
        assert!(!rt.pending_bounty_ticks.contains_key(&(sys_id, char_b)));
    });
}

#[test]
fn seed_persisted_abandoned_drones_classifies_sentry_mobile_and_fighter() {
    let sde_arc = Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())));
    let mut sde = (*sde_arc).clone();
    let mut base_type = sde.types.values().next().expect("seed type").clone();
    base_type.type_id = 996_001;
    base_type.category_id = 18;
    base_type.group_id = 100;
    base_type.default_attributes.clear();
    base_type.default_attributes.insert(37, 0.0); // max_vel < 1.0 -> sentry
    base_type.default_attributes.insert(1272, 25.0);
    sde.types.insert(996_001, base_type.clone());

    let mut mobile = base_type.clone();
    mobile.type_id = 996_002;
    mobile.default_attributes.insert(37, 1_500.0); // max_vel >= 1.0 -> mobile drone
    mobile.default_attributes.insert(1272, 5.0);
    sde.types.insert(996_002, mobile);

    let mut fighter = base_type;
    fighter.type_id = 996_003;
    fighter.category_id = 87;
    fighter.default_attributes.clear();
    fighter.default_attributes.insert(2215, 9.0);
    sde.types.insert(996_003, fighter);

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142i64;
    let sentry_id = 1_096_001i64;
    let mobile_item_id = 1_096_002i64;
    let mobile_ball_id = 1_096_902i64;
    let fighter_id = 1_096_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let owner_id = snap.characters.keys().copied().next().expect("seed char");
        snap.items.insert(
            sentry_id,
            make_item(sentry_id, 996_001, owner_id, sys_id, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mobile_item_id,
            make_item(mobile_item_id, 996_002, owner_id, sys_id, InventoryFlag::None, 1),
        );
        snap.items.insert(
            fighter_id,
            make_item(fighter_id, 996_003, owner_id, sys_id, InventoryFlag::None, 5),
        );
        snap.sp6_lifecycle.space_object_meta.insert(
            mobile_item_id,
            PersistedSpaceObjectMeta {
                drone_ball_id: Some(mobile_ball_id),
                ..Default::default()
            },
        );
    });

    seed_persisted_abandoned_drones(&ctx);

    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let s = rt.active_drones_in_space.get(&sentry_id).expect("sentry seeded");
        assert!(s.is_abandoned);
        assert!(s.is_sentry);

        let m = rt.active_drones_in_space.get(&mobile_ball_id).expect("mobile drone under drone_ball_id");
        assert!(m.is_abandoned);
        assert!(!m.is_sentry);
        assert_eq!(m.drone_item_id, mobile_item_id);
    });
    let f = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_id)
        .cloned()
        .expect("fighter squadron seeded");
    assert!(f.is_abandoned);
    assert_eq!(f.squadron_size, 5);
}
