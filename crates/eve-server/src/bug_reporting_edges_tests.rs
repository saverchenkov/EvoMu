use super::*;
use crate::router::ServerContext;
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystem};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_bug_dir(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "evomu_bug_edges_{tag}_{}_{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build_bug_test_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.values().next().unwrap().clone();
    sde.solar_systems.entry(30_000_142).or_insert_with(|| SolarSystem {
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        name: "Jita".into(),
        security_status: 0.9459,
        radius: 1.0e13,
        galactic_pos_ly: [0.0, 0.0, 0.0],
        planet_ids: vec![],
        stations: vec![],
        wormhole_class_id: None,
    });
    let mut t = base_type;
    t.type_id = 587;
    t.group_id = 25;
    t.category_id = 6;
    t.name = "Rifter".into();
    sde.types.insert(587, t);

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

fn pyrep_ascii_str(rep: &PyRep) -> Option<String> {
    match rep {
        PyRep::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        PyRep::String(s) => Some(s.clone()),
        _ => None,
    }
}

#[test]
fn runtime_storage_limits_fifo_eviction_disk_enforcement_and_lookup() {
    let dir = unique_bug_dir("runtime_limits");
    let rt = BugReportingRuntime::new();
    rt.set_queue_dir_override(Some(dir.clone()));
    rt.set_storage_limits(0, 0);
    assert_eq!(rt.max_reports.load(Ordering::Relaxed), 1);
    assert_eq!(rt.max_total_bytes.load(Ordering::Relaxed), 1024);
    assert_eq!(rt.stored_report_count(), 0);

    rt.set_storage_limits(2, 1600);

    let p1 = dir.join("EVOMU-1.json");
    let p2 = dir.join("EVOMU-2.json");
    let p3 = dir.join("EVOMU-3.json");
    std::fs::write(&p1, "{\"k\":1}").unwrap();
    std::fs::write(&p2, "{\"k\":2}").unwrap();
    std::fs::write(&p3, "{\"k\":3}").unwrap();
    let att1 = dir.join("attachments").join("EVOMU-1");
    std::fs::create_dir_all(&att1).unwrap();
    std::fs::write(att1.join("log.txt"), "hello").unwrap();

    rt.register_report_path("EVOMU-1", p1.clone());
    rt.register_report_path("EVOMU-1", p1.clone());
    assert_eq!(rt.stored_report_count(), 1);

    rt.register_report_path("EVOMU-2", p2.clone());
    assert_eq!(rt.stored_report_count(), 2);

    rt.register_report_path("EVOMU-3", p3.clone());
    assert_eq!(rt.stored_report_count(), 2);
    assert!(!p1.exists(), "evicted report json should be removed");
    assert!(!att1.exists(), "evicted report attachments dir should be removed");
    assert!(p2.exists() && p3.exists());

    assert_eq!(rt.lookup_report_path("EVOMU-2"), Some(p2.clone()));

    let untracked = dir.join("EVOMU-DISK_UNTRACKED.json");
    std::fs::write(&untracked, vec![b'x'; 1200]).unwrap();
    std::fs::create_dir_all(dir.join("subdir_ignored")).unwrap();
    std::fs::write(dir.join("ignore.txt"), "not json").unwrap();

    assert_eq!(rt.lookup_report_path("UNTRACKED"), Some(untracked));
    assert!(rt.lookup_report_path("MISSING").is_none());

    rt.set_storage_limits(1, 1024);
    rt.enforce_disk_limits(&dir, 1, 1024);
    let remaining_json = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .count();
    assert!(remaining_json <= 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_client_global_config_handles_ipv4_ipv6_loopback_and_esi_port_branches() {
    let _env_guard = crate::lock_test_env_read();
    let ctx = build_bug_test_ctx();
    ctx.set_esi_port(8443);

    for (hint, expected_url_host, expected_ejab) in [
        ("0.0.0.0", "127.0.0.1", "127.0.0.1"),
        ("::", "127.0.0.1", "127.0.0.1"),
        ("[::]", "127.0.0.1", "127.0.0.1"),
        ("::1", "127.0.0.1", "[::1]"),
        ("[::1]", "127.0.0.1", "[::1]"),
        ("0:0:0:0:0:0:0:1", "127.0.0.1", "[0:0:0:0:0:0:0:1]"),
        ("[0:0:0:0:0:0:0:1]", "127.0.0.1", "[0:0:0:0:0:0:0:1]"),
        ("10.0.0.1", "10.0.0.1", "10.0.0.1"),
        ("2001:db8::1", "[2001:db8::1]", "[2001:db8::1]"),
        ("[2001:db8::1]", "[2001:db8::1]", "[2001:db8::1]"),
    ] {
        let cfg = build_client_global_config(&ctx, Some(hint));
        let url = pyrep_ascii_str(cfg.get("bugReporting_BugReportServer").unwrap()).unwrap();
        assert!(
            url.contains(expected_url_host) && url.contains(":8443"),
            "hint {hint} produced url {url}"
        );
        let ejab = pyrep_ascii_str(cfg.get("ejabberdAddress").unwrap()).unwrap();
        assert_eq!(ejab, expected_ejab);
    }

    ctx.set_esi_port(0);
    let cfg_no_port = build_client_global_config(&ctx, None);
    assert!(cfg_no_port.contains_key("ejabberdAddress"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrich_session_snapshot_covers_alliance_corp_docked_ballpark_and_item_positions() {
    let ctx = build_bug_test_ctx();
    let cid = 93_000_001;
    let corp_id = 98_000_001;
    let alliance_id = 99_000_001;
    let ship_id = 93_100_001;
    let sys_id = 30_000_142;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.user_id = 42;
        ch.name = "Bug Pilot".into();
        ch.solar_system_id = sys_id;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(alliance_id);
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(cid, ch);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Bug Corp".into(); __corp.ticker = "BUGC".into(); __corp.ceo_id = cid; __corp.alliance_id = Some(alliance_id); __corp.hq_station_id = Some(60_003_760); __corp.tax_rate = 0.1; __corp.members = HashSet::from([cid]); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Bug Alliance".into(),
                short_name: "BUGA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..AllianceRecord::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        snap.item_positions.insert(ship_id, [111.0, 222.0, 333.0]);
    });

    let mut sess = BugSessionSnapshot {
        character_id: Some(cid),
        ..Default::default()
    };
    enrich_session_snapshot(&ctx, &mut sess, None);
    assert_eq!(sess.user_id, Some(42));
    assert_eq!(sess.alliance_id, Some(alliance_id));
    assert_eq!(sess.alliance_name.as_deref(), Some("Bug Alliance"));
    assert_eq!(sess.corporation_name.as_deref(), Some("Bug Corp"));
    assert_eq!(sess.solar_system_name.as_deref(), Some("Jita"));
    assert_eq!(sess.ship_type_id, Some(587));
    assert_eq!(sess.ship_type_name.as_deref(), Some("Rifter"));
    let coords = sess.coordinates.as_ref().unwrap();
    assert_eq!([coords.x, coords.y, coords.z], [111.0, 222.0, 333.0]);

    // Ballpark hit overrides coordinates
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(900.0, 800.0, 700.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1.0e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess.coordinates = None;
    enrich_session_snapshot(&ctx, &mut sess, None);
    let coords2 = sess.coordinates.as_ref().unwrap();
    assert_eq!([coords2.x, coords2.y, coords2.z], [900.0, 800.0, 700.0]);

    // Docked -> coordinates stay None; preset user_id/solar_system_id/station_id preserved
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(60_003_760);
    });
    let mut docked_sess = BugSessionSnapshot {
        user_id: Some(99),
        character_id: Some(cid),
        solar_system_id: Some(sys_id),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    enrich_session_snapshot(&ctx, &mut docked_sess, Some(ship_id));
    assert_eq!(docked_sess.user_id, Some(99));
    assert_eq!(docked_sess.station_id, Some(60_003_760));
    assert!(docked_sess.coordinates.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_incident_capture_and_disambiguate_uthread_edges() {
    let dir = unique_bug_dir("sched");
    let ctx = build_bug_test_ctx();
    let sess = SessionState::default();
    let bundle = BugIncidentBundle::build_snapshot(
        &ctx,
        &sess,
        0x1234,
        "server_error",
        "Traceback\nValueError",
    );

    schedule_incident_capture(
        bundle.clone(),
        Some(dir.clone()),
        Some(std::time::Duration::from_millis(10)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let written = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"));
    assert!(written, "background incident capture should write json");

    // Error branch when queue_dir points to a regular file
    let file_as_dir = dir.join("not_a_dir");
    std::fs::write(&file_as_dir, "blocking").unwrap();
    schedule_incident_capture(
        bundle,
        Some(file_as_dir),
        Some(std::time::Duration::from_millis(5)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    // disambiguate_uthread_stack_hash variants
    let base_hash = 0x5555_i64;
    let only_fmt = "Traceback\nFormatted exception info:\nKeyError: 'foo'\nDone";
    let h_fmt = disambiguate_uthread_stack_hash(base_hash, "MollycoddledUthread", only_fmt);
    assert_ne!(h_fmt, base_hash);

    let only_thrown = "Traceback\nThrown at:\n<pre>File foo.py:42</pre>\nEnd";
    let h_thrown = disambiguate_uthread_stack_hash(base_hash, "MollycoddledUthread", only_thrown);
    assert_ne!(h_thrown, base_hash);
    assert_ne!(h_thrown, h_fmt);

    assert_eq!(
        disambiguate_uthread_stack_hash(base_hash, "MollycoddledUthread", "plain trace without markers"),
        base_hash
    );
    assert_eq!(
        disambiguate_uthread_stack_hash(base_hash, "client_log", only_fmt),
        base_hash
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn mint_pkce_bearer(ctx: &ServerContext, username: &str, pw: &str, cid: i64) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            username,
            pw,
            cid,
            "evomu-client",
            "http://localhost/callback",
            vec!["publicData".into()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-client",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token
}

fn build_raw_zip(entries: &[(&str, u16, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, method, data) in entries {
        let offset = out.len() as u32;
        let nb = name.as_bytes();
        out.extend_from_slice(&0x0403_4B50_u32.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(nb);
        out.extend_from_slice(data);

        cd.extend_from_slice(&0x0201_4B50_u32.to_le_bytes());
        cd.extend_from_slice(&20_u16.to_le_bytes());
        cd.extend_from_slice(&20_u16.to_le_bytes());
        cd.extend_from_slice(&0_u16.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&0_u32.to_le_bytes());
        cd.extend_from_slice(&0_u32.to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0_u16.to_le_bytes());
        cd.extend_from_slice(&0_u16.to_le_bytes());
        cd.extend_from_slice(&0_u16.to_le_bytes());
        cd.extend_from_slice(&0_u16.to_le_bytes());
        cd.extend_from_slice(&0_u32.to_le_bytes());
        cd.extend_from_slice(&offset.to_le_bytes());
        cd.extend_from_slice(nb);
    }
    let cd_offset = out.len() as u32;
    let cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4B50_u32.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

fn multipart_body(boundary: &str, filename: &str, content: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    let _ = write!(
        b,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    b.extend_from_slice(content);
    let _ = write!(b, "\r\n--{boundary}--\r\n");
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bugs_http_issue_and_attachment_cover_auth_session_limits_and_zip_edges() {
    let dir = unique_bug_dir("http_edges");
    let ctx = build_bug_test_ctx();
    ctx.bug_reporting.set_queue_dir_override(Some(dir.clone()));

    let uid1 = ctx.db.provision_player_account("bugger1", "pw1");
    let uid2 = ctx.db.provision_player_account("bugger2", "pw2");
    let cid1 = 93_001_001;
    let cid_other = 93_001_002;
    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = snap.characters.values().next().unwrap().clone();
        c1.character_id = cid1;
        c1.user_id = uid1;
        c1.name = "Bugger One".into();
        c1.solar_system_id = 30_000_142;
        snap.characters.insert(cid1, c1);

        let mut c2 = snap.characters.values().next().unwrap().clone();
        c2.character_id = cid_other;
        c2.user_id = uid2;
        c2.name = "Bugger Other".into();
        c2.solar_system_id = 30_000_142;
        snap.characters.insert(cid_other, c2);
    });
    let token_uid1 = mint_pkce_bearer(&ctx, "bugger1", "pw1", cid1);
    let empty_hdrs: HashMap<String, String> = HashMap::new();

    // 1. POST /bugs/api/issue > MAX_BUG_ISSUE_BODY_BYTES -> 413
    let big_body = vec![b'a'; 2 * 1024 * 1024 + 8];
    let resp_413 = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, &big_body);
    assert_eq!(resp_413.status_code, 413);

    // 2. Invalid Bearer token -> 401
    let mut bad_hdrs = HashMap::new();
    bad_hdrs.insert("authorization".into(), "Bearer invalid.jwt.token".into());
    let resp_401 = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &bad_hdrs,
        br#"{"summary":"t","description":"d"}"#,
    );
    assert_eq!(resp_401.status_code, 401);

    // 3. Valid Bearer token, NO online session -> 201
    let mut auth_hdrs = HashMap::new();
    auth_hdrs.insert("authorization".into(), format!("Bearer {token_uid1}"));
    let body_no_sess = serde_json::to_vec(&serde_json::json!({
        "summary": "No online session bug",
        "description": "Details",
        "Build": 2600123,
        "originalReporterId": 555666
    }))
    .unwrap();
    let r_no_sess = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &auth_hdrs, &body_no_sess);
    assert_eq!(r_no_sess.status_code, 201);

    // 4. Online session with character_id: None + spoofed char hint vs owned char
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid1,
        888_001,
        None,
        None,
        true,
        ctx.now_unix(),
    );
    let body_owned = serde_json::to_vec(&serde_json::json!({
        "summary": "Hint owned",
        "description": format!("Desc\n\nSession Info:\nCharacter: Bugger One ({cid1})\n"),
        "Build": "2600124",
        "originalReporterId": "777"
    }))
    .unwrap();
    let r_owned = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &auth_hdrs, &body_owned);
    assert_eq!(r_owned.status_code, 201);
    let owned_json: serde_json::Value = serde_json::from_slice(&r_owned.body).unwrap();
    let issue_id = owned_json["key"].as_str().unwrap().to_string();

    let body_spoof = serde_json::to_vec(&serde_json::json!({
        "summary": "Hint spoofed",
        "description": format!("Desc\n\nSession Info:\nCharacter: Bugger Other ({cid_other})\n")
    }))
    .unwrap();
    let r_spoof = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &auth_hdrs, &body_spoof);
    assert_eq!(r_spoof.status_code, 201);
    let spoof_json: serde_json::Value = serde_json::from_slice(&r_spoof.body).unwrap();
    let spoof_key = spoof_json["key"].as_str().unwrap();
    let saved_spoof = std::fs::read_to_string(
        ctx.bug_reporting.lookup_report_path(spoof_key).unwrap(),
    )
    .unwrap();
    assert!(
        !saved_spoof.contains(&cid_other.to_string()),
        "spoofed character hint must not attribute another user's character"
    );

    // 5. Online session WITH character_id: Some(cid1)
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid1,
        888_002,
        Some(cid1),
        None,
        true,
        ctx.now_unix(),
    );
    let r_online = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &auth_hdrs,
        br#"{"summary":"Online","description":"Ok"}"#,
    );
    assert_eq!(r_online.status_code, 201);

    // 6. Attachment > MAX_BUG_ATTACHMENT_BYTES -> 413
    let big_att = vec![0; 33 * 1024 * 1024];
    let r_att_413 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_id}"),
        &empty_hdrs,
        &big_att,
    );
    assert_eq!(r_att_413.status_code, 413);

    // 7. Attachment igbr.zip & fitting.txt
    let mut mp_hdrs = HashMap::new();
    mp_hdrs.insert("content-type".into(), "multipart/form-data; boundary=BOUND".into());
    let zip_bytes = build_raw_zip(&[
        ("fitting.txt", 0, b"   \n "),
        ("lastcrashes.txt", 0, b"crash line 1\ncrash line 2"),
    ]);
    let r_zip = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_id}"),
        &mp_hdrs,
        &multipart_body("BOUND", "igbr.zip", &zip_bytes),
    );
    assert_eq!(r_zip.status_code, 201);

    let r_fit = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_id}"),
        &mp_hdrs,
        &multipart_body("BOUND", "fitting.txt", b""),
    );
    assert_eq!(r_fit.status_code, 201);

    // 8. ZIP edge cases (bad local header, truncated CD entry, corrupt deflate method=8)
    let corrupt_deflate_zip = build_raw_zip(&[("client.log", 8, b"not-valid-deflate-bytes")]);
    assert!(extract_text_files_from_zip(&corrupt_deflate_zip, &["client.log"]).is_empty());

    let mut bad_local = build_raw_zip(&[("a.txt", 0, b"hi")]);
    bad_local[0..4].copy_from_slice(&[0, 0, 0, 0]);
    assert!(extract_text_files_from_zip(&bad_local, &["a.txt"]).is_empty());

    let trunc_cd = b"PK\x05\x06\x00\x00\x00\x00\x01\x00\x01\x00\x05\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    assert!(extract_text_files_from_zip(trunc_cd, &["a.txt"]).is_empty());

    // 9. fs::write fail -> 500
    let conflict_dir = dir.join("attachments").join(&issue_id).join("conflict.txt");
    std::fs::create_dir_all(&conflict_dir).unwrap();
    let r_500_write = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_id}"),
        &mp_hdrs,
        &multipart_body("BOUND", "conflict.txt", b"data"),
    );
    assert_eq!(r_500_write.status_code, 500);

    // 10. attach_dir creation fail -> 500
    let dir2 = unique_bug_dir("attach_dir_fail");
    std::fs::write(dir2.join("attachments"), "blocking file").unwrap();
    ctx.bug_reporting.set_queue_dir_override(Some(dir2.clone()));
    let r_500_dir = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_id}"),
        &mp_hdrs,
        &multipart_body("BOUND", "log.txt", b"data"),
    );
    assert_eq!(r_500_dir.status_code, 500);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
