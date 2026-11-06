//! Task T-6: Integration test for `tools/client-harness/run_journeys` verifying two-client
//! concurrent bridge orchestration, `result.json` schema for all 29 journeys in
//! `client_journeys.json`, honest `passed: false` reporting when `EVE.app` is not driven,
//! and log-hygiene rejection of `UnimplementedRPC` / `panicked at` / `Traceback` / `EXCEPTION`.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::{BridgeEnvelope, PyRep, MAX_FRAME_SIZE};
use eve_server::router::ServerContext;
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
                let result = if service == "timeControl" && method == "AdvanceClock" {
                    let secs = args.first().and_then(|a| match a {
                        PyRep::Integer(i) => Some(*i as u64),
                        _ => None,
                    }).unwrap_or(12);
                    ctx.advance_secs(secs);
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        let sys_h = ctx.systems.get_or_spawn(sys_id);
                        if let Ok(Some((_, ball))) = sys_h.get_ball_snapshot(ship_id).await {
                            if matches!(ball.mode, eve_destiny::MovementMode::Warp { .. }) {
                                for _ in 0..secs.min(60) {
                                    let _ = sys_h.tick().await;
                                }
                            }
                        }
                    }
                    Ok(PyRep::Bool(true))
                } else if (service == "timeControl" || service == "harness") && (method == "FastForwardSessionTimer" || method == "ClearSessionTimer") {
                    if let Some(cid) = session.character_id {
                        eve_server::services::space_destiny::clear_session_change_timer(&ctx, cid);
                    }
                    Ok(PyRep::Bool(true))
                } else {
                    ctx.dispatch_rpc(
                        &mut session,
                        &service,
                        bound_id,
                        &method,
                        &args,
                        &kwargs,
                        caller_trace.as_deref(),
                    )
                    .await
                };

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
    let mut ctx = ServerContext::new();
    ctx.feature_flags.free_fast_checkout_plex = true;
    ctx.feature_flags.starter_plex_balance = 50_000;
    let ctx = Arc::new(ctx);
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_nes_store_catalog(&ctx);
    ctx.db.provision_gm_account("other", "other-secret", 90_000_004, "ISD Other");
    let other_uid = ctx.db.authenticate("other", "other-secret").unwrap().user_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(90_000_004, 50_000);
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
    assert!(site_src.contains("schedule_startup") && site_src.contains("_install_logmodule_hook"));
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
