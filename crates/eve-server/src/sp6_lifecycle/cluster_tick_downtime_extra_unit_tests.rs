//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle::cluster_tick_downtime`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ContainerLifetimeEntry, ExtendedNpcEntity,
};
use crate::services::mission_fixture::{self, MissionFixture};
use crate::sp6_lifecycle::cluster_tick_downtime::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
use crate::sp6_lifecycle::{PersistedCombatHp, PersistedSpaceObjectMeta};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downtime_purges_npc_wreck_children_container_lifetimes_and_preserves_player_wrecks() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let npc_wreck_id = 950_000_001i64;
    let npc_wreck_loot_id = 950_000_002i64;
    let player_wreck_id = 950_000_003i64;
    let player_wreck_loot_id = 950_000_004i64;
    let old_mission_npc_bid = 950_000_010i64;
    let char_id = 90_000_001i64;
    let agent_accepted = 3_019_101u32;
    let agent_offered = 3_019_102u32;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for bid in [npc_wreck_id, player_wreck_id, old_mission_npc_bid] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 26557,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_all(snap);
        let st = snap.sp6_lifecycle.as_inner_mut();
        // 1. NPC wreck + child loot inside it (both removed)
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
        snap.items.insert(
            npc_wreck_loot_id,
            InventoryItemRecord {
                item_id: npc_wreck_loot_id,
                type_id: 34,
                owner_id: 1_000_125,
                location_id: npc_wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(npc_wreck_id, [1.0, 2.0, 3.0]);
        st.space_object_meta.insert(
            npc_wreck_id,
            PersistedSpaceObjectMeta {
                is_npc_wreck: true,
                ..Default::default()
            },
        );
        st.ship_combat_hp.insert(
            npc_wreck_id,
            PersistedCombatHp {
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 50.0,
            },
        );

        // 2. Player wreck (is_npc_wreck == false -> preserved along with its child loot)
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
        snap.items.insert(
            player_wreck_loot_id,
            InventoryItemRecord {
                item_id: player_wreck_loot_id,
                type_id: 34,
                owner_id: char_id,
                location_id: player_wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 900,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        st.space_object_meta.insert(
            player_wreck_id,
            PersistedSpaceObjectMeta {
                is_npc_wreck: false,
                ..Default::default()
            },
        );

        // 3. Accepted mission with 1 ball and npcs_remaining=1 (no initial count -> falls back to max(4))
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 801, agent_accepted as i64, sys_id)
                .current_room_id(3)
                .npcs_remaining(1)
                .spawned_npc_ball_ids(vec![old_mission_npc_bid]),
        );
        // 4. Offered mission -> untouched, and its initial counts/types are pruned by retain
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 802, agent_offered as i64, sys_id)
                .current_room_id(2)
                .npcs_remaining(2)
                .spawned_npc_ball_ids(vec![950_000_099])
                .initial_wave_count(Some(9)),
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            npc_wreck_id,
            ContainerLifetimeEntry {
                item_id: npc_wreck_id,
                solar_system_id: sys_id,
                type_id: 26557,
                owner_id: 1_000_125,
                created_unix: 1_000,
                lifetime_secs: 7_200,
                is_abandoned: false,
            },
        );
        rt.npc_entities.insert(
            old_mission_npc_bid,
            ExtendedNpcEntity {
                ball_id: old_mission_npc_bid,
                type_id: 10_019,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: sys_id,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 10_000.0,
                optimal_range: 5_000.0,
                orbit_range: 2_500.0,
                damage_vector: DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                custom_cargo_loot: None,
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
            },
        );
    });

    let (wrecks_removed, missions_reset) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(wrecks_removed, 1);
    assert_eq!(missions_reset, 1);

    assert!(sys_handle.get_ball_position_sync(npc_wreck_id).is_none());
    assert!(sys_handle.get_ball_position_sync(old_mission_npc_bid).is_none());
    assert!(sys_handle.get_ball_position_sync(player_wreck_id).is_some());

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.container_lifetimes.contains_key(&npc_wreck_id));
        assert!(!rt.npc_entities.contains_key(&old_mission_npc_bid));
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&npc_wreck_id));
        assert!(!snap.items.contains_key(&npc_wreck_loot_id));
        assert!(snap.items.contains_key(&player_wreck_id));
        assert!(snap.items.contains_key(&player_wreck_loot_id));

        let m_acc = mission_fixture::get(snap, char_id, 801).unwrap();
        assert_eq!(m_acc.npcs_remaining, 4);
        assert_eq!(m_acc.current_room_id, 1);
        assert!(m_acc.spawned_npc_ball_ids.is_empty());
        assert!(m_acc.awaiting_downtime_respawn);

        let m_off = mission_fixture::get(snap, char_id, 802).unwrap();
        assert_eq!(m_off.current_room_id, 2);
        assert_eq!(m_off.spawned_npc_ball_ids, vec![950_000_099]);
        assert!(m_off.initial_wave_count.is_none());
    });

    // Now test spawn_mission_wave_on_pocket_entry_if_needed with empty wave_types Vec -> default_npc_tid
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, char_id, 801, |m| m.initial_wave_types = Some(Vec::new()));
    });
    let spawned =
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, DVec3::ZERO);
    assert_eq!(spawned, 4);
    ctx.db.with_snapshot(|snap| {
        assert!(!mission_fixture::get(snap, char_id, 801)
            .unwrap()
            .awaiting_downtime_respawn);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downtime_probe_recovery_cargo_fallback_expired_missing_ship_and_safe_logoff_disconnect() {
    let ctx = test_ctx();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now_unix)));
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let full_ship_id = 955_888_002_i64;
    let missing_ship_id = 955_888_999_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            full_ship_id,
            InventoryItemRecord {
                item_id: full_ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Fill full_ship_id cargo so probe cannot fit
        snap.items.insert(
            955_888_010,
            InventoryItemRecord {
                item_id: 955_888_010,
                type_id: 34,
                owner_id: char_id,
                location_id: full_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mk_probe = |probe_id, sid, state, launched| crate::router::ActiveScannerProbe {
        probe_id,
        probe_type_id: 30013,
        owner_char_id: char_id,
        ship_id: sid,
        launcher_module_id: 0,
        solar_system_id: sys_id,
        position: [0.0; 3],
        destination: [0.0; 3],
        range_step: 1,
        scan_range_m: 1_000_000.0,
        base_sensor_strength: 40.0,
        max_flight_time_s: 3600.0,
        launched_at_unix: launched,
        state,
        warp_end_ft: None,
    };

    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        // 1. Expired probe (purged)
        map.insert(955_901, mk_probe(955_901, ship_id, 1, now_unix - 4_000));
        // 2. Missing ship probe (unrecovered)
        map.insert(955_902, mk_probe(955_902, missing_ship_id, 1, now_unix));
        // 3 & 4. Two valid probes for ship_id with no launcher -> Cargo fallback (creates stack + merges stack)
        map.insert(955_903, mk_probe(955_903, ship_id, 1, now_unix));
        map.insert(955_904, mk_probe(955_904, ship_id, 1, now_unix));
        // 5. Full cargo ship probe -> unrecovered
        map.insert(955_905, mk_probe(955_905, full_ship_id, 1, now_unix));
    }

    let (recovered, purged) =
        crate::sp6_lifecycle::cluster_tick_downtime::recover_scanner_probes_on_downtime(&ctx);
    assert_eq!(recovered, 2);
    assert_eq!(purged, 1);
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&955_902));
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&955_905));

    // Test disconnect_safe_logoff_sessions with active online session and bound_objects entry
    ctx.db
        .acquire_online_lock(1, 888, Some(char_id), true, now_unix)
        .unwrap();
    let sess = eve_machonet::SessionState {
        session_id: 889,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let _ = ctx.bind_object(&sess, "beyonce", sys_id as i64);
    crate::sp6_lifecycle::cluster_tick_downtime::disconnect_safe_logoff_sessions(&ctx, char_id);
    assert!(ctx.db.get_online_session(1).is_none());
    assert!(ctx.bound_objects.read().unwrap().is_empty());
}

#[test]
fn safe_logoff_transitions_live_closer_to_charsel_preserving_closer_and_clearing_bindings() {
    let ctx = test_ctx();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now_unix)));
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(888, close_tx);
    ctx.db
        .acquire_online_lock_with_computer_hash(
            1,
            888,
            Some(char_id),
            Some("hash-1360".to_string()),
            true,
            now_unix,
        )
        .unwrap();
    let sess_live = eve_machonet::SessionState {
        session_id: 888,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    ctx.bind_object(&sess_live, "beyonce", sys_id as i64).unwrap();

    // Also include a stale headless bound session to verify it is disconnected separately.
    let (stale_tx, mut stale_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(889, stale_tx);
    let sess_stale = eve_machonet::SessionState {
        session_id: 889,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    ctx.bind_object(&sess_stale, "beyonce", sys_id as i64).unwrap();

    crate::sp6_lifecycle::cluster_tick_downtime::disconnect_safe_logoff_sessions(&ctx, char_id);

    // Live TCP session keeps its online lock (with character_id cleared), computer_hash,
    // and closer channel so it transitions cleanly to character selection (#1360, #1386).
    let (code, reason) = close_rx
        .try_recv()
        .expect("live closer receives SafeLogoffCharsel");
    assert_eq!(code, "SafeLogoffCharsel");
    assert_eq!(reason, char_id.to_string());
    assert!(
        ctx.session_closers.read().unwrap().contains_key(&888),
        "live closer must remain registered across safe logoff to character selection"
    );
    let online = ctx
        .db
        .get_online_session(1)
        .expect("online lock must stay reserved at character selection");
    assert_eq!(online.session_id, 888);
    assert!(online.character_id.is_none());
    assert_eq!(online.computer_hash.as_deref(), Some("hash-1360"));
    assert!(ctx.bound_objects.read().unwrap().is_empty());

    // Re-selecting a character on the same session succeeds; a different session cannot hijack it.
    assert!(ctx
        .db
        .acquire_online_lock(1, 890, Some(char_id), false, now_unix + 5)
        .is_err());
    assert!(ctx
        .db
        .acquire_online_lock(1, 888, Some(char_id), false, now_unix + 5)
        .is_ok());

    // Stale bound-only session was disconnected normally and unregistered.
    let (stale_code, _) = stale_rx.try_recv().expect("stale session disconnected");
    assert_eq!(stale_code, "SafeLogoff");
    assert!(!ctx.session_closers.read().unwrap().contains_key(&889));
}

#[test]
fn safe_logoff_clears_online_lock_and_runs_logoff_hooks_before_hook_callback() {
    let ctx = test_ctx();
    let now_unix = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let watcher_id = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().user_id);
    let sid = 910_u64;

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(sid, close_tx);
    ctx.db
        .acquire_online_lock_with_computer_hash(
            user_id,
            sid,
            Some(char_id),
            Some("hash-1386-hook".to_string()),
            true,
            now_unix,
        )
        .unwrap();

    let watcher_uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&watcher_id).unwrap().user_id);
    ctx.db
        .acquire_online_lock(watcher_uid, 912, Some(watcher_id), true, now_unix)
        .unwrap();

    ctx.esi_state.char_contacts.write().unwrap().insert(
        watcher_id,
        vec![crate::esi::runtime_state::EsiContactEntry {
            contact_id: char_id,
            contact_type: "character".to_string(),
            standing: 0.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![],
        }],
    );
    let token = crate::services::fleet_social::ensure_xmpp_auth_token(&ctx, char_id);
    assert!(!token.is_empty());
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .char_rebased_system_gen
        .insert(char_id, (sys_id, 42));

    let mut hook_ran = false;
    crate::sp6_lifecycle::cluster_tick_downtime::finalize_safe_logoff_sessions_with_hook(
        &ctx,
        char_id,
        |emit_activation| {
            hook_ran = true;
            assert!(emit_activation);
            let online = ctx
                .db
                .get_online_session(user_id)
                .expect("online lock must stay reserved for charsel before pre-signal hook");
            assert_eq!(online.session_id, sid);
            assert!(
                online.character_id.is_none(),
                "online_lock.character_id must be cleared BEFORE ball removal / OnSafeLogoffActivated"
            );
            assert!(
                !ctx.systems
                    .cluster_nodes
                    .read()
                    .unwrap()
                    .char_rebased_system_gen
                    .contains_key(&char_id),
                "char_rebased_system_gen must be cleared before pre-signal hook"
            );
            ctx.db.with_snapshot(|snap| {
                assert!(snap
                    .kv(eve_db::Ns::XmppAuthToken)
                    .get::<String>(char_id, "")
                    .is_none());
                assert!(!snap
                    .service_state_blobs
                    .contains_key(&format!("xmpp:token:{char_id}")));
            });
            let watcher_notifs = ctx.drain_notifications_for_char(watcher_id);
            assert!(
                watcher_notifs.iter().any(|n| {
                    n.event == "OnContactLoggedOff"
                        && n.args == vec![eve_proto::PyRep::Integer(char_id)]
                }),
                "watcher must receive OnContactLoggedOff on safe logoff: {watcher_notifs:?}"
            );
            assert!(
                watcher_notifs.iter().any(|n| {
                    n.event == "OnClientContactChange"
                        && n.args
                            == vec![
                                eve_proto::PyRep::Integer(char_id),
                                eve_proto::PyRep::Bool(false),
                            ]
                }),
                "watcher must receive OnClientContactChange(false) on safe logoff: {watcher_notifs:?}"
            );
            assert!(
                close_rx.try_recv().is_err(),
                "SafeLogoffCharsel must be sent only after before_charsel_signal finishes"
            );
        },
    );
    assert!(hook_ran);

    let (code, reason) = close_rx
        .try_recv()
        .expect("live closer receives SafeLogoffCharsel after hook");
    assert_eq!(code, "SafeLogoffCharsel");
    assert_eq!(reason, char_id.to_string());
}

#[tokio::test]
async fn safe_logoff_advance_secs_removes_ball_and_clears_lock_watchlist_xmpp_and_rebased_gen() {
    let ctx = Arc::new(test_ctx());
    let now_unix = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let watcher_id = 90_000_002_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().user_id);
    let sid = 911_u64;

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(sid, close_tx);
    ctx.db
        .acquire_online_lock_with_computer_hash(
            user_id,
            sid,
            Some(char_id),
            Some("hash-1386-adv".to_string()),
            true,
            now_unix,
        )
        .unwrap();

    let jita = ctx.systems.get_or_spawn(sys_id);
    jita.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();
    assert!(jita.get_ball_snapshot(ship_id).await.unwrap().is_some());

    let watcher_uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&watcher_id).unwrap().user_id);
    ctx.db
        .acquire_online_lock(watcher_uid, 913, Some(watcher_id), true, now_unix)
        .unwrap();

    ctx.esi_state.char_contacts.write().unwrap().insert(
        watcher_id,
        vec![crate::esi::runtime_state::EsiContactEntry {
            contact_id: char_id,
            contact_type: "character".to_string(),
            standing: 0.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![],
        }],
    );
    let _ = crate::services::fleet_social::ensure_xmpp_auth_token(&ctx, char_id);
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .char_rebased_system_gen
        .insert(char_id, (sys_id, 7));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.sp6_lifecycle.as_inner_mut().safe_logoffs.insert(
            char_id,
            crate::sp6_lifecycle::SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 10,
                completed: false,
            },
        );
    });

    ctx.advance_secs(30);

    assert!(
        jita.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "safe logoff expiry must remove the ship ball"
    );
    let online = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock stays reserved at charsel");
    assert_eq!(online.session_id, sid);
    assert!(online.character_id.is_none());

    let (code, reason) = close_rx
        .try_recv()
        .expect("live closer receives SafeLogoffCharsel");
    assert_eq!(code, "SafeLogoffCharsel");
    assert_eq!(reason, char_id.to_string());

    let char_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        char_notifs
            .iter()
            .any(|n| n.event == "OnSafeLogoffActivated"),
        "OnSafeLogoffActivated must be emitted: {char_notifs:?}"
    );
    let watcher_notifs = ctx.drain_notifications_for_char(watcher_id);
    assert!(watcher_notifs
        .iter()
        .any(|n| n.event == "OnContactLoggedOff"));
    assert!(watcher_notifs
        .iter()
        .any(|n| n.event == "OnClientContactChange"
            && n.args
                == vec![
                    eve_proto::PyRep::Integer(char_id),
                    eve_proto::PyRep::Bool(false),
                ]));
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .kv(eve_db::Ns::XmppAuthToken)
            .get::<String>(char_id, "")
            .is_none());
        assert!(!snap
            .service_state_blobs
            .contains_key(&format!("xmpp:token:{char_id}")));
    });
    assert!(!ctx
        .systems
        .cluster_nodes
        .read()
        .unwrap()
        .char_rebased_system_gen
        .contains_key(&char_id));
}
