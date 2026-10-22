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
    mut stream: TcpStream,
    ctx: Arc<ServerContext>,
    server_log: Arc<Mutex<std::fs::File>>,
) {
    let sess_id = NEXT_HARNESS_SESS_ID.fetch_add(1, Ordering::Relaxed);
    let mut session = SessionState::new(sess_id);

    loop {
        let mut len_buf = [0u8; 4];
        if stream.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let frame_len = u32::from_le_bytes(len_buf) as usize;
        if frame_len == 0 || frame_len > MAX_FRAME_SIZE {
            break;
        }
        let mut payload = vec![0u8; frame_len];
        if stream.read_exact(&mut payload).await.is_err() {
            break;
        }
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

        let resp = BridgeEnvelope::RpcResponse { req_id, result };
        let Ok(out_bytes) = serde_json::to_vec(&resp) else {
            break;
        };
        if stream
            .write_all(&(out_bytes.len() as u32).to_le_bytes())
            .await
            .is_err()
        {
            break;
        }
        if stream.write_all(&out_bytes).await.is_err() {
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
                let _ = stream
                    .write_all(&(ev_bytes.len() as u32).to_le_bytes())
                    .await;
                let _ = stream.write_all(&ev_bytes).await;
            }
        }
    }

    if let Some(uid) = session.user_id {
        ctx.db.release_online_lock(uid, session.session_id);
    }
    ctx.release_session_bindings(session.session_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_t6_client_harness_run_journeys_two_clients_and_log_hygiene() {
    // Task T-6: Real-client & dual-client bridge journey harness (`tools/client-harness/run_journeys`).
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

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
