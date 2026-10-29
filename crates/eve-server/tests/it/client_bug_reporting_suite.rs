//! Integration tests for client exception reporting, native V24 `F12 -> Report a Bug`
//! (`bugreporter` `/bugs/api/*`), MachoNet `config_vals` advertisement, RPC flight recorder,
//! server log ring buffer, and GitHub sync staging (`tools/sync_bugs_to_github.py`).

use eve_machonet::{
    CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake,
    HandshakeCommand, HandshakeIdentity, HandshakeStepOutput, MachoHandshakeStateMachine,
    SessionState, VersionExchange,
};
use eve_proto::PyRep;
use eve_server::bug_reporting::{
    build_client_global_config, extract_text_files_from_zip, handle_bugs_http_request,
    BugIncidentBundle,
};
use eve_server::flight_recorder::FlightRecorder;
use eve_server::log_ring_buffer::{global_log_ring, LogRingBuffer};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

/// Builds a valid standard 32-bit PKZIP archive in memory with raw DEFLATE (`method = 8`) entries,
/// matching the `zipfile.ZipFile(..., ZIP_DEFLATED)` archive uploaded as `igbr.zip` by V24 `BugReportingService`.
fn build_test_deflate_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut central_dir = Vec::new();

    for &(name, raw_data) in entries {
        let compressed = miniz_oxide::deflate::compress_to_vec(raw_data, 6);
        let local_offset = buf.len() as u32;
        let name_bytes = name.as_bytes();
        let comp_len = compressed.len() as u32;
        let uncomp_len = raw_data.len() as u32;

        // Local file header (`0x04034b50`)
        buf.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes()); // version needed
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&8u16.to_le_bytes()); // compression method = 8 (DEFLATE)
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod time
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod date
        buf.extend_from_slice(&0u32.to_le_bytes()); // crc32 (unused by reader)
        buf.extend_from_slice(&comp_len.to_le_bytes());
        buf.extend_from_slice(&uncomp_len.to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra len
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&compressed);

        // Central directory file header (`0x02014b50`)
        central_dir.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // flags
        central_dir.extend_from_slice(&8u16.to_le_bytes()); // method = 8 (DEFLATE)
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // mod date
        central_dir.extend_from_slice(&0u32.to_le_bytes()); // crc32
        central_dir.extend_from_slice(&comp_len.to_le_bytes());
        central_dir.extend_from_slice(&uncomp_len.to_le_bytes());
        central_dir.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central_dir.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central_dir.extend_from_slice(&local_offset.to_le_bytes());
        central_dir.extend_from_slice(name_bytes);
    }

    let cd_offset = buf.len() as u32;
    let cd_size = central_dir.len() as u32;
    let count = entries.len() as u16;
    buf.extend_from_slice(&central_dir);

    // End of Central Directory record (`0x06054b50`)
    buf.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk num
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk with CD
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_offset.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // comment len
    buf
}

fn build_multipart_upload(boundary: &str, filename: &str, content_type: &str, file_bytes: &[u8]) -> (HashMap<String, String>, Vec<u8>) {
    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        format!("multipart/form-data; boundary={boundary}"),
    );

    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"File\"; filename=\"{filename}\"\r\n").as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (headers, body)
}

#[test]
fn test_flight_recorder_recording_and_redaction() {
    let recorder = FlightRecorder::new();
    let session_id = 42;

    // 1. Normal RPC call (including multi-byte UTF-8 argument > 32 bytes to verify char-boundary safety)
    recorder.record(
        session_id,
        "map",
        None,
        "GetSystemsInIncursions",
        &[PyRep::String("Жилой комплекс Нового Эдема — проверка границы UTF-8 символов".into())],
        &HashMap::new(),
        Duration::from_millis(15),
        "Ok(Tuple[3])",
    );

    // 2. Sensitive credential RPC call (must redact arguments)
    recorder.record(
        session_id,
        "authentication",
        None,
        "SetLanguageID",
        &[PyRep::String("testuser".into()), PyRep::String("supersecretpassword".into())],
        &HashMap::new(),
        Duration::from_millis(5),
        "Ok(None)",
    );

    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let history = recorder.get_history(session_id, now_millis, 10);
    assert_eq!(history.len(), 2);

    assert_eq!(history[0].service, "map");
    assert_eq!(history[0].method, "GetSystemsInIncursions");
    assert_eq!(history[0].status, "Ok(Tuple[3])");

    assert_eq!(history[1].service, "authentication");
    assert_eq!(history[1].method, "SetLanguageID");
    assert_eq!(history[1].args_summary, "(***redacted credentials***)");
}

#[test]
fn test_log_ring_buffer_snapshot() {
    let ring = LogRingBuffer::new(5);

    ring.record("INFO", "test::target", "Message 1");
    ring.record("WARN", "test::target", "Message 2");
    ring.record("ERROR", "test::target", "Message 3");

    let snapshot = ring.snapshot_recent(10);
    assert_eq!(snapshot.len(), 3);
    assert!(snapshot[0].contains("Message 1"));
    assert!(snapshot[1].contains("Message 2"));
    assert!(snapshot[2].contains("Message 3"));

    ring.record("INFO", "test::target", "Message 4");
    ring.record("INFO", "test::target", "Message 5");
    ring.record("INFO", "test::target", "Message 6");

    let snapshot2 = ring.snapshot_recent(10);
    assert_eq!(snapshot2.len(), 5);
    assert!(!snapshot2.iter().any(|s| s.contains("Message 1")));
    assert!(snapshot2.iter().any(|s| s.contains("Message 6")));
}

#[tokio::test]
async fn test_bug_incident_bundle_snapshot_and_queue() {
    let ctx = ServerContext::new();
    let mut session = SessionState::new(101);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142); // Jita
    session.station_id = Some(60_003_760); // Jita IV-4

    ctx.flight_recorder.record(
        101,
        "stationSvc",
        None,
        "GetStation",
        &[PyRep::Integer(60_003_760)],
        &HashMap::new(),
        Duration::from_millis(2),
        "Ok(KeyVal)",
    );

    global_log_ring().record("INFO", "test", "Pre-incident log line");

    let hash: i64 = 0x1234_5678_abcd;
    let stack_key = "eve.client.script.ui.shared.maps.map:Open";
    let trace = "Traceback (most recent call last):\n  File \"map.pyj\", line 412, in Open\nTypeError: 'NoneType' object is not subscriptable";

    let bundle = BugIncidentBundle::build_snapshot(&ctx, &session, hash, stack_key, trace);

    assert_eq!(bundle.stack_hash, hash);
    assert_eq!(bundle.stack_key, stack_key);
    assert_eq!(bundle.report_source, "exception");
    assert_eq!(bundle.session.character_name.as_deref(), Some("Aura Capsuleer"));
    assert_eq!(bundle.session.solar_system_name.as_deref(), Some("Jita"));
    assert_eq!(bundle.session.station_id, Some(60_003_760));
    assert_eq!(bundle.flight_recorder.len(), 1);
    assert_eq!(bundle.flight_recorder[0].service, "stationSvc");

    let temp_dir = std::env::temp_dir().join(format!("evomu_bug_test_{}", bundle.incident_id));
    let saved_path = bundle.save_to_queue(&temp_dir).expect("Failed to save bundle");
    assert!(saved_path.exists());

    let file_content = std::fs::read_to_string(&saved_path).expect("Failed to read back bundle");
    let loaded_bundle: BugIncidentBundle =
        serde_json::from_str(&file_content).expect("Failed to deserialize");
    assert_eq!(loaded_bundle.incident_id, bundle.incident_id);
    assert_eq!(loaded_bundle.stack_hash, hash);
    assert_eq!(
        loaded_bundle.session.character_name.as_deref(),
        Some("Aura Capsuleer")
    );

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn test_alert_send_client_stack_trace_alert_queues_bundle() {
    let ctx = ServerContext::new();
    let temp_dir =
        std::env::temp_dir().join(format!("evomu_test_alert_queue_{}", Uuid::new_v4()));
    ctx.bug_reporting.set_queue_dir_override(Some(temp_dir.clone()));

    let mut session = SessionState::new(202);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);

    let hash: i64 = 0x9876_5432_10fe;
    let stack_key = "carbon.common.script.sys.service:Run";
    let trace = "Traceback (most recent call last):\n  File \"service.pyj\", line 100, in Run\nZeroDivisionError: integer division or modulo by zero";

    let args = vec![
        PyRep::Tuple(vec![PyRep::Integer(hash), PyRep::String(stack_key.into())]),
        PyRep::String(trace.into()),
        PyRep::String("B".into()),
    ];

    let res = eve_server::services::client_login::handle(
        &ctx,
        &mut session,
        "alert",
        "SendClientStackTraceAlert",
        &args,
        &HashMap::new(),
        None,
    )
    .await;
    assert!(res.is_some());
    assert!(res.unwrap().is_ok());

    tokio::time::sleep(Duration::from_millis(1800)).await;

    let entries = std::fs::read_dir(&temp_dir).expect("queue dir should exist");
    let json_files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();

    assert_eq!(json_files.len(), 1, "Expected 1 queued incident file");
    let content = std::fs::read_to_string(&json_files[0]).expect("readable");
    let parsed: BugIncidentBundle = serde_json::from_str(&content).expect("valid JSON");
    assert_eq!(parsed.stack_hash, hash);
    assert_eq!(parsed.trace, trace);

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn test_macho_handshake_and_global_config_enable_v24_f12_bug_button() {
    let ctx = ServerContext::new();

    // 1. Verify LAN-host-aware config_vals advertised inside CryptoServerHandshake
    let cfg_vals = build_client_global_config(&ctx, Some("192.168.1.50"));
    assert_eq!(
        cfg_vals.get("bugReporting_ShowButton"),
        Some(&PyRep::ascii_bytes("1"))
    );
    assert_eq!(
        cfg_vals.get("bugReporting_BugReportServer"),
        Some(&PyRep::ascii_bytes("http://192.168.1.50:26004/bugs/api"))
    );

    let mut hm = MachoHandshakeStateMachine::new(VersionExchange::default(), 1, 9001, 9901);
    hm.set_config_vals(cfg_vals);
    hm.step(&VersionExchange::default().to_pyrep())
        .expect("version exchange");
    hm.step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .expect("crypto request");
    let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    let step3 = hm.step(&chal.to_pyrep()).expect("challenge");
    assert!(matches!(step3, HandshakeStepOutput::NeedsAuthentication(_)));

    let id = HandshakeIdentity {
        user_id: 1,
        role: 1,
        language_id: "EN".into(),
        user_type: 30,
        address: "192.168.1.99".into(),
        access_token: None,
        computer_hash: None,
    };
    let frames = hm
        .complete_challenge(&chal, Ok(id), 1)
        .expect("complete challenge");
    let srv_hs = CryptoServerHandshake::from_pyrep(&frames[1]).expect("decode server handshake");
    assert_eq!(
        srv_hs.config_vals.get("bugReporting_ShowButton"),
        Some(&PyRep::ascii_bytes("1"))
    );
    assert_eq!(
        srv_hs.config_vals.get("bugReporting_BugReportServer"),
        Some(&PyRep::ascii_bytes("http://192.168.1.50:26004/bugs/api"))
    );
    let res_pkt = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    assert!(matches!(
        hm.step(&res_pkt.to_pyrep()).expect("handshake result"),
        HandshakeStepOutput::Authenticated { .. }
    ));

    // 2. Verify `machoNet.GetGlobalConfig` also exposes `bugReporting_ShowButton` + `bugReporting_BugReportServer`
    let mut sess = SessionState::new(9001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let global_cfg = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetGlobalConfig", &[], &HashMap::new(), None)
        .await
        .expect("machoNet.GetGlobalConfig");
    let PyRep::Dict(entries) = global_cfg else {
        panic!("expected Dict from machoNet.GetGlobalConfig");
    };
    let find_key = |target: &str| -> Option<&PyRep> {
        entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == target => Some(v),
            _ => None,
        })
    };
    assert_eq!(
        find_key("bugReporting_ShowButton"),
        Some(&PyRep::ascii_bytes("1"))
    );
    assert_eq!(
        find_key("bugReporting_BugReportServer"),
        Some(&PyRep::ascii_bytes("http://127.0.0.1:26004/bugs/api"))
    );

    // 3. Verify `cache.GetServerVersionAndBuild` returns V24 2-tuple `(version, build)` for `BugReportingService.CollectData`
    let ver_tuple = ctx
        .dispatch_rpc(
            &mut sess,
            "cache",
            None,
            "GetServerVersionAndBuild",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("cache.GetServerVersionAndBuild");
    assert_eq!(
        ver_tuple,
        PyRep::Tuple(vec![PyRep::Real(24.01), PyRep::Integer(3_624_425)])
    );
}

#[tokio::test]
async fn test_f12_bugreporter_http_endpoints_zip_extraction_and_security_guards() {
    let ctx = ServerContext::new();
    let temp_dir = std::env::temp_dir().join(format!("evomu_f12_http_test_{}", Uuid::new_v4()));
    ctx.bug_reporting.set_queue_dir_override(Some(temp_dir.clone()));

    // Put character 90_000_001 (user_id = 1) online and undocked in Jita so session + coordinates + RPC history attach
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let c = snap.characters.get_mut(&90_000_001).expect("seeded char");
        c.station_id = None;
        c.structure_id = None;
        c.solar_system_id = 30_000_142;
        c.active_ship_id
    });
    assert!(ship_id > 0);
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 603, // Merlin
            position: glam::DVec3::new(12_500.0, -4_200.0, 88_000.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn player ship ball");

    ctx.db
        .acquire_online_lock_with_computer_hash(
            1,
            777,
            Some(90_000_001),
            Some("win11-playtest-pc".into()),
            true,
            ctx.now_unix(),
        )
        .expect("register online session");

    ctx.flight_recorder.record(
        777,
        "dogmaIM",
        None,
        "GetAllInfo",
        &[PyRep::Bool(true), PyRep::Bool(true)],
        &HashMap::new(),
        Duration::from_millis(4),
        "Ok(KeyVal)",
    );

    let empty_hdrs = HashMap::new();

    // 1. GET /bugs/api/status -> {"isOnline": true}
    let st_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/status", &empty_hdrs, b"");
    assert_eq!(st_resp.status_code, 200);
    let st_json: serde_json::Value = serde_json::from_slice(&st_resp.body).unwrap();
    assert_eq!(st_json["isOnline"], json!(true));

    // 2. GET /bugs/api/category -> {"categories": [{"id": ..., "value": ...}]}
    let cat_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/category", &empty_hdrs, b"");
    assert_eq!(cat_resp.status_code, 200);
    let cat_json: serde_json::Value = serde_json::from_slice(&cat_resp.body).unwrap();
    let cats = cat_json["categories"].as_array().expect("categories array");
    assert!(cats.len() >= 6);
    assert!(cats.iter().any(|c| c["id"] == "UI" && c["value"].is_string()));

    // 3. POST /bugs/api/issue -> 201 {"id": "EBR-...", "key": "EBR-..."}
    let issue_payload = json!({
        "categoryID": "UI",
        "summary": "Capacitor HUD ring flickers after undocking",
        "description": "After undocking from Jita 4-4, the HUD capacitor ring blinks.\n\nSession Info:\nCharacter: Aura Capsuleer (90000001)\nSolar System: Jita (30000142)",
        "reproductionSteps": "1. Dock in Jita 4-4\n2. Click Undock\n3. Observe capacitor ring",
        "computerInfo": "Windows 11 Pro 24H2 / RTX 4080 / DX12",
        "originalReporterId": "1",
        "Server": "EvoMu-LAN",
        "Build": 3624425,
        "labels": ["Client"]
    });
    let issue_bytes = serde_json::to_vec(&issue_payload).unwrap();
    let issue_resp =
        handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, &issue_bytes);
    assert_eq!(issue_resp.status_code, 201);
    let issue_json: serde_json::Value = serde_json::from_slice(&issue_resp.body).unwrap();
    let report_id = issue_json["id"].as_str().expect("id field").to_string();
    let report_key = issue_json["key"].as_str().expect("key field").to_string();
    assert_eq!(report_id, report_key);
    assert!(report_key.starts_with("EBR-"));

    // 4. POST /bugs/api/attachment/{issueID} for screenshot + DEFLATE-compressed `igbr.zip`
    let fake_jpeg = b"\xff\xd8\xff\xe0JFIF-ANNOTATED-SCREENSHOT";
    let (jpg_hdrs, jpg_body) = build_multipart_upload(
        "----EvoMuBugBoundary1",
        "igbr_0.jpg",
        "image/jpeg",
        fake_jpeg,
    );
    let att1 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_id}"),
        &jpg_hdrs,
        &jpg_body,
    );
    assert_eq!(att1.status_code, 201);

    let sample_eft = "[Merlin, Jita Patrol]\nLight Neutron Blaster II\nMagnetic Field Stabilizer II\n";
    let sample_logs = "2026-10-29T21:15:01Z INFO uicore initialized\n2026-10-29T21:15:04Z ERROR ShipHudSprite gauge NaN warning\n";
    let zip_bytes = build_test_deflate_zip(&[
        ("fitting.txt", sample_eft.as_bytes()),
        ("logs.txt", sample_logs.as_bytes()),
        ("settings.yaml", b"dx12: true\n"),
    ]);
    let extracted_direct = extract_text_files_from_zip(&zip_bytes, &["fitting.txt", "logs.txt"]);
    assert_eq!(extracted_direct.get("fitting.txt").map(String::as_str), Some(sample_eft));

    let (zip_hdrs, zip_body) = build_multipart_upload(
        "----EvoMuBugBoundary2",
        "igbr.zip",
        "application/zip",
        &zip_bytes,
    );
    let att2 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_id}"),
        &zip_hdrs,
        &zip_body,
    );
    assert_eq!(att2.status_code, 201);

    // 5. Negative / security boundary checks on `/bugs/api`
    let bad_json_resp =
        handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, b"{not-json");
    assert_eq!(bad_json_resp.status_code, 400);

    let traversal_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/..%2Fevil",
        &jpg_hdrs,
        &jpg_body,
    );
    assert_eq!(traversal_resp.status_code, 400);

    let raw_traversal_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/../evil",
        &jpg_hdrs,
        &jpg_body,
    );
    assert_eq!(raw_traversal_resp.status_code, 400);

    let missing_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/EBR-9999-MISSING",
        &jpg_hdrs,
        &jpg_body,
    );
    assert_eq!(missing_resp.status_code, 404);

    // Path-traversal in multipart filename must be stripped down to basename inside `attachments/{report_id}/`
    let (trav_file_hdrs, trav_file_body) = build_multipart_upload(
        "----EvoMuBugBoundary3",
        "../../escape_attempt.txt",
        "text/plain",
        b"confined",
    );
    let att3 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_id}"),
        &trav_file_hdrs,
        &trav_file_body,
    );
    assert_eq!(att3.status_code, 201);
    assert!(temp_dir
        .join("attachments")
        .join(&report_id)
        .join("escape_attempt.txt")
        .exists());
    assert!(!temp_dir.join("escape_attempt.txt").exists());

    // 6. Inspect persisted JSON bundle & staged attachments
    let bundle_path = ctx
        .bug_reporting
        .lookup_report_path(&report_id)
        .expect("saved report path");
    let bundle_raw = std::fs::read_to_string(&bundle_path).expect("read report json");
    let bundle: BugIncidentBundle = serde_json::from_str(&bundle_raw).expect("parse bundle");

    assert_eq!(bundle.report_source, "f12_manual");
    assert_eq!(
        bundle.session.character_name.as_deref(),
        Some("Aura Capsuleer")
    );
    assert_eq!(bundle.session.solar_system_name.as_deref(), Some("Jita"));
    let coords = bundle
        .session
        .coordinates
        .as_ref()
        .expect("in-space ship coordinates");
    assert!((coords.x - 12_500.0).abs() < 1.0);
    assert!((coords.y - (-4_200.0)).abs() < 1.0);
    assert!((coords.z - 88_000.0).abs() < 1.0);
    assert_eq!(bundle.flight_recorder.len(), 1);
    assert_eq!(bundle.flight_recorder[0].service, "dogmaIM");
    assert_eq!(bundle.flight_recorder[0].method, "GetAllInfo");

    let manual = bundle.manual_report.expect("manual_report populated");
    assert_eq!(manual.report_key, report_id);
    assert_eq!(manual.category_id, "UI");
    assert_eq!(
        manual.description,
        "After undocking from Jita 4-4, the HUD capacitor ring blinks."
    );
    assert!(manual.reproduction_steps.contains("Click Undock"));
    assert_eq!(manual.ship_fitting_eft.as_deref(), Some(sample_eft.trim()));
    assert_eq!(manual.client_logs_tail.len(), 2);
    assert!(manual.client_logs_tail[1].contains("ShipHudSprite gauge NaN warning"));
    assert!(manual
        .attachments
        .contains(&format!("attachments/{report_id}/igbr_0.jpg")));
    assert!(manual
        .attachments
        .contains(&format!("attachments/{report_id}/igbr.zip")));
    assert!(temp_dir
        .join("attachments")
        .join(&report_id)
        .join("igbr_0.jpg")
        .exists());
    assert!(temp_dir
        .join("attachments")
        .join(&report_id)
        .join("igbr.zip")
        .exists());

    let _ = std::fs::remove_dir_all(temp_dir);
}

#[test]
fn test_sync_bugs_cli_dry_run_supports_both_crashes_and_f12_reports() {
    let run_id = Uuid::new_v4();
    let temp_queue = std::env::temp_dir().join(format!("evomu_sync_cli_queue_{run_id}"));
    let temp_uploaded = std::env::temp_dir().join(format!("evomu_sync_cli_uploaded_{run_id}"));
    std::fs::create_dir_all(temp_queue.join("attachments").join("EBR-0001-ABCDEF")).unwrap();
    std::fs::write(
        temp_queue
            .join("attachments")
            .join("EBR-0001-ABCDEF")
            .join("igbr_0.jpg"),
        b"jpeg-bytes",
    )
    .unwrap();

    let crash_bundle = json!({
        "incident_id": "test-uuid-42",
        "timestamp": "2026-10-29T10:40:00Z",
        "timestamp_millis": 1793280000000u64,
        "stack_hash": 0x1234_5678_abcd_i64,
        "stack_key": "eve.client.script.ui.shared.maps.map:Open",
        "trace": "Traceback (most recent call last):\n  File \"map.pyj\", line 412, in Open\nTypeError: 'NoneType' object is not subscriptable",
        "report_source": "exception",
        "session": {
            "session_id": 1,
            "character_id": 90000001,
            "character_name": "Aura Capsuleer",
            "solar_system_id": 30000142,
            "solar_system_name": "Jita",
            "security_status": 0.9,
            "station_id": 60003760,
            "ship_type_name": "Drake",
            "address": "127.0.0.1:50000"
        },
        "flight_recorder": [
            {
                "offset_ms": -45,
                "service": "map",
                "method": "GetSystemsInIncursions",
                "args_summary": "()",
                "status": "Ok(Tuple[3])",
                "duration_micros": 850
            }
        ],
        "server_logs_before": ["2026-10-29T10:39:59Z INFO test"],
        "server_logs_after": ["2026-10-29T10:40:01Z INFO done"]
    });

    let manual_bundle = json!({
        "incident_id": "test-uuid-99",
        "timestamp": "2026-10-29T10:45:00Z",
        "timestamp_millis": 1793280300000u64,
        "stack_hash": -72057594037927935_i64,
        "stack_key": "f12:ui:Capacitor ring flickers",
        "trace": "",
        "report_source": "f12_manual",
        "manual_report": {
            "report_key": "EBR-0001-ABCDEF",
            "category_id": "UI",
            "category_name": "User Interface & Windows",
            "title": "Capacitor ring flickers",
            "description": "HUD gauge flickers right after undock.",
            "reproduction_steps": "1. Undock from Jita 4-4",
            "computer_info": "Win11 DX12",
            "server_name": "EvoMu",
            "client_build": 3624425,
            "labels": ["Client"],
            "attachments": ["attachments/EBR-0001-ABCDEF/igbr_0.jpg"],
            "ship_fitting_eft": "[Merlin, Fit]\nLight Neutron Blaster II",
            "client_logs_tail": ["ERROR gauge flicker"]
        },
        "session": {
            "session_id": 2,
            "character_id": 90000001,
            "character_name": "Aura Capsuleer",
            "solar_system_id": 30000142,
            "solar_system_name": "Jita",
            "security_status": 0.9,
            "coordinates": { "x": 1000.0, "y": 2000.0, "z": 3000.0 },
            "ship_type_name": "Merlin",
            "address": "192.168.1.20:54321"
        },
        "flight_recorder": [],
        "server_logs_before": [],
        "server_logs_after": []
    });

    std::fs::write(
        temp_queue.join("1_crash.json"),
        crash_bundle.to_string(),
    )
    .unwrap();
    std::fs::write(
        temp_queue.join("2_manual_EBR-0001-ABCDEF.json"),
        manual_bundle.to_string(),
    )
    .unwrap();

    let script_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tools/sync_bugs_to_github.py");

    let output = std::process::Command::new("python3")
        .arg(&script_path)
        .arg("--queue-dir")
        .arg(&temp_queue)
        .arg("--uploaded-dir")
        .arg(&temp_uploaded)
        .arg("--dry-run")
        .output()
        .expect("failed to execute python sync script");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Discovered 2 queued incident report(s)"));
    assert!(stdout.contains("[Client Crash] TypeError: 'NoneType' object is not subscriptable"));
    assert!(stdout.contains("[Bug Report][UI] Capacitor ring flickers"));
    assert!(stdout.contains("EBR-0001-ABCDEF"));
    assert!(stdout.contains("Processed 2/2 incident file(s)"));

    let _ = std::fs::remove_dir_all(temp_queue);
    let _ = std::fs::remove_dir_all(temp_uploaded);
}
