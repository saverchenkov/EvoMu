//! Deep branch-coverage and mutation-killing unit tests for `services::serverops_cluster`.

#![forbid(unsafe_code)]

use super::serverops_cluster::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    AccountRecord, OwnerNoteRecord, ServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn make_ctx(base_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[test]
fn compute_tidi_from_node_load_exact_arithmetic_and_boundaries() {
    // Threshold 0 clamps to 1
    assert_eq!(compute_tidi_from_node_load(0, 0), 1.0);
    assert_eq!(compute_tidi_from_node_load(1, 0), 1.0);
    assert_eq!(compute_tidi_from_node_load(2, 0), 0.50);
    assert_eq!(compute_tidi_from_node_load(4, 0), 0.25);
    assert_eq!(compute_tidi_from_node_load(20, 0), 0.10);

    // Exact threshold boundary (load <= threshold -> 1.0, load > threshold -> scaled)
    assert_eq!(compute_tidi_from_node_load(99, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(100, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(101, 100), 0.99);
    assert_eq!(compute_tidi_from_node_load(150, 100), 0.67);
    assert_eq!(compute_tidi_from_node_load(250, 100), 0.40);
    assert_eq!(compute_tidi_from_node_load(1_000, 100), 0.10);
    assert_eq!(compute_tidi_from_node_load(50_000, 100), 0.10);
}

#[tokio::test]
async fn system_tidi_factor_clamping_persistence_anchoring_and_override_clearing() {
    let (ctx, clock) = make_ctx(1_775_000_000);
    let jita = 30_000_142_u32;
    let unloaded_sys = 30_000_144_u32;
    let char_id = 90_000_001_i64;

    ctx.db
        .acquire_online_lock(1, 701, Some(char_id), true, 1_775_000_000)
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita;
        }
    });

    // Unloaded system reads from manual_tidi_overrides (clamped) or owner_notes (-7_500_000_000_000 - sys_id)
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);
    let nid = -(7_500_000_000_000_i64 + i64::from(unloaded_sys));
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -75,
                item_id: i64::from(unloaded_sys),
                label: format!("tidi:{unloaded_sys}"),
                note: "0.35".into(),
                timestamp_unix: 100,
            },
        );
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.35);

    // Persisted note below 0.10 or above 1.0 is clamped; invalid JSON falls back to 1.0
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "0.02".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.10);
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "2.50".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "not_json".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);

    // Manual override on unloaded system takes precedence over owner_notes and clamps
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .manual_tidi_overrides
        .insert(unloaded_sys, 0.05);
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.10);
    assert_eq!(clear_manual_system_tidi_override(&ctx, unloaded_sys), 1.0);

    // set_system_tidi_factor on active system: finite clamping, non-finite fallback, and DoSimClockRebase anchoring
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 0.50), 0.50);
    let notifs1 = ctx.drain_notifications_for_char(char_id);
    let rebase1 = notifs1
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase on first TiDi change");
    assert_eq!(rebase1.id_type, "solarsystemid2");
    let PyRep::Tuple(pair1) = &rebase1.args[0] else {
        panic!("expected tuple payload");
    };
    let (old_ft1, new_ft1) = (pair1[0].as_i64().unwrap(), pair1[1].as_i64().unwrap());
    // Seed elapsed with old_factor = 0.50 -> 5_000_000 ticks before wall clock
    assert_eq!(new_ft1 - old_ft1, 5_000_000);

    // Advance wall clock by 10 seconds (100_000_000 ticks) at 0.50 TiDi -> dilated elapsed is 50_000_000 ticks
    clock.advance_secs(10);
    assert_eq!(set_system_tidi_factor(&ctx, jita, f64::INFINITY), 1.0);
    let notifs2 = ctx.drain_notifications_for_char(char_id);
    let rebase2 = notifs2
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase on second TiDi change");
    let PyRep::Tuple(pair2) = &rebase2.args[0] else {
        panic!("expected tuple payload");
    };
    let (old_ft2, new_ft2) = (pair2[0].as_i64().unwrap(), pair2[1].as_i64().unwrap());
    assert_eq!(old_ft2, new_ft1);
    assert_eq!(new_ft2 - old_ft2, 50_000_000);

    assert_eq!(set_system_tidi_factor(&ctx, jita, f64::NEG_INFINITY), 1.0);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 1.75), 1.0);
    assert_eq!(clear_manual_system_tidi_override(&ctx, jita), 1.0);
}

#[tokio::test]
async fn worker_node_resolution_reinforcement_and_overloaded_rebalancing() {
    let (ctx, _) = make_ctx(1_775_000_000);
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let sys_c = 30_002_187_u32;

    set_cluster_node_load_threshold(&ctx, 0);
    assert_eq!(
        ctx.systems
            .cluster_nodes
            .read()
            .unwrap()
            .node_load_tidi_threshold,
        1
    );

    // Persisted owner_note (-7_600_000_000_000 - sys_id) is honored unless that node is reinforced for a different system
    let nid_c = -(7_600_000_000_000_i64 + i64::from(sys_c));
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            nid_c,
            OwnerNoteRecord {
                note_id: nid_c,
                character_id: -76,
                item_id: i64::from(sys_c),
                label: format!("node:{sys_c}"),
                note: "3".into(),
                timestamp_unix: 100,
            },
        );
    });
    assert_eq!(get_system_worker_node(&ctx, sys_c), 3);

    // Mark node 3 as reinforced for sys_a -> sys_c can no longer use persisted node 3
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .reinforced_nodes
        .insert(3, sys_a);
    assert_ne!(get_system_worker_node(&ctx, sys_c), 3);

    // If default constellation candidate is reinforced for another system and ALL worker_nodes are reinforced, falls back to 1
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        let workers = cn.worker_nodes.clone();
        for w in workers {
            cn.reinforced_nodes.insert(w, sys_a);
        }
    }
    assert_eq!(get_system_worker_node(&ctx, sys_c), 1);
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .reinforced_nodes
        .clear();

    // Spawn two active systems on the same worker node and populate balls so both have load > 0
    let ha = ctx.systems.get_or_spawn(sys_a);
    let hb = ctx.systems.get_or_spawn(sys_b);
    for (idx, h) in [(1_i64, &ha), (2_i64, &hb)] {
        for b_off in 0..3_i64 {
            let ball = Ball {
                id: 950_000 + idx * 10 + b_off,
                type_id: 587,
                position: DVec3::new((b_off as f64) * 1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            h.spawn_ball_with_meta(ball, CombatState::default(), SlimItemMeta::default())
                .await
                .unwrap();
        }
    }

    // Force both sys_a and sys_b onto general worker node 1 with threshold = 1
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        cn.worker_nodes = vec![1, 2, 3, 4];
        cn.system_node_overrides.insert(sys_a, 1);
        cn.system_node_overrides.insert(sys_b, 1);
        cn.node_load_tidi_threshold = 1;
    }
    assert!(get_worker_node_load(&ctx, 1) >= 2);

    // Recomputes load-induced TiDi (< 1.0) since load > threshold
    let tidi_map = recompute_cluster_node_tidi(&ctx);
    assert!(tidi_map[&sys_a] < 1.0);
    assert!(tidi_map[&sys_b] < 1.0);

    // Rebalance overloaded worker nodes migrates the lighter/secondary system off node 1 onto node 2
    let migrated_count = rebalance_overloaded_worker_nodes(&ctx);
    assert_eq!(migrated_count, 1);
    assert_ne!(
        get_system_worker_node(&ctx, sys_a),
        get_system_worker_node(&ctx, sys_b)
    );

    // Reinforcing onto node_id <= 16 adds to worker_nodes if not present and migrates siblings off that node
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        cn.system_node_overrides.insert(sys_b, 12);
    }
    let assigned = assign_or_reinforce_system_node(&ctx, sys_a, Some(12));
    assert_eq!(assigned, 12);
    assert_eq!(get_system_worker_node(&ctx, sys_a), 12);
    assert_ne!(get_system_worker_node(&ctx, sys_b), 12);
    assert!(ctx
        .cluster_state
        .read()
        .unwrap()
        .node_addresses
        .contains_key("127.0.0.1:26012"));
}

#[test]
fn countdown_warnings_history_bound_and_gm_preserving_downtime_shutdown() {
    let (ctx, _) = make_ctx(1_767_225_600);
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();

    // Seed 10 prior days in cluster_downtime_state to verify history pruning down to <= 9 days
    for day in 1..=10_u64 {
        let day_dt = day * 86_400 + dt_sec;
        let _ = step_downtime_countdown_warnings(&ctx, day_dt - 3_600);
    }
    let stored_days: usize = ctx.db.with_snapshot(|snap| {
        snap.cluster_and_runtime
            .cluster_downtime_state
            .emitted_warnings_by_day
            .len()
    });
    assert!(stored_days <= 9);

    // Exact boundary at secs_until_dt == 0 and secs_until_dt == 3601 -> no warnings
    let test_dt = 20 * 86_400 + dt_sec;
    assert!(step_downtime_countdown_warnings(&ctx, test_dt).is_empty());
    assert!(step_downtime_countdown_warnings(&ctx, test_dt - 3_601).is_empty());
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 3_600),
        vec![3_600]
    );
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 900),
        vec![900]
    );
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 300),
        vec![300]
    );

    // GM session vs non-GM session during initiate_cluster_downtime_shutdown
    let gm_uid = 777_u64;
    let player_uid = 888_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "gm_acct".into(),
            AccountRecord {
                user_id: gm_uid,
                username: "gm_acct".into(),
                password_token: "x".into(),
                role: eve_db::service_roles::ROLE_GMH,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.accounts.insert(
            "player_acct".into(),
            AccountRecord {
                user_id: player_uid,
                username: "player_acct".into(),
                password_token: "x".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(gm_uid, 9001, Some(90_000_001), true, 1_767_225_600)
        .unwrap();
    ctx.db
        .acquire_online_lock(player_uid, 9002, Some(90_000_002), true, 1_767_225_600)
        .unwrap();

    let disconnected = initiate_cluster_downtime_shutdown(&ctx);
    assert_eq!(disconnected, 1);
    let online_after = ctx.db.online_sessions_snapshot();
    assert!(online_after.iter().any(|&(u, s)| u == gm_uid && s == 9001));
    assert!(!online_after.iter().any(|&(u, _)| u == player_uid));

    let epoch = complete_cluster_downtime_restart(&ctx);
    assert!(epoch >= 1);
    assert!(!ctx.is_shutting_down());
}

#[test]
fn mutant_kill_downtime_persistence_maintenance_map_and_admin_cluster_rpcs() {
    use eve_machonet::{service_roles, SessionState};
    use std::collections::HashMap;

    let (ctx, clock) = make_ctx(1_767_225_600);
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let test_dt = 25 * 86_400 + dt_sec;

    // 1. load_downtime_state / save_downtime_state deduplication and active downtime rollover
    let w1 = step_downtime_countdown_warnings(&ctx, test_dt - 3_600);
    assert_eq!(w1, vec![3_600]);
    assert!(has_downtime_countdown_armed_for_day(&ctx, test_dt - 3_600));
    let w2 = step_downtime_countdown_warnings(&ctx, test_dt - 3_600);
    assert!(w2.is_empty(), "Persisted warning state must suppress duplicate countdown broadcast");

    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.cluster_downtime_state.cluster_restart_epoch = 41;
    });
    assert!(!is_downtime_active_in_db(&ctx));
    let dt_report = start_daily_downtime_window(&ctx);
    assert!(is_downtime_active_in_db(&ctx));
    assert_eq!(dt_report["is1100UtcWindow"], 0);
    let expected_keys = [
        "is1100UtcWindow",
        "replenishedAsteroids",
        "respawnedBeltAsteroids",
        "rolledUpMarketHistoryPairs",
        "recomputedIndustrySystems",
        "expiredMarketOrders",
        "expiredContracts",
        "sovSystemsDecayed",
        "missionDeclinesReset",
        "essBanksRolledOver",
        "autoPaidBills",
        "expiredRaffles",
        "dailyGoalsReset",
        "evacuatedAbyssalPilots",
        "npcWrecksCleaned",
        "missionNpcsRespawned",
        "wormholeSitesDespawned",
        "fwSystemsCaptured",
    ];
    for key in expected_keys {
        assert!(dt_report.contains_key(key), "missing maintenance key {key}");
    }
    assert_eq!(dt_report.len(), expected_keys.len() + 1); // + disconnectedSessions
    assert!(dt_report["recomputedIndustrySystems"] > 0);

    // Advance past 15-minute window and verify epoch persists from 41 -> 42 and downtime_until_unix clears
    let now_u = ctx.now_unix();
    assert_eq!(maybe_complete_scheduled_downtime_restart(&ctx, now_u + 899), None);
    assert_eq!(maybe_complete_scheduled_downtime_restart(&ctx, now_u + 900), Some(42));
    assert!(!is_downtime_active_in_db(&ctx));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.cluster_downtime_state.cluster_restart_epoch, 42);
        assert_eq!(snap.cluster_and_runtime.cluster_downtime_state.downtime_until_unix, None);
    });

    // Execute daily maintenance jobs directly outside & inside the 11:00 UTC window
    let outside = execute_daily_maintenance_jobs(&ctx);
    assert_eq!(outside.len(), expected_keys.len());
    assert_eq!(outside["is1100UtcWindow"], 0);
    assert!(outside["recomputedIndustrySystems"] > 0);
    clock.advance_secs(dt_sec + 120);
    assert_eq!(clock.now_unix_secs(), 1_767_225_600 + dt_sec + 120);
    let inside = execute_daily_maintenance_jobs(&ctx);
    assert_eq!(inside["is1100UtcWindow"], 1);

    // 2. handle_admin_cluster_rpc: unknown -> None, auth gate, AddClusterNode, RemoveClusterNode, AdminBroadcast, SetServiceState
    let kw = HashMap::new();
    let mut player_sess = SessionState::new(10);
    player_sess.role = 0;
    let mut gm_sess = SessionState::new(11);
    gm_sess.user_id = Some(1);
    gm_sess.role = service_roles::ROLE_GMH;
    gm_sess.character_id = Some(90_000_001);

    assert!(handle_admin_cluster_rpc(&ctx, &gm_sess, "DoesNotExist", &[], &kw).is_none());
    assert!(handle_admin_cluster_rpc(&ctx, &player_sess, "AddClusterNode", &[], &kw)
        .unwrap()
        .is_err());

    // Invalid explicit ID <= 0 fails
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &gm_sess, "AddClusterNode", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let auto_nid = handle_admin_cluster_rpc(&ctx, &gm_sess, "AddClusterNode", &[], &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(auto_nid, 3);
    assert_eq!(
        handle_admin_cluster_rpc(
            &ctx,
            &gm_sess,
            "AddClusterNode",
            &[PyRep::Integer(5), PyRep::String("10.0.0.5:26005".into())],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(5)
    );
    ctx.db.with_snapshot(|snap| {
        let tbl = snap.cluster_and_runtime.cluster_node_table.as_ref().unwrap();
        assert_eq!(tbl.get("5").map(String::as_str), Some("10.0.0.5:26005"));
    });
    assert!(ctx
        .cluster_state
        .read()
        .unwrap()
        .node_addresses
        .contains_key("10.0.0.5:26005"));

    // RemoveClusterNode: primary node <= 1 rejected; node 5 removed cleanly
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &gm_sess, "RemoveClusterNode", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &gm_sess, "RemoveClusterNode", &[PyRep::Integer(5)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .cluster_and_runtime
            .cluster_node_table
            .as_ref()
            .unwrap()
            .contains_key("5"));
    });
    assert!(!ctx
        .cluster_state
        .read()
        .unwrap()
        .node_addresses
        .contains_key("10.0.0.5:26005"));

    // AdminBroadcast: empty rejected; valid logs and emits broadcast
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &gm_sess, "AdminBroadcast", &[PyRep::String("   ".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_admin_cluster_rpc(
            &ctx,
            &gm_sess,
            "AdminBroadcast",
            &[PyRep::String("Maintenance in 5m".into())],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let last = snap.cluster_and_runtime.cluster_admin_broadcasts.last().unwrap();
        assert_eq!(last["message"], "Maintenance in 5m");
    });

    // SetServiceState: empty rejected; offline/Bool(true) persisted
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &gm_sess, "SetServiceState", &[PyRep::String("".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_admin_cluster_rpc(
            &ctx,
            &gm_sess,
            "SetServiceState",
            &[PyRep::String("market_node".into()), PyRep::String("offline".into())],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.cluster_service_states.get("market_node"), Some(&false));
    });
    assert_eq!(
        handle_admin_cluster_rpc(
            &ctx,
            &gm_sess,
            "SetServiceState",
            &[PyRep::String("market_node".into()), PyRep::Bool(true)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.cluster_service_states.get("market_node"), Some(&true));
    });
}

#[tokio::test]
async fn downtime_shutdown_logs_off_headless_sessions_and_preserves_shutdown_notice() {
    use crate::router::NotificationTarget;

    let (ctx, _) = make_ctx(1_767_225_600);
    let gm_uid = 700_011_u64;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let (player_uid, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "dt_gm".into(),
            AccountRecord {
                user_id: gm_uid,
                username: "dt_gm".into(),
                password_token: "x".into(),
                role: eve_db::service_roles::ROLE_GM_MASK,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        let uid = ch.user_id;
        let sid = ch.active_ship_id;
        snap.item_positions.insert(sid, [1_000_000.0, 0.0, 0.0]);
        (uid, sid)
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 606,
                position: DVec3::new(1_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;

    ctx.db
        .acquire_online_lock(gm_uid, 9101, None, true, 1_767_225_600)
        .unwrap();
    ctx.db
        .acquire_online_lock(player_uid, 9102, Some(char_id), true, 1_767_225_600)
        .unwrap();
    let _ = crate::services::fleet_social::ensure_xmpp_auth_token(&ctx, char_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRemoteMessage",
        "charid",
        vec![PyRep::String("stale".into())],
    );

    let disconnected = initiate_cluster_downtime_shutdown(&ctx);
    assert_eq!(disconnected, 1);

    // Non-GM headless session must have executed `logoff_character_sync`:
    // lock released, XMPP token revoked, ship moved into `disconnected_ships`,
    // stale notifications cleared, and `OnClusterShutdownInitiated` preserved.
    let online_after = ctx.db.online_sessions_snapshot();
    assert!(online_after.iter().any(|&(u, s)| u == gm_uid && s == 9101));
    assert!(!online_after.iter().any(|&(u, _)| u == player_uid));
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id));
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .kv(eve_db::Ns::XmppAuthToken)
            .get::<String>(char_id, "")
            .is_none());
        assert!(!snap
            .service_state_blobs
            .contains_key(&format!("xmpp:token:{char_id}")));
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs
        .iter()
        .any(|n| n.event == "OnClusterShutdownInitiated"));
    assert!(!notifs.iter().any(|n| n.event == "OnRemoteMessage"));
}
