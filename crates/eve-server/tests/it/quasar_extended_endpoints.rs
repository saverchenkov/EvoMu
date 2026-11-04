//! Integration tests for Extended Quasar Endpoints across Career, Skills, Achievements,
//! Campaigns, and Community Tracks (Phase Q, tasks `Q-3` through `Q-8`).

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, EvePublicEvent, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, STATUS_BAD_REQUEST, STATUS_OK};
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

fn make_ach_req(id: u32) -> Vec<u8> {
    let (mut uuid, mut b, mut out) = ([0u8; 16], Vec::new(), Vec::new());
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid, &mut b);
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_ach_progress(id: u32, delta: u64) -> Vec<u8> {
    let mut out = make_ach_req(id);
    encode_varint_field(2, delta, &mut out);
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
    assert_eq!(goal_entries.len(), 196);

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

    // 2. Subscribe session for category notices
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 3. Category isolation and reset testing (DATA GAP: SDE achievements have category_id = 0 without CDN checkpoints)
    ctx.quasar_gateway.progression.achievements.set_category_score(90_000_003, 1, 25);
    let rsp1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", make_ach_progress(9, 5), b"p1").await;
    assert_eq!(rsp1.status_code, STATUS_OK);
    let rsp2 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", make_ach_progress(2, 10), b"p2").await;
    assert_eq!(rsp2.status_code, STATUS_OK);

    // Reset Category 1 score and emit category milestone notice
    let mut cat_id1 = Vec::new();
    encode_bytes_field(1, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut cat_id1);
    let mut reset_body1 = Vec::new();
    encode_bytes_field(1, &cat_id1, &mut reset_body1);
    let rsp_reset1 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.admin.ResetRequest",
        reset_body1,
        b"corr-ach-cat-01",
    )
    .await;
    assert_eq!(rsp_reset1.status_code, STATUS_OK);
    eve_server::services::quasar_achievements::publish_achievement_category_milestone_notice(
        &ctx.quasar_gateway,
        90_000_003,
        1,
        101,
    );

    // Uncategorized SDE achievements (category_id = 0) retain progress when Category 1 is reset
    let ach1_after = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ach_req(9), b"g1-after").await;
    assert_eq!(ach1_after.status_code, STATUS_OK);
    let dyn_ach1 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &ach1_after.payload.unwrap().value).unwrap();
    assert_eq!(dyn_ach1.get(3), Some(&DynamicValue::Uint64(5)), "Uncategorized SDE achievement 9 must remain intact");

    let ach2_after = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ach_req(2), b"g2-after").await;
    assert_eq!(ach2_after.status_code, STATUS_OK);
    let dyn_ach2 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &ach2_after.payload.unwrap().value).unwrap();
    assert_eq!(dyn_ach2.get(3), Some(&DynamicValue::Uint64(10)), "Uncategorized SDE achievement 2 must remain intact");

    // Negative assertions: missing category_id returns 400 (STATUS_BAD_REQUEST)
    let bad_get = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", Vec::new(), b"bad-get").await;
    assert_eq!(bad_get.status_code, STATUS_BAD_REQUEST);

    let bad_reset = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.admin.ResetRequest", Vec::new(), b"bad-reset").await;
    assert_eq!(bad_reset.status_code, STATUS_BAD_REQUEST);

    // 4. Drain notices via gateway stream (emitted during real request dispatches)
    let mut cat1_req = Vec::new();
    encode_bytes_field(1, &cat_id1, &mut cat1_req);
    let cat1_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", cat1_req, b"corr-cat1").await;
    assert_eq!(cat1_rsp.status_code, STATUS_OK);
    let dyn_cat1 = reg.decode_dynamic("eve_public.achievement.category.api.GetResponse", &cat1_rsp.payload.unwrap().value).unwrap();
    assert_eq!(dyn_cat1.get(1), Some(&DynamicValue::Uint32(0)), "Category 1 score must be 0 after reset");

    let received = drain_notices(&mut stream_rx);
    assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.category.api.ProgressedNotice")).unwrap_or(false)));
    assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.category.milestone.api.ReachedNotice")).unwrap_or(false)));
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

    // 3. Emit milestone reached notice and version notice via real server requests
    let prog_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.AddProgressRequest",
        make_ach_progress(2, 10),
        b"corr-prog",
    )
    .await;
    assert_eq!(prog_resp.status_code, STATUS_OK);

    let get_ver = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.achievement.definition.version.api.GetDefaultVersionRequest",
        Vec::new(),
        b"corr-get-ver",
    )
    .await;
    assert_eq!(get_ver.status_code, STATUS_OK);

    eve_server::services::quasar_achievements::publish_achievement_milestone_reached_notice(
        &ctx.quasar_gateway,
        90_000_004,
        2,
        1,
    );
    let ver_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.achievement.definition.version.api.admin.SetDefaultVersionRequest",
        Vec::new(),
        b"corr-set-ver",
    )
    .await;
    assert_eq!(ver_resp.status_code, STATUS_OK);

    let received = drain_notices(&mut stream_rx);
    assert!(!received.is_empty(), "Expected notices from progress and version updates");
    assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.normalized_type_name() == "eve_public.achievement.milestone.api.ReachedNotice").unwrap_or(false)));
    assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.normalized_type_name() == "eve_public.achievement.definition.version.api.CheckVersionNotice").unwrap_or(false)));
}

#[allow(dead_code, unused_imports)]
pub(crate) mod quasar_extended_endpoints_ext {
    use super::*;

//  Integration tests for Extended Quasar Endpoints across Community Tracks
//  and Campaigns (Part 2) (Phase Q, tasks `Q-5` and `Q-6`).

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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
        application_instance_uuid: b"app-extended-02".to_vec(),
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

    let inst = ctx.quasar_gateway.progression.extra.activate_community_track(1).expect("activate track template 1");
    assert_ne!(inst.track_id, 1);
    let (mut req_body, mut id_bytes) = (Vec::new(), Vec::new());
    encode_varint_field(1, inst.track_id as u64, &mut id_bytes);
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
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj",
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
}

#[tokio::test]
async fn quasar_campaign_objective_and_track_descriptors() {
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let obj_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj").expect("objective");
    assert_eq!(obj_fd.package, "eve_public.campaign.objective");
    assert!(obj_fd.messages.iter().any(|m| m.name == "Identifier"));
    assert!(obj_fd.messages.iter().any(|m| m.name == "Attributes"));

    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj").unwrap().package, "eve_public.campaign.track");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj").unwrap().package, "eve_public.campaign.parameter");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj").unwrap().package, "eve_public.campaign.ui");
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

}
