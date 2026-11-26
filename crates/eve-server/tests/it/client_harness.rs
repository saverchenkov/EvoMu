//! Task T-6: Integration test for `tools/client-harness/run_journeys` verifying two-client
//! concurrent bridge orchestration, `result.json` schema for all 29 journeys in
//! `client_journeys.json`, honest `passed: false` reporting when `EVE.app` is not driven,
//! and log-hygiene rejection of `UnimplementedRPC` / `panicked at` / `Traceback` / `EXCEPTION`.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::{BridgeEnvelope, PyRep, MAX_FRAME_SIZE};
use eve_server::router::{NotificationTarget, ServerContext};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static NEXT_HARNESS_SESS_ID: AtomicU64 = AtomicU64::new(50_000);

async fn serve_harness_bridge_client(
    stream: TcpStream,
    ctx: Arc<ServerContext>,
    server_log: Arc<Mutex<std::fs::File>>,
) {
    let sess_id = NEXT_HARNESS_SESS_ID.fetch_add(1, Ordering::Relaxed);
    let mut session = SessionState::new(sess_id);
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut sys_rx: Option<tokio::sync::broadcast::Receiver<eve_server::system_actor::TickBroadcast>> = None;

    let (mut reader, mut writer) = stream.into_split();
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let reader_task = tokio::spawn(async move {
        loop {
            let mut len_buf = [0u8; 4];
            if reader.read_exact(&mut len_buf).await.is_err() {
                break;
            }
            let frame_len = u32::from_le_bytes(len_buf) as usize;
            if frame_len == 0 || frame_len > MAX_FRAME_SIZE {
                break;
            }
            let mut payload = vec![0u8; frame_len];
            if reader.read_exact(&mut payload).await.is_err() {
                break;
            }
            if frame_tx.send(payload).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            biased;
            frame_opt = frame_rx.recv() => {
                let Some(payload) = frame_opt else {
                    break;
                };
                let Ok(BridgeEnvelope::RpcRequest {
                    req_id,
                    service,
                    bound_id,
                    method,
                    args,
                    kwargs,
                    caller_trace,
                }) = serde_json::from_slice::<BridgeEnvelope>(&payload)
                else {
                    break;
                };

                let prev_session = session.clone();
                let result = ctx
                    .dispatch_rpc(
                        &mut session,
                        &service,
                        bound_id,
                        &method,
                        &args,
                        &kwargs,
                        caller_trace.as_deref(),
                    )
                    .await;

                {
                    if let Ok(mut f) = server_log.lock() {
                        match &result {
                            Ok(_) => {
                                let _ = writeln!(
                                    f,
                                    "[server] sess={sess_id} req={req_id} {service}.{method} -> Ok"
                                );
                            }
                            Err(e) => {
                                let _ = writeln!(
                                    f,
                                    "[server] sess={sess_id} req={req_id} {service}.{method} -> Err({}: {})",
                                    e.code, e.message
                                );
                            }
                        }
                        let _ = f.flush();
                    }
                }

                if session.solar_system_id != prev_session.solar_system_id {
                    if let Some(new_sys) = session.solar_system_id {
                        sys_rx = Some(ctx.systems.get_or_spawn(new_sys).subscribe());
                    }
                }

                let resp = BridgeEnvelope::RpcResponse { req_id, result };
                let Ok(out_bytes) = serde_json::to_vec(&resp) else {
                    break;
                };
                if writer
                    .write_all(&(out_bytes.len() as u32).to_le_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                if writer.write_all(&out_bytes).await.is_err() {
                    break;
                }

                let diff = prev_session.compute_diff(&session);
                if !diff.is_empty() {
                    let changes_dict = PyRep::Dict(
                        diff.into_iter()
                            .map(|(k, (old_v, new_v))| {
                                (PyRep::String(k), PyRep::Tuple(vec![old_v, new_v]))
                            })
                            .collect(),
                    );
                    let ev = BridgeEnvelope::ServerEvent {
                        tick: 0,
                        event: "OnSessionChanged".to_string(),
                        args: vec![PyRep::Bool(false), changes_dict],
                    };
                    if let Ok(ev_bytes) = serde_json::to_vec(&ev) {
                        let _ = writer
                            .write_all(&(ev_bytes.len() as u32).to_le_bytes())
                            .await;
                        let _ = writer.write_all(&ev_bytes).await;
                    }
                }

                let _ = session.drain_notifications();

                if session.character_id.is_some() && session.station_id.is_none() {
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        let handle = ctx.systems.get_or_spawn(sys_id);
                        if let Ok((tick, events, pyrep)) = handle.drain_observer_updates(ship_id).await {
                            if !events.is_empty() {
                                let ev = BridgeEnvelope::ServerEvent {
                                    tick,
                                    event: "DoDestinyUpdate".to_string(),
                                    args: vec![pyrep],
                                };
                                if let Ok(bytes) = serde_json::to_vec(&ev) {
                                    let _ = writer
                                        .write_all(&(bytes.len() as u32).to_le_bytes())
                                        .await;
                                    let _ = writer.write_all(&bytes).await;
                                }
                            }
                        }
                    }
                }
            }
            notify_res = notify_rx.recv() => {
                if let Ok((target_cid, notif)) = notify_res {
                    if session.character_id == Some(target_cid) {
                        let server_event = BridgeEnvelope::ServerEvent {
                            tick: 0,
                            event: notif.event,
                            args: notif.args,
                        };
                        if let Ok(out_bytes) = serde_json::to_vec(&server_event) {
                            let _ = writer
                                .write_all(&(out_bytes.len() as u32).to_le_bytes())
                                .await;
                            let _ = writer.write_all(&out_bytes).await;
                        }
                    }
                }
            }
            tick_res = async {
                match &mut sys_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Ok(tick_msg) = tick_res {
                    if session.character_id.is_some()
                        && session.station_id.is_none()
                        && session.solar_system_id == Some(tick_msg.solar_system_id)
                    {
                        if let Some(ship_id) = session.ship_id {
                            if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&ship_id) {
                                let server_event = BridgeEnvelope::ServerEvent {
                                    tick: tick_msg.tick,
                                    event: "DoDestinyUpdate".to_string(),
                                    args: vec![updates_pyrep.clone()],
                                };
                                if let Ok(out_bytes) = serde_json::to_vec(&server_event) {
                                    let _ = writer
                                        .write_all(&(out_bytes.len() as u32).to_le_bytes())
                                        .await;
                                    let _ = writer.write_all(&out_bytes).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    reader_task.abort();
    if let Some(uid) = session.user_id {
        ctx.db.release_online_lock(uid, session.session_id);
    }
    ctx.release_session_bindings(session.session_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_t6_client_harness_run_journeys_two_clients_and_log_hygiene() {
    // Task T-6: Real-client & dual-client bridge journey harness (`tools/client-harness/run_journeys`).
    let ctx = ServerContext::new();
    {
        let mut flags = ctx
            .feature_flags
            .write()
            .expect("feature_flags lock poisoned");
        flags.free_fast_checkout_plex = true;
        flags.starter_plex_balance = 50_000;
    }
    let ctx = Arc::new(ctx);
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    ctx.db
        .provision_gm_account("admin", "evomu-scenario-gm-secret", 90_000_003, "ISD Admin");
    let other_uid = ctx.db.authenticate("other", "other-secret").unwrap().user_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(90_000_002, 50_000);
        snap.account_plex_vault.insert(other_uid, 50_000);
    });
    ctx.db
        .add_or_stack_item(
            90_000_001,
            100_000_001,
            eve_db::InventoryFlag::Cargo,
            53977,
            5,
        )
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral bridge port");
    let bridge_port = listener.local_addr().unwrap().port();

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let harness_bin = repo_root.join("tools").join("client-harness").join("run_journeys");
    let journeys_json = repo_root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("client_journeys.json");

    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_t6_harness_{}_{}",
        std::process::id(),
        bridge_port
    ));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("create temp harness dir");
    let server_log_path = tmp_dir.join("server.log");
    let server_log_file = Arc::new(Mutex::new(
        std::fs::File::create(&server_log_path).expect("create server.log"),
    ));

    let ctx_clone = Arc::clone(&ctx);
    let log_clone = Arc::clone(&server_log_file);
    let accept_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_clone);
            let l = Arc::clone(&log_clone);
            tokio::spawn(async move {
                serve_harness_bridge_client(stream, c, l).await;
            });
        }
    });

    // 1. Run `run_journeys --bridge-harness-test` with two concurrent bridge clients
    let out1 = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--out")
        .arg(&tmp_dir)
        .arg("--journeys")
        .arg(&journeys_json)
        .arg("--bridge-port")
        .arg(bridge_port.to_string())
        .arg("--server-log")
        .arg(&server_log_path)
        .arg("--bridge-harness-test")
        .output()
        .expect("execute tools/client-harness/run_journeys --bridge-harness-test");
    assert!(
        out1.status.success(),
        "run_journeys --bridge-harness-test failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr)
    );

    let result_raw =
        std::fs::read_to_string(tmp_dir.join("result.json")).expect("read result.json");
    let result_val: serde_json::Value =
        serde_json::from_str(&result_raw).expect("parse result.json");

    assert_eq!(
        result_val
            .get("concurrent_clients_tested")
            .and_then(|v| v.as_u64()),
        Some(2)
    );
    assert_eq!(
        result_val
            .get("log_errors")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(0)
    );
    let client_logs = result_val
        .get("client_logs")
        .and_then(|v| v.as_array())
        .expect("client_logs array");
    assert_eq!(client_logs.len(), 2);
    for cl in client_logs {
        let p = PathBuf::from(cl.as_str().unwrap());
        assert!(p.exists(), "client log {} must exist on disk", p.display());
    }

    let journeys_map = result_val
        .get("journeys")
        .and_then(|v| v.as_object())
        .expect("journeys map");
    assert_eq!(
        journeys_map.len(),
        29,
        "result.json must contain all 29 journeys from client_journeys.json"
    );

    for (jid, jentry) in journeys_map {
        assert_eq!(
            jentry.get("passed").and_then(|v| v.as_bool()),
            Some(false),
            "Journey {jid} must honestly report passed=false when real EVE.app was not driven"
        );
    }
    for verified_id in [
        "launch_login",
        "create_character",
        "station_ui",
        "mail_notifications",
        "fleet",
        "planetary",
        "undock_navigate",
        "multiplayer_presence",
        "local_chat",
        "wormholes_filaments",
        "market_trade",
        "pve_combat",
        "daily_goals_events",
        "cosmetics_store",
    ] {
        assert_eq!(
            journeys_map
                .get(verified_id)
                .and_then(|v| v.get("bridge_verified"))
                .and_then(|v| v.as_bool()),
            Some(true),
            "Journey {verified_id} must report bridge_verified=true"
        );
    }

    // 2. Without --bridge-harness-test (i.e. final_gate.py mode), run_journeys must exit 1 because real EVE.app journeys are not yet passed
    ctx.advance_secs(3600);
    let out_gate_mode = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--out")
        .arg(&tmp_dir)
        .arg("--journeys")
        .arg(&journeys_json)
        .arg("--bridge-port")
        .arg(bridge_port.to_string())
        .arg("--server-log")
        .arg(&server_log_path)
        .output()
        .expect("execute run_journeys in final_gate mode");
    assert_eq!(
        out_gate_mode.status.code(),
        Some(1),
        "run_journeys in final_gate mode must exit 1 until all 29 real EVE.app journeys pass"
    );

    // 3. Injecting an UnimplementedRPC line into server.log must cause run_journeys to fail with exit code 2
    ctx.advance_secs(3600);
    {
        let mut f = server_log_file.lock().unwrap();
        writeln!(f, "[server] ERROR UnimplementedRPC on synthetic.Method").unwrap();
        f.flush().unwrap();
    }
    let out_dirty_log = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--out")
        .arg(&tmp_dir)
        .arg("--journeys")
        .arg(&journeys_json)
        .arg("--bridge-port")
        .arg(bridge_port.to_string())
        .arg("--server-log")
        .arg(&server_log_path)
        .arg("--bridge-harness-test")
        .output()
        .expect("execute run_journeys with UnimplementedRPC in server.log");
    assert_eq!(
        out_dirty_log.status.code(),
        Some(2),
        "run_journeys must exit 2 when UnimplementedRPC is present in server.log"
    );

    accept_task.abort();
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_z1_to_z8_real_client_session_diff_and_journey_contract() {
    let mut prev = SessionState::new(70_001);
    prev.user_id = Some(1);
    let mut next = prev.clone();
    next.character_id = Some(90_000_001);
    next.corporation_id = Some(1_000_035);
    next.solar_system_id = Some(30_000_142);
    next.constellation_id = Some(20_000_020);
    next.region_id = Some(10_000_002);
    next.station_id = Some(60_003_760);
    next.ship_id = Some(100_000_001);
    let diff = next.compute_diff(&prev);
    let keys: Vec<String> = diff.iter().map(|(k, _)| k.to_string()).collect();
    assert!(keys.contains(&"charid".to_string()));
    assert!(keys.contains(&"corpid".to_string()));
    assert!(keys.contains(&"solarsystemid2".to_string()));
    assert!(keys.contains(&"stationid".to_string()));
    assert!(keys.contains(&"shipid".to_string()));
    assert!(
        !keys.contains(&"stationid2".to_string()),
        "V24.01 Session.__persistvars__ must never emit obsolete stationid2: {keys:?}"
    );

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let journeys_path = repo_root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("client_journeys.json");
    let raw = std::fs::read_to_string(&journeys_path).expect("read client_journeys.json");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse client_journeys.json");
    let journeys = parsed["journeys"].as_array().expect("journeys array");
    assert_eq!(journeys.len(), 29, "client_journeys.json must list 29 real client journeys");

    let driver_path = repo_root
        .join("tools")
        .join("client-harness")
        .join("evomu_inclient_driver.py");
    let site_path = repo_root
        .join("tools")
        .join("client-harness")
        .join("evomu_site.py");
    let driver_src = std::fs::read_to_string(&driver_path).expect("read evomu_inclient_driver.py");
    let site_src = std::fs::read_to_string(&site_path).expect("read evomu_site.py");
    assert!(
        site_src.contains("schedule_startup")
            && site_src.contains("_install_logmodule_hook")
            && site_src.contains("_install_xmpp_port_guard")
            && site_src.contains("_install_camera_dock_guard")
    );
    for j in journeys {
        let jid = j["id"].as_str().expect("journey id string");
        assert!(
            driver_src.contains(&format!("\"{jid}\"")),
            "evomu_inclient_driver.py missing live journey handler for '{jid}'"
        );
    }

    let ctx = Arc::new(ServerContext::new());
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(70_002);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_sel = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect("GetCharactersToSelect");
    let first_name = char_sel
        .crowset_rows()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.dict_get("characterName").cloned());
    assert_eq!(
        first_name,
        Some(PyRep::String("Aura Capsuleer".into())),
        "charUnboundMgr must return seeded Aura Capsuleer row"
    );
}

#[tokio::test]
async fn test_c_drain_pending_frames_emits_session_change_detaches_space_and_updates_shadow() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let sess = SessionState::new(70_099);
    let mut harness = crate::common::wrap_wire_harness(&ctx, sess);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &HashMap::new(),
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &HashMap::new(),
        )
        .await;
    harness
        .call_ok("stationSvc", None, "Undock", &[], &HashMap::new())
        .await;
    harness
        .call_ok(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_000_142), PyRep::None],
            &HashMap::new(),
        )
        .await;
    assert_eq!(harness.shadow().and_then(|s| s.session.station_id), None);
    let _ = harness.drain_frames();

    // Simulate out-of-band docking/respawn state change + char notification while client is idle.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
    });
    let _ = ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnSessionChanged",
        "charid",
        vec![PyRep::None],
    );
    ctx.advance_sim_ms(1_000).await;

    let drained = harness.drain_pending_frames();
    assert!(
        drained.iter().any(|f| matches!(
            f.kind,
            eve_testkit::client_harness::ClientWireFrameKind::SessionChange { .. }
        )),
        "drain_pending_frames must emit SessionChangeNotification on out-of-band station respawn: {drained:?}"
    );
    assert_eq!(
        harness.shadow().and_then(|s| s.session.station_id),
        Some(60_003_760),
        "drain_pending_frames must update ShadowClient via SessionChangeNotification"
    );
    assert!(
        !harness.drain_frames().is_empty(),
        "drain_pending_frames must append frames to buffered_frames"
    );
}

#[tokio::test]
async fn test_client_harness_enforces_read_only_no_commit_contract_live_wire() {
    use eve_proto::BridgeRpcError;
    use eve_server::router::rpc_registry::{
        rpc_entry_with_caller, RpcCallerClass, RpcDispatchHandler, RpcEntry, RpcRegistry,
    };
    use eve_testkit::client_harness::{
        ContractValidationMode, RecordedRpcCall, RpcCallOutcome,
    };

    fn test_read_commit_handler(
        ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        method: &str,
        args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        if method == "ReadCommitsClean" {
            return Some(Ok(PyRep::Dict(Vec::new())));
        }
        let should_commit = args.first().and_then(|v| v.as_bool()).unwrap_or(true);
        let should_err = args.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
        if should_commit {
            ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
            });
        }
        if should_err {
            Some(Err(BridgeRpcError::with_client_error(
                "InvalidArgument",
                "forced test error",
                eve_proto::ClientError::CustomInfo("forced test error".into()),
            )))
        } else {
            Some(Ok(PyRep::Dict(Vec::new())))
        }
    }

    static TEST_SLICE: &[RpcEntry] = &[
        rpc_entry_with_caller(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            "ReadCommits",
            RpcCallerClass::PreAuth,
            RpcDispatchHandler::Sync(test_read_commit_handler),
        )
        .read_only(),
        rpc_entry_with_caller(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            "ReadCommitsClean",
            RpcCallerClass::PreAuth,
            RpcDispatchHandler::Sync(test_read_commit_handler),
        )
        .read_only(),
        rpc_entry_with_caller(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            "WriteCommits",
            RpcCallerClass::PreAuth,
            RpcDispatchHandler::Sync(test_read_commit_handler),
        ),
    ];

    let mut server = ServerContext::new();
    server.rpc_registry = Arc::new(RpcRegistry::from_slices(&[TEST_SLICE]));
    let ctx = Arc::new(server);
    crate::common::pin_clock(&ctx);

    let kw = HashMap::new();
    let mut report_harness = crate::common::new_wire_harness(&ctx, 70_120)
        .with_mode(ContractValidationMode::ReportOnly);

    // 1. Read-marked RPC that commits and returns Ok produces `read_rpc_committed`.
    let bad_read = report_harness
        .call_exchange(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            None,
            "ReadCommits",
            &[PyRep::Bool(true), PyRep::Bool(false)],
            &kw,
        )
        .await;
    assert!(bad_read.result.is_ok());
    assert!(
        bad_read.violations.contains(&String::from(
            "read_rpc_committed: testReadAccessSvc.ReadCommits (1 commits)"
        )),
        "expected read_rpc_committed violation, got {:?}",
        bad_read.violations
    );

    // 2. Same read-marked RPC returning Err does NOT produce `read_rpc_committed`.
    let err_read = report_harness
        .call_exchange(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            None,
            "ReadCommits",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
        )
        .await;
    assert!(err_read.result.is_err());
    assert!(
        !err_read
            .violations
            .iter()
            .any(|v| v.starts_with("read_rpc_committed:")),
        "read RPC returning Err must not produce read_rpc_committed: {:?}",
        err_read.violations
    );

    // 3. Clean Read RPC and Write RPC with commit pass without violations.
    let clean_read = report_harness
        .call_exchange(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            None,
            "ReadCommitsClean",
            &[],
            &kw,
        )
        .await;
    assert!(clean_read.result.is_ok());
    assert!(clean_read.violations.is_empty());

    let ok_write = report_harness
        .call_exchange(
            // rpc-lint: allow synthetic read-only commit enforcement test RPC
            "testReadAccessSvc",
            None,
            "WriteCommits",
            &[PyRep::Bool(true), PyRep::Bool(false)],
            &kw,
        )
        .await;
    assert!(ok_write.result.is_ok());
    assert!(ok_write.violations.is_empty());

    assert_eq!(
        report_harness.recorded_calls(),
        &[
            RecordedRpcCall {
                rpc_name: "testReadAccessSvc.ReadCommits".to_string(),
                commits: 1,
                outcome: RpcCallOutcome::Ok,
            },
            RecordedRpcCall {
                rpc_name: "testReadAccessSvc.ReadCommits".to_string(),
                commits: 1,
                outcome: RpcCallOutcome::Err,
            },
            RecordedRpcCall {
                rpc_name: "testReadAccessSvc.ReadCommitsClean".to_string(),
                commits: 0,
                outcome: RpcCallOutcome::Ok,
            },
            RecordedRpcCall {
                rpc_name: "testReadAccessSvc.WriteCommits".to_string(),
                commits: 1,
                outcome: RpcCallOutcome::Ok,
            },
        ]
    );
    assert_eq!(
        report_harness.never_committed_ok_rpcs(),
        std::collections::BTreeSet::from(["testReadAccessSvc.ReadCommitsClean".to_string()])
    );

    // 4. Default Ratcheted mode panics on committing Read RPC.
    let ctx_panic = Arc::clone(&ctx);
    let panic_err = tokio::spawn(async move {
        let mut h = crate::common::new_wire_harness(&ctx_panic, 70_121);
        let res = h
            .call(
                // rpc-lint: allow synthetic read-only commit enforcement test RPC
                "testReadAccessSvc",
                None,
                "ReadCommits",
                &[PyRep::Bool(true), PyRep::Bool(false)],
                &HashMap::new(),
            )
            .await;
        assert!(res.is_ok());
    })
    .await
    .expect_err("default Ratcheted ClientHarness must panic on read_rpc_committed");
    assert!(panic_err.is_panic());
}

fn decode_wire_user_error_key(wire_body: &PyRep) -> String {
    let PyRep::Tuple(parts) = wire_body else {
        panic!("expected Macho ErrorResponse tuple, got {wire_body:?}");
    };
    assert_eq!(
        parts.get(1),
        Some(&PyRep::Integer(eve_proto::MACHONETERR_WRAPPEDEXCEPTION)),
        "expected MACHONETERR_WRAPPEDEXCEPTION wire wrapper"
    );
    let PyRep::Tuple(inner) = parts.get(2).expect("error payload inner tuple") else {
        panic!("expected inner ErrorResponse tuple, got {:?}", parts.get(2));
    };
    let PyRep::Bytes(raw) = inner.first().expect("marshaled exception bytes") else {
        panic!("expected marshaled exception Bytes, got {:?}", inner.first());
    };
    let PyRep::ObjectEx { header, .. } =
        eve_proto::marshal::decode(raw).expect("decode wrapped exception ObjectEx")
    else {
        panic!("expected ObjectEx exception");
    };
    let PyRep::Tuple(h) = header.as_ref() else {
        panic!("expected ObjectEx header tuple");
    };
    assert_eq!(
        h.first(),
        Some(&PyRep::Token("eveexceptions.UserError".into())),
        "expected eveexceptions.UserError, got {:?}",
        h.first()
    );
    let PyRep::Tuple(u_args) = h.get(1).expect("UserError header args") else {
        panic!("expected UserError header args tuple");
    };
    u_args
        .first()
        .and_then(PyRep::as_str)
        .expect("UserError key string")
        .to_owned()
}

#[tokio::test]
async fn test_fault_policy_wire_delivery_dogma_load_ammo_and_beyonce_warp_in_warp() {
    let ctx = Arc::new(ServerContext::new());
    ctx.seed_demo_world_fixtures();
    crate::common::pin_clock(&ctx);
    let mut pilot =
        crate::common::gm_pilot_in_space(&ctx, 70_130, 90_070_130, 587, 30_000_142).await;
    assert_eq!(
        ctx.flight_recorder.unmapped_custom_info_fallback_total(),
        0,
        "clean space bootstrap must record zero unmapped CustomInfo fallbacks"
    );

    let kw = HashMap::new();
    let load_err = {
        let mut harness = crate::common::borrow_wire_harness(&ctx, &mut pilot.sess);
        harness
            .call(
                "dogmaIM",
                None,
                "LoadAmmo",
                &[
                    PyRep::Integer(pilot.ship_id),
                    PyRep::List(Vec::new()),
                    PyRep::List(vec![PyRep::Integer(999_999_999)]),
                ],
                &kw,
            )
            .await
            .expect_err("dogmaIM.LoadAmmo missing charge must fail with ItemNotFound")
    };
    assert_eq!(load_err.code, "ItemNotFound");
    assert!(!load_err.used_unmapped_custom_info_fallback);
    assert_eq!(decode_wire_user_error_key(&load_err.wire_body), "ItemNotFound");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_movement_mode(
        pilot.ship_id,
        eve_destiny::MovementMode::Warp {
            origin: eve_destiny::DVec3::ZERO,
            destination: eve_destiny::DVec3::new(10_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 10_000_000.0,
            elapsed_warp_time: 1.0,
            phase: eve_destiny::WarpPhase::Cruising,
        },
    )
    .await
    .expect("put pilot ship in cruising warp");

    let warp_err = {
        let mut harness = crate::common::borrow_wire_harness(&ctx, &mut pilot.sess);
        harness
            .call(
                "beyonce",
                Some(pilot.beyonce),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
                &kw,
            )
            .await
            .expect_err("beyonce.CmdWarpToStuff while warping must fail with ShipAlreadyInWarp")
    };
    assert_eq!(warp_err.code, "ShipAlreadyInWarp");
    assert!(!warp_err.used_unmapped_custom_info_fallback);
    assert_eq!(decode_wire_user_error_key(&warp_err.wire_body), "ShipInWarp");
    assert_eq!(
        ctx.flight_recorder.unmapped_custom_info_fallback_total(),
        0,
        "policy-mapped LoadAmmo and CmdWarpToStuff must not count as unmapped CustomInfo fallbacks"
    );
}

#[tokio::test]
async fn test_unmapped_custom_info_fallback_wire_telemetry_and_harness_gate() {
    use eve_proto::BridgeRpcError;
    use eve_server::router::rpc_registry::{
        rpc_entry_with_caller, RpcCallerClass, RpcDispatchHandler, RpcEntry, RpcRegistry,
    };
    use eve_testkit::client_harness::ContractValidationMode;
    use eve_testkit::unmapped_custom_info_ratchet::UnmappedCustomInfoRatchet;

    fn test_unmapped_handler(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        Some(Err(BridgeRpcError {
            code: "SyntheticUnmappedWireCode".into(),
            message: "SyntheticUnmappedWireCode".into(),
            client_error: None,
        }))
    }

    static TEST_UNMAPPED_SLICE: &[RpcEntry] = &[rpc_entry_with_caller(
        // rpc-lint: allow synthetic unmapped fault telemetry wire test RPC
        "testUnmappedFaultSvc",
        "TriggerUnmapped",
        RpcCallerClass::PreAuth,
        RpcDispatchHandler::Sync(test_unmapped_handler),
    )];

    let mut server = ServerContext::new();
    server.rpc_registry = Arc::new(RpcRegistry::from_slices(&[TEST_UNMAPPED_SLICE]));
    let ctx = Arc::new(server);
    crate::common::pin_clock(&ctx);

    let kw = HashMap::new();
    let expected_key = "testUnmappedFaultSvc.TriggerUnmapped:SyntheticUnmappedWireCode";

    // 1. ReportOnly records fallback violation and FlightRecorder telemetry without panicking.
    let mut report_harness = crate::common::new_wire_harness(&ctx, 70_131)
        .with_mode(ContractValidationMode::ReportOnly);
    let report_out = report_harness
        .call_exchange(
            // rpc-lint: allow synthetic unmapped fault telemetry wire test RPC
            "testUnmappedFaultSvc",
            None,
            "TriggerUnmapped",
            &[],
            &kw,
        )
        .await;
    let report_err = report_out
        .result
        .expect_err("synthetic RPC must return wire ErrorResponse");
    assert_eq!(report_err.code, "SyntheticUnmappedWireCode");
    assert!(report_err.used_unmapped_custom_info_fallback);
    assert!(
        report_out
            .violations
            .contains(&format!("unmapped_custom_info_fallback: {expected_key}")),
        "expected unmapped_custom_info_fallback violation, got {:?}",
        report_out.violations
    );
    assert_eq!(ctx.flight_recorder.unmapped_custom_info_fallback_total(), 1);
    assert_eq!(
        ctx.flight_recorder
            .unmapped_custom_info_fallback_counts()
            .get(expected_key)
            .copied(),
        Some(1)
    );

    // 2. Default Ratcheted mode panics on unallowlisted fallback while FlightRecorder still counts.
    let ctx_panic = Arc::clone(&ctx);
    let panic_err = tokio::spawn(async move {
        let mut h = crate::common::new_wire_harness(&ctx_panic, 70_132);
        let res = h
            .call(
                // rpc-lint: allow synthetic unmapped fault telemetry wire test RPC
                "testUnmappedFaultSvc",
                None,
                "TriggerUnmapped",
                &[],
                &HashMap::new(),
            )
            .await;
        assert!(res.is_err());
    })
    .await
    .expect_err("Ratcheted ClientHarness must panic on unallowlisted wire CustomInfo fallback");
    assert!(panic_err.is_panic());
    assert_eq!(ctx.flight_recorder.unmapped_custom_info_fallback_total(), 2);

    // 3. Ratcheted with explicit ratchet entry permits it, whereas Strict still panics.
    let allowlist = UnmappedCustomInfoRatchet {
        version: 1,
        description: "synthetic wire test allowlist".into(),
        allowed_fallbacks: std::collections::BTreeSet::from([expected_key.to_owned()]),
    };
    let mut allowed_harness =
        crate::common::new_wire_harness(&ctx, 70_133).with_unmapped_custom_info_ratchet(allowlist.clone());
    let allowed_err = allowed_harness
        .call(
            // rpc-lint: allow synthetic unmapped fault telemetry wire test RPC
            "testUnmappedFaultSvc",
            None,
            "TriggerUnmapped",
            &[],
            &kw,
        )
        .await
        .expect_err("allowlisted fallback is returned cleanly in Ratcheted mode");
    assert!(allowed_err.used_unmapped_custom_info_fallback);
    assert_eq!(ctx.flight_recorder.unmapped_custom_info_fallback_total(), 3);

    let ctx_strict = Arc::clone(&ctx);
    let strict_err = tokio::spawn(async move {
        let mut h = crate::common::new_wire_harness(&ctx_strict, 70_134)
            .with_mode(ContractValidationMode::Strict)
            .with_unmapped_custom_info_ratchet(allowlist);
        let res = h
            .call(
                // rpc-lint: allow synthetic unmapped fault telemetry wire test RPC
                "testUnmappedFaultSvc",
                None,
                "TriggerUnmapped",
                &[],
                &HashMap::new(),
            )
            .await;
        assert!(res.is_err());
    })
    .await
    .expect_err("Strict ClientHarness must reject even allowlisted wire CustomInfo fallback");
    assert!(strict_err.is_panic());
    assert_eq!(ctx.flight_recorder.unmapped_custom_info_fallback_total(), 4);
}
