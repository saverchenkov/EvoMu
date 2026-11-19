//! Unit tests for automated client exception capture, `F12` bug reporter HTTP API,
//! multipart attachment handling, and `igbr.zip` decompression (`bug_reporting`).

#![forbid(unsafe_code)]

use super::*;

fn make_temp_queue_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("evomu_{prefix}_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp bug queue dir");
    dir
}

/// Builds a minimal valid 32-bit PKZIP archive with `(filename, raw_bytes, compress_deflate)` entries.
fn build_test_zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central_dir = Vec::new();

    for &(name, raw, deflate) in entries {
        let local_offset = out.len() as u32;
        let (method, payload): (u16, Vec<u8>) = if deflate {
            (8, miniz_oxide::deflate::compress_to_vec(raw, 6))
        } else {
            (0, raw.to_vec())
        };
        let name_bytes = name.as_bytes();
        let comp_len = payload.len() as u32;
        let uncomp_len = raw.len() as u32;

        // Local file header (30 bytes + filename)
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (unused by reader)
        out.extend_from_slice(&comp_len.to_le_bytes());
        out.extend_from_slice(&uncomp_len.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&payload);

        // Central directory file header (46 bytes + filename)
        central_dir.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // flags
        central_dir.extend_from_slice(&method.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u32.to_le_bytes());
        central_dir.extend_from_slice(&comp_len.to_le_bytes());
        central_dir.extend_from_slice(&uncomp_len.to_le_bytes());
        central_dir.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central_dir.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central_dir.extend_from_slice(&local_offset.to_le_bytes());
        central_dir.extend_from_slice(name_bytes);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central_dir.len() as u32;
    out.extend_from_slice(&central_dir);

    // End of central directory (22 bytes)
    let count = entries.len() as u16;
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn build_multipart_body(boundary: &str, filename: &str, content: &[u8]) -> (HashMap<String, String>, Vec<u8>) {
    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        format!("multipart/form-data; boundary=\"{boundary}\""),
    );
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (headers, body)
}

#[test]
fn test_runtime_queue_dir_and_disk_lookup_fallback() {
    let dir = make_temp_queue_dir("bug_rt");
    let rt = BugReportingRuntime::new();
    assert_eq!(rt.next_seq(), 1);
    assert_eq!(rt.next_seq(), 2);

    rt.set_queue_dir_override(Some(dir.clone()));
    assert_eq!(rt.effective_queue_dir(), dir);

    let staged_file = dir.join("1700000000000_0000000000000001_EBR-0042-DISK99.json");
    std::fs::write(&staged_file, b"{}").expect("write dummy staged file");

    // Disk fallback discovers files ending with `_{report_key}.json` even before in-memory registration
    assert_eq!(
        rt.lookup_report_path("EBR-0042-DISK99").as_deref(),
        Some(staged_file.as_path())
    );
    assert!(rt.lookup_report_path("EBR-9999-MISSING").is_none());

    let mem_file = dir.join("custom_mem.json");
    rt.register_report_path("EBR-0001-MEM", mem_file.clone());
    assert_eq!(
        rt.lookup_report_path("EBR-0001-MEM").as_deref(),
        Some(mem_file.as_path())
    );

    rt.set_queue_dir_override(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_client_global_config_host_normalization_and_extras() {
    let _env_guard = crate::lock_test_env_read();
    let ctx = ServerContext::new();
    ctx.set_esi_port(26_001);
    if let Ok(mut extra) = ctx.global_config_values.write() {
        extra.insert("customFlag".to_string(), PyRep::ascii_bytes("enabled"));
    }

    let cases = [
        (None, "127.0.0.1", "http://127.0.0.1:26001/bugs/api"),
        (Some("0.0.0.0"), "127.0.0.1", "http://127.0.0.1:26001/bugs/api"),
        (Some("[::1]"), "[::1]", "http://127.0.0.1:26001/bugs/api"),
        (
            Some("::ffff:10.20.30.40"),
            "[::ffff:10.20.30.40]",
            "http://10.20.30.40:26001/bugs/api",
        ),
        (Some("2001:db8::5"), "[2001:db8::5]", "http://[2001:db8::5]:26001/bugs/api"),
        (Some("192.168.1.99"), "192.168.1.99", "http://192.168.1.99:26001/bugs/api"),
    ];

    for (hint, expected_ejabberd, expected_bug_url) in cases {
        let cfg = build_client_global_config(&ctx, hint);
        assert_eq!(
            cfg.get("bugReporting_ShowButton"),
            Some(&PyRep::ascii_bytes("1"))
        );
        assert_eq!(
            cfg.get("bugReporting_BugReportServer"),
            Some(&PyRep::ascii_bytes(expected_bug_url))
        );
        assert_eq!(
            cfg.get("ejabberdAddress"),
            Some(&PyRep::ascii_bytes(expected_ejabberd))
        );
        assert_eq!(
            cfg.get("ejabberdPort"),
            Some(&PyRep::Integer(5222))
        );
        assert_eq!(
            cfg.get("ejabberd_client_port"),
            Some(&PyRep::Integer(5222))
        );
        assert_eq!(
            cfg.get("customFlag"),
            Some(&PyRep::ascii_bytes("enabled"))
        );
    }

    ctx.set_esi_port(0);
    ctx.set_xmpp_port(25_232);
    let no_http_cfg = build_client_global_config(&ctx, Some("127.0.0.1"));
    assert!(!no_http_cfg.contains_key("bugReporting_BugReportServer"));
    assert!(!no_http_cfg.contains_key("bugReporting_ShowButton"));
    assert_eq!(
        no_http_cfg.get("ejabberd_client_address"),
        Some(&PyRep::ascii_bytes("127.0.0.1"))
    );
    assert_eq!(
        no_http_cfg.get("ejabberdPort"),
        Some(&PyRep::Integer(25_232))
    );
    assert_eq!(
        no_http_cfg.get("ejabberd_client_port"),
        Some(&PyRep::Integer(25_232))
    );
}

#[test]
fn test_helpers_stack_hash_sanitization_and_zip_extraction() {
    // Non-uthread stack hashes pass through untouched
    assert_eq!(
        disambiguate_uthread_stack_hash(123_456, "scanner.py:10", "KeyError: 'foo'"),
        123_456
    );
    // Uthread wrapper without trace frames preserves original hash
    assert_eq!(
        disambiguate_uthread_stack_hash(-1_040_548_311, "MollycoddledUthread", "no frames"),
        -1_040_548_311
    );

    let trace_a = "Thrown at:\n<pre>outer.py:10</pre>\n<pre>leaf_a.py:99</pre>\nFormatted exception info: KeyError: 'alpha'\nEXCEPTION END";
    let trace_b = "Thrown at:\n<pre>outer.py:10</pre>\n<pre>leaf_b.py:42</pre>\nFormatted exception info: ValueError: 'beta'\nEXCEPTION END";
    let hash_a = disambiguate_uthread_stack_hash(-1_040_548_311, "MollycoddledUthread", trace_a);
    let hash_b = disambiguate_uthread_stack_hash(-1_040_548_311, "MollycoddledUthread", trace_b);
    assert_ne!(hash_a, -1_040_548_311);
    assert_ne!(hash_a, hash_b);
    assert_eq!(
        hash_a,
        disambiguate_uthread_stack_hash(-1_040_548_311, "MollycoddledUthread", trace_a)
    );

    // Server InternalError / DatabasePersistenceError RuntimeErrors must disambiguate even
    // outside MollycoddledUthread so server panics never collide with client-call-site bugs
    // such as #286 (16470365), #342 (1335766134), and #346 (-655675819).
    let warp_stack_key = "autopilot.py:90 -> CmdWarpToStuff";
    let warp_user_error_trace =
        "Thrown at:\n<pre>spaceMgr.py:210</pre>\nFormatted exception info: UserError: ('WarpTooShort', {})\nEXCEPTION END";
    assert_eq!(
        disambiguate_uthread_stack_hash(1_335_766_134, warp_stack_key, warp_user_error_trace),
        1_335_766_134
    );

    let internal_err_a =
        "Thrown at:\n<pre>spaceMgr.py:210</pre>\nFormatted exception info: RuntimeError: InternalError: RPC handler panicked\nEXCEPTION END";
    let internal_err_b =
        "Thrown at:\n<pre>spaceMgr.py:210</pre>\nFormatted exception info: RuntimeError: InternalError: Quasar mismatch\nEXCEPTION END";
    let db_persist_err =
        "Thrown at:\n<pre>spaceMgr.py:210</pre>\nFormatted exception info:\nRuntimeError: DatabasePersistenceError: WAL write failed\nEXCEPTION END";
    let h_int_a = disambiguate_uthread_stack_hash(1_335_766_134, warp_stack_key, internal_err_a);
    let h_int_b = disambiguate_uthread_stack_hash(1_335_766_134, warp_stack_key, internal_err_b);
    let h_db = disambiguate_uthread_stack_hash(16_470_365, "menuEntryData.py:44", db_persist_err);
    assert_ne!(h_int_a, 1_335_766_134);
    assert_ne!(h_int_a, h_int_b);
    assert_ne!(h_db, 16_470_365);
    // Identical InternalError at the same call site still deduplicates deterministically.
    assert_eq!(
        h_int_a,
        disambiguate_uthread_stack_hash(1_335_766_134, warp_stack_key, internal_err_a)
    );
    // Non-internal RuntimeError outside MollycoddledUthread stays untouched, and markers after EXCEPTION END are ignored.
    assert_eq!(
        disambiguate_uthread_stack_hash(-655_675_819, "autopilot.py:55 -> CmdDock", "Formatted exception info: RuntimeError: WarpDistanceTooShort\nEXCEPTION END\nRuntimeError: InternalError: ignored"),
        -655_675_819
    );

    // Session info parser
    assert_eq!(
        parse_char_id_from_session_info("Intro\nCharacter: Aura Pilot (90000001)\nSolarSystem: Jita"),
        Some(90_000_001)
    );
    assert_eq!(parse_char_id_from_session_info("Character: NoParens"), None);
    assert_eq!(parse_char_id_from_session_info("Character: BadId (abc)"), None);

    // Report key and filename sanitization
    assert!(is_safe_report_key("EBR-0001-ABC_123"));
    assert!(!is_safe_report_key(""));
    assert!(!is_safe_report_key("../escape"));
    assert!(!is_safe_report_key(&"x".repeat(65)));

    assert_eq!(
        sanitize_attachment_filename(r"C:\Users\pilot\Desktop\screen shot?1.png"),
        "screen_shot_1.png"
    );
    assert_eq!(sanitize_attachment_filename(".."), "attachment.bin");
    assert_eq!(sanitize_attachment_filename(""), "attachment.bin");
    assert_eq!(sanitize_attachment_filename(&"a".repeat(120)).len(), 96);

    assert_eq!(category_display_name("combat"), "Combat, Fitting & Dogma");
    assert_eq!(category_display_name("Custom"), "Custom");

    // Log tail summarizer keeps last 60 non-empty lines
    let mut raw_logs = String::new();
    for i in 1..=75 {
        raw_logs.push_str(&format!("line {i}   \n\n"));
    }
    let tail = summarize_client_logs(&raw_logs);
    assert_eq!(tail.len(), 60);
    assert_eq!(tail.first().map(String::as_str), Some("line 16"));
    assert_eq!(tail.last().map(String::as_str), Some("line 75"));

    // ZIP extraction: both Stored (method 0) and Deflated (method 8)
    let zip_bytes = build_test_zip(&[
        ("nested/fitting.txt", b"[Rifter, Test Fit]\n200mm AutoCannon I", false),
        ("logs.txt", b"2026-11-05 10:00:00 Client initialized\n2026-11-05 10:00:05 Undocked", true),
        ("ignored.bin", b"\x00\x01\x02", false),
    ]);
    let extracted = extract_text_files_from_zip(&zip_bytes, &["fitting.txt", "logs.txt"]);
    assert_eq!(extracted.len(), 2);
    assert_eq!(
        extracted.get("fitting.txt").map(String::as_str),
        Some("[Rifter, Test Fit]\n200mm AutoCannon I")
    );
    assert!(extracted
        .get("logs.txt")
        .is_some_and(|s| s.contains("Client initialized")));
    assert!(extract_text_files_from_zip(b"short", &["logs.txt"]).is_empty());
    assert!(extract_text_files_from_zip(&[0u8; 64], &["logs.txt"]).is_empty());
}

#[tokio::test]
async fn test_bundle_snapshot_enrichment_and_http_bug_lifecycle() {
    let dir = make_temp_queue_dir("bug_http");
    let ctx = ServerContext::new();
    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    let mut session = SessionState::new(77);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(100_000_001);
    session.address = "127.0.0.1:51234".to_string();

    ctx.flight_recorder.record(
        77,
        "dogmaIM",
        Some(10_001),
        "Activate",
        &[PyRep::Integer(1)],
        &HashMap::new(),
        std::time::Duration::from_micros(420),
        "ok",
    );

    let bundle = BugIncidentBundle::build_snapshot(
        &ctx,
        &session,
        -1_040_548_311,
        "MollycoddledUthread",
        "Thrown at:\n<pre>gunnery.py:88</pre>\nFormatted exception info: ZeroDivisionError\nEXCEPTION END",
    );
    assert_eq!(bundle.report_source, "exception");
    assert_eq!(
        bundle.session.character_name.as_deref(),
        Some("Aura Capsuleer")
    );
    assert_eq!(bundle.session.solar_system_name.as_deref(), Some("Jita"));
    assert!(bundle.session.ship_type_id.is_some());
    assert!(!bundle.flight_recorder.is_empty());

    let saved_exc = bundle.save_to_queue(&dir).expect("save exception bundle");
    assert!(saved_exc.exists());

    // Exercise async trailing capture
    schedule_incident_capture(
        bundle.clone(),
        Some(dir.clone()),
        Some(Duration::from_millis(15)),
    );
    tokio::time::sleep(Duration::from_millis(60)).await;

    // 1. GET /bugs/api/status & /bugs/api/category
    let empty_hdrs = HashMap::new();
    let status_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/status", &empty_hdrs, b"");
    assert_eq!(status_resp.status_code, 200);
    let status_json: serde_json::Value = serde_json::from_slice(&status_resp.body).unwrap();
    assert_eq!(status_json["isOnline"], json!(true));

    let cat_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/category", &empty_hdrs, b"");
    assert_eq!(cat_resp.status_code, 200);
    let cat_json: serde_json::Value = serde_json::from_slice(&cat_resp.body).unwrap();
    assert_eq!(cat_json["categories"].as_array().map(Vec::len), Some(8));

    // 2. POST /bugs/api/issue invalid JSON -> 400, valid JSON -> 201
    let bad_issue = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, b"{bad");
    assert_eq!(bad_issue.status_code, 400);

    let issue_payload = serde_json::to_vec(&json!({
        "categoryID": "Combat",
        "summary": "Hybrid turret tracking anomaly",
        "description": "Turrets miss stationary target.\n\nSession Info:\nCharacter: Aura (90000001)",
        "reproductionSteps": "1. Lock target\n2. Fire guns",
        "computerInfo": "macOS arm64",
        "originalReporterId": 1,
        "Server": "EvoMu-Local",
        "Build": eve_machonet::V24_BOOT_BUILD,
        "labels": ["combat", "turrets"]
    }))
    .unwrap();
    let issue_resp =
        handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, &issue_payload);
    assert_eq!(issue_resp.status_code, 201);
    let issue_json: serde_json::Value = serde_json::from_slice(&issue_resp.body).unwrap();
    let report_key = issue_json["key"].as_str().unwrap().to_string();
    assert!(report_key.starts_with("EBR-"));
    assert_eq!(issue_json["id"], json!(report_key));

    // 3. Attachment validation and igbr.zip extraction
    let bad_key_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/../bad",
        &empty_hdrs,
        b"",
    );
    assert_eq!(bad_key_resp.status_code, 400);

    let missing_key_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/EBR-9999-MISSING",
        &empty_hdrs,
        b"",
    );
    assert_eq!(missing_key_resp.status_code, 404);

    let bad_mp_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_key}"),
        &empty_hdrs,
        b"not multipart",
    );
    assert_eq!(bad_mp_resp.status_code, 400);

    let igbr_zip = build_test_zip(&[
        ("fitting.txt", b"[Merlin, Rail Fit]\n150mm Railgun I", true),
        ("logs.txt", b"Client log line 1\nClient log line 2", true),
    ]);
    let (mp_hdrs, mp_body) = build_multipart_body("----EvoMuBoundary77", "igbr.zip", &igbr_zip);
    let attach_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_key}"),
        &mp_hdrs,
        &mp_body,
    );
    assert_eq!(attach_resp.status_code, 201);

    // Direct fitting.txt & logs.txt uploads plus attachment cap enforcement (MAX_ATTACHMENTS_PER_REPORT = 16)
    let (fit_hdrs, fit_body) =
        build_multipart_body("----B2", "fitting.txt", b"[Tristan, Drone Fit]\nHobgoblin I");
    assert_eq!(
        handle_bugs_http_request(
            &ctx,
            "POST",
            &format!("/bugs/api/attachment/{report_key}"),
            &fit_hdrs,
            &fit_body,
        )
        .status_code,
        201
    );

    let staged_path = ctx.bug_reporting.lookup_report_path(&report_key).unwrap();
    let staged_bundle: BugIncidentBundle =
        serde_json::from_slice(&std::fs::read(&staged_path).unwrap()).unwrap();
    let manual = staged_bundle.manual_report.as_ref().unwrap();
    assert_eq!(manual.description, "Turrets miss stationary target.");
    assert_eq!(manual.category_name, "Combat, Fitting & Dogma");
    assert_eq!(
        manual.client_build,
        Some(i64::from(eve_machonet::V24_BOOT_BUILD))
    );
    assert_eq!(
        manual.ship_fitting_eft.as_deref(),
        Some("[Tristan, Drone Fit]\nHobgoblin I")
    );
    assert_eq!(
        manual.client_logs_tail,
        vec!["Client log line 1".to_string(), "Client log line 2".to_string()]
    );
    assert_eq!(manual.attachments.len(), 2);

    // Fill remaining 14 attachment slots and verify the 17th is rejected with 429
    for idx in 0..14 {
        let (h, b) = build_multipart_body("----B3", &format!("shot_{idx}.png"), b"PNGDATA");
        let r = handle_bugs_http_request(
            &ctx,
            "POST",
            &format!("/bugs/api/attachment/{report_key}"),
            &h,
            &b,
        );
        assert_eq!(r.status_code, 201);
    }
    let (h_over, b_over) = build_multipart_body("----B3", "shot_over.png", b"PNGDATA");
    let over_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{report_key}"),
        &h_over,
        &b_over,
    );
    assert_eq!(over_resp.status_code, 429);

    let unknown_route = handle_bugs_http_request(&ctx, "GET", "/bugs/api/unknown", &empty_hdrs, b"");
    assert_eq!(unknown_route.status_code, 404);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_bundle_cumulative_rpc_and_handshake_telemetry() {
    let ctx = ServerContext::new();
    let empty_kwargs = HashMap::new();
    for idx in 0..34 {
        ctx.flight_recorder.record(
            777,
            "dogmaIM",
            Some(10_001),
            "Activate",
            &[PyRep::Integer(idx as i64)],
            &empty_kwargs,
            Duration::from_millis(1),
            "ok",
        );
    }
    ctx.flight_recorder.record(
        777,
        "publicGatewaySvc",
        None,
        "Send",
        &[],
        &empty_kwargs,
        Duration::from_millis(2),
        "OK",
    );
    ctx.flight_recorder.record_gateway_request(Some(777), Some(42));
    ctx.flight_recorder.record_gateway_request(Some(777), Some(42));
    ctx.flight_recorder.record_handshake_builds(
        777,
        Some(42),
        Some(3_698_100),
        eve_machonet::V24_BOOT_BUILD,
        true,
    );

    let mut session = SessionState::new(777);
    session.user_id = Some(42);
    session.character_id = Some(90_000_001);

    let bundle = BugIncidentBundle::build_snapshot(
        &ctx,
        &session,
        -12345,
        "ui.window:Open",
        "Traceback (most recent call last):\n  File \"window.py\", line 10, in Open\nKeyError: 'id'",
    );
    assert_eq!(bundle.server_request_count, 35);
    assert_eq!(bundle.gateway_request_count, 3);
    assert_eq!(bundle.flight_recorder.len(), 20);
    let hs = bundle.handshake.as_ref().expect("handshake metadata populated");
    assert_eq!(hs.client_boot_build, Some(3_698_100));
    assert_eq!(hs.server_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(hs.build_mismatch, Some(true));

    let encoded = serde_json::to_string(&bundle).expect("serialize bundle");
    let decoded: BugIncidentBundle = serde_json::from_str(&encoded).expect("deserialize bundle");
    assert_eq!(decoded.server_request_count, 35);
    assert_eq!(decoded.gateway_request_count, 3);
    assert_eq!(decoded.handshake.as_ref(), Some(hs));
}

#[test]
fn test_bundle_telemetry_defaults_and_user_fallback() {
    // Legacy bundle JSON without new BF-1344-02 / BF-1344-03 fields still deserializes cleanly.
    let legacy_json = serde_json::json!({
        "incident_id": "legacy-1",
        "timestamp": "2026-11-18T00:00:00Z",
        "timestamp_millis": 1000,
        "stack_hash": 1,
        "stack_key": "k",
        "trace": "",
        "session": BugSessionSnapshot::default(),
        "flight_recorder": [],
        "server_logs_before": [],
        "server_logs_after": []
    });
    let parsed: BugIncidentBundle = serde_json::from_value(legacy_json).expect("legacy defaults");
    assert_eq!(parsed.server_request_count, 0);
    assert_eq!(parsed.gateway_request_count, 0);
    assert!(parsed.handshake.is_none());

    // User-keyed Quasar gateway & handshake build lookups still resolve when session_id is 0.
    let ctx = ServerContext::new();
    ctx.flight_recorder.record_gateway_request(None, Some(99));
    ctx.flight_recorder.record_handshake_builds(
        888,
        Some(99),
        Some(eve_machonet::V24_BOOT_BUILD),
        eve_machonet::V24_BOOT_BUILD,
        false,
    );

    let mut http_only_session = SessionState::new(0);
    http_only_session.user_id = Some(99);
    let snap = BugIncidentBundle::build_snapshot(&ctx, &http_only_session, 0, "UI", "");
    assert_eq!(snap.server_request_count, 0);
    assert_eq!(snap.gateway_request_count, 1);
    let hs = snap.handshake.as_ref().expect("user-fallback handshake");
    assert_eq!(hs.client_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(hs.server_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(hs.build_mismatch, Some(false));

    // When session_id > 0, stale prior user_gateway_counts must NOT leak across sessions,
    // and upgrading the client build clears prior build_mismatch flags.
    ctx.flight_recorder.record_handshake_builds(
        900,
        Some(123),
        Some(3_600_000),
        eve_machonet::V24_BOOT_BUILD,
        true,
    );
    ctx.flight_recorder.record_gateway_request(Some(900), Some(123));
    ctx.flight_recorder.reset_user_telemetry(123);
    ctx.flight_recorder.record_handshake_builds(
        901,
        Some(123),
        Some(eve_machonet::V24_BOOT_BUILD),
        eve_machonet::V24_BOOT_BUILD,
        false,
    );
    let mut upgraded_session = SessionState::new(901);
    upgraded_session.user_id = Some(123);
    let upgraded_snap = BugIncidentBundle::build_snapshot(&ctx, &upgraded_session, 0, "UI", "");
    assert_eq!(upgraded_snap.gateway_request_count, 0);
    let upgraded_hs = upgraded_snap.handshake.as_ref().expect("upgraded handshake");
    assert_eq!(upgraded_hs.client_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(upgraded_hs.build_mismatch, Some(false));

    // Unknown client build omits `build_mismatch` in serialized JSON instead of claiming false compatibility.
    let mut bare_session = SessionState::new(999);
    bare_session.user_id = Some(555);
    let bare = BugIncidentBundle::build_snapshot(&ctx, &bare_session, 1, "UI", "");
    let bare_hs = bare.handshake.as_ref().expect("default server build");
    assert_eq!(bare_hs.client_boot_build, None);
    assert_eq!(bare_hs.server_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(bare_hs.build_mismatch, None);
    let bare_json = serde_json::to_value(&bare).expect("serialize bare");
    assert!(
        bare_json["handshake"].get("build_mismatch").is_none(),
        "unknown client build must omit build_mismatch instead of emitting false"
    );
}
