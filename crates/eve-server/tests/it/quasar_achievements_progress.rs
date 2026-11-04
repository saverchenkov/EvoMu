//! End-to-end integration tests for authentic Quasar AIR Achievements remediation:
//! - `quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj`
//! - `quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj`
//! - `quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj`
//! - `quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj`
//! - `quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj`

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_SERVICE_UNAVAILABLE,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn build_sess(ctx: &ServerContext, sid: u64, cid: u32, corp_id: u32, is_admin: bool) -> SessionState {
    let template = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].clone());
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.entry(cid as i64).or_insert_with(|| {
            let mut c = template;
            c.character_id = cid as i64;
            c.corporation_id = corp_id as i64;
            c.name = format!("Pilot {cid}");
            c.balance = eve_db::IskCents::from_isk(500_000);
            c.free_skill_points = 0.0;
            c
        });
    });
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = if is_admin {
        eve_machonet::service_roles::ROLE_GM_DEFAULT
    } else {
        eve_machonet::service_roles::ROLE_PLAYER
    };
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-ach-rem".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    for _ in 0..1024 {
        match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
        }
    }
    out
}

fn make_u32_ident_req(id: u32) -> Vec<u8> {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut req_body = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut req_body);
    req_body
}

#[tokio::test]
async fn test_parity_quasar_achievement_remediation_suite() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
    ]);

    // 1. Verify all 5 descriptor schemas match quasar_descriptors.sqlite
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");
    let has_msg = |fd: &eve_proto::quasar::FileDescriptorProto, name: &str| {
        fd.messages.iter().any(|m| m.name == name)
    };

    let admin_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj")
        .expect("admin requests fd");
    assert_eq!(admin_fd.package, "eve_public.achievement.api.admin");
    assert!(has_msg(admin_fd, "AddProgressRequest"));
    assert!(has_msg(admin_fd, "AddProgressResponse"));
    assert!(has_msg(admin_fd, "CompleteRequest"));
    assert!(has_msg(admin_fd, "CompleteResponse"));
    assert!(has_msg(admin_fd, "ResetRequest"));
    assert!(has_msg(admin_fd, "ResetResponse"));
    assert!(has_msg(admin_fd, "ResetAllRequest"));
    assert!(has_msg(admin_fd, "ResetAllResponse"));

    let not_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/api/notices_pb2.pyj")
        .expect("notices fd");
    assert_eq!(not_fd.package, "eve_public.achievement.api");
    assert!(has_msg(not_fd, "ProgressedNotice"));
    assert!(has_msg(not_fd, "CompletedNotice"));

    let req_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/api/requests_pb2.pyj")
        .expect("requests fd");
    assert_eq!(req_fd.package, "eve_public.achievement.api");
    assert!(has_msg(req_fd, "GetRequest"));
    assert!(has_msg(req_fd, "GetResponse"));

    let cat_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj")
        .expect("category requests fd");
    assert_eq!(cat_fd.package, "eve_public.achievement.category.api");
    assert!(has_msg(cat_fd, "GetRequest"));
    assert!(has_msg(cat_fd, "GetResponse"));

    let cdn_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj")
        .expect("cdn requests fd");
    assert_eq!(cdn_fd.package, "eve_public.achievement.definition.api.cdn");
    assert!(has_msg(cdn_fd, "GetLatestRequest"));
    assert!(has_msg(cdn_fd, "GetLatestResponse"));

    // 2. Setup server context, sessions, and notice subscriber
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let cid = 90_000_825u32;
    let mut admin_sess = build_sess(&ctx, 60_901, cid, 98_000_001, true);
    let mut player_sess = build_sess(&ctx, 60_902, cid + 1, 98_000_001, false);

    ctx.dispatch_rpc(&mut admin_sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(admin_sess.session_id).expect("stream rx");

    // 3. SDE Definition Verification & CDN Checkpoint Queries
    let ach_defs = &ctx.quasar_gateway.progression.achievements.definitions;
    assert_eq!(ach_defs.len(), 60, "Must load 60 enabled SDE achievements");
    for dis_id in [3u32, 4, 14, 37, 51, 54, 56, 65] {
        assert!(
            !ach_defs.iter().any(|d| d.id == dis_id),
            "Disabled achievement {dis_id} must be excluded"
        );
    }
    let ach2 = ach_defs.iter().find(|d| d.id == 2).expect("achievement 2 'Mine Ore'");
    assert_eq!(ach2.name, "Mine Ore");
    assert_eq!(ach2.target, 3);

    // CDN GetLatestRequest with no_local_version_available = true -> Checkpoint URL & IEEE CRC32
    let mut cdn_req_nolocal = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req_nolocal);
    let r_cdn = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_req_nolocal, b"c-01").await;
    assert_eq!(r_cdn.status_code, STATUS_OK);
    let dyn_cdn = reg
        .decode_dynamic("eve_public.achievement.definition.api.cdn.GetLatestResponse", &r_cdn.payload.as_ref().unwrap().value)
        .expect("decode GetLatestResponse");
    assert!(dyn_cdn.get(1).is_some(), "checkpoint field 1 must be present");

    let wm_cdn = WireMessage::decode(&r_cdn.payload.as_ref().unwrap().value).expect("wire cdn");
    let cp_bytes = wm_cdn.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let cp_wm = WireMessage::decode(cp_bytes).expect("wire checkpoint");
    let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).unwrap();
    assert!(url_str.contains("/cdn/achievements/v24.1.60/"), "Checkpoint URL must include authentic path");

    // CDN GetLatestRequest with current_version Specification and no no_local flag -> STATUS_SERVICE_UNAVAILABLE fallback
    let mut ver_msg = Vec::new();
    encode_varint_field(1, 24, &mut ver_msg);
    encode_varint_field(2, 1, &mut ver_msg);
    encode_varint_field(3, 60, &mut ver_msg);
    let mut cdn_req_ver = Vec::new();
    encode_bytes_field(1, &ver_msg, &mut cdn_req_ver);
    let r_cdn_fallback = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_req_ver, b"c-02").await;
    assert_eq!(r_cdn_fallback.status_code, STATUS_SERVICE_UNAVAILABLE, "Must signal local definition fallback");

    // 4. Category Queries
    let r_cat_bad = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.category.api.GetRequest", Vec::new(), b"cat-01").await;
    assert_eq!(r_cat_bad.status_code, STATUS_BAD_REQUEST, "Missing category identifier returns BAD_REQUEST");

    let r_cat = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.category.api.GetRequest", make_u32_ident_req(1), b"cat-02").await;
    assert_eq!(r_cat.status_code, STATUS_OK);
    let dyn_cat = reg
        .decode_dynamic("eve_public.achievement.category.api.GetResponse", &r_cat.payload.unwrap().value)
        .expect("decode category GetResponse");
    assert_eq!(dyn_cat.get(1).and_then(|v| match v { DynamicValue::Uint32(u) => Some(*u), _ => None }), Some(0));

    // 5. Incomplete Achievement Query via GetRequest
    let r_dis = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(3), b"g-01").await;
    assert_eq!(r_dis.status_code, STATUS_NOT_FOUND, "Disabled achievement returns NOT_FOUND");

    let r_get_missing = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", Vec::new(), b"g-02").await;
    assert_eq!(r_get_missing.status_code, STATUS_BAD_REQUEST, "Missing achievement ID returns BAD_REQUEST");

    let r_get_unk = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(9999), b"g-03").await;
    assert_eq!(r_get_unk.status_code, STATUS_NOT_FOUND, "Unknown achievement ID returns NOT_FOUND");

    let r_get1 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"g-04").await;
    assert_eq!(r_get1.status_code, STATUS_OK);
    let dyn_get1 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &r_get1.payload.unwrap().value)
        .expect("decode GetResponse");
    assert!(dyn_get1.get(1).is_none(), "completed_date absent on incomplete achievement");
    assert!(matches!(dyn_get1.get(2), Some(DynamicValue::Bool(true))), "not_completed true");
    assert_eq!(dyn_get1.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(0));

    // 6. Admin Role Enforcement
    let mut add_prog_body = make_u32_ident_req(2);
    encode_varint_field(2, 1, &mut add_prog_body);
    let r_non_admin = quasar_rpc(&ctx, &mut player_sess, "eve_public.achievement.api.admin.AddProgressRequest", add_prog_body.clone(), b"ad-01").await;
    assert_eq!(r_non_admin.status_code, STATUS_FORBIDDEN, "Non-admin call rejected");

    // 7. Admin AddProgressRequest & ProgressedNotice
    drain_notices(&mut stream_rx);
    let r_add_bad = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.AddProgressRequest", Vec::new(), b"ad-02").await;
    assert_eq!(r_add_bad.status_code, STATUS_BAD_REQUEST);

    let r_add1 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.AddProgressRequest", add_prog_body, b"ad-03").await;
    assert_eq!(r_add1.status_code, STATUS_OK);

    let notices_add = drain_notices(&mut stream_rx);
    let not_prog = notices_add
        .iter()
        .find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.achievement.api.ProgressedNotice")))
        .expect("ProgressedNotice emitted");
    let dyn_prog = reg
        .decode_dynamic("eve_public.achievement.api.ProgressedNotice", &not_prog.payload.as_ref().unwrap().value)
        .expect("decode ProgressedNotice");
    assert_eq!(dyn_prog.get(2).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(1));

    // Verify progress advanced to 1
    let r_get2 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"g-05").await;
    assert_eq!(r_get2.status_code, STATUS_OK);
    let dyn_get2 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &r_get2.payload.unwrap().value)
        .expect("decode GetResponse");
    assert_eq!(dyn_get2.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(1));

    // Admin AddProgressRequest via field 4 (unique_item_count)
    let mut add_prog_f4 = make_u32_ident_req(2);
    encode_varint_field(4, 1, &mut add_prog_f4);
    let r_add2 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.AddProgressRequest", add_prog_f4, b"ad-04").await;
    assert_eq!(r_add2.status_code, STATUS_OK);

    let notices_f4 = drain_notices(&mut stream_rx);
    let not_prog2 = notices_f4
        .iter()
        .find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.achievement.api.ProgressedNotice")))
        .expect("second ProgressedNotice emitted");
    let dyn_prog2 = reg
        .decode_dynamic("eve_public.achievement.api.ProgressedNotice", &not_prog2.payload.as_ref().unwrap().value)
        .expect("decode ProgressedNotice");
    assert_eq!(dyn_prog2.get(2).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(2));

    // 8. Admin CompleteRequest & CompletedNotice
    let r_comp_bad = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.CompleteRequest", Vec::new(), b"ad-05").await;
    assert_eq!(r_comp_bad.status_code, STATUS_BAD_REQUEST);

    let r_comp = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.CompleteRequest", make_u32_ident_req(2), b"ad-06").await;
    assert_eq!(r_comp.status_code, STATUS_OK);

    let notices_comp = drain_notices(&mut stream_rx);
    assert!(
        notices_comp.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.achievement.api.CompletedNotice"))),
        "CompletedNotice must be emitted upon completion"
    );

    // Verify completed achievement query returns completed_date present, not_completed absent
    let r_get3 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"g-06").await;
    assert_eq!(r_get3.status_code, STATUS_OK);
    let dyn_get3 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &r_get3.payload.unwrap().value)
        .expect("decode completed GetResponse");
    assert!(dyn_get3.get(1).is_some(), "completed_date must be present");
    assert!(dyn_get3.get(2).is_none(), "not_completed must be absent");
    assert_eq!(dyn_get3.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(3));

    // 9. Admin ResetRequest & Category ResetRequest
    let r_reset = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.ResetRequest", make_u32_ident_req(2), b"ad-07").await;
    assert_eq!(r_reset.status_code, STATUS_OK);

    let r_get4 = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"g-07").await;
    assert_eq!(r_get4.status_code, STATUS_OK);
    let dyn_get4 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &r_get4.payload.unwrap().value)
        .expect("decode reset GetResponse");
    assert!(matches!(dyn_get4.get(2), Some(DynamicValue::Bool(true))), "not_completed restored to true");
    assert_eq!(dyn_get4.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(0));

    // Category admin reset
    let r_cat_reset = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.category.api.admin.ResetRequest", make_u32_ident_req(1), b"ad-08").await;
    assert_eq!(r_cat_reset.status_code, STATUS_OK);

    // Global reset all
    let r_reset_all = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.ResetAllRequest", Vec::new(), b"ad-09").await;
    assert_eq!(r_reset_all.status_code, STATUS_OK);
}
