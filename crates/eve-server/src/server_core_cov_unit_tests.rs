//! Unit tests boosting branch and line coverage for non-service core modules:
//! `bug_reporting`, `quasar_grpc::normalize_notices`, `system_actor`,
//! `macho_listener`, `sso`, and `rng`.

#![forbid(unsafe_code)]

use crate::bug_reporting::{
    build_client_global_config, extract_text_files_from_zip, handle_bugs_http_request,
    BugIncidentBundle, MAX_BUG_ATTACHMENT_BYTES, MAX_BUG_ISSUE_BODY_BYTES,
};
use crate::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
use crate::quasar_grpc::*;
use crate::router::client_compat::normalize_macho_notification;
use crate::router::{NotificationTarget, ServerContext};
use crate::sso::handle_http_request_with_ctx;
use crate::system_actor::*;
use eve_db::WorldRepository;
use eve_destiny::{Ball, BallId, Ballpark, CombatState, DamageVector, MovementMode};
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
    SessionState, VersionExchange, MACHO_PROXY_NODE_ID,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_varint_field,
    DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| ServerContext::new().sde))
}

fn fresh_ctx() -> Arc<ServerContext> {
    Arc::new(
        ServerContext::with_sde_and_repository(
            shared_sde(),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    )
}

struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mint_access_token(ctx: &ServerContext, char_id: i64) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            vec!["publicData".to_string()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token
}

fn build_stored_zip(entries: &[(&str, &[u8], bool, bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    let mut count = 0u16;
    for &(name, data, deflate, corrupt_off) in entries {
        let name_b = name.as_bytes();
        let (method, comp_data) = if deflate {
            (8u16, miniz_oxide::deflate::compress_to_vec(data, 6))
        } else {
            (0u16, data.to_vec())
        };
        let local_off = if corrupt_off {
            out.len().saturating_add(99_999) as u32
        } else {
            out.len() as u32
        };
        if !corrupt_off {
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(comp_data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_b);
            out.extend_from_slice(&comp_data);
        }

        cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&(comp_data.len() as u32).to_le_bytes());
        let reported_uncomp = if name == "huge.txt" {
            8 * 1024 * 1024
        } else {
            data.len() as u32
        };
        cd.extend_from_slice(&reported_uncomp.to_le_bytes());
        cd.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&local_off.to_le_bytes());
        cd.extend_from_slice(name_b);
        count = count.saturating_add(1);
    }
    let cd_offset = out.len() as u32;
    let cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bug_reporting_authenticated_submission_attachments_zip_and_eviction() {
    let ctx = fresh_ctx();
    let temp_dir = TempDirGuard(std::env::temp_dir().join(format!("evomu_cov_br_{}", Uuid::new_v4())));
    std::fs::create_dir_all(&temp_dir.0).expect("mkdir");
    ctx.bug_reporting.set_queue_dir_override(Some(temp_dir.0.clone()));
    ctx.bug_reporting.set_storage_limits(10, 512);

    {
        let mut gcv = ctx.global_config_values.write().unwrap();
        gcv.insert("customFlag".to_string(), PyRep::ascii_bytes("enabled"));
    }
    let gc_with_hint = build_client_global_config(&ctx, Some("127.0.0.1:26000"));
    assert_eq!(
        gc_with_hint.get("customFlag"),
        Some(&PyRep::ascii_bytes("enabled"))
    );
    let gc_no_hint = build_client_global_config(&ctx, None);
    assert!(!gc_no_hint.is_empty());

    let (uid, cid, ship_id, corp_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_001).expect("char 90_000_001");
        (ch.user_id, ch.character_id, ch.active_ship_id, ch.corporation_id)
    });
    let alliance_id = 99_000_055_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "BugCov Alliance".to_string(),
                short_name: "BCA".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.alliance_id = Some(alliance_id);
        }
        snap.item_positions.insert(ship_id, [10_000.0, 20_000.0, -5_000.0]);
    });
    let _ = ctx.db.acquire_online_lock(uid, 91_001, Some(cid), false, ctx.now_unix());
    ctx.flight_recorder.record(
        91_001,
        "beyonce",
        None,
        "CmdStopShip",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
        std::time::Duration::from_millis(3),
        "Ok",
    );
    let token = mint_access_token(&ctx, cid);

    let mut bad_auth_hdrs = HashMap::new();
    bad_auth_hdrs.insert("authorization".to_string(), "Bearer invalid-jwt-token".to_string());
    let bad_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &bad_auth_hdrs,
        br#"{"categoryID":"Gameplay","summary":"bad auth","description":"desc"}"#,
    );
    assert_eq!(bad_resp.status_code, 401);

    let mut auth_hdrs = HashMap::new();
    auth_hdrs.insert("authorization".to_string(), format!("Bearer {token}"));
    let stack = "MollycoddledUthread\nThrown at:\n<pre>frame1</pre>\n<pre>leaf_fn</pre>\nFormatted exception info: RuntimeError: test";
    let issue_payload = json!({
        "categoryID": "Combat",
        "summary": "Auth bug submission",
        "description": format!("Details\n\nSession Info:\nCharacter: Test Pilot ({cid})"),
        "stackTrace": stack,
        "clientVersion": "24.01",
    });
    let ok_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &auth_hdrs,
        issue_payload.to_string().as_bytes(),
    );
    assert_eq!(ok_resp.status_code, 201);
    let created: serde_json::Value = serde_json::from_slice(&ok_resp.body).unwrap();
    let issue_key = created["key"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_millis(1650)).await;

    let cat_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/categories", &HashMap::new(), &[]);
    assert_eq!(cat_resp.status_code, 200);
    let status_resp = handle_bugs_http_request(&ctx, "GET", "/bugs/api/status", &HashMap::new(), &[]);
    assert_eq!(status_resp.status_code, 200);

    let oversized = vec![b'x'; MAX_BUG_ATTACHMENT_BYTES + 1];
    let r_413 = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_key}"),
        &auth_hdrs,
        &oversized,
    );
    assert_eq!(r_413.status_code, 413);

    let r_413_issue = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &HashMap::new(),
        &vec![b'y'; MAX_BUG_ISSUE_BODY_BYTES + 1],
    );
    assert_eq!(r_413_issue.status_code, 413);

    let r_bad_key = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/attachment/..bad",
        &auth_hdrs,
        b"hi",
    );
    assert_eq!(r_bad_key.status_code, 400);

    let boundary = "BOUND123";
    for (fname, content) in [
        ("logs.txt", b"line1\nline2\n".as_slice()),
        ("fitting.txt", b"[Rifter, TestFit]\n200mm AutoCannon II\n".as_slice()),
    ] {
        let mut mp = Vec::new();
        mp.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{fname}\"\r\n\r\n").as_bytes());
        mp.extend_from_slice(content);
        mp.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut hdrs = auth_hdrs.clone();
        hdrs.insert("content-type".to_string(), format!("multipart/form-data; boundary={boundary}"));
        let r_att = handle_bugs_http_request(
            &ctx,
            "POST",
            &format!("/bugs/api/attachment/{issue_key}"),
            &hdrs,
            &mp,
        );
        assert_eq!(r_att.status_code, 201);
    }

    let zip_bytes = build_stored_zip(&[
        ("subdir/fitting.txt", b"[Punisher, ZipFit]", true, false),
        ("logs.txt", b"zip log line", false, false),
        ("huge.txt", b"small", false, false),
        ("corrupt.txt", b"bad", false, true),
    ]);
    let mut mp_zip = Vec::new();
    mp_zip.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"igbr.zip\"\r\n\r\n").as_bytes());
    mp_zip.extend_from_slice(&zip_bytes);
    mp_zip.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut zip_hdrs = auth_hdrs.clone();
    zip_hdrs.insert("content-type".to_string(), format!("multipart/form-data; boundary={boundary}"));
    let r_zip = handle_bugs_http_request(
        &ctx,
        "POST",
        &format!("/bugs/api/attachment/{issue_key}"),
        &zip_hdrs,
        &mp_zip,
    );
    assert_eq!(r_zip.status_code, 201);

    assert!(extract_text_files_from_zip(&[0u8; 10], &["logs.txt"]).is_empty());
    assert!(extract_text_files_from_zip(&[0u8; 40], &["logs.txt"]).is_empty());

    let r2 = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &auth_hdrs,
        br#"{"categoryID":"UI","summary":"Second report triggers eviction","description":"Second"}"#,
    );
    assert_eq!(r2.status_code, 201);

    let sess = SessionState {
        user_id: Some(uid),
        character_id: Some(cid),
        corporation_id: Some(corp_id),
        alliance_id: Some(alliance_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    let snap_bundle = BugIncidentBundle::build_snapshot(
        &ctx,
        &sess,
        42,
        "manual-snap",
        "trace",
    );
    assert_eq!(snap_bundle.stack_key, "manual-snap");
    assert_eq!(snap_bundle.session.character_id, Some(cid));
}

fn make_field(name: &str, number: u32, label: FieldLabel, field_type: FieldType, type_name: &str) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: name.to_string(),
        number,
        label,
        field_type,
        type_name: type_name.to_string(),
        oneof_index: None,
        json_name: name.to_string(),
    }
}

#[test]
fn normalize_notices_covers_bookmarks_bean_item_change_and_any_coercion() {
    let bm = normalize_macho_notification(
        "OnSharedBookmarksFolderUpdated",
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(101),
            PyRep::Integer(202),
            PyRep::List(vec![
                PyRep::Integer(7),
                PyRep::keyval([("bookmarkID", PyRep::Integer(8))]),
            ]),
        ])])],
    );
    assert_eq!(bm.len(), 1);
    let PyRep::List(folders) = &bm[0] else { panic!("expected list") };
    assert!(matches!(&folders[0], PyRep::Tuple(_)));

    assert_eq!(
        normalize_macho_notification("OnBeanPrime", Vec::new()),
        vec![PyRep::List(Vec::new())]
    );
    let bp_single_list = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::List(vec![PyRep::Integer(5)])],
    );
    assert_eq!(bp_single_list.len(), 1);
    let bp_int = normalize_macho_notification("OnBeanPrime", vec![PyRep::Integer(42)]);
    let PyRep::List(bp_items) = &bp_int[0] else { panic!("list") };
    assert_eq!(bp_items[0].dict_get("errorID"), Some(&PyRep::Integer(42)));
    let bp_dict = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::Dict(vec![(PyRep::String("k".into()), PyRep::Integer(1))])],
    );
    assert_eq!(bp_dict.len(), 1);
    let bp_kv = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::keyval([("custom", PyRep::Integer(9))])],
    );
    assert_eq!(bp_kv.len(), 1);
    let bp_str = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::String("raw".into()), PyRep::Integer(2)],
    );
    assert_eq!(bp_str.len(), 1);

    assert_eq!(normalize_macho_notification("OnOrganizationContactsUpdated", vec![PyRep::Integer(1)]).len(), 2);
    assert!(normalize_macho_notification("OnGodmaShipEffect", Vec::new()).is_empty());

    let mac = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1); 8]),
                PyRep::Tuple(vec![PyRep::Integer(2); 7]),
            ]),
            PyRep::Integer(999),
        ],
    );
    assert!(!mac.is_empty());

    let ic1 = normalize_macho_notification(
        "OnItemChange",
        vec![PyRep::Dict(vec![(PyRep::String("a".into()), PyRep::Integer(1))])],
    );
    assert_eq!(ic1.len(), 1);
    let ic2 = normalize_macho_notification(
        "OnItemChange",
        vec![
            PyRep::keyval([("itemID", PyRep::Integer(5))]),
            PyRep::Dict(vec![(PyRep::Integer(10), PyRep::Integer(20))]),
        ],
    );
    assert_eq!(ic2.len(), 3);

    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    let leaf_desc = DescriptorProto {
        name: "CovLeaf".to_string(),
        full_name: ".CovLeaf".to_string(),
        fields: vec![
            make_field("id", 1, FieldLabel::Optional, FieldType::Int64, ""),
            make_field("label", 2, FieldLabel::Optional, FieldType::String, ""),
        ],
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };
    let msg_desc = DescriptorProto {
        name: "CovTestMsg".to_string(),
        full_name: ".CovTestMsg".to_string(),
        fields: vec![
            make_field("b", 1, FieldLabel::Optional, FieldType::Bool, ""),
            make_field("i32", 2, FieldLabel::Optional, FieldType::Int32, ""),
            make_field("i64", 3, FieldLabel::Optional, FieldType::Int64, ""),
            make_field("u32", 4, FieldLabel::Optional, FieldType::Uint32, ""),
            make_field("u64", 5, FieldLabel::Optional, FieldType::Uint64, ""),
            make_field("si32", 6, FieldLabel::Optional, FieldType::Sint32, ""),
            make_field("si64", 7, FieldLabel::Optional, FieldType::Sint64, ""),
            make_field("flt", 8, FieldLabel::Optional, FieldType::Float, ""),
            make_field("dbl", 9, FieldLabel::Optional, FieldType::Double, ""),
            make_field("str_f", 10, FieldLabel::Optional, FieldType::String, ""),
            make_field("bytes_f", 11, FieldLabel::Optional, FieldType::Bytes, ""),
            make_field("nested", 12, FieldLabel::Optional, FieldType::Message, ".CovLeaf"),
            make_field("rep", 13, FieldLabel::Repeated, FieldType::Message, ".CovLeaf"),
            make_field("f32", 14, FieldLabel::Optional, FieldType::Fixed32, ""),
            make_field("f64", 15, FieldLabel::Optional, FieldType::Fixed64, ""),
        ],
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };
    reg.messages_by_full_name.insert(".CovLeaf".to_string(), leaf_desc.clone());
    reg.messages_by_full_name.insert("CovLeaf".to_string(), leaf_desc);
    reg.messages_by_full_name.insert(".CovTestMsg".to_string(), msg_desc.clone());
    reg.messages_by_full_name.insert("CovTestMsg".to_string(), msg_desc);

    let mut bad_wire = Vec::new();
    encode_bytes_field(1, b"1", &mut bad_wire);
    encode_bytes_field(2, b"-5", &mut bad_wire);
    encode_bytes_field(3, b"123", &mut bad_wire);
    encode_bytes_field(4, b"44", &mut bad_wire);
    encode_bytes_field(5, b"55", &mut bad_wire);
    encode_bytes_field(6, b"-7", &mut bad_wire);
    encode_bytes_field(7, b"-8", &mut bad_wire);
    encode_bytes_field(8, b"1.5", &mut bad_wire);
    encode_bytes_field(9, b"2.5", &mut bad_wire);
    encode_varint_field(10, 42, &mut bad_wire);
    encode_varint_field(11, 99, &mut bad_wire);
    encode_varint_field(12, 777, &mut bad_wire);
    encode_varint_field(13, 888, &mut bad_wire);
    encode_bytes_field(14, b"12", &mut bad_wire);
    encode_bytes_field(15, b"34", &mut bad_wire);
    encode_fixed64_field(10, 0x4000_0000_0000_0000, &mut bad_wire);
    encode_fixed32_field(11, 0x4040_0000, &mut bad_wire);

    let mut any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}CovTestMsg"),
        value: bad_wire,
    };
    normalize_quasar_any_payload(&reg, &mut any);
    assert!(!any.value.is_empty());

    let mut leaf_bytes_wire = Vec::new();
    encode_bytes_field(12, b"alpha", &mut leaf_bytes_wire);
    let mut any_leaf_bytes = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}CovTestMsg"),
        value: leaf_bytes_wire,
    };
    normalize_quasar_any_payload(&reg, &mut any_leaf_bytes);
    assert!(!any_leaf_bytes.value.is_empty());
}

fn make_ball(id: BallId, type_id: u32, position: DVec3, radius: f64, max_velocity: f64) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_cycles_tackle_celestials_and_disruptors_comprehensive() {
    let ctx = fresh_ctx();
    let sys_id: SolarSystemId = 30_000_142;

    let lock_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lock_hits_c = Arc::clone(&lock_hits);
    ctx.systems.set_target_lock_completed_hook(Arc::new(move |_cid, _tid| {
        lock_hits_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    let death_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let death_hits_c = Arc::clone(&death_hits);
    ctx.systems.set_npc_death_hook(Arc::new(move |_sid, _bid, _att| {
        let c = Arc::clone(&death_hits_c);
        Box::pin(async move {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }));
    let step_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let step_hits_c = Arc::clone(&step_hits);
    ctx.systems.set_system_step_hook(Arc::new(move |_sid, _dt, _dead| {
        let c = Arc::clone(&step_hits_c);
        Box::pin(async move {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }));
    let deact_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deact_hits_c = Arc::clone(&deact_hits);
    ctx.systems.set_module_deactivation_hook(Arc::new(move |_cid, _c| {
        deact_hits_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));

    let handle = ctx.systems.get_or_spawn(sys_id);

    let ship_a: BallId = 900_001;
    let ship_b: BallId = 900_002;
    let wreck_id: BallId = 900_003;
    let mod_rah: i64 = 800_001;
    let mod_adc: i64 = 800_002;
    let mod_ehe: i64 = 800_003;
    let mod_hic: i64 = 800_004;
    let mod_grap: i64 = 800_005;
    let mod_mjd: i64 = 800_006;
    let mod_mjfg: i64 = 800_007;
    let mod_auto: i64 = 800_008;
    let mod_capb: i64 = 800_009;
    let mod_salv: i64 = 800_010;
    let mod_lance: i64 = 800_011;
    let mod_rep: i64 = 800_012;
    let mod_neut: i64 = 800_013;

    assert!((handle.set_tidi_factor_raw(f64::NAN) - 1.0).abs() < 1e-6);
    assert!((handle.set_tidi_factor_raw(0.01) - 1.0).abs() < 1e-6);
    assert!((handle.tidi_factor() - 0.10).abs() < 1e-6);
    assert!((handle.set_tidi_factor_raw(2.0) - 0.10).abs() < 1e-6);
    assert!((handle.tidi_factor() - 1.0).abs() < 1e-6);

    let mut cs_a = CombatState {
        shield_hp: 400.0,
        max_shield_hp: 1000.0,
        armor_hp: 400.0,
        max_armor_hp: 1000.0,
        structure_hp: 400.0,
        max_structure_hp: 1000.0,
        capacitor: 500.0,
        max_capacitor: 1000.0,
        signature_radius: 120.0,
        ..Default::default()
    };
    cs_a.shield_resonance = [0.5; 4];
    cs_a.armor_resonance = [0.6; 4];
    cs_a.structure_resonance = [0.7; 4];

    let cs_b = CombatState {
        shield_hp: 100.0,
        max_shield_hp: 500.0,
        armor_hp: 100.0,
        max_armor_hp: 500.0,
        structure_hp: 50.0,
        max_structure_hp: 200.0,
        capacitor: 200.0,
        max_capacitor: 500.0,
        signature_radius: 80.0,
        ..Default::default()
    };
    let b_a = make_ball(ship_a, 671, DVec3::new(1000.0, 0.0, 0.0), 40.0, 300.0);
    let b_b = make_ball(ship_b, 587, DVec3::new(3500.0, 0.0, 0.0), 35.0, 400.0);
    let b_w = make_ball(wreck_id, 26557, DVec3::new(2000.0, 0.0, 0.0), 50.0, 0.0);

    handle.spawn_ball_with_combat(b_a, cs_a).await.unwrap();
    handle.spawn_ball_with_combat(b_b, cs_b.clone()).await.unwrap();
    handle
        .spawn_ball_with_combat(
            b_w,
            CombatState {
                structure_hp: 10.0,
                max_structure_hp: 10.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(!handle.is_ball_warping_sync(ship_a));
    assert!(handle.committed_warp_destination_sync(ship_a).is_none());
    assert!(!handle.is_ball_aligning_warp_sync(ship_a));
    assert!(!ctx.systems.is_ball_in_committed_warp_sync(sys_id, ship_a));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            810_001,
            eve_db::InventoryItemRecord {
                item_id: 810_001,
                type_id: 28_668,
                owner_id: 90_000_001,
                location_id: ship_a,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                dynamic_attributes: HashMap::new(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            810_002,
            eve_db::InventoryItemRecord {
                item_id: 810_002,
                type_id: 11_289,
                owner_id: 90_000_001,
                location_id: ship_a,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: false,
                dynamic_attributes: HashMap::new(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            mod_rep,
            eve_db::InventoryItemRecord {
                item_id: mod_rep,
                type_id: 3170,
                owner_id: 90_000_001,
                location_id: ship_a,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: [(eve_sde::attr_ids::DAMAGE, 5.0)].into_iter().collect(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_active_charges.insert(mod_capb, 11_289);
    });

    let mut bp = Ballpark::new(sys_id);
    let ball_a = make_ball(ship_a, 671, DVec3::new(0.0, 0.0, 0.0), 40.0, 300.0);
    let ball_b = make_ball(ship_b, 587, DVec3::new(1500.0, 0.0, 0.0), 35.0, 400.0);
    let mut cycles = Vec::new();
    let mut base_scram = HashMap::new();
    let mut base_core = HashMap::new();
    let mut last_totals = HashMap::new();
    let sde_opt = Some(shared_sde());
    let db_opt = Some(Arc::clone(&ctx.db));

    spawn_ball_with_combat_in_actor(
        &mut bp, &mut cycles, &mut base_scram, &mut base_core, &sde_opt, &db_opt, None,
        ball_a,
        CombatState {
            shield_hp: 200.0, max_shield_hp: 1000.0,
            armor_hp: 200.0, max_armor_hp: 1000.0,
            structure_hp: 200.0, max_structure_hp: 1000.0,
            capacitor: 900.0, max_capacitor: 1000.0,
            signature_radius: 100.0,
            ..Default::default()
        },
        None,
    );
    spawn_ball_with_combat_in_actor(
        &mut bp, &mut cycles, &mut base_scram, &mut base_core, &sde_opt, &db_opt, None,
        ball_b,
        CombatState {
            shield_hp: 100.0, max_shield_hp: 500.0,
            armor_hp: 100.0, max_armor_hp: 500.0,
            structure_hp: 100.0, max_structure_hp: 500.0,
            capacitor: 300.0, max_capacitor: 500.0,
            signature_radius: 50.0,
            ..Default::default()
        },
        None,
    );

    let mut modified_cs = cs_b.clone();
    modified_cs.warp_scramble_strength = 3;
    modified_cs.warp_core_strength = 2;
    set_combat_state_in_actor(
        &mut bp, &mut cycles, &mut base_scram, &mut base_core, &mut last_totals, &db_opt, None,
        ship_b, modified_cs,
    );

    for cyc in [
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_rah, module_type_id: 4405, owner_char_id: Some(90_000_001),
            effect_name: "reactiveArmorHardener".into(), duration_s: 1.0, is_rah: true, rah_shift_amount: 0.06,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_adc, module_type_id: 2048, owner_char_id: Some(90_000_001),
            effect_name: "assaultDamageControl".into(), duration_s: 2.0, is_adc: true,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_hic, module_type_id: 12076, owner_char_id: Some(90_000_001),
            effect_name: "warpDisruptSphere".into(), duration_s: 2.0,
            hic_bubble_radius_m: Some(20_000.0), hic_velocity_multiplier: Some(0.5),
            hic_mass_addition: Some(500_000.0), hic_sig_multiplier: Some(1.5),
            grants_interdiction_nullification: true,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_grap, module_type_id: 41218, owner_char_id: Some(90_000_001),
            effect_name: "stasisWeb".into(), target_id: Some(ship_b), duration_s: 2.0,
            target_velocity_multiplier: Some(0.2), grappler_optimal_m: 1_000.0, grappler_falloff_m: 8_000.0, grappler_speed_factor_pct: -80.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_mjd, module_type_id: 4383, owner_char_id: Some(90_000_001),
            effect_name: "microJumpDrive".into(), duration_s: 1.0, mjd_jump_distance_m: Some(100_000.0),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_mjfg, module_type_id: 37453, owner_char_id: Some(90_000_001),
            effect_name: "microJumpPortalDrive".into(), duration_s: 1.0, mjd_jump_distance_m: Some(100_000.0),
            mjfg_field_params: Some((6_000.0, 25, 0)),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_auto, module_type_id: 2185, owner_char_id: Some(90_000_001),
            effect_name: "autoTargeting".into(), duration_s: 1.0, is_auto_targeter: true,
            auto_target_max_range: 50_000.0, auto_target_max_locks: 4, auto_target_scan_res: 200.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_capb, module_type_id: 3568, owner_char_id: Some(90_000_001),
            effect_name: "capacitorBooster".into(), duration_s: 1.0, repeat: 1, cap_booster_inject_amount: 100.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_rep, module_type_id: 3170, owner_char_id: Some(90_000_001),
            effect_name: "armorRepair".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            armor_repair_amount: 50.0, hull_repair_amount: 50.0, shield_boost_amount: 50.0,
            remote_shield_amount: 25.0, remote_cap_amount: 25.0, cap_drain_target: 10.0,
            remote_optimal: 5_000.0, remote_falloff: 5_000.0, cap_need_per_cycle: 10.0,
            pending_overloaded: Some(true),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_salv, module_type_id: 25861, owner_char_id: Some(90_000_001),
            effect_name: "salvaging".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            cap_need_per_cycle: 5.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_lance, module_type_id: 40631, owner_char_id: Some(90_000_001),
            effect_name: "superWeaponLanceWarmup".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            lance_beam_dir: Some(DVec3::X),
            ..Default::default()
        },
    ] {
        handle_add_module_cycle(&mut bp, &mut cycles, &base_scram, &base_core, &sde_opt, &db_opt, None, cyc).unwrap();
    }

    handle_add_module_cycle(
        &mut bp, &mut cycles, &base_scram, &base_core, &sde_opt, &db_opt, None,
        ActiveModuleCycle {
            ship_id: ship_b, module_id: mod_ehe, module_type_id: 47257, owner_char_id: Some(90_000_002),
            effect_name: "emergencyHullEnergizer".into(), duration_s: 1.0,
            ehe_hull_resists: Some([0.05; 4]),
            ..Default::default()
        },
    ).unwrap();

    let _ = handle.max_active_module_cycle_expiry_unix_sync(ship_a, 0, ctx.now_unix());
    let _ = ctx.systems.max_active_module_cycle_expiry_unix_sync(sys_id, ship_a, 0, ctx.now_unix());
    assert!(!handle.has_active_mjd_or_mjfg_spool_sync(ship_a));
    assert!(!handle.is_module_mjd_or_mjfg_spooling_sync(ship_a, mod_mjd));

    let mut pending_disruptors = vec![PendingAnchoringDisruptor {
        id: 950_001, type_id: 12198, position: DVec3::new(500.0, 0.0, 0.0), radius: 20_000.0, remaining_s: 0.5,
    }];
    let mut nanites = vec![ActiveNaniteRepair {
        char_id: 90_000_001, ship_id: ship_a, module_id: mod_neut,
        initial_damage_hp: 10.0, paste_total_needed: 10, paste_consumed_so_far: 0, duration_s: 2.0, elapsed_s: 0.0,
    }];
    let mut rack_heat = HashMap::new();
    let mut rah_inc = HashMap::from([(ship_a, [10.0, 20.0, 30.0, 40.0])]);
    let mut pending_actions = Vec::new();
    let _ = step_cycles_and_capacitor(
        &mut bp, &mut cycles, &mut pending_disruptors, &mut nanites,
        &mut rack_heat, &mut rah_inc, &base_scram, &base_core,
        &sde_opt, &db_opt, &Some(Arc::clone(&ctx.mechanic_rng)), &ctx.systems.tactical_state,
        &mut pending_actions, 1.0, None,
    );
    assert!(!pending_actions.is_empty());

    let _ = step_cycles_and_capacitor(
        &mut bp, &mut cycles, &mut pending_disruptors, &mut nanites,
        &mut rack_heat, &mut rah_inc, &base_scram, &base_core,
        &sde_opt, &db_opt, &Some(Arc::clone(&ctx.mechanic_rng)), &ctx.systems.tactical_state,
        &mut pending_actions, 1.0, None,
    );

    for c in cycles {
        cleanup_removed_module_cycle(&mut bp, &c, &db_opt);
    }

    try_remove_ball_for_db(&ctx.db, sys_id, wreck_id);
    assert!(handle.force_stop_warp(ship_a).is_ok());
    assert_eq!(handle.respawn_depleted_belts_on_downtime(), 0);
    assert!(handle.active_entity_and_cycle_load() >= 2);
    handle.try_step_manual(0.1);
    let _ = handle.step_manual(0.1).await.unwrap();
    assert!(step_hits.load(std::sync::atomic::Ordering::Relaxed) >= 1);

    let _ = handle
        .apply_damage_with_attacker(wreck_id, DamageVector::new(1000.0, 0.0, 0.0, 0.0), Some(90_000_001))
        .await
        .unwrap();

    handle.cancel_ship_nanite_repairs_sync(ship_a, true);
    assert!(!handle.has_active_nanite_repair_sync(ship_a, mod_neut));
    assert!(!handle.has_active_module_cycle_sync(ship_a, mod_rep));

    handle.schedule_mobile_warp_disruptor(950_002, 12198, DVec3::ZERO, 20_000.0, 60.0).await.unwrap();
    let ecm_hits = handle.apply_ecm_burst(ship_a, 10_000.0, [5.0; 4], 10.0, 0.5).await.unwrap();
    assert!(ecm_hits.len() <= 10);
    handle.try_remove_module_cycle(ship_a, mod_rep);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macho_listener_resolve_req_errors_session_change_and_sso_proxy() {
    let ctx = fresh_ctx();
    let token = mint_access_token(&ctx, 90_000_001);

    let mut auth_hdr = HashMap::new();
    auth_hdr.insert("authorization".to_string(), format!("Bearer {token}"));
    let esi_proxy = handle_http_request_with_ctx(
        &ctx,
        "GET",
        "/latest/status/",
        &auth_hdr,
        &[],
    );
    assert_eq!(esi_proxy.status_code, 200);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = Some(ctx.now_unix() + 3600);
            acc.ban_reason = Some("test ban".to_string());
        }
    });
    let mut form_hdr = HashMap::new();
    form_hdr.insert("content-type".to_string(), "application/x-www-form-urlencoded".to_string());
    let banned_resp = handle_http_request_with_ctx(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &form_hdr,
        b"username=demo&password=demo-secret&client_id=evomu-cli&redirect_uri=http%3A%2F%2Flocalhost%2Fcallback&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256",
    );
    assert_eq!(banned_resp.status_code, 403);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = Some(ctx.now_unix() + 3600);
            acc.ban_reason = Some("SUSPENDED:test susp".to_string());
        }
    });
    let susp_resp = handle_http_request_with_ctx(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &form_hdr,
        b"username=demo&password=demo-secret&client_id=evomu-cli&redirect_uri=http%3A%2F%2Flocalhost%2Fcallback&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256",
    );
    assert_eq!(susp_resp.status_code, 403);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = None;
            acc.ban_reason = None;
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_macho_client(stream, ctx_srv).await;
        }
    });

    let mut s = TcpStream::connect(addr).await.unwrap();
    let srv_vx = VersionExchange::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert!(srv_vx.macho_version > 0);
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(), Some("OK CC"));

    let mut chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    chal.user_language_id = "EN".to_string();
    write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap().as_i64(), Some(2));
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    write_framed_pyrep(
        &mut s,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        }).to_pyrep(),
    ).await.unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let init_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else { panic!("expected client addr") };
    let _sess_chg = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();

    let ping_pkt = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client { client_id, call_id: 0, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 },
        user_id: Some(ack.user_id),
        payload: PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::String("client".into())])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &ping_pkt.to_pyrep()).await.unwrap();
    let ping_rsp = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert!(MachoPacket::from_pyrep(&ping_rsp).is_some());

    let resolve_pkt = MachoPacket {
        packet_type: MachoPacketType::ResolveReq,
        source: MachoAddress::Client { client_id, call_id: 0, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("machoNet".into()), call_id: 0 },
        user_id: Some(ack.user_id),
        payload: PyRep::String("beyonce".into()),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &resolve_pkt.to_pyrep()).await.unwrap();
    let resolve_rsp = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(resolve_rsp.packet_type, MachoPacketType::ResolveRsp);

    let call_unknown = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client { client_id, call_id: 10, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("unknown_service_cov".into()), call_id: 0 },
        user_id: Some(ack.user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("missing_method"),
                PyRep::Tuple(Vec::new()),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &call_unknown.to_pyrep()).await.unwrap();
    let err_rsp = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(err_rsp.packet_type, MachoPacketType::ErrorResponse);

    // Select character so character-targeted notifications & session change notifications deliver
    let select_pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client { client_id, call_id: 11, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("charUnboundMgr".into()), call_id: 0 },
        user_id: Some(ack.user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("SelectCharacterID"),
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &select_pkt.to_pyrep()).await.unwrap();
    loop {
        let pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            break;
        }
    }

    ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnSessionChanged",
        "charid",
        vec![PyRep::Integer(123)],
    );
    let _notice = read_framed_pyrep(&mut s).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_async_handle_commands_cycles_tackle_disruptors_and_celestials() {
    let ctx = fresh_ctx();
    let sys_id: SolarSystemId = 30_000_142;
    let ship_a: BallId = 1_066_000_001;
    let ship_b: BallId = 1_066_000_002;
    let ship_c: BallId = 1_066_000_003;
    let ship_d: BallId = 1_066_000_004;
    let non_ship: BallId = 1_066_000_005;

    let lock_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lock_hits_c = Arc::clone(&lock_hits);
    ctx.systems.set_target_lock_completed_hook(Arc::new(move |_cid, _tid| {
        lock_hits_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    ctx.systems.set_npc_death_hook(Arc::new(|_sid, _bid, _att| Box::pin(async {})));
    ctx.systems.set_system_step_hook(Arc::new(|_sid, _dt, _dead| Box::pin(async {})));
    ctx.systems.set_module_deactivation_hook(Arc::new(|_ship_id, _mod_id| {}));

    let mod_turret = 1_066_001_001_i64;
    let mod_turret2 = 1_066_001_002_i64;
    let mod_crys_stack = 1_066_001_003_i64;
    let mod_crys_dmg = 1_066_001_004_i64;
    let mod_smartbomb = 1_066_001_005_i64;
    let mod_cap_rep1 = 1_066_001_006_i64;
    let mod_cap_rep2 = 1_066_001_007_i64;
    let mod_grap = 1_066_001_008_i64;
    let mod_asb = 1_066_001_009_i64;
    let mod_aar = 1_066_001_010_i64;
    let mod_raar = 1_066_001_011_i64;
    let mod_cyno = 1_066_001_012_i64;
    let mod_lance = 1_066_001_013_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = ship_a;
        }
        for (item_id, type_id, flag, qty, dmg) in [
            (mod_turret, 484, eve_db::InventoryFlag::HiSlot0, 1, 0.0),
            (mod_turret2, 484, eve_db::InventoryFlag::HiSlot1, 1, 0.0),
            (mod_crys_stack, 12_814, eve_db::InventoryFlag::HiSlot0, 3, 0.5),
            (mod_crys_dmg, 12_814, eve_db::InventoryFlag::HiSlot1, 1, 0.2),
            (mod_smartbomb, 1547, eve_db::InventoryFlag::HiSlot2, 1, 0.0),
            (mod_cap_rep1, 3616, eve_db::InventoryFlag::HiSlot3, 1, 0.0),
            (mod_cap_rep2, 3616, eve_db::InventoryFlag::HiSlot4, 1, 0.0),
            (mod_grap, 41_040, eve_db::InventoryFlag::MedSlot0, 1, 0.0),
            (mod_asb, 32_772, eve_db::InventoryFlag::MedSlot1, 1, 0.0),
            (mod_aar, 33_076, eve_db::InventoryFlag::LoSlot0, 1, 0.0),
            (mod_raar, 41_476, eve_db::InventoryFlag::HiSlot5, 1, 0.0),
            (1_066_002_001, 28_668, eve_db::InventoryFlag::LoSlot0, 2, 0.0),
        ] {
            let mut dyn_attrs = HashMap::new();
            if dmg > 0.0 {
                dyn_attrs.insert(eve_sde::attr_ids::DAMAGE, dmg);
            }
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: 90_000_001,
                    location_id: ship_a,
                    flag,
                    quantity: qty,
                    singleton: qty == 1 && type_id != 28_668,
                    dynamic_attributes: dyn_attrs,
                    version: 1,
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_a, 603, DVec3::ZERO),                        // Merlin (grav)
        (ship_b, 587, DVec3::new(2_000.0, 0.0, 0.0)),      // Rifter (ladar)
        (ship_c, 594, DVec3::new(3_000.0, 0.0, 0.0)),      // Incursus (mag)
        (ship_d, 597, DVec3::new(4_000.0, 0.0, 0.0)),      // Punisher (radar)
        (non_ship, 34, DVec3::new(1_000.0, 0.0, 0.0)),     // Tritanium non-ship
    ] {
        let b = make_ball(id, tid, pos, 40.0, 300.0);
        sys.spawn_ball(b).await.unwrap();
    }
    for id in [ship_a, ship_b, ship_c, ship_d] {
        sys.set_combat_state(
            id,
            CombatState {
                shield_hp: 100.0,
                max_shield_hp: 500.0,
                armor_hp: 100.0,
                max_armor_hp: 500.0,
                structure_hp: if id == ship_d { 10.0 } else { 500.0 },
                max_structure_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 1000.0,
                signature_radius: 50.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    // Make ship_c invulnerable then fast-forward, exercise idle fast_forward branches
    sys.fast_forward_clock(ship_b, 10).await.unwrap();
    sys.set_movement_mode(ship_a, MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(100_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 100_000.0,
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    }).await.unwrap();
    let _ = ctx.systems.is_ball_in_committed_warp_sync(sys_id, ship_a);
    let _ = sys.committed_warp_destination_sync(ship_a);
    let _ = sys.is_ball_aligning_warp_sync(ship_a);
    sys.fast_forward_clock(ship_a, 30).await.unwrap();
    assert!(!sys.is_ball_warping_sync(ship_a));

    // Exercise async SystemHandle setters/getters on celestials.rs
    let _ = sys.set_warp_scramble_strength(ship_a, 2).await.unwrap();
    let _ = sys.set_warp_core_strength(ship_a, 3).await.unwrap();
    let _ = sys.add_warp_bubble(9_900_001, DVec3::ZERO, 20_000.0).await.unwrap();
    assert!(sys.remove_warp_bubble(9_900_001).await.unwrap());
    let _ = sys.set_speed_fraction(ship_a, 0.5).await.unwrap();
    assert!(sys.set_ball_position(ship_a, DVec3::new(10.0, 0.0, 0.0)).await);
    assert!(sys.emit_destiny_event(ship_a, eve_destiny::DestinyEvent::Stop { ball_id: ship_a }).await);
    let _ = sys.goto_point(ship_a, DVec3::new(100.0, 0.0, 0.0)).await.unwrap();
    let _ = sys.push_set_state(ship_a).await.unwrap();
    assert!(sys.get_ball_snapshot(ship_a).await.unwrap().is_some());
    let _ = sys.get_ball_group_id(ship_a).await.unwrap();
    assert!(!sys.list_balls().await.unwrap().is_empty());
    assert!(!sys.list_balls_in_bubble(ship_a).await.unwrap().is_empty());
    assert!(!sys.list_dynamic_balls_in_bubble(ship_a).await.unwrap().is_empty());
    let _ = sys.get_set_state(ship_a).await.unwrap();
    let _ = sys.get_bubble_id_for_ball(ship_a).await.unwrap();
    let _ = sys.drain_observer_updates(ship_a).await.unwrap();
    sys.set_capacitor(ship_a, 800.0).await.unwrap();
    let _ = sys.get_combat_state(ship_a).await.unwrap();
    let _ = sys.heal_combat_state(ship_a).await.unwrap();
    let _ = sys.set_interdiction_nullified(ship_a, true).await.unwrap();
    let _ = sys.micro_jump(ship_a, 100_000.0).await.unwrap();
    let _ = sys.micro_jump_field(ship_a, 100_000.0, 6_000.0, 10).await.unwrap();
    let _ = sys.is_ball_scrambled(ship_a).await.unwrap();
    let _ = sys.add_interdiction_sphere(9_900_002, DVec3::ZERO, 20_000.0).await.unwrap();
    let _ = sys.add_mobile_warp_disruptor(9_900_003, DVec3::ZERO, 20_000.0).await.unwrap();
    let _ = sys.apply_ecm_jam(ship_b, 5.0).await.unwrap();
    let _ = sys.apply_ecm_burst(ship_a, 10_000.0, [20.0; 4], 5.0, 0.0).await.unwrap();
    let _ = sys.apply_ecm_burst_ships(ship_a, 10_000.0, [20.0; 4], 5.0, 0.0, Some(HashMap::from([(ship_b, (1, 1.0, 0.0))]))).await.unwrap();
    sys.record_incoming_armor_damage(ship_a, [50.0, 10.0, 10.0, 10.0]).await.unwrap();

    // Target lock completion during step_manual
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id: ship_b,
        lock_time_ms: 100.0,
        remaining_lock_ms: 100.0,
        locked: false,
        max_range_m: None,
    }).await.unwrap();
    assert_eq!(sys.get_target_locks(ship_a).await.unwrap().len(), 1);
    let _ = sys.tick().await.unwrap();
    assert!(lock_hits.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    assert!(sys.remove_target_lock(ship_a, ship_b).await.unwrap());
    assert_eq!(sys.clear_target_locks(ship_a).await.unwrap(), 0);

    // Nanite repair start/sync/cancel/stop + module cycles
    let nr = ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: ship_a,
        module_id: mod_turret,
        initial_damage_hp: 10.0,
        paste_total_needed: 5,
        paste_consumed_so_far: 0,
        duration_s: 5.0,
        elapsed_s: 0.0,
    };
    sys.start_nanite_repair(nr).await.unwrap();
    assert!(sys.has_any_active_nanite_repair_sync(ship_a));
    assert!(sys.is_module_being_nanite_repaired_sync(ship_a, mod_turret));
    assert_eq!(sys.cancel_nanite_repairs_for_ship_sync(ship_a).len(), 1);
    let _ = sys.stop_nanite_repair(ship_a, mod_turret).await.unwrap();

    // Module cycles covering SDE grappler fallback, T2 crystal shatter/damage, smartbomb RepeatCycleStart,
    // capital remote repair impedance, ASB/AAR, cyno/lance expiry, and ship destruction in step_manual
    let now = ctx.now_unix();
    for cyc in [
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_turret, module_type_id: 484, owner_char_id: Some(90_000_001),
            effect_name: "targetAttack".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            linked_turrets: vec![mod_turret], ammo_type_id: Some(12_814),
            crystal_volatility_chance: 1.0, crystal_volatility_damage: 0.6,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_turret2, module_type_id: 484, owner_char_id: Some(90_000_001),
            effect_name: "targetAttack".into(), target_id: Some(ship_d), duration_s: 1.0, repeat: 1,
            linked_turrets: vec![mod_turret2], ammo_type_id: Some(12_814),
            crystal_volatility_chance: 1.0, crystal_volatility_damage: 0.3,
            damage_vector: DamageVector::new(1000.0, 0.0, 0.0, 0.0),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_smartbomb, module_type_id: 1547, owner_char_id: Some(90_000_001),
            effect_name: "empWave".into(), duration_s: 1.0, repeat: 1,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_cap_rep1, module_type_id: 3616, owner_char_id: Some(90_000_001),
            effect_name: "shieldTransfer".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            remote_shield_amount: 500.0, remote_optimal: 10_000.0, remote_falloff: 10_000.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_cap_rep2, module_type_id: 3616, owner_char_id: Some(90_000_001),
            effect_name: "shieldTransfer".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            remote_shield_amount: 500.0, remote_optimal: 10_000.0, remote_falloff: 10_000.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_grap, module_type_id: 41_040, owner_char_id: Some(90_000_001),
            effect_name: "stasisWeb".into(), target_id: Some(ship_b), duration_s: 2.0,
            target_velocity_multiplier: Some(0.5),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_asb, module_type_id: 32_772, owner_char_id: Some(90_000_001),
            effect_name: "shieldBoosting".into(), duration_s: 1.0, repeat: 1,
            is_asb: true, asb_fallback_cap_need: 20.0, shield_boost_amount: 50.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_aar, module_type_id: 33_076, owner_char_id: Some(90_000_001),
            effect_name: "armorRepair".into(), duration_s: 1.0, repeat: 1,
            is_aar: true, aar_paste_rate: 1, aar_base_armor_repair: 50.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_raar, module_type_id: 41_476, owner_char_id: Some(90_000_001),
            effect_name: "armorRepair".into(), target_id: Some(ship_b), duration_s: 1.0, repeat: 1,
            is_aar: true, aar_paste_rate: 10, aar_base_armor_repair: 50.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_cyno, module_type_id: 21_096, owner_char_id: Some(90_000_001),
            effect_name: "cynosuralGeneration".into(), duration_s: 60.0, started_at_unix: now.saturating_sub(5),
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_a, module_id: mod_lance, module_type_id: 40_631, owner_char_id: Some(90_000_001),
            effect_name: "superWeaponLanceWarmup".into(), duration_s: 5.0, repeat: 3, is_overloaded: true,
            ..Default::default()
        },
    ] {
        sys.add_module_cycle(cyc).await.unwrap();
    }

    assert!(sys.has_module_cycle(ship_a, mod_smartbomb).await);
    assert!(sys.has_active_overloaded_cycle_sync(ship_a));
    assert!(!sys.active_offensive_aggressors_on_sync(ship_d, sde_opt_ref(&ctx)).is_empty());
    assert!(!sys.has_active_industrial_core_sync(ship_a));
    assert!(sys.max_active_module_cycle_expiry_unix_sync(ship_a, now + 10, now) >= now + 10);
    assert!(sys.update_module_overload(ship_a, mod_smartbomb, true).await.unwrap());
    let _ = sys.step_manual(1.0).await.unwrap();
    let _ = sys.sim_elapsed_secs();
    let _ = sys.take_pending_cycle_actions();
    sys.try_remove_module_cycle(ship_a, mod_smartbomb);
    let _ = sys.remove_module_cycle(ship_a, mod_aar).await.unwrap();
    sys.stop_non_warp_module_cycles(ship_a).await.unwrap();
}

fn sde_opt_ref(ctx: &ServerContext) -> Option<&eve_sde::SdeRegistry> {
    Some(ctx.sde.as_ref())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macho_listener_displacement_space_ballpark_and_error_paths() {
    use tokio::io::AsyncWriteExt;

    let ctx = fresh_ctx();
    // Place demo character 90_000_001 in space so SelectCharacterID initializes ballpark readiness path
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let c = Arc::clone(&ctx_srv);
                    tokio::spawn(async move {
                        let _ = handle_macho_client(stream, c).await;
                    });
                }
                Err(_) => break,
            }
        }
    });

    // 1. Pre-auth corrupt marshal frame triggers HANDSHAKE_INCOMPATIBLEPROTOCOL close
    {
        let mut bad = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut bad).await.unwrap().unwrap();
        let corrupt = [0xffu8, 0x00, 0x01, 0x02];
        bad.write_all(&(corrupt.len() as u32).to_le_bytes()).await.unwrap();
        bad.write_all(&corrupt).await.unwrap();
        let close_rsp = read_framed_pyrep(&mut bad).await.unwrap().unwrap();
        assert!(matches!(close_rsp, PyRep::Object { .. }));
    }

    async fn connect_and_select(addr: std::net::SocketAddr) -> (TcpStream, u64, u64) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(), Some("OK CC"));
        let mut chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        chal.user_language_id = "EN".to_string();
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        write_framed_pyrep(
            &mut s,
            &(CryptoHandshakeResult {
                challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
                func_output: Vec::new(),
                func_result: PyRep::Integer(0),
            }).to_pyrep(),
        ).await.unwrap();
        let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let init_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let MachoAddress::Client { client_id, .. } = init_pkt.destination else { panic!("client_id") };
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let sel = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client { client_id, call_id: 1, service: None },
            destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("charUnboundMgr".into()), call_id: 0 },
            user_id: Some(ack.user_id),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes("SelectCharacterID"),
                    PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(&mut s, &sel.to_pyrep()).await.unwrap();
        loop {
            let pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
            if pkt.packet_type == MachoPacketType::CallRsp {
                break;
            }
        }
        (s, ack.user_id, client_id)
    }

    // 2. Connect client A, then displace with client B
    let (mut client_a, _uid_a, _cid_a) = connect_and_select(addr).await;
    let (mut client_b, uid_b, cid_b) = connect_and_select(addr).await;
    // Client A receives GPSTransportClosed from displacement
    let disp_rsp = read_framed_pyrep(&mut client_a).await.unwrap().unwrap();
    assert!(matches!(disp_rsp, PyRep::Object { .. }));

    // 3. On client B: post-auth non-MachoPacket frame ignored, slash SlashError, invalid bound OID
    write_framed_pyrep(&mut client_b, &PyRep::Integer(42)).await.unwrap();

    for (call_id, svc, method, args) in [
        (20, "slash", "SlashCmd", vec![PyRep::String("/nonexistent_cov_cmd".into())]),
        (21, "N=1:999999", "BadBoundMethod", Vec::new()),
        (22, "beyonce", "MachoBindObject", vec![PyRep::Integer(30_000_142), PyRep::None]),
    ] {
        let dest = if svc.starts_with("N=") {
            MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some(svc.into()), call_id: 0 }
        } else {
            MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some(svc.into()), call_id: 0 }
        };
        let call_pkt = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client { client_id: cid_b, call_id, service: None },
            destination: dest,
            user_id: Some(uid_b),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes(method),
                    PyRep::Tuple(args),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(&mut client_b, &call_pkt.to_pyrep()).await.unwrap();
        loop {
            let p = MachoPacket::from_pyrep(&read_framed_pyrep(&mut client_b).await.unwrap().unwrap()).unwrap();
            if matches!(p.packet_type, MachoPacketType::CallRsp | MachoPacketType::ErrorResponse) {
                break;
            }
        }
    }

    // 4. Trigger tick on solar system while ballpark_ready is set -> DoDestinyUpdate delivery
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys.goto_point(ctx.db.with_snapshot(|s| s.characters[&90_000_001].active_ship_id), DVec3::new(1000.0, 0.0, 0.0)).await;
    let _ = sys.tick().await;
    let tick_pkt = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        read_framed_pyrep(&mut client_b),
    ).await;
    assert!(tick_pkt.is_ok());
}
