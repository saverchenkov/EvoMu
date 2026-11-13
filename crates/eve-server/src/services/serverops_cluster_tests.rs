use super::*;
use crate::services::account_services::account_services_tests::test_ctx;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_serverops_cluster_tidi_node_balancing_and_downtime_jobs() {
    let ctx = test_ctx();
    let jita = 30_000_142_u32;
    let perimeter = 30_000_144_u32;

    // 1. compute_tidi_from_node_load boundaries
    assert_eq!(compute_tidi_from_node_load(0, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(100, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(200, 100), 0.5);
    assert_eq!(compute_tidi_from_node_load(10_000, 100), 0.10);

    // 2. Auto TiDi & threshold config
    assert!(!is_auto_cluster_tidi_enabled(&ctx));
    set_auto_cluster_tidi_enabled(&ctx, true);
    assert!(is_auto_cluster_tidi_enabled(&ctx));
    set_cluster_node_load_threshold(&ctx, 50);
    set_auto_cluster_tidi_enabled(&ctx, false);

    // 3. Manual system TiDi & clear override
    assert_eq!(get_system_tidi_factor(&ctx, jita), 1.0);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 0.25), 0.25);
    assert_eq!(get_system_tidi_factor(&ctx, jita), 0.25);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 0.01), 0.10); // clamped to 0.10
    assert_eq!(set_system_tidi_factor(&ctx, jita, f64::NAN), 1.0);
    let restored = clear_manual_system_tidi_override(&ctx, jita);
    assert_eq!(restored, 1.0);

    // 4. Worker node assignment, reinforcement & rebalancing
    let _ = ctx.systems.get_or_spawn(jita);
    let _ = ctx.systems.get_or_spawn(perimeter);
    let init_node = get_system_worker_node(&ctx, jita);
    assert!(init_node > 0);
    let _ = get_worker_node_load(&ctx, init_node);
    assert_eq!(assign_or_reinforce_system_node(&ctx, jita, None), init_node);

    let reinf_node = reinforce_solar_system_for_fleet_fight(&ctx, jita);
    assert!(reinf_node >= 100);
    assert_eq!(get_system_worker_node(&ctx, jita), reinf_node);
    let _ = rebalance_overloaded_worker_nodes(&ctx);

    // 5. Scheduled downtime & countdown warnings
    assert!(is_scheduled_downtime_enabled(&ctx));
    set_scheduled_downtime_enabled(&ctx, false);
    assert!(!is_scheduled_downtime_enabled(&ctx));
    set_scheduled_downtime_enabled(&ctx, true);

    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let base_day_unix = 1_767_225_600_u64; // 2026-01-01 00:00:00 UTC
    let dt_unix = base_day_unix + dt_sec;

    // 2 hours before downtime -> no warnings
    assert!(step_downtime_countdown_warnings(&ctx, dt_unix - 7200).is_empty());
    assert!(!has_downtime_countdown_armed_for_day(&ctx, dt_unix));

    // 30 minutes before downtime -> 3600s threshold fires
    let w1 = step_downtime_countdown_warnings(&ctx, dt_unix - 1800);
    assert_eq!(w1, vec![3_600]);
    assert!(has_downtime_countdown_armed_for_day(&ctx, dt_unix));
    // Calling again at same time does not re-fire 3600
    assert!(step_downtime_countdown_warnings(&ctx, dt_unix - 1800).is_empty());

    // 4 minutes before downtime -> 900s and 300s thresholds fire
    let w2 = step_downtime_countdown_warnings(&ctx, dt_unix - 240);
    assert_eq!(w2, vec![900, 300]);

    // 6. Seed market transaction & sov state and run daily downtime window + restart
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_transactions.insert(
            9001,
            eve_db::ledger::MarketTransactionRecord {
                transaction_id: 9001,
                date_unix: now,
                type_id: 34,
                quantity: 100,
                price: eve_db::IskCents::from_isk(5),
                buyer_id: 90_000_001,
                seller_id: 90_000_002,
                location_id: 60_003_760,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 1,
            },
        );
        snap.system_states.insert(
            30_004_759,
            eve_db::SystemDynamicState {
                solar_system_id: 30_004_759,
                sov_alliance_id: Some(99_000_001),
                military_index: 3,
                industrial_index: 2,
                strategic_index: 1,
                ess_linked_character_id: Some(90_000_001),
                ..Default::default()
            },
        );
    });

    assert!(maybe_complete_scheduled_downtime_restart(&ctx, now).is_none());
    let win_report = start_daily_downtime_window(&ctx);
    assert!(ctx.is_shutting_down());
    assert!(win_report.contains_key("rolledUpMarketHistoryPairs"));
    assert!(maybe_complete_scheduled_downtime_restart(&ctx, now + 60).is_none());
    let restart_epoch = maybe_complete_scheduled_downtime_restart(&ctx, now + 20 * 60).unwrap();
    assert!(restart_epoch >= 1);
    assert!(!ctx.is_shutting_down());

    let full_report = run_daily_downtime_and_jobs(&ctx);
    assert!(full_report.get("clusterRestartEpoch").copied().unwrap_or(0) > restart_epoch as i64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_admin_cluster_rpc_auth_node_crud_broadcast_and_service_state() {
    use crate::services::account_services::account_services_tests::auth_session;

    let ctx = test_ctx();
    let kw = HashMap::new();

    // Unknown method returns None
    let unauth = SessionState::default();
    assert!(handle_admin_cluster_rpc(&ctx, &unauth, "UnknownClusterOp", &[], &kw).is_none());

    // Unauthenticated & player without GM/Admin rejected
    assert!(handle_admin_cluster_rpc(&ctx, &unauth, "RebalanceNodes", &[], &kw)
        .unwrap()
        .is_err());
    let player = auth_session(1, 90_000_001, 1_000_009, 30_000_142);
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &player, "RebalanceNodes", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );

    // ROLE_ADMIN allowed
    let mut admin = player.clone();
    admin.role = eve_db::service_roles::ROLE_ADMIN;
    assert!(require_gm_or_admin(&admin).is_ok());

    // AddClusterNode: <=0 rejected, explicit + auto-ID kwargs accepted & persisted
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "AddClusterNode", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let n2 = handle_admin_cluster_rpc(
        &ctx,
        &admin,
        "AddClusterNode",
        &[PyRep::Integer(2), PyRep::String("10.0.0.2:26002".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(n2, PyRep::Integer(2));

    let mut add_kw = HashMap::new();
    add_kw.insert("address".to_string(), PyRep::String("10.0.0.3:26003".into()));
    let n3 = handle_admin_cluster_rpc(&ctx, &admin, "AddClusterNode", &[], &add_kw)
        .unwrap()
        .unwrap();
    assert_eq!(n3, PyRep::Integer(3));

    // RemoveClusterNode <=1 rejected, 2 removes both state & persisted blob
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "RemoveClusterNode", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "RemoveClusterNode", &[PyRep::Integer(2)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let table: std::collections::BTreeMap<String, String> =
            snap.cluster_and_runtime.cluster_node_table.clone().unwrap();
        assert!(!table.contains_key("2"));
        assert!(table.contains_key("3"));
    });

    // AdminBroadcast empty rejected & >100 entry log retention cap
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "AdminBroadcast", &[PyRep::String("  ".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let seed_log: Vec<serde_json::Value> = (0..105)
            .map(|i| serde_json::json!({"message": format!("old-{i}"), "timestamp_unix": 1}))
            .collect();
        snap.cluster_and_runtime.cluster_admin_broadcasts = serde_json::from_value(serde_json::Value::Array(seed_log)).unwrap();
    });
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "AdminBroadcast", &[PyRep::String("Downtime soon".into())], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let log: Vec<serde_json::Value> =
            serde_json::to_value(&snap.cluster_and_runtime.cluster_admin_broadcasts).unwrap().as_array().unwrap().clone();
        assert_eq!(log.len(), 100);
        assert_eq!(log.last().unwrap()["message"], "Downtime soon");
    });

    // SetServiceState validation + bool/int/string variants
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "SetServiceState", &[PyRep::String("".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let _ = handle_admin_cluster_rpc(
        &ctx,
        &admin,
        "SetServiceState",
        &[PyRep::String("svc_alpha".into()), PyRep::String("stopped".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let _ = handle_admin_cluster_rpc(
        &ctx,
        &admin,
        "SetServiceState",
        &[PyRep::String("svc_beta".into()), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let states: std::collections::BTreeMap<String, bool> =
            snap.cluster_and_runtime.cluster_service_states.clone();
        assert_eq!(states.get("svc_alpha"), Some(&false));
        assert_eq!(states.get("svc_beta"), Some(&true));
    });

    // ReinforceNode invalid, system reinforcement via kwargs, node-only fallback, RebalanceNodes
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "ReinforceNode", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let mut rkw = HashMap::new();
    rkw.insert("solarSystemID".to_string(), PyRep::Integer(30_000_142));
    rkw.insert("nodeID".to_string(), PyRep::Integer(205));
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "ReinforceNode", &[], &rkw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(205)
    );
    assert_eq!(
        handle_admin_cluster_rpc(&ctx, &admin, "ReinforceNode", &[PyRep::Integer(77)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(77)
    );
    assert!(matches!(
        handle_admin_cluster_rpc(&ctx, &admin, "RebalanceNodes", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_downtime_active_in_db_shutdown_disconnects_and_tidi_recompute_guards() {
    use eve_db::{OnlineSessionRecord, ServerClock, VirtualServerClock};
    use std::sync::Arc;

    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = test_ctx();
    ctx.set_clock(clock.clone());

    assert!(!is_downtime_active_in_db(&ctx));
    let (tx_player, mut rx_player) = tokio::sync::mpsc::unbounded_channel();
    let (tx_gm, mut rx_gm) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut closers = ctx.session_closers.write().unwrap();
        closers.insert(1, tx_player);
        closers.insert(2, tx_gm);
    }
    {
        let mut bounds = ctx.bound_objects.write().unwrap();
        bounds.insert(
            9001,
            crate::router::BoundObjectEntry {
                bound_id: 9001,
                session_id: 1,
                user_id: 1,
                character_id: 90_000_001,
                service: "ship".into(),
                object_id: 100_000_001,
                oid_str: "ship".into(),
                ref_id: 1,
            },
        );
        bounds.insert(
            9002,
            crate::router::BoundObjectEntry {
                bound_id: 9002,
                session_id: 2,
                user_id: 2,
                character_id: 90_000_002,
                service: "ship".into(),
                object_id: 100_000_002,
                oid_str: "ship".into(),
                ref_id: 2,
            },
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a2) = snap.accounts.get_mut("other") {
            a2.role = eve_db::service_roles::ROLE_GMH;
        }
        snap.sessions.insert(
            1,
            OnlineSessionRecord {
                user_id: 1,
                username: "demo".into(),
                session_id: 1,
                character_id: Some(90_000_001),
                connected_at_unix: 1_699_999_000,
                computer_hash: None,
            },
        );
        snap.sessions.insert(
            2,
            OnlineSessionRecord {
                user_id: 2,
                username: "other".into(),
                session_id: 2,
                character_id: Some(90_000_002),
                connected_at_unix: 1_699_999_000,
                computer_hash: None,
            },
        );
    });

    let report = start_daily_downtime_window(&ctx);
    assert_eq!(report.get("disconnectedSessions").copied(), Some(1));
    assert!(is_downtime_active_in_db(&ctx));
    assert!(rx_player.try_recv().is_ok());
    assert!(rx_gm.try_recv().is_err());
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&9001));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&9002));

    clock.advance_secs(16 * 60);
    assert!(!is_downtime_active_in_db(&ctx));
    let _ = complete_cluster_downtime_restart(&ctx);
    assert!(!is_downtime_active_in_db(&ctx));

    // Recompute TiDi returns empty when auto disabled or no active systems, and preserves manual override
    assert!(recompute_cluster_node_tidi(&ctx).is_empty());
    set_auto_cluster_tidi_enabled(&ctx, true);
    let _ = ctx.systems.get_or_spawn(30_000_142);
    let _ = set_system_tidi_factor(&ctx, 30_000_142, 0.35);
    let _ = recompute_cluster_node_tidi(&ctx);
    assert_eq!(get_system_tidi_factor(&ctx, 30_000_142), 0.35);
}
