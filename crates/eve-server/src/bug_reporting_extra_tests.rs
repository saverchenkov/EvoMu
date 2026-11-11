//! Additional unit and mutation-killing tests for `bug_reporting`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

fn make_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("evomu_bug_extra_{tag}_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn test_find_bytes_and_multipart_edge_cases() {
    assert_eq!(find_bytes(b"hello", b""), None);
    assert_eq!(find_bytes(b"hi", b"hello"), None);
    assert_eq!(find_bytes(b"hello", b"hello"), Some(0));
    assert_eq!(find_bytes(b"xxhelloyy", b"hello"), Some(2));
    assert_eq!(find_bytes(b"xxhello", b"hello"), Some(2));
    assert_eq!(find_bytes(b"xxhell", b"hello"), None);

    let mut hdrs = HashMap::new();
    assert!(extract_multipart_file(&hdrs, b"anything").is_none());

    hdrs.insert("content-type".to_string(), "application/json".to_string());
    assert!(extract_multipart_file(&hdrs, b"anything").is_none());

    hdrs.insert(
        "content-type".to_string(),
        "multipart/form-data; boundary=\"\"".to_string(),
    );
    assert!(extract_multipart_file(&hdrs, b"anything").is_none());

    hdrs.insert(
        "content-type".to_string(),
        "multipart/form-data; boundary=BOUND".to_string(),
    );
    // Missing boundary in body
    assert!(extract_multipart_file(&hdrs, b"no boundary here").is_none());
    // Boundary not followed by \r\n or \n
    assert!(extract_multipart_file(&hdrs, b"--BOUNDxx\r\n\r\ndata").is_none());
    // Missing \r\n\r\n header separator
    assert!(extract_multipart_file(&hdrs, b"--BOUND\r\nContent-Disposition: form-data").is_none());

    // LF-only after boundary, no filename attribute -> defaults to attachment.bin, no closing boundary -> takes rest
    let lf_body = b"--BOUND\nContent-Type: text/plain\r\n\r\nraw_payload_bytes";
    let (fname, data) = extract_multipart_file(&hdrs, lf_body).expect("LF after boundary should parse");
    assert_eq!(fname, "attachment.bin");
    assert_eq!(data, b"raw_payload_bytes");
}

#[test]
fn test_report_key_filename_category_and_log_summary_boundaries() {
    // Exact 64-char boundary for is_safe_report_key
    let key_64 = "A".repeat(64);
    let key_65 = "A".repeat(65);
    assert!(is_safe_report_key(&key_64));
    assert!(!is_safe_report_key(&key_65));
    assert!(is_safe_report_key("a-b_9"));
    assert!(!is_safe_report_key("a.b"));
    assert!(!is_safe_report_key("a/b"));

    // Exact 96-char boundary and dot cases for sanitize_attachment_filename
    assert_eq!(sanitize_attachment_filename("."), "attachment.bin");
    assert_eq!(sanitize_attachment_filename(".."), "attachment.bin");
    assert_eq!(sanitize_attachment_filename("folder/"), "attachment.bin");
    let name_96 = "x".repeat(96);
    assert_eq!(sanitize_attachment_filename(&name_96).len(), 96);
    let name_97 = "x".repeat(97);
    assert_eq!(sanitize_attachment_filename(&name_97).len(), 96);

    // All 8 canonical categories
    for (id, expected) in [
        ("gameplay", "Gameplay & Mechanics"),
        ("ui", "User Interface & Windows"),
        ("combat", "Combat, Fitting & Dogma"),
        ("space", "Navigation, Space & Structures"),
        ("economy", "Market, Industry & Contracts"),
        ("pve", "Missions, NPCs & Exploration"),
        ("graphic", "Graphics, Audio & Performance"),
        ("localization", "Localization & Text"),
    ] {
        assert_eq!(category_display_name(id), expected);
    }

    // Exact 60 vs 61 lines in summarize_client_logs
    let logs_60: String = (1..=60).map(|i| format!("L{i}\n")).collect();
    let sum_60 = summarize_client_logs(&logs_60);
    assert_eq!(sum_60.len(), 60);
    assert_eq!(sum_60[0], "L1");
    assert_eq!(sum_60[59], "L60");

    let logs_61: String = (1..=61).map(|i| format!("L{i}\n")).collect();
    let sum_61 = summarize_client_logs(&logs_61);
    assert_eq!(sum_61.len(), 60);
    assert_eq!(sum_61[0], "L2");
    assert_eq!(sum_61[59], "L61");
}

#[test]
fn test_uthread_hash_disambiguation_and_session_info_parsing_branches() {
    // Disambiguate when MollycoddledUthread is in trace even if not in stack_key
    let trace_only_formatted =
        "MollycoddledUthread\n  Formatted exception info: AttributeError: 'NoneType'\n";
    let h1 = disambiguate_uthread_stack_hash(999, "normal_key", trace_only_formatted);
    assert_ne!(h1, 999);

    // Disambiguate when only Thrown at + <pre> frame is present
    let trace_only_thrown =
        "Thrown at:\n<pre>first.py:1</pre>\n<pre>leaf.py:2</pre>\nEXCEPTION END\n<pre>after_end.py:3</pre>";
    let h2 = disambiguate_uthread_stack_hash(999, "MollycoddledUthread", trace_only_thrown);
    assert_ne!(h2, 999);
    assert_ne!(h1, h2);

    // Verify <pre> after EXCEPTION END is ignored by take_while
    let trace_without_after =
        "Thrown at:\n<pre>first.py:1</pre>\n<pre>leaf.py:2</pre>\nEXCEPTION END";
    assert_eq!(
        h2,
        disambiguate_uthread_stack_hash(999, "MollycoddledUthread", trace_without_after)
    );

    // Session info parser with nickname parens before character ID parens
    assert_eq!(
        parse_char_id_from_session_info("Character: Pilot (Nickname) ( 90000077 )\n"),
        Some(90_000_077)
    );
    assert_eq!(
        parse_char_id_from_session_info("Character: Unclosed (90000077\n"),
        None
    );
    assert_eq!(parse_char_id_from_session_info("No marker here"), None);
}

#[test]
fn test_zip_extractor_corrupt_and_limit_guards() {
    // Craft a ZIP with an unsupported method (99) and an oversized entry (> 4 MiB)
    let mut out = Vec::new();
    let mut cd = Vec::new();
    let name = b"logs.txt";
    let payload = b"hello";

    // Local header
    out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
    out.extend_from_slice(&[0u8; 22]);
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(payload);

    // Central directory entry 1: unsupported method = 99
    cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    cd.extend_from_slice(&[0u8; 6]);
    cd.extend_from_slice(&99u16.to_le_bytes()); // method 99
    cd.extend_from_slice(&[0u8; 8]);
    cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
    cd.extend_from_slice(&[0u8; 14]);
    cd.extend_from_slice(&0u32.to_le_bytes()); // local_hdr_off = 0
    cd.extend_from_slice(name);

    // Central directory entry 2: uncomp_size exceeds MAX_ZIP_TEXT_ENTRY_BYTES
    cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    cd.extend_from_slice(&[0u8; 6]);
    cd.extend_from_slice(&0u16.to_le_bytes()); // method 0
    cd.extend_from_slice(&[0u8; 8]);
    cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    cd.extend_from_slice(&((MAX_ZIP_TEXT_ENTRY_BYTES + 1) as u32).to_le_bytes());
    cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
    cd.extend_from_slice(&[0u8; 14]);
    cd.extend_from_slice(&0u32.to_le_bytes());
    cd.extend_from_slice(name);

    let cd_off = out.len() as u32;
    let cd_len = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    let res = extract_text_files_from_zip(&out, &["logs.txt"]);
    assert!(res.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_enrich_session_snapshot_in_space_coordinates_and_categories_route() {
    let dir = make_temp_dir("enrich");
    let ctx = ServerContext::new();
    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    // Spawn pilot ship in space in Jita (30000142) at known coordinates
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let ship_id = 100_000_001;
    jita.spawn_ball(Ball {
        id: ship_id,
        type_id: 606,
        position: DVec3::new(12_345.0, -67_890.0, 42.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();

    // Undock character 90000001 in DB snapshot so station_id and structure_id are None
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = 30_000_142;
            c.active_ship_id = ship_id;
        }
    });

    let mut sess = SessionState::new(88);
    sess.character_id = Some(90_000_001);
    let bundle = BugIncidentBundle::build_snapshot(&ctx, &sess, 42, "key", "trace");
    assert_eq!(bundle.session.user_id, Some(1));
    assert_eq!(bundle.session.solar_system_id, Some(30_000_142));
    let coords = bundle
        .session
        .coordinates
        .expect("in-space ship should capture ballpark coordinates");
    assert_eq!(coords.x, 12_345.0);
    assert_eq!(coords.y, -67_890.0);
    assert_eq!(coords.z, 42.0);

    // Fallback to db.item_positions when ship is not in live ballpark
    let fallback_ship_id = 100_999_888;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions
            .insert(fallback_ship_id, [111.0, 222.0, 333.0]);
    });
    let mut snap2 = BugSessionSnapshot {
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    enrich_session_snapshot(&ctx, &mut snap2, Some(fallback_ship_id));
    assert!(snap2.security_status.is_some());
    let coords2 = snap2
        .coordinates
        .expect("should fallback to db.item_positions");
    assert_eq!((coords2.x, coords2.y, coords2.z), (111.0, 222.0, 333.0));

    // Verify GET /bugs/api/categories/ (plural + trailing slash)
    let hdrs = HashMap::new();
    let resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/categories/", &hdrs, b"");
    assert_eq!(resp.status_code, 200);

    // Verify POST /bugs/api/issue with string reporter ID and string Build
    let body = serde_json::to_vec(&json!({
        "categoryID": "UI",
        "summary": "Window overlap",
        "description": "Details",
        "originalReporterId": "1",
        "Build": "3624425"
    }))
    .unwrap();
    let post_resp = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue/", &hdrs, &body);
    assert_eq!(post_resp.status_code, 201);
    let post_val: serde_json::Value = serde_json::from_slice(&post_resp.body).unwrap();
    let rkey = post_val["key"].as_str().unwrap();

    // Upload direct logs.txt and verify deduplicated attachment list on repeat upload
    let mut mp_hdrs = HashMap::new();
    mp_hdrs.insert(
        "content-type".to_string(),
        "multipart/form-data; boundary=B".to_string(),
    );
    let mp_body = b"--B\r\nContent-Disposition: form-data; filename=\"logs.txt\"\r\n\r\n Direct log entry \r\n--B--";
    for _ in 0..2 {
        let a_resp = handle_bugs_http_request(
            &ctx,
            "POST",
            &format!("/bugs/api/attachment/{rkey}"),
            &mp_hdrs,
            mp_body,
        );
        assert_eq!(a_resp.status_code, 201);
    }
    let staged_path = ctx.bug_reporting.lookup_report_path(rkey).unwrap();
    let loaded: BugIncidentBundle =
        serde_json::from_slice(&std::fs::read(&staged_path).unwrap()).unwrap();
    let manual = loaded.manual_report.unwrap();
    assert_eq!(manual.attachments.len(), 1);
    assert_eq!(manual.client_logs_tail, vec![" Direct log entry".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bug_issue_authenticated_binding_corp_alliance_and_session_info_fallback() {
    let dir = make_temp_dir("auth_issue");
    let ctx = ServerContext::new();
    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let now = ctx.now_unix();

    // Token with character_id = 0 so auth_cid is None and Session Info / character lookup branches run
    let code_no_cid = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            0,
            "bug-reporter-cli",
            "http://127.0.0.1/cb",
            vec![],
            challenge,
            "S256",
            now,
        )
        .expect("authorize pkce");
    let tok_no_cid = ctx
        .db
        .exchange_pkce_code_with_redirect(
            &code_no_cid,
            "bug-reporter-cli",
            verifier,
            Some("http://127.0.0.1/cb"),
            now,
        )
        .expect("exchange pkce");

    // 1. Invalid Authorization Bearer header must be rejected with 401
    let mut bad_auth = HashMap::new();
    bad_auth.insert(
        "Authorization".to_string(),
        "Bearer invalid.jwt.token".to_string(),
    );
    let body_basic = serde_json::to_vec(&json!({
        "categoryID": "Gameplay",
        "summary": "Auth failure test",
        "description": "Body"
    }))
    .unwrap();
    let r_401 = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &bad_auth, &body_basic);
    assert_eq!(r_401.status_code, 401);

    let mut auth_hdrs = HashMap::new();
    auth_hdrs.insert(
        "authorization".to_string(),
        format!("Bearer {}", tok_no_cid.access_token),
    );

    // 2. Offline user with foreign Session Info character id rejects foreign id and falls back to user's own character
    let body_foreign_session_info = serde_json::to_vec(&json!({
        "categoryID": "Space",
        "summary": "Foreign session info spoofing test",
        "description": "Main report body\n\nSession Info:\nCharacter: SpoofedPilot (999999999)\nSystem: Jita"
    }))
    .unwrap();
    let r_foreign = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &auth_hdrs,
        &body_foreign_session_info,
    );
    assert_eq!(r_foreign.status_code, 201);
    let key_foreign = serde_json::from_slice::<serde_json::Value>(&r_foreign.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let b_foreign: BugIncidentBundle = serde_json::from_slice(
        &std::fs::read(ctx.bug_reporting.lookup_report_path(&key_foreign).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(b_foreign.session.user_id, Some(1));
    assert_eq!(b_foreign.session.session_id, 0);
    assert!(b_foreign.flight_recorder.is_empty());
    let chosen_cid = b_foreign.session.character_id.expect("falls back to owned char");
    assert_ne!(chosen_cid, 999_999_999);

    // 3. Offline user with owned Session Info character id binds that owned character id via parse_char_id_from_session_info
    let body_owned_session_info = serde_json::to_vec(&json!({
        "categoryID": "Combat",
        "summary": "Owned session info char test",
        "description": "Details\n\nSession Info:\nCharacter: Demo Pilot (90000001)"
    }))
    .unwrap();
    let r_owned = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &auth_hdrs,
        &body_owned_session_info,
    );
    assert_eq!(r_owned.status_code, 201);
    let key_owned = serde_json::from_slice::<serde_json::Value>(&r_owned.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let b_owned: BugIncidentBundle = serde_json::from_slice(
        &std::fs::read(ctx.bug_reporting.lookup_report_path(&key_owned).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(b_owned.session.character_id, Some(90_000_001));

    // 4. Online user session + alliance/corp enrichment + FlightRecorder capture
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            99_000_123,
            eve_db::AllianceRecord {
                alliance_id: 99_000_123,
                name: "Test Coalition".to_string(),
                short_name: "TCOA".to_string(),
                executor_corp_id: 98_000_001,
                member_corp_ids: [98_000_001].into_iter().collect(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.alliance_id = Some(99_000_123);
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    ctx.db
        .acquire_online_lock(1, 55_001, Some(90_000_001), true, now)
        .expect("acquire online lock");
    ctx.flight_recorder.record(
        55_001,
        "map",
        Some(10),
        "GetSolarSystemStats", // rpc-lint: allow test fixture
        &[eve_proto::PyRep::Integer(30_000_142)],
        &HashMap::new(),
        std::time::Duration::from_millis(3),
        "ok",
    );

    let r_online = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &auth_hdrs, &body_basic);
    assert_eq!(r_online.status_code, 201);
    let key_online = serde_json::from_slice::<serde_json::Value>(&r_online.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let b_online: BugIncidentBundle = serde_json::from_slice(
        &std::fs::read(ctx.bug_reporting.lookup_report_path(&key_online).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(b_online.session.session_id, 55_001);
    assert_eq!(
        b_online.session.corporation_name.as_deref(),
        Some("Demo Corporation")
    );
    assert_eq!(
        b_online.session.alliance_name.as_deref(),
        Some("Test Coalition")
    );
    assert_eq!(b_online.session.station_id, Some(60_003_760));
    assert!(b_online.session.coordinates.is_none());
    assert_eq!(b_online.flight_recorder.len(), 1);
    assert_eq!(b_online.flight_recorder[0].service, "map");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_enforce_disk_limits_byte_eviction_tree_bytes_and_lookup_miss() {
    let dir = make_temp_dir("disk_limits");
    let state = BugReportingRuntime::default();

    // directory_tree_bytes on missing path, single file, and nested dir
    let missing = dir.join("does_not_exist");
    assert_eq!(directory_tree_bytes(&missing), 0);
    state.enforce_disk_limits(&missing, 5, 1024);

    let solo_file = dir.join("solo.txt");
    std::fs::write(&solo_file, vec![0x61u8; 17]).unwrap();
    assert_eq!(directory_tree_bytes(&solo_file), 17);

    // Non-file .json directory entry and non-.json file must be ignored cleanly
    std::fs::create_dir_all(dir.join("ignored_dir.json")).unwrap();
    std::fs::write(dir.join("notes.txt"), b"keep").unwrap();

    let p1 = dir.join("0001_KEYOLD.json");
    std::fs::write(&p1, vec![b'{'; 30]).unwrap();
    let a1 = dir.join("attachments").join("KEYOLD");
    std::fs::create_dir_all(&a1).unwrap();
    std::fs::write(a1.join("blob.bin"), vec![0x42u8; 200]).unwrap();
    state.register_report_path("KEYOLD", p1.clone());

    std::thread::sleep(std::time::Duration::from_millis(20));

    let p2 = dir.join("0002_KEYNEW.json");
    std::fs::write(&p2, vec![b'{'; 30]).unwrap();
    let a2 = dir.join("attachments").join("KEYNEW");
    std::fs::create_dir_all(&a2).unwrap();
    std::fs::write(a2.join("small.bin"), vec![0x43u8; 10]).unwrap();
    state.register_report_path("KEYNEW", p2.clone());

    // max_reports = 10 (>= remaining 2), so ONLY total_bytes > 80 forces oldest eviction
    state.enforce_disk_limits(&dir, 10, 80);

    assert!(!p1.exists(), "oldest bundle should be evicted by byte cap");
    assert!(!a1.exists(), "oldest attachment dir should be removed");
    assert!(p2.exists(), "newer bundle should survive");
    assert!(a2.exists(), "newer attachment dir should survive");
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join("ignored_dir.json").is_dir());
    assert!(state.lookup_report_path("KEYOLD").is_none());
    assert_eq!(state.lookup_report_path("KEYNEW"), Some(p2));

    // lookup_report_path returns None when queue_dir does not exist
    let state2 = BugReportingRuntime::default();
    state2.set_queue_dir_override(Some(missing));
    assert!(state2.lookup_report_path("ANY").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_attachment_error_paths_zip_edge_cases_and_capture_failure() {
    let dir = make_temp_dir("attach_edges");
    let ctx = ServerContext::new();
    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    let hdrs = HashMap::new();
    let issue_body = serde_json::to_vec(&json!({
        "categoryID": "UI",
        "summary": "Attachment edges",
        "description": "Body"
    }))
    .unwrap();
    let post_resp = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &hdrs, &issue_body);
    assert_eq!(post_resp.status_code, 201);
    let rkey = serde_json::from_slice::<serde_json::Value>(&post_resp.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // 1. Oversized body (> MAX_BUG_ATTACHMENT_BYTES) -> 413
    let too_big = vec![b'a'; MAX_BUG_ATTACHMENT_BYTES + 1];
    let resp_413 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{rkey}"),
        &hdrs,
        &too_big,
    );
    assert_eq!(resp_413.status_code, 413);

    let mut mp_hdrs = HashMap::new();
    mp_hdrs.insert(
        "content-type".to_string(),
        "multipart/form-data; boundary=B".to_string(),
    );

    // 2. Whitespace-only fitting.txt does not populate ship_fitting_eft
    let empty_fit = b"--B\r\nContent-Disposition: form-data; filename=\"fitting.txt\"\r\n\r\n   \t\r\n\r\n--B--";
    let resp_fit = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{rkey}"),
        &mp_hdrs,
        empty_fit,
    );
    assert_eq!(resp_fit.status_code, 201);

    // 3. Corrupt igbr.zip of > 65_557 bytes with no EOCD completes without panic and leaves fitting None
    let mut big_no_eocd = b"--B\r\nContent-Disposition: form-data; filename=\"igbr.zip\"\r\n\r\n".to_vec();
    big_no_eocd.extend(vec![0x11u8; 66_000]);
    big_no_eocd.extend_from_slice(b"\r\n--B--");
    let resp_big_zip = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{rkey}"),
        &mp_hdrs,
        &big_no_eocd,
    );
    assert_eq!(resp_big_zip.status_code, 201);

    let staged_path = ctx.bug_reporting.lookup_report_path(&rkey).unwrap();
    let loaded: BugIncidentBundle =
        serde_json::from_slice(&std::fs::read(&staged_path).unwrap()).unwrap();
    assert!(loaded.manual_report.unwrap().ship_fitting_eft.is_none());

    // 4. Blocked file write (pre-existing directory named clash.txt) -> 500
    let clash_dir = dir.join("attachments").join(&rkey).join("clash.txt");
    std::fs::create_dir_all(&clash_dir).unwrap();
    let clash_mp = b"--B\r\nContent-Disposition: form-data; filename=\"clash.txt\"\r\n\r\nhi\r\n--B--";
    let resp_500_write = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{rkey}"),
        &mp_hdrs,
        clash_mp,
    );
    assert_eq!(resp_500_write.status_code, 500);

    // 5. Blocked attachments root (file instead of dir) -> create_dir_all fails -> 500
    let blocker_dir = make_temp_dir("attach_blocker");
    std::fs::write(blocker_dir.join("attachments"), b"not a directory").unwrap();
    ctx.bug_reporting
        .set_queue_dir_override(Some(blocker_dir.clone()));
    let r2 = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &hdrs, &issue_body);
    let rkey2 = serde_json::from_slice::<serde_json::Value>(&r2.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let ok_mp = b"--B\r\nContent-Disposition: form-data; filename=\"note.txt\"\r\n\r\nhi\r\n--B--";
    let resp_500_mkdir = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{rkey2}"),
        &mp_hdrs,
        ok_mp,
    );
    assert_eq!(resp_500_mkdir.status_code, 500);

    // 6. extract_text_files_from_zip boundary cases: <22 bytes, >65557 no EOCD, invalid local sig, corrupt deflate, truncated CD
    assert!(extract_text_files_from_zip(&[0u8; 21], &["logs.txt"]).is_empty());
    assert!(extract_text_files_from_zip(&vec![0u8; 66_000], &["logs.txt"]).is_empty());

    // Build a synthetic ZIP exercising invalid local header sig, out-of-range data slice, and corrupt deflate (method=8)
    let mut z = Vec::new();
    let mut cd = Vec::new();
    let fname = b"logs.txt";
    // Local header at offset 0 with method=8 and garbage payload, plus local_name_len=2, local_extra_len=1
    let bad_local_off = z.len() as u32;
    z.extend_from_slice(b"NOTP"); // bad local sig for entry 1

    let deflate_local_off = z.len() as u32;
    z.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
    z.extend_from_slice(&[0u8; 22]);
    z.extend_from_slice(&2u16.to_le_bytes()); // local_name_len = 2
    z.extend_from_slice(&1u16.to_le_bytes()); // local_extra_len = 1
    z.extend_from_slice(b"xyZ");
    let bad_deflate = [0xffu8, 0x00, 0xfe, 0xaa];
    z.extend_from_slice(&bad_deflate);

    let push_cd = |cd_buf: &mut Vec<u8>, method: u16, csize: u32, usize_: u32, off: u32| {
        cd_buf.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        cd_buf.extend_from_slice(&[0u8; 6]);
        cd_buf.extend_from_slice(&method.to_le_bytes());
        cd_buf.extend_from_slice(&[0u8; 8]);
        cd_buf.extend_from_slice(&csize.to_le_bytes());
        cd_buf.extend_from_slice(&usize_.to_le_bytes());
        cd_buf.extend_from_slice(&(fname.len() as u16).to_le_bytes());
        cd_buf.extend_from_slice(&[0u8; 14]);
        cd_buf.extend_from_slice(&off.to_le_bytes());
        cd_buf.extend_from_slice(fname);
    };
    push_cd(&mut cd, 0, 1, 1, bad_local_off); // bad local header sig
    push_cd(&mut cd, 0, 5000, 5000, deflate_local_off); // data slice out of bounds
    push_cd(&mut cd, 8, bad_deflate.len() as u32, 10, deflate_local_off); // corrupt deflate
    push_cd(&mut cd, 0, (MAX_ZIP_TEXT_ENTRY_BYTES + 1) as u32, 10, deflate_local_off); // comp_size limit

    let cd_off = z.len() as u32;
    let cd_len = cd.len() as u32;
    z.extend_from_slice(&cd);
    z.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    z.extend_from_slice(&[0u8; 4]);
    z.extend_from_slice(&4u16.to_le_bytes());
    z.extend_from_slice(&4u16.to_le_bytes());
    z.extend_from_slice(&cd_len.to_le_bytes());
    z.extend_from_slice(&cd_off.to_le_bytes());
    z.extend_from_slice(&0u16.to_le_bytes());
    assert!(extract_text_files_from_zip(&z, &["logs.txt"]).is_empty());

    // Truncated CD (EOCD claims 1 entry at offset near end where cursor + 46 > len)
    let mut trunc = vec![0u8; 10];
    let trunc_cd_off = trunc.len() as u32;
    trunc.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    trunc.extend_from_slice(&[0u8; 4]);
    trunc.extend_from_slice(&1u16.to_le_bytes());
    trunc.extend_from_slice(&1u16.to_le_bytes());
    trunc.extend_from_slice(&0u32.to_le_bytes());
    trunc.extend_from_slice(&trunc_cd_off.to_le_bytes());
    trunc.extend_from_slice(&0u16.to_le_bytes());
    assert!(extract_text_files_from_zip(&trunc, &["logs.txt"]).is_empty());

    // 7. schedule_incident_capture handles save_to_queue failure when queue_dir is a regular file
    let bad_qdir = dir.join("queue_as_file");
    std::fs::write(&bad_qdir, b"file").unwrap();
    let sess = SessionState::new(9001);
    let fail_bundle = BugIncidentBundle::build_snapshot(&ctx, &sess, 7, "stack", "trace");
    schedule_incident_capture(
        fail_bundle,
        Some(bad_qdir),
        Some(std::time::Duration::from_millis(1)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&blocker_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bug_reporting_env_eviction_auth_enrichment_and_zip_bounds() {
    let dir = make_temp_dir("auth_evict");
    let ctx = ServerContext::new();

    // 1. Test EVOMU_BUG_QUEUE_DIR & EVOMU_BUG_SERVER_URL env fallbacks under env write lock
    {
        let _env_guard = crate::lock_test_env_write();
        std::env::set_var("EVOMU_BUG_QUEUE_DIR", &dir);
        std::env::set_var("EVOMU_BUG_SERVER_URL", "http://bugs.example.test/api/");
        let rt_env = BugReportingRuntime::new();
        assert_eq!(rt_env.effective_queue_dir(), dir);
        std::env::remove_var("EVOMU_BUG_QUEUE_DIR");
        assert_eq!(rt_env.effective_queue_dir(), PathBuf::from(DEFAULT_BUG_QUEUE_DIR));

        let gcfg = build_client_global_config(&ctx, None);
        assert_eq!(
            gcfg.get("bugReporting_BugReportServer"),
            Some(&PyRep::ascii_bytes("http://bugs.example.test/api"))
        );
        std::env::remove_var("EVOMU_BUG_SERVER_URL");
    }

    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    // 2. Test Corp + Alliance enrichment in enrich_session_snapshot and default_report_source serde fallback
    let ally_id = 99_001_234;
    let corp_id = 98_001_234;
    let char_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "Test Alliance Please Ignore".to_string(),
                short_name: "TAPI".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: std::iter::once(corp_id).collect(),
            },
        );
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Dreddit".to_string(); __corp.ticker = "DRDT".to_string(); __corp.ceo_id = char_id; __corp.alliance_id = Some(ally_id); __corp.tax_rate = 0.05; __corp.members = std::iter::once(char_id).collect(); __corp.member_roles = HashMap::new(); __corp.applications = HashMap::new(); __corp.wallet_divisions = Vec::new(); __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = Some(60_003_760); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
        }
    });

    let mut corp_snap = BugSessionSnapshot {
        character_id: Some(char_id),
        ..Default::default()
    };
    enrich_session_snapshot(&ctx, &mut corp_snap, None);
    assert_eq!(corp_snap.corporation_name.as_deref(), Some("Dreddit"));
    assert_eq!(
        corp_snap.alliance_name.as_deref(),
        Some("Test Alliance Please Ignore")
    );

    // Deserialize BugIncidentBundle JSON omitting `report_source` to trigger `default_report_source`
    let min_bundle_json = serde_json::json!({
        "incident_id": "inc-1",
        "timestamp": "2026-11-08T00:00:00Z",
        "timestamp_millis": 1,
        "stack_hash": 0,
        "stack_key": "k",
        "trace": "",
        "session": BugSessionSnapshot::default(),
        "flight_recorder": [],
        "server_logs_before": [],
        "server_logs_after": []
    });
    let parsed_min: BugIncidentBundle =
        serde_json::from_value(min_bundle_json).expect("deserialize bundle with default source");
    assert_eq!(parsed_min.report_source, "exception");

    // 3. Authenticated POST /bugs/api/issue:
    //    (a) invalid Bearer token -> 401
    let mut bad_auth_hdrs = HashMap::new();
    bad_auth_hdrs.insert("Authorization".to_string(), "Bearer invalid.jwt.token".to_string());
    let unauth_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &bad_auth_hdrs,
        br#"{"categoryID":"UI","summary":"Bad auth"}"#,
    );
    assert_eq!(unauth_resp.status_code, 401);

    //    (b) valid account-scoped Bearer token (char_id == 0) with NO online session:
    //        - with Session Info containing owned char_id -> resolves via parse_char_id_from_session_info
    let now = ctx.now_unix();
    let acct_code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            0,
            "evomu-cli",
            "http://localhost/cb",
            vec![],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    let acct_tok = ctx
        .db
        .exchange_pkce_code(
            &acct_code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;
    let mut acct_hdrs = HashMap::new();
    acct_hdrs.insert("Authorization".to_string(), format!("Bearer {acct_tok}"));

    let body_with_sess_info = serde_json::to_vec(&json!({
        "categoryID": "Gameplay",
        "summary": "Owned char from session info",
        "description": format!("Something broke\n\nSession Info:\nCharacter: Aura ({char_id})")
    }))
    .unwrap();
    let resp_b = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &acct_hdrs, &body_with_sess_info);
    assert_eq!(resp_b.status_code, 201);
    let key_b = serde_json::from_slice::<serde_json::Value>(&resp_b.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let path_b = ctx.bug_reporting.lookup_report_path(&key_b).unwrap();
    let bundle_b: BugIncidentBundle =
        serde_json::from_str(&std::fs::read_to_string(&path_b).unwrap()).unwrap();
    assert_eq!(bundle_b.session.character_id, Some(char_id));
    assert_eq!(bundle_b.session.user_id, Some(1));
    assert!(bundle_b.flight_recorder.is_empty());

    //        - without Session Info -> falls back to first character owned by user_id
    let body_no_sess_info = serde_json::to_vec(&json!({
        "categoryID": "Gameplay",
        "summary": "Owned char fallback"
    }))
    .unwrap();
    let resp_c = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &acct_hdrs, &body_no_sess_info);
    assert_eq!(resp_c.status_code, 201);
    let key_c = serde_json::from_slice::<serde_json::Value>(&resp_c.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let path_c = ctx.bug_reporting.lookup_report_path(&key_c).unwrap();
    let bundle_c: BugIncidentBundle =
        serde_json::from_str(&std::fs::read_to_string(&path_c).unwrap()).unwrap();
    assert!(bundle_c.session.character_id.is_some());

    //    (c) valid character-scoped Bearer token WITH online session + FlightRecorder entries
    let _ = ctx.db.acquire_online_lock(1, 55_123, Some(char_id), false, now);
    ctx.flight_recorder.record(
        55_123,
        "marketProxy",
        None,
        "GetOrders",
        &[PyRep::Integer(34)],
        &HashMap::new(),
        Duration::from_micros(250),
        "ok",
    );
    let char_code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/cb",
            vec![],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    let char_tok = ctx
        .db
        .exchange_pkce_code(
            &char_code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;
    let mut char_hdrs = HashMap::new();
    char_hdrs.insert("Authorization".to_string(), format!("Bearer {char_tok}"));
    let resp_d = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &char_hdrs, &body_no_sess_info);
    assert_eq!(resp_d.status_code, 201);
    let key_d = serde_json::from_slice::<serde_json::Value>(&resp_d.body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let path_d = ctx.bug_reporting.lookup_report_path(&key_d).unwrap();
    let bundle_d: BugIncidentBundle =
        serde_json::from_str(&std::fs::read_to_string(&path_d).unwrap()).unwrap();
    assert_eq!(bundle_d.session.session_id, 55_123);
    assert_eq!(bundle_d.flight_recorder.len(), 1);

    // 4. Oversized attachment (> MAX_BUG_ATTACHMENT_BYTES) returns 413
    let huge_attach = vec![b'x'; MAX_BUG_ATTACHMENT_BYTES + 1];
    let resp_413 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{key_d}"),
        &HashMap::new(),
        &huge_attach,
    );
    assert_eq!(resp_413.status_code, 413);

    // 5. Disk limits eviction by byte budget and attachment directory removal
    let evict_dir = make_temp_dir("evict_bytes");
    let rt_evict = BugReportingRuntime::new();
    rt_evict.set_queue_dir_override(Some(evict_dir.clone()));
    rt_evict.set_storage_limits(10, 1024); // byte cap triggers eviction even when count <= 10
    assert_eq!(directory_tree_bytes(&evict_dir.join("does_not_exist")), 0);
    std::fs::write(evict_dir.join("readme.txt"), b"ignore non-json").unwrap();
    std::fs::create_dir_all(evict_dir.join("subdir_ignored")).unwrap();

    let key_old = "EBR-0001-OLDKEY";
    let old_json = evict_dir.join(format!("1000_{key_old}.json"));
    std::fs::write(&old_json, vec![b'a'; 900]).unwrap();
    let old_attach_dir = evict_dir.join("attachments").join(key_old);
    std::fs::create_dir_all(&old_attach_dir).unwrap();
    std::fs::write(old_attach_dir.join("shot.png"), vec![b'b'; 300]).unwrap();
    rt_evict.register_report_path(key_old, old_json.clone());

    let key_new = "EBR-0002-NEWKEY";
    let new_json = evict_dir.join(format!("2000_{key_new}.json"));
    std::fs::write(&new_json, vec![b'c'; 600]).unwrap();
    rt_evict.register_report_path(key_new, new_json.clone());

    // Old report and its attachment directory must have been evicted to stay under 1024 bytes
    assert!(!old_json.exists());
    assert!(!old_attach_dir.exists());
    assert!(new_json.exists());

    // Registering 11 reports with limit=2 triggers memory FIFO eviction + remove_report_files with attachments
    rt_evict.set_storage_limits(2, 64 * 1024);
    let attach_new = evict_dir.join("attachments").join(key_new);
    std::fs::create_dir_all(&attach_new).unwrap();
    std::fs::write(attach_new.join("log.txt"), b"hi").unwrap();
    let p3 = evict_dir.join("3000_0000000000000003_EBR-0003-THREE.json");
    std::fs::write(&p3, b"{}").unwrap();
    rt_evict.register_report_path("EBR-0003-THREE", p3);
    let p4 = evict_dir.join("4000_0000000000000004_EBR-0004-FOUR.json");
    std::fs::write(&p4, b"{}").unwrap();
    rt_evict.register_report_path("EBR-0004-FOUR", p4);
    assert!(!new_json.exists());
    assert!(!attach_new.exists());

    // 6. schedule_incident_capture error branch when queue_dir points to an existing regular file
    let blocking_file = evict_dir.join("not_a_dir");
    std::fs::write(&blocking_file, b"file").unwrap();
    schedule_incident_capture(
        parsed_min,
        Some(blocking_file),
        Some(Duration::from_millis(5)),
    );
    tokio::time::sleep(Duration::from_millis(30)).await;

    // 7. Corrupt ZIP edge cases: bad local header signature, truncated comp_slice, and truncated CD entry
    let mut bad_zip = Vec::new();
    bad_zip.extend_from_slice(&[0u8; 30]); // invalid local header (not 0x504b0304)
    let mut cd = Vec::new();
    // CD entry 1: points to local_hdr_off = 0 (invalid local header sig)
    cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    cd.extend_from_slice(&[0u8; 6]);
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&[0u8; 8]);
    cd.extend_from_slice(&5u32.to_le_bytes());
    cd.extend_from_slice(&5u32.to_le_bytes());
    cd.extend_from_slice(&8u16.to_le_bytes()); // "logs.txt"
    cd.extend_from_slice(&[0u8; 14]);
    cd.extend_from_slice(&0u32.to_le_bytes());
    cd.extend_from_slice(b"logs.txt");
    // CD entry 2: valid local header at offset 0? No, place valid local header with truncated comp_size
    let good_lh_off = bad_zip.len() as u32;
    bad_zip.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
    bad_zip.extend_from_slice(&[0u8; 22]);
    bad_zip.extend_from_slice(&8u16.to_le_bytes());
    bad_zip.extend_from_slice(&0u16.to_le_bytes());
    bad_zip.extend_from_slice(b"logs.txt");
    // Write only 2 bytes instead of 100
    bad_zip.extend_from_slice(b"ab");
    cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    cd.extend_from_slice(&[0u8; 6]);
    cd.extend_from_slice(&0u16.to_le_bytes());
    cd.extend_from_slice(&[0u8; 8]);
    cd.extend_from_slice(&100u32.to_le_bytes()); // comp_size exceeds buffer
    cd.extend_from_slice(&100u32.to_le_bytes());
    cd.extend_from_slice(&8u16.to_le_bytes());
    cd.extend_from_slice(&[0u8; 14]);
    cd.extend_from_slice(&good_lh_off.to_le_bytes());
    cd.extend_from_slice(b"logs.txt");
    // CD entry 3: truncated filename length (claims 500 bytes)
    cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
    cd.extend_from_slice(&[0u8; 24]);
    cd.extend_from_slice(&500u16.to_le_bytes());
    cd.extend_from_slice(&[0u8; 16]);

    let cd_off = bad_zip.len() as u32;
    let cd_len = cd.len() as u32;
    bad_zip.extend_from_slice(&cd);
    bad_zip.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    bad_zip.extend_from_slice(&[0u8; 4]);
    bad_zip.extend_from_slice(&3u16.to_le_bytes());
    bad_zip.extend_from_slice(&3u16.to_le_bytes());
    bad_zip.extend_from_slice(&cd_len.to_le_bytes());
    bad_zip.extend_from_slice(&cd_off.to_le_bytes());
    bad_zip.extend_from_slice(&0u16.to_le_bytes());
    assert!(extract_text_files_from_zip(&bad_zip, &["logs.txt"]).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&evict_dir);
}
