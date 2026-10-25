//! Integration tests validating remediation of Quasar Career, Campaign Definitions,
//! and Achievements authentic data-driven semantics.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
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

fn build_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
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
        application_instance_uuid: b"app-remediated-01".to_vec(),
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
    while let Ok(msg) = rx.try_recv() {
        out.push(msg);
    }
    out
}

#[tokio::test]
async fn test_authentic_career_goals_and_milestones_remediated() {
    // parity: feat:quasar.career, quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.career",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(41_001, 90_000_010, 98_000_001);

    // 1. GetAllRequest returns all 188 authentic career goals
    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new(), b"cg-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    let r1_payload = r1.payload.expect("r1 payload");
    assert_eq!(r1_payload.normalized_type_name(), "eve_public.career.goal.api.GetAllResponse");

    let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllResponse");
    let goal_fields: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(goal_fields.len(), 188, "Must return 188 authentic AIR Career Program goals");

    // Inspect first goal: verify field 1 is Identifier { uuid: bytes }
    let g1_bytes = goal_fields[0].value.as_bytes().expect("g1 bytes");
    let g1_wm = WireMessage::decode(g1_bytes).expect("decode g1");
    let id_field = g1_wm.fields.iter().find(|f| f.field_number == 1).expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().expect("id bytes")).expect("decode id");
    let uuid_bytes = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("uuid");
    assert_eq!(uuid_bytes.len(), 16, "Career goal identifier uuid must be 16 bytes");

    // 2. GetDefinitionsRequest returns all 188 authentic career goal definitions
    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), b"cg-02").await;
    assert_eq!(r2.status_code, STATUS_OK);
    let r2_payload = r2.payload.expect("r2 payload");
    assert_eq!(r2_payload.normalized_type_name(), "eve_public.career.goal.api.GetDefinitionsResponse");
    let wm2 = WireMessage::decode(&r2_payload.value).expect("decode GetDefinitionsResponse");
    let def_fields: Vec<_> = wm2.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(def_fields.len(), 188, "Must return 188 authentic career goal definitions");

    // 3. Admin ProgressRequest and CompleteRequest with live notice publication
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

    let enforcer_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut prog_req = Vec::new();
    let mut id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut prog_req);
    encode_varint_field(2, 5, &mut prog_req);

    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", prog_req, b"cg-03").await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let notices = drain_notices(&mut stream_rx);
    assert!(!notices.is_empty(), "ProgressedNotice must be emitted");
    let prog_not = notices.iter().find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("ProgressedNotice"))).expect("ProgressedNotice");
    let not_wm = WireMessage::decode(&prog_not.payload.as_ref().unwrap().value).expect("decode notice");
    assert!(not_wm.fields.iter().any(|f| f.field_number == 1), "Notice must contain goal Identifier");
}

#[tokio::test]
async fn test_authentic_campaign_definitions_remediated() {
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(41_002, 90_000_011, 98_000_001);

    // 1. Contribution method definitions: GetAllRequest
    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new(), b"cm-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    let r1_payload = r1.payload.expect("r1 payload");
    assert_eq!(r1_payload.normalized_type_name(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse");
    assert!(!r1_payload.value.is_empty(), "Contribution methods payload must not be empty");

    let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllResponse");
    let methods: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(methods.len(), 22, "Must return 22 authentic contribution methods");

    // Inspect first contribution method definition
    let m1_bytes = methods[0].value.as_bytes().expect("m1 bytes");
    let m1_wm = WireMessage::decode(m1_bytes).expect("decode m1");
    let id_f = m1_wm.fields.iter().find(|f| f.field_number == 1).expect("definition Identifier");
    let id_wm = WireMessage::decode(id_f.value.as_bytes().expect("id bytes")).expect("decode Identifier");
    let name_str = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("method name");
    assert!(!name_str.is_empty(), "Method name must be non-empty");

    // 2. Trigger eventmatch definitions: GetAllRequest
    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new(), b"em-01").await;
    assert_eq!(r2.status_code, STATUS_OK);
    let r2_payload = r2.payload.expect("r2 payload");
    assert_eq!(r2_payload.normalized_type_name(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse");
    assert!(!r2_payload.value.is_empty(), "Trigger eventmatches payload must not be empty");

    let wm2 = WireMessage::decode(&r2_payload.value).expect("decode GetAllResponse");
    let triggers: Vec<_> = wm2.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(triggers.len(), 10, "Must return 10 authentic trigger eventmatches");
}

#[tokio::test]
async fn test_authentic_achievements_and_cdn_metadata_remediated() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(41_003, 90_000_012, 98_000_001);

    // 1. CDN GetLatestRequest returns valid checkpoint metadata with patch 68 and CRC32 IEEE
    let cdn_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new(), b"cdn-01").await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    let cdn_payload = cdn_rsp.payload.expect("cdn payload");
    assert_eq!(cdn_payload.normalized_type_name(), "eve_public.achievement.definition.api.cdn.GetLatestResponse");

    let wm_cdn = WireMessage::decode(&cdn_payload.value).expect("decode GetLatestResponse");
    let cp_field = wm_cdn.fields.iter().find(|f| f.field_number == 1).expect("checkpoint field");
    let cp_wm = WireMessage::decode(cp_field.value.as_bytes().expect("cp bytes")).expect("decode Checkpoint");

    // field 1: url
    let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("url");
    assert!(url_str.contains("v24.1.68") || url_str.contains("achievements"), "URL must reference achievement definitions: {url_str}");

    // field 2: version specification (major: 24, minor: 1, patch: 68)
    let spec_field = cp_wm.fields.iter().find(|f| f.field_number == 2).expect("spec field");
    let spec_wm = WireMessage::decode(spec_field.value.as_bytes().expect("spec bytes")).expect("decode spec");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(24), "Major 24");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1), "Minor 1");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(68), "Patch 68 (68 real SDE achievements)");

    // field 3: crc Checksum
    let crc_field = cp_wm.fields.iter().find(|f| f.field_number == 3).expect("crc field");
    let crc_wm = WireMessage::decode(crc_field.value.as_bytes().expect("crc bytes")).expect("decode crc");
    let crc_val = crc_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).expect("crc val");
    assert!(crc_val > 0, "CRC32 checksum must be non-zero");

    // 2. Character Achievements & Showcase: GetRequest, GetUnclaimedRequest, GetCharacterScoreRequest
    let mut req_body = Vec::new();
    let mut id_bytes = Vec::new();
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&101u32.to_be_bytes());
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut req_body);

    let get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", req_body, b"ach-01").await;
    assert_eq!(get_rsp.status_code, STATUS_OK);

    let uncl_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new(), b"ach-02").await;
    assert_eq!(uncl_rsp.status_code, STATUS_OK);

    let score_rsp = quasar_rpc(&ctx, &mut sess, "eve.achievement.showcase.api.GetCharacterScoreRequest", Vec::new(), b"ach-03").await;
    assert_eq!(score_rsp.status_code, STATUS_OK);
}
