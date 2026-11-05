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

#[tokio::test]
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
