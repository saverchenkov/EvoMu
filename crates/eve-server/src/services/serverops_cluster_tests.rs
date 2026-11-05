use super::*;
use crate::services::account_services::account_services_tests::test_ctx;

#[tokio::test]
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
        snap.market_transactions.push(eve_db::ledger::MarketTransactionRecord {
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
        });
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
