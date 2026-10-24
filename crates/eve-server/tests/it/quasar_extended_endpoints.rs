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
    corr_id: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr_id.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-extended-01".to_vec(),
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

// ============================================================================
// 1. Career Program Extended Endpoints & Definitions
// ============================================================================

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
    let career_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/career/career_pb2.pyj")
        .expect("career_pb2");
    assert_eq!(career_fd.package, "eve_public.career");
    let career_enum = career_fd
        .enums
        .iter()
        .find(|e| e.name == "Path")
        .expect("Path enum");
    assert_eq!(career_enum.values.len(), 5); // UNSPECIFIED, EXPLORATION, INDUSTRIALIST, ENFORCER, SOLDIER_OF_FORTUNE

    let goal_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/career/goal_pb2.pyj")
        .expect("goal_pb2");
    assert_eq!(goal_fd.package, "eve_public.career.goal");

    // 1. Query career goals GetAllRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
        b"corr-career-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.status_message, "OK");
    assert_eq!(rsp.correlation_uuid, b"corr-career-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );

    let wm = WireMessage::decode(&payload.value).expect("decode GetAllResponse");
    let goal_entries: Vec<_> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(goal_entries.len(), 8);

    // 2. Query GetDefinitionsRequest
    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetDefinitionsRequest",
        Vec::new(),
        b"corr-career-02",
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(
        def_rsp.payload.unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetDefinitionsResponse"
    );

    // 3. Emit live ProgressedNotice and verify notice envelope
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let mut notice_data = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &[0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut notice_data);
    encode_varint_field(2, 10, &mut notice_data);

    let notice_payload = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"),
        value: notice_data,
    };
    let (published, count) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_001), notice_payload, ctx.now_unix())
        .expect("publish_notice");
    assert_eq!(count, 1);
    assert_eq!(published.internal_origin, "evomu.quasar.notice_publisher");

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.ProgressedNotice"
    );
}

// ============================================================================
// 2. Skill Plan Extended Endpoints & Definitions
// ============================================================================

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

    // 1. Verify schema descriptors in registry
    let milestone_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj")
        .expect("skill/plan/milestone");
    assert_eq!(milestone_fd.package, "eve_public.skill.plan.milestone");

    let certified_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/skill/plan/certified_pb2.pyj")
        .expect("skill/plan/certified");
    assert_eq!(certified_fd.package, "eve_public.skill.plan.certified");

    let corp_cat_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj")
        .expect("corporation/skill/plan/category");
    assert_eq!(corp_cat_fd.package, "eve_public.corporation.skill.plan.category");

    let skilltype_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/skill/skilltype_pb2.pyj")
        .expect("skilltype");
    assert_eq!(skilltype_fd.package, "eve_public.skilltype");

    // 2. Query skill type via eve.skilltype.GetRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.skilltype.GetRequest",
        Vec::new(),
        b"corr-skill-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-skill-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(payload.normalized_type_name(), "eve.skilltype.GetResponse");

    let wm = WireMessage::decode(&payload.value).expect("decode skilltype");
    assert_eq!(wm.fields.len(), 1);

    // 3. Query corp skill plans
    let corp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.corporation.skill.plan.GetAllRequest",
        Vec::new(),
        b"corr-skill-02",
    )
    .await;
    assert_eq!(corp_rsp.status_code, STATUS_OK);
    assert_eq!(
        corp_rsp.payload.unwrap().normalized_type_name(),
        "eve_public.corporation.skill.plan.GetAllResponse"
    );

    // 4. Instantiate sample messages and verify roundtrip
    let sample_milestone = reg
        .instantiate_populated_sample("eve_public.skill.plan.milestone.Identifier", 42)
        .expect("instantiate milestone Identifier");
    assert_eq!(
        sample_milestone.full_name,
        "eve_public.skill.plan.milestone.Identifier"
    );

    let sample_certified = reg
        .instantiate_populated_sample("eve_public.skill.plan.certified.Identifier", 43)
        .expect("instantiate certified Identifier");
    assert_eq!(
        sample_certified.full_name,
        "eve_public.skill.plan.certified.Identifier"
    );

    let sample_corp_cat = reg
        .instantiate_populated_sample("eve_public.corporation.skill.plan.category.Identifier", 44)
        .expect("instantiate corp cat Identifier");
    assert_eq!(
        sample_corp_cat.full_name,
        "eve_public.corporation.skill.plan.category.Identifier"
    );

    let sample_skilltype = reg
        .instantiate_populated_sample("eve_public.skilltype.Identifier", 45)
        .expect("instantiate skilltype Identifier");
    assert_eq!(
        sample_skilltype.full_name,
        "eve_public.skilltype.Identifier"
    );
}

// ============================================================================
// 3. Achievement Category Endpoints & Notices
// ============================================================================

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

    // 1. Verify descriptors
    let cat_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj")
        .expect("category def");
    assert_eq!(cat_def_fd.package, "eve_public.achievement.category.definition");
    assert_eq!(cat_def_fd.messages[0].name, "Identifier");

    let cat_ms_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj")
        .expect("category milestone def");
    assert_eq!(cat_ms_fd.package, "eve_public.achievement.category.milestone.definition");
    assert_eq!(cat_ms_fd.messages[0].name, "Identifier");

    // 2. Admin Reset category score
    let mut reset_body = Vec::new();
    let mut cat_id_bytes = Vec::new();
    encode_bytes_field(1, &[1u8; 16], &mut cat_id_bytes);
    encode_bytes_field(1, &cat_id_bytes, &mut reset_body);

    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.admin.ResetRequest",
        reset_body,
        b"corr-ach-cat-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-ach-cat-01");

    // 3. Subscribe session for category notices
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 4. Emit category ProgressedNotice
    let mut cat_prog_data = Vec::new();
    let mut cat_id = Vec::new();
    encode_bytes_field(1, &[0xAA; 16], &mut cat_id);
    encode_bytes_field(1, &cat_id, &mut cat_prog_data);
    encode_varint_field(2, 450, &mut cat_prog_data);

    let cat_prog_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.ProgressedNotice"),
        value: cat_prog_data,
    };
    let (_, delivered1) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_003), cat_prog_notice, ctx.now_unix())
        .expect("publish_notice 1");
    assert_eq!(delivered1, 1);

    // 5. Emit category milestone ReachedNotice
    let mut cat_ms_data = Vec::new();
    encode_bytes_field(1, &cat_id, &mut cat_ms_data);
    let mut ms_id = Vec::new();
    encode_bytes_field(1, &[0xBB; 16], &mut ms_id);
    encode_bytes_field(2, &ms_id, &mut cat_ms_data);

    let cat_ms_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.milestone.api.ReachedNotice"),
        value: cat_ms_data,
    };
    let (_, delivered2) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_003), cat_ms_notice, ctx.now_unix())
        .expect("publish_notice 2");
    assert_eq!(delivered2, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.category.api.ProgressedNotice"
    );
    assert_eq!(
        received[1].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.category.milestone.api.ReachedNotice"
    );
}

// ============================================================================
// 4. Achievement Milestones, Rewards, Contributions & Domain Events
// ============================================================================

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

    // 1. Verify schema definitions
    let ach_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj")
        .expect("achievement def");
    assert_eq!(ach_def_fd.package, "eve_public.achievement.definition");

    let rew_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj")
        .expect("reward def");
    assert_eq!(rew_def_fd.package, "eve_public.achievement.reward.definition");

    let ms_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj")
        .expect("milestone def");
    assert_eq!(ms_def_fd.package, "eve_public.achievement.milestone.definition");

    let contrib_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj")
        .expect("contribution");
    assert_eq!(contrib_fd.package, "eve_public.achievement.contribution");

    let events_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/achievement/api/events_pb2.pyj")
        .expect("events");
    assert_eq!(events_fd.package, "eve_public.achievement.api");
    assert_eq!(events_fd.messages.len(), 4); // Pinned, Unpinned, Tracked, Untracked

    // 2. Publish domain events via Events.Publish
    let mut pin_data = Vec::new();
    let mut char_id_bytes = Vec::new();
    encode_varint_field(1, 90_000_004, &mut char_id_bytes);
    encode_bytes_field(1, &char_id_bytes, &mut pin_data);
    let mut ach_id_bytes = Vec::new();
    encode_bytes_field(1, &[0x41, 0x43, 0x48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut ach_id_bytes);
    encode_bytes_field(2, &ach_id_bytes, &mut pin_data);

    let event = EvePublicEvent {
        occurred: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        uuid: b"evt-uuid-0000001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.Pinned"),
            value: pin_data,
        }),
        journey: b"jrn-01".to_vec(),
        external_origin: "eve-client".to_string(),
        application_instance_uuid: b"app-extended-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway.publish_event(&sess, event, ctx.now_unix());

    let published_events = ctx.quasar_gateway.published_events.read().unwrap();
    assert_eq!(published_events.len(), 1);
    assert_eq!(
        published_events[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.api.Pinned"
    );
    drop(published_events);

    // 3. Emit milestone reached notice and version notice
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let mut ms_notice_data = Vec::new();
    encode_bytes_field(1, &ach_id_bytes, &mut ms_notice_data);
    let mut ms_uuid = Vec::new();
    encode_bytes_field(1, &[0x31; 16], &mut ms_uuid);
    encode_bytes_field(2, &ms_uuid, &mut ms_notice_data);

    let ms_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.milestone.api.ReachedNotice"),
        value: ms_notice_data,
    };
    let (_, delivered1) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_004), ms_notice, ctx.now_unix())
        .expect("publish_notice");
    assert_eq!(delivered1, 1);

    // Emit version check notice
    let mut ver_data = Vec::new();
    let mut spec_data = Vec::new();
    encode_varint_field(1, 24, &mut spec_data); // major
    encode_varint_field(2, 1, &mut spec_data);  // minor
    encode_varint_field(3, 0, &mut spec_data);  // patch
    encode_bytes_field(1, &spec_data, &mut ver_data);

    let ver_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.version.api.CheckVersionNotice"),
        value: ver_data,
    };
    let (_, delivered2) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Broadcast, ver_notice, ctx.now_unix())
        .expect("publish_notice");
    assert_eq!(delivered2, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.milestone.api.ReachedNotice"
    );
    assert_eq!(
        received[1].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.definition.version.api.CheckVersionNotice"
    );
}

// ============================================================================
// 5. Community Track Definitions & Instances
// ============================================================================

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

    // 1. Verify descriptors in registry
    let def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj")
        .expect("communitytrack definition");
    assert_eq!(def_fd.package, "eve_public.communitytrack.definition");
    assert_eq!(def_fd.messages[0].name, "Identifier");

    let inst_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj")
        .expect("communitytrack instance");
    assert_eq!(inst_fd.package, "eve_public.communitytrack.instance");
    assert_eq!(inst_fd.messages.len(), 2); // Identifier, Attributes

    // 2. Query community track via eve_public.communitytrack.api.GetRequest
    let mut req_body = Vec::new();
    let mut id_bytes = Vec::new();
    encode_varint_field(1, 1, &mut id_bytes); // sequential id = 1
    encode_bytes_field(1, &id_bytes, &mut req_body);

    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.communitytrack.api.GetRequest",
        req_body,
        b"corr-ctrack-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-ctrack-01");
    let payload = rsp.payload.expect("payload present");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.communitytrack.api.GetResponse"
    );

    // 3. Dynamic message instantiation and field verification
    let sample_def = reg
        .instantiate_populated_sample("eve_public.communitytrack.definition.Identifier", 101)
        .expect("instantiate def Identifier");
    assert_eq!(
        sample_def.full_name,
        "eve_public.communitytrack.definition.Identifier"
    );

    let sample_inst = reg
        .instantiate_populated_sample("eve_public.communitytrack.instance.Attributes", 102)
        .expect("instantiate instance Attributes");
    assert_eq!(
        sample_inst.full_name,
        "eve_public.communitytrack.instance.Attributes"
    );
    assert!(sample_inst.fields.contains_key(&1)); // definition
    assert!(sample_inst.fields.contains_key(&2)); // progress
}

// ============================================================================
// 6. Campaign Core & Contribution Methods
// ============================================================================

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

    // 1. Verify descriptors
    let camp_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/campaign_pb2.pyj")
        .expect("campaign_pb2");
    assert_eq!(camp_fd.package, "eve_public.campaign");
    assert!(camp_fd.messages.iter().any(|m| m.name == "Identifier"));
    assert!(camp_fd.messages.iter().any(|m| m.name == "Attributes"));

    let cm_conf_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj")
        .expect("cm configuration");
    assert_eq!(cm_conf_fd.package, "eve_public.campaign.contributionmethod");

    let cm_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj")
        .expect("cm definition");
    assert_eq!(cm_def_fd.package, "eve_public.campaign.contributionmethod.definition");

    let cm_param_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj")
        .expect("cm parameter");
    assert_eq!(cm_param_fd.package, "eve_public.campaign.contributionmethod.parameter");

    let part_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj")
        .expect("participation");
    assert_eq!(part_fd.package, "eve_public.campaign.participation");

    let side_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj")
        .expect("sideffect");
    assert_eq!(side_fd.package, "eve_public.campaign.sideffect");

    // 2. Query campaign GetAllRequest
    let r1 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.api.GetAllRequest",
        Vec::new(),
        b"corr-camp-01",
    )
    .await;
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(r1.correlation_uuid, b"corr-camp-01");
    assert_eq!(
        r1.payload.unwrap().normalized_type_name(),
        "eve_public.campaign.api.GetAllResponse"
    );

    // 3. Query contributionmethod definition GetAllRequest
    let r2 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        Vec::new(),
        b"corr-camp-02",
    )
    .await;
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(
        r2.payload.unwrap().normalized_type_name(),
        "eve_public.campaign.contributionmethod.definition.api.GetAllResponse"
    );

    // 4. Emit campaign lifecycle notices
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let mut start_data = Vec::new();
    let mut camp_id = Vec::new();
    encode_bytes_field(1, &[0x43, 0x4D, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut camp_id);
    encode_bytes_field(1, &camp_id, &mut start_data);

    let start_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.StartedNotice"),
        value: start_data,
    };
    let (_, del1) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Broadcast, start_notice, ctx.now_unix())
        .expect("publish start");
    assert_eq!(del1, 1);

    let mut comp_data = Vec::new();
    encode_bytes_field(1, &camp_id, &mut comp_data);
    let comp_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.CompletedNotice"),
        value: comp_data,
    };
    let (_, del2) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Broadcast, comp_notice, ctx.now_unix())
        .expect("publish comp");
    assert_eq!(del2, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.api.StartedNotice"
    );
    assert_eq!(
        received[1].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.api.CompletedNotice"
    );
}

// ============================================================================
// 7. Campaign Objectives, Tracks & Notices
// ============================================================================

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

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");

    // 1. Verify schema definitions
    let obj_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj")
        .expect("objective");
    assert_eq!(obj_fd.package, "eve_public.campaign.objective");
    assert!(obj_fd.messages.iter().any(|m| m.name == "Identifier"));
    assert!(obj_fd.messages.iter().any(|m| m.name == "Attributes"));

    let track_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj")
        .expect("track");
    assert_eq!(track_fd.package, "eve_public.campaign.track");

    let param_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj")
        .expect("parameter");
    assert_eq!(param_fd.package, "eve_public.campaign.parameter");

    let ui_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj")
        .expect("ui");
    assert_eq!(ui_fd.package, "eve_public.campaign.ui");

    // 2. Query objective and track RPCs
    let mut obj_id_req = Vec::new();
    let mut obj_id_inner = Vec::new();
    let mut obj_uuid = [0u8; 16];
    obj_uuid[12..16].copy_from_slice(&101u32.to_be_bytes());
    encode_bytes_field(1, &obj_uuid, &mut obj_id_inner);
    encode_bytes_field(1, &obj_id_inner, &mut obj_id_req);
    let obj_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.objective.api.GetRequest",
        obj_id_req,
        b"corr-obj-01",
    )
    .await;
    assert_eq!(obj_rsp.status_code, STATUS_OK);
    assert_eq!(
        obj_rsp.payload.unwrap().normalized_type_name(),
        "eve_public.campaign.objective.api.GetResponse"
    );

    let trk_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        Vec::new(),
        b"corr-trk-01",
    )
    .await;
    assert_eq!(trk_rsp.status_code, STATUS_OK);
    assert_eq!(
        trk_rsp.payload.unwrap().normalized_type_name(),
        "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"
    );

    // 3. Emit ContributedNotice for objective and track
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let mut obj_notice_data = Vec::new();
    let mut obj_id = Vec::new();
    encode_bytes_field(1, &[0x4F, 0x42, 0x4A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut obj_id);
    encode_bytes_field(1, &obj_id, &mut obj_notice_data);
    encode_varint_field(2, 500, &mut obj_notice_data);

    let obj_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.objective.api.ContributedNotice"),
        value: obj_notice_data,
    };
    let (_, del1) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_007), obj_notice, ctx.now_unix())
        .expect("publish obj notice");
    assert_eq!(del1, 1);

    let mut trk_notice_data = Vec::new();
    let mut trk_id = Vec::new();
    encode_bytes_field(1, &[0x54, 0x52, 0x4B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut trk_id);
    encode_bytes_field(1, &trk_id, &mut trk_notice_data);
    encode_varint_field(2, 1250, &mut trk_notice_data);

    let trk_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.track.api.ContributedNotice"),
        value: trk_notice_data,
    };
    let (_, del2) = ctx
        .quasar_gateway
        .publish_notice(NoticeScope::Character(90_000_007), trk_notice, ctx.now_unix())
        .expect("publish trk notice");
    assert_eq!(del2, 1);

    let received = drain_notices(&mut stream_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.objective.api.ContributedNotice"
    );
    assert_eq!(
        received[1].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.track.api.ContributedNotice"
    );
}

// ============================================================================
// 8. Campaign Triggers & Event Matching
// ============================================================================

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

    // 1. Verify trigger descriptors
    let trigger_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj")
        .expect("trigger_pb2");
    assert_eq!(trigger_fd.package, "eve_public.campaign.trigger");
    assert_eq!(trigger_fd.messages[0].name, "MaxFires");

    let em_def_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj")
        .expect("eventmatch definition");
    assert_eq!(em_def_fd.package, "eve_public.campaign.trigger.eventmatch.definition");

    let em_conf_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj")
        .expect("eventmatch config");
    assert_eq!(em_conf_fd.package, "eve_public.campaign.trigger.eventmatch");

    let imm_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj")
        .expect("immediate");
    assert_eq!(imm_fd.package, "eve_public.campaign.trigger.immediate");

    let obj_comp_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj")
        .expect("objectivecompleted");
    assert_eq!(obj_comp_fd.package, "eve_public.campaign.trigger.objectivecompleted");

    let obj_fail_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj")
        .expect("objectivefailed");
    assert_eq!(obj_fail_fd.package, "eve_public.campaign.trigger.objectivefailed");

    let rel_sched_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj")
        .expect("relativeschedule");
    assert_eq!(rel_sched_fd.package, "eve_public.campaign.trigger.relativeschedule");

    let track_target_fd = reg
        .get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj")
        .expect("tracktargetprogressreached");
    assert_eq!(track_target_fd.package, "eve_public.campaign.trigger.tracktargetprogressreached");

    // 2. Query eventmatch definitions
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        Vec::new(),
        b"corr-trig-01",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(rsp.correlation_uuid, b"corr-trig-01");
    assert_eq!(
        rsp.payload.unwrap().normalized_type_name(),
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"
    );

    // 3. Dynamic schema instantiation and validation for trigger message types
    let sample_immediate = reg
        .instantiate_populated_sample("eve_public.campaign.trigger.immediate.Configuration", 201)
        .expect("instantiate immediate");
    assert_eq!(
        sample_immediate.full_name,
        "eve_public.campaign.trigger.immediate.Configuration"
    );
    assert!(sample_immediate.fields.contains_key(&1)); // key

    let sample_max_fires = reg
        .instantiate_populated_sample("eve_public.campaign.trigger.MaxFires", 202)
        .expect("instantiate MaxFires");
    assert_eq!(
        sample_max_fires.full_name,
        "eve_public.campaign.trigger.MaxFires"
    );

    let sample_eventmatch = reg
        .instantiate_populated_sample("eve_public.campaign.trigger.eventmatch.Configuration", 203)
        .expect("instantiate eventmatch");
    assert_eq!(
        sample_eventmatch.full_name,
        "eve_public.campaign.trigger.eventmatch.Configuration"
    );

    let sample_obj_comp = reg
        .instantiate_populated_sample("eve_public.campaign.trigger.objectivecompleted.Configuration", 204)
        .expect("instantiate obj comp");
    assert_eq!(
        sample_obj_comp.full_name,
        "eve_public.campaign.trigger.objectivecompleted.Configuration"
    );

    let sample_rel_sched = reg
        .instantiate_populated_sample("eve_public.campaign.trigger.relativeschedule.Configuration", 205)
        .expect("instantiate rel sched");
    assert_eq!(
        sample_rel_sched.full_name,
        "eve_public.campaign.trigger.relativeschedule.Configuration"
    );
}
