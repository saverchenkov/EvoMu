//! Acceptance tests for `#1087`: slow-work thresholds, logarithmic warn limiter,
//! synchronous `lifecycle.jsonl` with forced-panic subprocess verification, startup
//! presence summary, transient item isolation, cross-table ID allocator guard, and
//! `#1077` ops telemetry snapshot providers.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, OnlineSessionRecord, TxError, UpwellStructureRecord,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::lifecycle::{
    global_lifecycle_gauges, install_panic_observer, set_last_tick_lateness_ms, set_open_sessions,
    set_pending_wal_records, LifecycleLogger, LifecycleRecord,
};
use eve_server::ops_telemetry::{
    classify_bootstrap_phase, collect_ops_telemetry_snapshot, log_startup_presence_summary,
    recent_bootstrap_timings, record_bootstrap_timing, record_tick_summary, BootstrapPacketTiming,
    TickSummary, MAX_TICK_SUMMARIES,
};
use eve_server::router::client_wire::dispatch_call_on_wire;
use eve_server::router::ServerContext;
use eve_server::sso::handle_http_request_with_peer;
use eve_server::warn_limiter::{
    check_global_warn, global_warn_limiter, global_warn_suppressed_summary, WarnCategory, WarnKey,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn unique_temp_dir(label: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "evomu_it_1087_{label}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn forced_panic_subprocess_writes_lifecycle_jsonl_with_reason_panic() {
    // parity: feat:serverops.server-tick
    if std::env::var("EVOMU_FORCE_PANIC_SUBPROCESS_1087").as_deref() == Ok("1") {
        set_open_sessions(12);
        set_pending_wal_records(3);
        set_last_tick_lateness_ms(27);
        let logger = Arc::new(LifecycleLogger::from_env());
        install_panic_observer(logger);
        panic!("forced_1087_panic");
    }

    let dir = unique_temp_dir("panic_subproc");
    let exe = std::env::current_exe().expect("current_exe");
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("ops_observability_1087::forced_panic_subprocess_writes_lifecycle_jsonl_with_reason_panic")
        .arg("--nocapture")
        .env("EVOMU_FORCE_PANIC_SUBPROCESS_1087", "1")
        .env("EVOMU_LIFECYCLE_LOG_DIR", &dir)
        .output()
        .expect("spawn forced panic subprocess");

    assert!(
        !output.status.success(),
        "forced panic subprocess must exit non-zero"
    );

    let log_path = dir.join("lifecycle.jsonl");
    assert!(log_path.exists(), "lifecycle.jsonl must be written on panic");
    let raw = std::fs::read_to_string(&log_path).expect("read lifecycle.jsonl");
    let records: Vec<LifecycleRecord> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<LifecycleRecord>(l).expect("valid jsonl line"))
        .collect();
    assert!(!records.is_empty(), "expected at least one lifecycle record");

    let panic_rec = records
        .iter()
        .find(|r| r.reason == "panic")
        .expect("expected record with reason == panic");
    assert!(panic_rec.pid > 0);
    assert!(panic_rec.ts_unix > 0);
    assert!(!panic_rec.ts_utc.is_empty());
    assert_eq!(panic_rec.open_sessions, 12);
    assert_eq!(panic_rec.pending_wal_records, 3);
    assert_eq!(panic_rec.last_tick_lateness_ms, 27);
    assert!(
        panic_rec
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("forced_1087_panic")),
        "panic detail must capture panic message: {:?}",
        panic_rec.detail
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_telemetry_warn_limiter_bootstrap_and_startup_presence_acceptance() {
    // parity: feat:serverops.server-tick
    let ctx = ServerContext::new();

    // Startup presence summary reports all required categories.
    let presence = log_startup_presence_summary(&ctx, 18);
    let lines = presence.format_lines();
    assert_eq!(lines.len(), 5);

    // Tick summary ring caps at 120 entries and updates lifecycle lateness gauge.
    for seq in 1_u64..=(MAX_TICK_SUMMARIES as u64 + 10) {
        record_tick_summary(TickSummary {
            tick_seq: seq,
            timestamp_unix: 1_700_000_000 + seq,
            duration_ms: 2,
            lateness_ms: seq as i64,
            active_systems: 1,
        });
    }
    let (_, _, gauge_lateness) = global_lifecycle_gauges().snapshot();
    assert_eq!(gauge_lateness, (MAX_TICK_SUMMARIES as i64) + 10);

    // Logarithmic warn limiter covers all 4 categories and exposes suppressed counts.
    let unique_rpc = "unknown.test_rpc_1087";
    for _ in 0..10 {
        let _ = check_global_warn(WarnCategory::UnknownRpc, unique_rpc);
    }
    let _ = check_global_warn(WarnCategory::UnknownQuasarNoticeType, "test.Notice1087");
    let _ = check_global_warn(WarnCategory::NotInCatalog, "test.Unlisted1087");
    let _ = check_global_warn(WarnCategory::MarshalOddity, "test.Malformed1087");
    assert_eq!(
        global_warn_limiter().suppressed_count(&WarnKey::new(WarnCategory::UnknownRpc, unique_rpc)),
        5 // 1,2,3,4,8 logged; 5,6,7,9,10 suppressed
    );

    // Bootstrap classification & timing ledger.
    assert_eq!(classify_bootstrap_phase("charUnboundMgr", "SelectCharacterID"), Some("login"));
    assert_eq!(classify_bootstrap_phase("ship", "Undock"), Some("undock"));
    record_bootstrap_timing(BootstrapPacketTiming {
        phase: "undock".into(),
        service: "ship".into(),
        method: "Undock".into(),
        session_id: 9001,
        character_id: Some(90_000_001),
        elapsed_since_request_ms: 14,
        timestamp_unix: 1_700_000_100,
    });

    let ops = collect_ops_telemetry_snapshot(&ctx);
    assert_eq!(ops.last_ticks.len(), MAX_TICK_SUMMARIES);
    assert!(ops.warn_suppressed.total_suppressed >= 5);
    assert_eq!(global_warn_suppressed_summary().total_suppressed, ops.warn_suppressed.total_suppressed);
    assert!(ops
        .recent_bootstrap_timings
        .iter()
        .any(|b| b.phase == "undock" && b.elapsed_since_request_ms == 14));
}

#[test]
fn transient_items_excluded_from_sqlite_and_cross_table_id_guard_survives_restart() {
    // parity: feat:serverops.persistence
    let dir = unique_temp_dir("db_persistence");
    let db_path = dir.join("world.sqlite");

    let transient_id = 200_910_001_i64;
    let seeded_struct_id = 200_910_050_i64;
    {
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
        repo.tx("transient_only_tx_1087", |wtx| -> Result<(), TxError> {
            wtx.snap.insert_transient_item(InventoryItemRecord {
                item_id: transient_id,
                type_id: 587,
                owner_id: 1_000_180,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            });
            wtx.snap
                .transient_item_attributes
                .insert(transient_id, HashMap::from([(9_u16, 100.0)]));
            Ok(())
        })
        .expect("transient_only_tx");
        assert_eq!(
            repo.with_snapshot(|snap| snap.wal_seq),
            0,
            "transient-only tx must not bump wal_seq or write WAL"
        );
        repo.with_mut_snapshot(|snap| {
            snap.structures.insert(
                seeded_struct_id,
                UpwellStructureRecord {
                    structure_id: seeded_struct_id,
                    type_id: 35832,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    name: "Guard Citadel".into(),
                    ..Default::default()
                },
            );
        });
        repo.with_sqlite_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', 200000000), ('next_entity_block', 200000000)",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .expect("reset counter");
    }

    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen");
    reopened.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&transient_id));
        assert!(!snap.is_transient_item(transient_id));
        assert!(snap.next_entity_id >= seeded_struct_id);
    });

    let next_id = reopened
        .tx("alloc_after_structure_seed", |wtx| wtx.alloc_entity_id())
        .expect("alloc");
    assert!(next_id > seeded_struct_id);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn wire_login_and_undock_record_bootstrap_timings_and_ops_telemetry() {
    // parity: feat:serverops.server-tick
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(91_087);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let sel = dispatch_call_on_wire(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
    )
    .await;
    assert!(
        sel.rpc_result.is_ok(),
        "SelectCharacterID failed: {:?}",
        sel.rpc_result
    );

    let undock =
        dispatch_call_on_wire(&ctx, &mut sess, "ship", None, "Undock", &[], &kw).await;
    assert!(
        undock.rpc_result.is_ok(),
        "ship.Undock failed: {:?}",
        undock.rpc_result
    );

    let timings = recent_bootstrap_timings();
    assert!(
        timings.iter().any(|b| {
            b.phase == "login"
                && b.service == "charUnboundMgr"
                && b.method == "SelectCharacterID"
                && b.session_id == 91_087
                && b.character_id == Some(90_000_001)
        }),
        "expected login bootstrap timing from execute_call_req_wire, got {timings:?}"
    );
    assert!(
        timings.iter().any(|b| {
            b.phase == "undock"
                && b.service == "ship"
                && b.method == "Undock"
                && b.session_id == 91_087
                && b.character_id == Some(90_000_001)
        }),
        "expected undock bootstrap timing from execute_call_req_wire, got {timings:?}"
    );

    let ops = collect_ops_telemetry_snapshot(&ctx);
    assert!(ops
        .recent_bootstrap_timings
        .iter()
        .any(|b| b.session_id == 91_087 && b.method == "Undock"));
}

#[tokio::test]
async fn system_unload_reclaims_transient_items_across_direct_and_persistent_parents() {
    // parity: feat:serverops.persistence
    let ctx = ServerContext::new();
    let idle_sys = 30_002_187_u32;
    let active_sys = 30_000_142_u32;
    let idle_handle = ctx.systems.get_or_spawn(idle_sys);
    assert!(
        idle_handle.is_hibernated(),
        "empty system must start hibernated for unload test"
    );
    drop(idle_handle);

    let direct_hull_id = 200_920_001_i64;
    let nested_mod_id = 200_920_002_i64;
    let persistent_hull_id = 200_920_003_i64;
    let mounted_transient_id = 200_920_004_i64;
    let active_sys_transient_id = 200_920_005_i64;

    let mk_item = |item_id: i64, location_id: i64| InventoryItemRecord {
        item_id,
        type_id: 587,
        owner_id: 1_000_180,
        location_id,
        flag: InventoryFlag::None,
        quantity: 1,
        singleton: true,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.insert_transient_item(mk_item(direct_hull_id, i64::from(idle_sys)));
        snap.insert_transient_item(mk_item(nested_mod_id, direct_hull_id));
        snap.items
            .insert(persistent_hull_id, mk_item(persistent_hull_id, i64::from(idle_sys)));
        snap.insert_transient_item(mk_item(mounted_transient_id, persistent_hull_id));
        snap.insert_transient_item(mk_item(active_sys_transient_id, i64::from(active_sys)));
        snap.transient_item_attributes
            .insert(mounted_transient_id, HashMap::from([(9_u16, 42.0)]));
    });

    ctx.clock().advance_secs(600);
    let evicted = ctx.systems.evict_idle_systems_with_min_ticks(300, 0).await;
    assert!(
        evicted.contains(&idle_sys),
        "evict_idle_systems_with_min_ticks must evict idle system {idle_sys}"
    );

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.is_transient_item(direct_hull_id));
        assert!(!snap.is_transient_item(nested_mod_id));
        assert!(!snap.is_transient_item(mounted_transient_id));
        assert!(!snap
            .transient_item_attributes
            .contains_key(&mounted_transient_id));
        assert!(snap.items.contains_key(&persistent_hull_id));
        assert!(snap.is_transient_item(active_sys_transient_id));
    });
}

#[tokio::test]
async fn ops_http_endpoint_surfaces_ticks_sessions_tidi_warn_suppressed_and_p99() {
    // parity: feat:serverops.server-tick
    let ctx = ServerContext::new();
    ctx.set_gm_ops_enabled(true);

    let expected_sys_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).map(|c| c.solar_system_id))
        .expect("demo character 90000001 must exist");
    let _sys = ctx.systems.get_or_spawn(expected_sys_id);
    {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .expect("cluster_nodes write lock");
        cn.manual_tidi_overrides.insert(expected_sys_id, 0.5);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.sessions.insert(
            91_088,
            OnlineSessionRecord {
                user_id: 1001,
                username: "ops_probe_1087".into(),
                session_id: 91_088,
                character_id: Some(90_000_001),
                connected_at_unix: 1_700_000_200,
                computer_hash: None,
            },
        );
    });

    record_tick_summary(TickSummary {
        tick_seq: 91_087_100,
        timestamp_unix: 1_700_000_201,
        duration_ms: 3,
        lateness_ms: 1,
        active_systems: 1,
    });
    for _ in 0..6 {
        let _ = check_global_warn(WarnCategory::UnknownRpc, "unknown.ops_http_1087");
    }
    ctx.flight_recorder
        .record_rpc_latency("beyonce", "MachoBindObject", Duration::from_millis(7));

    let loopback: IpAddr = "127.0.0.1".parse().expect("loopback v4");
    let mut headers = HashMap::new();
    headers.insert("host".into(), "127.0.0.1:26004".into());
    let resp = handle_http_request_with_peer(&ctx, "GET", "/ops", &headers, b"", Some(loopback));
    assert_eq!(resp.status_code, 200);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.body).expect("valid /ops JSON response");

    assert_eq!(body["gm_mode"], true);
    let last_ticks = body["last_ticks"]
        .as_array()
        .expect("last_ticks must be an array");
    assert!(!last_ticks.is_empty());
    assert!(last_ticks.len() <= MAX_TICK_SUMMARIES);

    assert!(body["per_system_sessions"]
        .as_array()
        .expect("per_system_sessions must be an array")
        .iter()
        .any(|s| s["solar_system_id"] == expected_sys_id && s["session_count"] == 1));
    assert!(body["tidi"]["systems"]
        .as_array()
        .expect("tidi.systems must be an array")
        .iter()
        .any(|s| s["solar_system_id"] == expected_sys_id && s["manual_override"] == 0.5));
    assert!(body["tidi"]["manual_overrides"]
        .as_array()
        .expect("tidi.manual_overrides must be an array")
        .iter()
        .any(|o| o["solar_system_id"] == expected_sys_id && o["factor"] == 0.5));
    assert!(
        body["warn_suppressed"]["total_suppressed"]
            .as_u64()
            .expect("warn_suppressed.total_suppressed u64")
            >= 2
    );
    assert!(body["recent_bootstrap_timings"].is_array());
    assert!(body["latency_histograms"]["overall_p99_ms"].is_number());
    assert_eq!(body["latency_histograms"]["total_calls"], 1);
}
