//! End-to-end integration tests for the 12 remediated `quasar:*` parity rows.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
    STATUS_SERVICE_UNAVAILABLE,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;
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

fn build_sess(ctx: &ServerContext, sid: u64, cid: u32, corp_id: u32) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.entry(cid as i64).or_insert_with(|| eve_db::CharacterRecord {
            character_id: cid as i64,
            user_id: 1,
            name: format!("Pilot {cid}"),
            corporation_id: corp_id as i64,
            alliance_id: None,
            war_faction_id: None,
            fleet_id: None,
            autopilot_waypoints: Vec::new(),
            station_id: None,
            structure_id: None,
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: 80_000_001,
            balance: eve_db::IskCents::from_isk(100_000),
            plex_balance: 0,
            loyalty_points: HashMap::new(),
            standings: HashMap::new(),
            security_status: 0.0,
            clone_grade: 2,
            home_station_id: 60003760,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 1,
            school_id: 1,
            neural: eve_db::NeuralAttributes::default(),
            skill_points: 1_000_000.0,
            activity_status: None,
            free_skill_points: 0.0,
            last_clone_jump_unix: 0,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: String::new(),
            title: String::new(),
            gender: 1,
            create_date_unix: 0,
            version: 1,
            system_epoch: 1,
        });
    });
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
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
        application_instance_uuid: b"app-quasar-batch-01".to_vec(),
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
async fn test_quasar_achievements_admin_notices_and_queries() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
    ]);

    let ctx = std::sync::Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(&ctx, 51_001, 91_000_001, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

    // 1. GetRequest on disabled achievement #3 -> NOT_FOUND; on enabled #2 -> not_completed=true, progress=0
    let dis_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(3), b"a-01").await;
    assert_eq!(dis_rsp.status_code, STATUS_NOT_FOUND);

    let get_rsp1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"a-02").await;
    assert_eq!(get_rsp1.status_code, STATUS_OK);
    let dyn_get1 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &get_rsp1.payload.unwrap().value)
        .expect("decode GetResponse");
    assert!(matches!(dyn_get1.get(2), Some(DynamicValue::Bool(true))));
    assert_eq!(dyn_get1.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(0));

    // 2. Admin AddProgressRequest: missing ID -> BAD_REQUEST; valid ID 2, delta 1 -> OK + ProgressedNotice
    let bad_add = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", Vec::new(), b"a-03").await;
    assert_eq!(bad_add.status_code, STATUS_BAD_REQUEST);

    let mut add_req = make_u32_ident_req(2);
    encode_varint_field(2, 1, &mut add_req);
    let add_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", add_req, b"a-04").await;
    assert_eq!(add_rsp.status_code, STATUS_OK);

    let notices1 = drain_notices(&mut stream_rx);
    let prog_not = notices1
        .iter()
        .find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.achievement.api.ProgressedNotice")))
        .expect("ProgressedNotice emitted");
    let dyn_prog = reg
        .decode_dynamic("eve_public.achievement.api.ProgressedNotice", &prog_not.payload.as_ref().unwrap().value)
        .expect("decode ProgressedNotice");
    assert_eq!(dyn_prog.get(2).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(1));

    // 3. Admin CompleteRequest: missing ID -> BAD_REQUEST; valid ID 2 -> OK + CompletedNotice
    let bad_comp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", Vec::new(), b"a-05").await;
    assert_eq!(bad_comp.status_code, STATUS_BAD_REQUEST);

    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", make_u32_ident_req(2), b"a-06").await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    let notices2 = drain_notices(&mut stream_rx);
    assert!(
        notices2.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.achievement.api.CompletedNotice"))),
        "CompletedNotice must be emitted"
    );

    let get_rsp2 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_u32_ident_req(2), b"a-07").await;
    assert_eq!(get_rsp2.status_code, STATUS_OK);
    let dyn_get2 = reg
        .decode_dynamic("eve_public.achievement.api.GetResponse", &get_rsp2.payload.unwrap().value)
        .expect("decode completed GetResponse");
    assert!(dyn_get2.get(1).is_some());
    assert!(dyn_get2.get(2).is_none());
    assert_eq!(dyn_get2.get(3).and_then(|v| match v { DynamicValue::Uint64(u) => Some(*u), _ => None }), Some(3));

    // 4. Admin ResetRequest & ResetAllRequest
    let bad_rst = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.ResetRequest", Vec::new(), b"a-08").await;
    assert_eq!(bad_rst.status_code, STATUS_BAD_REQUEST);
    let rst_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.ResetRequest", make_u32_ident_req(2), b"a-09").await;
    assert_eq!(rst_rsp.status_code, STATUS_OK);
    let rst_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.ResetAllRequest", Vec::new(), b"a-10").await;
    assert_eq!(rst_all_rsp.status_code, STATUS_OK);

    // 5. Category GetRequest & Showcase GetCharacterScoreRequest
    let cat_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", make_u32_ident_req(1), b"a-11").await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    let dyn_cat = reg
        .decode_dynamic("eve_public.achievement.category.api.GetResponse", &cat_rsp.payload.unwrap().value)
        .expect("decode category GetResponse");
    assert!(matches!(dyn_cat.get(1), Some(DynamicValue::Uint32(0))));

    let sc_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new(), b"a-12").await;
    assert_eq!(sc_rsp.status_code, STATUS_OK);
    let dyn_sc = reg
        .decode_dynamic("eve_public.achievement.showcase.api.GetCharacterScoreResponse", &sc_rsp.payload.unwrap().value)
        .expect("decode GetCharacterScoreResponse");
    assert!(matches!(dyn_sc.get(1), Some(DynamicValue::Uint32(0))));

    // 6. CDN GetLatestRequest (no_local_version_available -> Checkpoint; current_version -> SERVICE_UNAVAILABLE)
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let cdn_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_req, b"a-13").await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    let wm_cdn = WireMessage::decode(&cdn_rsp.payload.unwrap().value).expect("decode GetLatestResponse");
    let cp_wm = WireMessage::decode(wm_cdn.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).unwrap();
    assert!(url_str.contains("/cdn/achievements/v24.1.60/"));

    let mut ver_msg = Vec::new();
    encode_varint_field(1, 24, &mut ver_msg);
    let mut cdn_ver_req = Vec::new();
    encode_bytes_field(1, &ver_msg, &mut cdn_ver_req);
    let cdn_unavail = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_ver_req, b"a-14").await;
    assert_eq!(cdn_unavail.status_code, STATUS_SERVICE_UNAVAILABLE);

    // 7. Reward GetUnclaimedRequest & ClaimRequest
    let uncl_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new(), b"a-15").await;
    assert_eq!(uncl_rsp.status_code, STATUS_OK);
    let mut claim_req = Vec::new();
    encode_varint_field(3, 1, &mut claim_req);
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", claim_req, b"a-16").await;
    assert_eq!(claim_rsp.status_code, STATUS_PRECONDITION_FAILED);
}

#[tokio::test]
async fn test_quasar_career_goals_requests_admin_and_notices() {
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(&ctx, 51_002, 91_000_002, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

    // 1. GetDefinitionsRequest & GetAllRequest
    let defs_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), b"c-01").await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    let dyn_defs = reg
        .decode_dynamic("eve_public.career.goal.api.GetDefinitionsResponse", &defs_rsp.payload.unwrap().value)
        .expect("decode GetDefinitionsResponse");
    assert!(matches!(dyn_defs.get(1), Some(DynamicValue::Repeated(items)) if items.len() == 196));

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new(), b"c-02").await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let dyn_all = reg
        .decode_dynamic("eve_public.career.goal.api.GetAllResponse", &all_rsp.payload.unwrap().value)
        .expect("decode GetAllResponse");
    assert!(matches!(dyn_all.get(1), Some(DynamicValue::Repeated(items)) if items.len() == 196));

    // 2. Admin ProgressRequest: zero delta -> BAD_REQUEST; unknown UUID -> NOT_FOUND; valid UUID -> OK + ProgressedNotice
    let goal_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut id_inner = Vec::new();
    encode_bytes_field(1, &goal_uuid, &mut id_inner);

    let mut zero_prog = Vec::new();
    encode_bytes_field(1, &id_inner, &mut zero_prog);
    encode_varint_field(2, 0, &mut zero_prog);
    let zero_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", zero_prog, b"c-03").await;
    assert_eq!(zero_rsp.status_code, STATUS_BAD_REQUEST);

    let mut unk_id = Vec::new();
    encode_bytes_field(1, &[0xFF; 16], &mut unk_id);
    let mut unk_prog = Vec::new();
    encode_bytes_field(1, &unk_id, &mut unk_prog);
    encode_varint_field(2, 5, &mut unk_prog);
    let unk_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", unk_prog, b"c-04").await;
    assert_eq!(unk_rsp.status_code, STATUS_NOT_FOUND);

    let mut prog_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut prog_req);
    encode_varint_field(2, 5, &mut prog_req);
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", prog_req, b"c-05").await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let notices1 = drain_notices(&mut stream_rx);
    let prog_not = notices1
        .iter()
        .find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.ProgressedNotice")))
        .expect("ProgressedNotice emitted");
    let dyn_prog = reg
        .decode_dynamic("eve_public.career.goal.api.ProgressedNotice", &prog_not.payload.as_ref().unwrap().value)
        .expect("decode ProgressedNotice");
    assert_eq!(dyn_prog.get(2).and_then(|v| match v { DynamicValue::Uint32(u) => Some(*u), _ => None }), Some(5));

    // 3. Admin CompleteRequest -> OK + CompletedNotice
    let mut comp_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut comp_req);
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.CompleteRequest", comp_req, b"c-06").await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);
    let notices2 = drain_notices(&mut stream_rx);
    assert!(
        notices2.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.CompletedNotice"))),
        "CompletedNotice must be emitted"
    );

    // 4. ClaimRewardRequest -> credits ISK
    let bal_before = ctx.db.with_snapshot(|snap| snap.characters.get(&91_000_002).unwrap().balance);
    let mut claim_req = Vec::new();
    encode_bytes_field(1, &id_inner, &mut claim_req);
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", claim_req, b"c-07").await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);
    let bal_after = ctx.db.with_snapshot(|snap| snap.characters.get(&91_000_002).unwrap().balance);
    assert!(bal_after > bal_before, "ClaimRewardRequest must credit character ISK balance");
}

#[tokio::test]
async fn test_quasar_ai_assistant_chat_requests() {
    // parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(&ctx, 51_003, 91_000_003, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    // 1. Empty query -> BAD_REQUEST
    let bad_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", Vec::new(), b"ai-01").await;
    assert_eq!(bad_rsp.status_code, STATUS_BAD_REQUEST);

    // 2. Solar system query ("Jita") -> SDE-backed response with fallback=false
    let mut jita_req = Vec::new();
    encode_string_field(1, "Tell me about Jita", &mut jita_req);
    let jita_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", jita_req, b"ai-02").await;
    assert_eq!(jita_rsp.status_code, STATUS_OK);
    let dyn_jita = reg
        .decode_dynamic("eve_public.ai.assistant.chat.api.QueryResponse", &jita_rsp.payload.unwrap().value)
        .expect("decode QueryResponse");
    match dyn_jita.get(1) {
        Some(DynamicValue::String(s)) => {
            assert!(s.contains("Jita") && s.contains("30000142"), "Expected Jita SDE details, got {s}");
        }
        other => panic!("Expected string response, got {other:?}"),
    }
    assert!(matches!(dyn_jita.get(2), Some(DynamicValue::Bool(false))));
    assert!(dyn_jita.get(3).is_some(), "formatted_response must be populated");

    // 3. Ship/item type query ("Drake") -> SDE-backed response with fallback=false
    let mut drake_req = Vec::new();
    encode_string_field(1, "What is a Drake?", &mut drake_req);
    let drake_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", drake_req, b"ai-03").await;
    assert_eq!(drake_rsp.status_code, STATUS_OK);
    let dyn_drake = reg
        .decode_dynamic("eve_public.ai.assistant.chat.api.QueryResponse", &drake_rsp.payload.unwrap().value)
        .expect("decode Drake QueryResponse");
    match dyn_drake.get(1) {
        Some(DynamicValue::String(s)) => {
            assert!(s.contains("Drake"), "Expected Drake SDE details, got {s}");
        }
        other => panic!("Expected string response, got {other:?}"),
    }

    // 4. General query -> fallback=true with current solar system context
    let mut gen_req = Vec::new();
    encode_string_field(1, "How do I undock?", &mut gen_req);
    let gen_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", gen_req, b"ai-04").await;
    assert_eq!(gen_rsp.status_code, STATUS_OK);
    let dyn_gen = reg
        .decode_dynamic("eve_public.ai.assistant.chat.api.QueryResponse", &gen_rsp.payload.unwrap().value)
        .expect("decode fallback QueryResponse");
    assert!(matches!(dyn_gen.get(2), Some(DynamicValue::Bool(true))));
}

#[tokio::test]
async fn test_quasar_campaign_trigger_eventmatch_definitions() {
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(&ctx, 51_004, 91_000_004, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        Vec::new(),
        b"em-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetAllResponse payload");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"
    );

    let dyn_rsp = reg
        .decode_dynamic("eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse", &payload.value)
        .expect("decode GetAllResponse");
    let defs = match dyn_rsp.get(1) {
        Some(DynamicValue::Repeated(items)) => items,
        other => panic!("Expected Repeated definitions, got {other:?}"),
    };
    assert_eq!(defs.len(), 51, "Must return all 51 SDE trigger types from triggertypes.static");

    let first_def = match &defs[0] {
        DynamicValue::Message(m) => m,
        other => panic!("Expected Definition message, got {other:?}"),
    };
    let first_name = match first_def.get(1) {
        Some(DynamicValue::Message(id_msg)) => match id_msg.get(1) {
            Some(DynamicValue::String(s)) => s.clone(),
            other => panic!("Expected Identifier.name string, got {other:?}"),
        },
        other => panic!("Expected Identifier message, got {other:?}"),
    };
    assert!(!first_name.is_empty(), "Trigger eventmatch name must be non-empty");
}
