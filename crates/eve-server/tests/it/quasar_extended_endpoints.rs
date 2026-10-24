//! Integration tests for Extended Quasar Endpoints across Career, Skills, Achievements,
//! Campaigns, and Community Tracks (Phase Q, tasks `Q-3` through `Q-8`).

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicEvent, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
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

async fn quasar_rpc(ctx: &ServerContext, sess: &mut SessionState, type_url: &str, payload_value: Vec<u8>, corr_id: &[u8]) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: corr_id.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) { type_url.to_string() } else { format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}") },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-extended-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None).await.expect("SendRequest");
    extract_response_proto(&rep)
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() { out.push(msg); }
    out
}

#[tokio::test]
async fn quasar_career_extended_endpoints_and_definitions() {
    // parity: quasar:eveProto/generated/eve_public/career/career_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/career/career_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_001, 90_000_001, 98_000_001);

    let reg = QuasarDescriptorRegistry::open_default().expect("open default registry");
    let career_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/career/career_pb2.pyj").expect("career_pb2");
    assert_eq!(career_fd.package, "eve_public.career");
    let career_enum = career_fd.enums.iter().find(|e| e.name == "Path").expect("Path enum");
    assert_eq!(career_enum.values.len(), 5);

    let goal_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/career/goal_pb2.pyj").expect("goal_pb2");
    assert_eq!(goal_fd.package, "eve_public.career.goal");

    let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new(), b"corr-career-01").await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.status_message, "OK");
    assert_eq!(rsp.correlation_uuid, b"corr-career-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(payload.normalized_type_name(), "eve_public.career.goal.api.GetAllResponse");
    let wm = WireMessage::decode(&payload.value).expect("decode GetAllResponse");
    assert_eq!(wm.fields.iter().filter(|f| f.field_number == 1).count(), 8);

    let def_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), b"corr-career-02").await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(def_rsp.payload.unwrap().normalized_type_name(), "eve_public.career.goal.api.GetDefinitionsResponse");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver");

    let (mut notice_data, mut id_bytes) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut notice_data);
    encode_varint_field(2, 10, &mut notice_data);

    let notice_payload = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"), value: notice_data };
    let (published, count) = ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_001), notice_payload, ctx.now_unix()).expect("publish_notice");
    assert_eq!(count, 1);
    assert_eq!(published.internal_origin, "evomu.quasar.notice_publisher");

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.career.goal.api.ProgressedNotice");
}

#[tokio::test]
async fn quasar_skill_plan_extended_endpoints_and_definitions() {
    // parity: quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj",
        "quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_002, 90_000_002, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj").unwrap().package, "eve_public.skill.plan.milestone");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/skill/plan/certified_pb2.pyj").unwrap().package, "eve_public.skill.plan.certified");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj").unwrap().package, "eve_public.corporation.skill.plan.category");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/skill/skilltype_pb2.pyj").unwrap().package, "eve_public.skilltype");

    let rsp = quasar_rpc(&ctx, &mut sess, "eve.skilltype.GetRequest", Vec::new(), b"corr-skill-01").await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-skill-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(payload.normalized_type_name(), "eve.skilltype.GetResponse");
    assert_eq!(WireMessage::decode(&payload.value).expect("decode skilltype").fields.len(), 1);

    let corp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), b"corr-skill-02").await;
    assert_eq!(corp_rsp.status_code, STATUS_OK);
    assert_eq!(corp_rsp.payload.unwrap().normalized_type_name(), "eve_public.corporation.skill.plan.GetAllResponse");

    for (name, seed) in [
        ("eve_public.skill.plan.milestone.Identifier", 42),
        ("eve_public.skill.plan.certified.Identifier", 43),
        ("eve_public.corporation.skill.plan.category.Identifier", 44),
        ("eve_public.skilltype.Identifier", 45),
    ] {
        assert_eq!(reg.instantiate_populated_sample(name, seed).unwrap().full_name, name);
    }
}

#[tokio::test]
async fn quasar_achievement_category_endpoints_and_notices() {
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/category/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_003, 90_000_003, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    let cat_def_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj").expect("category def");
    assert_eq!(cat_def_fd.package, "eve_public.achievement.category.definition");
    assert_eq!(cat_def_fd.messages[0].name, "Identifier");

    let cat_ms_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj").expect("category milestone def");
    assert_eq!(cat_ms_fd.package, "eve_public.achievement.category.milestone.definition");
    assert_eq!(cat_ms_fd.messages[0].name, "Identifier");

    let (mut reset_body, mut cat_id_bytes) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[1u8; 16], &mut cat_id_bytes);
    encode_bytes_field(1, &cat_id_bytes, &mut reset_body);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.admin.ResetRequest", reset_body, b"corr-ach-cat-01").await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-ach-cat-01");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver");

    let (mut cat_prog_data, mut cat_id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0xAA; 16], &mut cat_id);
    encode_bytes_field(1, &cat_id, &mut cat_prog_data);
    encode_varint_field(2, 450, &mut cat_prog_data);
    let cat_prog_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.ProgressedNotice"), value: cat_prog_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_003), cat_prog_notice, ctx.now_unix()).unwrap().1, 1);

    let (mut cat_ms_data, mut ms_id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &cat_id, &mut cat_ms_data);
    encode_bytes_field(1, &[0xBB; 16], &mut ms_id);
    encode_bytes_field(2, &ms_id, &mut cat_ms_data);
    let cat_ms_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.milestone.api.ReachedNotice"), value: cat_ms_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_003), cat_ms_notice, ctx.now_unix()).unwrap().1, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.category.api.ProgressedNotice");
    assert_eq!(received[1].payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.category.milestone.api.ReachedNotice");
}

#[tokio::test]
async fn quasar_achievement_milestones_rewards_and_events() {
    // parity: quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_004, 90_000_004, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj").unwrap().package, "eve_public.achievement.definition");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj").unwrap().package, "eve_public.achievement.reward.definition");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj").unwrap().package, "eve_public.achievement.milestone.definition");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj").unwrap().package, "eve_public.achievement.contribution");
    let events_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/achievement/api/events_pb2.pyj").expect("events");
    assert_eq!(events_fd.package, "eve_public.achievement.api");
    assert_eq!(events_fd.messages.len(), 4);

    let (mut pin_data, mut char_id_bytes, mut ach_id_bytes) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 90_000_004, &mut char_id_bytes);
    encode_bytes_field(1, &char_id_bytes, &mut pin_data);
    encode_bytes_field(1, &[0x41, 0x43, 0x48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut ach_id_bytes);
    encode_bytes_field(2, &ach_id_bytes, &mut pin_data);

    let event = EvePublicEvent {
        occurred: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        uuid: b"evt-uuid-0000001".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.Pinned"), value: pin_data }),
        journey: b"jrn-01".to_vec(),
        external_origin: "eve-client".to_string(),
        application_instance_uuid: b"app-extended-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway.publish_event(&sess, event, ctx.now_unix());
    let published_events = ctx.quasar_gateway.published_events.read().unwrap();
    assert_eq!(published_events.len(), 1);
    assert_eq!(published_events[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.api.Pinned");
    drop(published_events);

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver");

    let (mut ms_notice_data, mut ms_uuid) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &ach_id_bytes, &mut ms_notice_data);
    encode_bytes_field(1, &[0x31; 16], &mut ms_uuid);
    encode_bytes_field(2, &ms_uuid, &mut ms_notice_data);
    let ms_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.milestone.api.ReachedNotice"), value: ms_notice_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_004), ms_notice, ctx.now_unix()).unwrap().1, 1);

    let (mut ver_data, mut spec_data) = (Vec::new(), Vec::new());
    encode_varint_field(1, 24, &mut spec_data);
    encode_varint_field(2, 1, &mut spec_data);
    encode_varint_field(3, 0, &mut spec_data);
    encode_bytes_field(1, &spec_data, &mut ver_data);
    let ver_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.version.api.CheckVersionNotice"), value: ver_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Broadcast, ver_notice, ctx.now_unix()).unwrap().1, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.milestone.api.ReachedNotice");
    assert_eq!(received[1].payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.definition.version.api.CheckVersionNotice");
}

#[tokio::test]
async fn quasar_community_track_definitions_and_instances() {
    // parity: quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_005, 90_000_005, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    let def_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj").expect("communitytrack definition");
    assert_eq!(def_fd.package, "eve_public.communitytrack.definition");
    assert_eq!(def_fd.messages[0].name, "Identifier");

    let inst_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj").expect("communitytrack instance");
    assert_eq!(inst_fd.package, "eve_public.communitytrack.instance");
    assert_eq!(inst_fd.messages.len(), 2);

    let (mut req_body, mut id_bytes) = (Vec::new(), Vec::new());
    encode_varint_field(1, 1, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut req_body);

    let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", req_body, b"corr-ctrack-01").await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-ctrack-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(payload.normalized_type_name(), "eve_public.communitytrack.api.GetResponse");
    let wm = WireMessage::decode(&payload.value).expect("decode GetResponse");
    assert!(wm.fields.iter().any(|f| f.field_number == 1));

    assert_eq!(reg.instantiate_populated_sample("eve_public.communitytrack.definition.Identifier", 101).unwrap().full_name, "eve_public.communitytrack.definition.Identifier");
    let sample_inst = reg.instantiate_populated_sample("eve_public.communitytrack.instance.Attributes", 102).expect("instantiate instance Attributes");
    assert_eq!(sample_inst.full_name, "eve_public.communitytrack.instance.Attributes");
    assert!(sample_inst.fields.contains_key(&1));
    assert!(sample_inst.fields.contains_key(&2));
}

#[tokio::test]
async fn quasar_campaign_core_and_contribution_methods() {
    // parity: quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_006, 90_000_006, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    let camp_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/campaign_pb2.pyj").expect("campaign_pb2");
    assert_eq!(camp_fd.package, "eve_public.campaign");
    assert!(camp_fd.messages.iter().any(|m| m.name == "Identifier"));
    assert!(camp_fd.messages.iter().any(|m| m.name == "Attributes"));

    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod.definition");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod.parameter");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj").unwrap().package, "eve_public.campaign.participation");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj").unwrap().package, "eve_public.campaign.sideffect");

    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new(), b"corr-camp-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(r1.correlation_uuid, b"corr-camp-01");
    assert_eq!(r1.payload.unwrap().normalized_type_name(), "eve_public.campaign.api.GetAllResponse");

    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new(), b"corr-camp-02").await;
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(r2.payload.unwrap().normalized_type_name(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver");

    let (mut start_data, mut camp_id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0x43, 0x4D, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut camp_id);
    encode_bytes_field(1, &camp_id, &mut start_data);
    let start_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.StartedNotice"), value: start_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Broadcast, start_notice, ctx.now_unix()).unwrap().1, 1);

    let mut comp_data = Vec::new();
    encode_bytes_field(1, &camp_id, &mut comp_data);
    let comp_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.CompletedNotice"), value: comp_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Broadcast, comp_notice, ctx.now_unix()).unwrap().1, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.api.StartedNotice");
    assert_eq!(received[1].payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.api.CompletedNotice");
}

#[tokio::test]
async fn quasar_campaign_objectives_tracks_and_notices() {
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_007, 90_000_007, 98_000_001);
    ctx.quasar_gateway.progression.extra.register_campaign(
        eve_server::services::quasar_progression_extra::CampaignRecord {
            id: 1, name: "Amarr-Minmatar Frontline Campaign".into(), description: "Secure strategic outposts".into(), active: true, objectives: vec![101],
        },
        vec![eve_server::services::quasar_progression_extra::CampaignObjectiveRecord {
            id: 101, campaign_id: 1, name: "Capture Outposts".into(),
            current_progress: 0, target_progress: 100, reward_pool_isk: 0,
            committed_participants: Vec::new(), contributors: Default::default(),
        }],
    );

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let obj_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj").expect("objective");
    assert_eq!(obj_fd.package, "eve_public.campaign.objective");
    assert!(obj_fd.messages.iter().any(|m| m.name == "Identifier"));
    assert!(obj_fd.messages.iter().any(|m| m.name == "Attributes"));

    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj").unwrap().package, "eve_public.campaign.track");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj").unwrap().package, "eve_public.campaign.parameter");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj").unwrap().package, "eve_public.campaign.ui");

    let (mut obj_id_req, mut obj_id_inner, mut obj_uuid) = (Vec::new(), Vec::new(), [0u8; 16]);
    obj_uuid[12..16].copy_from_slice(&101u32.to_be_bytes());
    encode_bytes_field(1, &obj_uuid, &mut obj_id_inner);
    encode_bytes_field(1, &obj_id_inner, &mut obj_id_req);
    let obj_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", obj_id_req, b"corr-obj-01").await;
    assert_eq!(obj_rsp.status_code, STATUS_OK);
    assert_eq!(obj_rsp.payload.unwrap().normalized_type_name(), "eve_public.campaign.objective.api.GetResponse");

    let trk_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", Vec::new(), b"corr-trk-01").await;
    assert_eq!(trk_rsp.status_code, STATUS_OK);
    assert_eq!(trk_rsp.payload.unwrap().normalized_type_name(), "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse");

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver");

    let (mut obj_notice_data, mut obj_id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0x4F, 0x42, 0x4A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut obj_id);
    encode_bytes_field(1, &obj_id, &mut obj_notice_data);
    encode_varint_field(2, 500, &mut obj_notice_data);
    let obj_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.ContributedNotice"), value: obj_notice_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_007), obj_notice, ctx.now_unix()).unwrap().1, 1);

    let (mut trk_notice_data, mut trk_id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0x54, 0x52, 0x4B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut trk_id);
    encode_bytes_field(1, &trk_id, &mut trk_notice_data);
    encode_varint_field(2, 1250, &mut trk_notice_data);
    let trk_notice = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.ContributedNotice"), value: trk_notice_data };
    assert_eq!(ctx.quasar_gateway.publish_notice(NoticeScope::Character(90_000_007), trk_notice, ctx.now_unix()).unwrap().1, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.objective.api.ContributedNotice");
    assert_eq!(received[1].payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.track.api.ContributedNotice");
}

#[tokio::test]
async fn quasar_campaign_triggers_and_event_matching() {
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(20_008, 90_000_008, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    let trigger_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj").expect("trigger_pb2");
    assert_eq!(trigger_fd.package, "eve_public.campaign.trigger");
    assert_eq!(trigger_fd.messages[0].name, "MaxFires");

    for (pyj, pkg) in [
        ("eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj", "eve_public.campaign.trigger.eventmatch.definition"),
        ("eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj", "eve_public.campaign.trigger.eventmatch"),
        ("eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj", "eve_public.campaign.trigger.immediate"),
        ("eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj", "eve_public.campaign.trigger.objectivecompleted"),
        ("eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj", "eve_public.campaign.trigger.objectivefailed"),
        ("eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj", "eve_public.campaign.trigger.relativeschedule"),
        ("eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj", "eve_public.campaign.trigger.tracktargetprogressreached"),
    ] {
        assert_eq!(reg.get_file_by_pyj(pyj).unwrap().package, pkg);
    }

    let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new(), b"corr-trig-01").await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-trig-01");
    assert_eq!(rsp.payload.unwrap().normalized_type_name(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse");

    let sample_immediate = reg.instantiate_populated_sample("eve_public.campaign.trigger.immediate.Configuration", 201).expect("instantiate immediate");
    assert_eq!(sample_immediate.full_name, "eve_public.campaign.trigger.immediate.Configuration");
    assert!(sample_immediate.fields.contains_key(&1));

    for (name, seed) in [
        ("eve_public.campaign.trigger.MaxFires", 202),
        ("eve_public.campaign.trigger.eventmatch.Configuration", 203),
        ("eve_public.campaign.trigger.objectivecompleted.Configuration", 204),
        ("eve_public.campaign.trigger.relativeschedule.Configuration", 205),
    ] {
        assert_eq!(reg.instantiate_populated_sample(name, seed).unwrap().full_name, name);
    }
}
