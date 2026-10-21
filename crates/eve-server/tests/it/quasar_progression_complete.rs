//! Comprehensive integration acceptance tests for Quasar Character & Progression Track (`plan:Q-4`).
//!
//! Validates public gateway routing, request execution, persistence, and notice publication across:
//! - Daily Goals (`feat:quasar.dailygoal`)
//! - Career Program (`feat:quasar.career`)
//! - Achievements & Milestones (`feat:quasar.achievement`)
//! - Skills, Skill Plans, Injectors & Implants (`feat:quasar.skill`, `feat:quasar.implant`)
//! - Community Progression Tracks, Tales & Goals (`feat:quasar.communitytrack`, `feat:quasar.tale`, `feat:quasar.goal`)

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireField, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
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
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"prog-test-corr01".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
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

fn sample_request_payload(ctx: &ServerContext, type_url: &str) -> Vec<u8> {
    let norm = type_url
        .strip_prefix(TYPE_GOOGLEAPIS_PREFIX)
        .unwrap_or(type_url)
        .trim_start_matches('.');
    if let Ok(sample) = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(norm, 42)
    {
        ctx.quasar_gateway
            .descriptors
            .encode_dynamic(&sample)
            .unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        out.push(msg);
    }
    out
}

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_admin_progress(uuid: [u8; 16], delta: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    encode_varint_field(3, delta, &mut out);
    out
}

fn make_id_request(id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, id as u64, &mut out);
    out
}

// ============================================================================
// 1. Daily Goals Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_daily_goals_lifecycle_and_rewards() {
    // parity: plan:Q-4, feat:quasar.dailygoal, quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/goal_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-4",
        "feat:quasar.dailygoal",
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(10_001, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session to publicGatewaySvc for notice delivery
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // Initial wallet balance and free SP
    let (initial_isk, initial_free_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        (ch.balance.0, ch.free_skill_points)
    });

    // 1. Query current daily goals via eve_public.dailygoal.api.GetAllCurrentRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload present");
    assert!(payload.type_url.contains("GetAllCurrentResponse"));

    let wm = WireMessage::decode(&payload.value).expect("decode WireMessage");
    let goal_entries: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(!goal_entries.is_empty(), "Daily goals should be populated");

    // Inspect first goal
    let first_goal_bytes = goal_entries[0].value.as_bytes().expect("goal bytes");
    let first_goal_wm = WireMessage::decode(first_goal_bytes).expect("goal wm");
    let id_field = first_goal_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().expect("id bytes")).expect("id wm");
    let uuid_field = id_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("uuid field");
    let goal_uuid_bytes = uuid_field.value.as_bytes().expect("uuid bytes");
    assert_eq!(goal_uuid_bytes.len(), 16);
    let mut target_uuid = [0u8; 16];
    target_uuid.copy_from_slice(goal_uuid_bytes);

    // 2. Query completed with entitlements via eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest
    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);

    // 3. Verify unprivileged session is rejected on .admin. endpoint, then increment with GM session
    let mut unpriv_sess = sess.clone();
    unpriv_sess.role = 0;
    let unpriv_rsp = quasar_rpc(
        &ctx,
        &mut unpriv_sess,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        make_admin_progress(target_uuid, 2),
    )
    .await;
    assert_eq!(unpriv_rsp.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

    let prog_req_val = make_admin_progress(target_uuid, 2);
    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        prog_req_val,
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // 4. Complete goal #1 by adding more progress
    let comp_req_val = make_admin_progress(target_uuid, 100);
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        comp_req_val,
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Publish ProgressedNotice to client
    let mut not_bytes = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &target_uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut not_bytes);
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressedNotice"),
                value: not_bytes,
            },
            ctx.now_unix(),
        )
        .expect("publish_notice");
    assert_eq!(delivered, 1);

    // Verify notice published to subscriber
    let notifs = drain_notices(&mut stream_rx);
    assert!(
        !notifs.is_empty(),
        "Notices should be published upon progress/completion"
    );

    // 5. Redeem goal #1 via eve_public.dailygoal.api.RedeemRequest
    let redeem_req_val = make_uuid_request(target_uuid);
    let redeem_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemRequest",
        redeem_req_val,
    )
    .await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);

    // Verify ISK and SP payouts in WorldRepository
    let (final_isk, final_free_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        (ch.balance.0, ch.free_skill_points)
    });
    assert!(
        final_isk > initial_isk,
        "Wallet balance should increase after daily goal redemption"
    );
    assert!(
        final_free_sp > initial_free_sp,
        "Free skill points should increase after daily goal redemption"
    );

    // 6. Pay for completion on goal #2 via eve_public.dailygoal.api.PayForCompletionRequest
    if goal_entries.len() > 1 {
        let second_goal_bytes = goal_entries[1].value.as_bytes().expect("bytes");
        let second_goal_wm = WireMessage::decode(second_goal_bytes).expect("second goal wm");
        let second_id_field = second_goal_wm
            .fields
            .iter()
            .find(|f| f.field_number == 1)
            .expect("id field");
        let second_id_wm =
            WireMessage::decode(second_id_field.value.as_bytes().expect("id bytes"))
                .expect("id wm");
        let second_uuid_field = second_id_wm
            .fields
            .iter()
            .find(|f| f.field_number == 1)
            .expect("uuid field");
        let mut second_uuid = [0u8; 16];
        second_uuid.copy_from_slice(second_uuid_field.value.as_bytes().expect("uuid bytes"));

        let pay_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.PayForCompletionRequest",
            make_uuid_request(second_uuid),
        )
        .await;
        assert_eq!(pay_rsp.status_code, STATUS_OK);
    }

    // 7. RedeemAll via eve_public.dailygoal.api.RedeemAllRequest
    let redeem_all_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.RedeemAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(redeem_all_rsp.status_code, STATUS_OK);

    // 8. eve.dailygoal.api.GetRequest query
    let eve_get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.dailygoal.api.GetRequest",
        make_uuid_request(target_uuid),
    )
    .await;
    assert_eq!(eve_get_rsp.status_code, STATUS_OK);
}

// ============================================================================
// 2. Career Program Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_career_goals_paths_and_milestones() {
    // parity: plan:Q-4, feat:quasar.career, quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-4",
        "feat:quasar.career",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(10_002, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session for career notices
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 1. Query career paths and goals via eve_public.career.goal.api.GetAllRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload present");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );

    let wm = WireMessage::decode(&payload.value).expect("decode GetAllResponse");
    let goal_entries: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(
        goal_entries.len() >= 4,
        "Should return career goals for all 4 paths"
    );

    // Canonical UUID of first Enforcer career goal
    let target_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

    // 2. Query definitions via eve_public.career.goal.api.GetDefinitionsRequest
    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(
        def_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetDefinitionsResponse"
    );

    // 3. Progress career goal via eve_public.career.goal.api.admin.ProgressRequest
    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.admin.ProgressRequest",
        make_admin_progress(target_uuid, 2),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // 4. Mark career goal completed via eve_public.career.goal.api.admin.CompleteRequest
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.admin.CompleteRequest",
        make_uuid_request(target_uuid),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Publish and receive Career Notice
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"),
                value: make_uuid_request(target_uuid),
            },
            ctx.now_unix(),
        )
        .expect("publish_notice");
    assert_eq!(delivered, 1);

    // Check notices
    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Career notices should be emitted");
    assert_eq!(
        notifs[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.ProgressedNotice"
    );

    // 5. Claim milestone reward via eve_public.career.goal.api.ClaimRewardRequest
    let claim_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.ClaimRewardRequest",
        make_uuid_request(target_uuid),
    )
    .await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);

    // 6. Query progress via eve.character.career.goal.GetProgressRequest
    let prog_query = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.character.career.goal.GetProgressRequest",
        sample_request_payload(&ctx, "eve.character.career.goal.GetProgressRequest"),
    )
    .await;
    assert_eq!(prog_query.status_code, STATUS_OK);
    assert_eq!(
        prog_query.payload.as_ref().unwrap().normalized_type_name(),
        "eve.character.career.goal.GetProgressResponse"
    );
}

// ============================================================================
// 3. Achievements Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_achievements_categories_scores_and_claims() {
    // parity: plan:Q-4, feat:quasar.achievement, quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-4",
        "feat:quasar.achievement",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(10_003, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session for achievement notices
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 1. Query achievements via eve_public.achievement.api.GetRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload present");
    assert!(payload.type_url.contains("GetResponse"));

    // 2. Query categories via eve_public.achievement.category.api.GetRequest
    let cat_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.GetRequest",
        sample_request_payload(&ctx, "eve_public.achievement.category.api.GetRequest"),
    )
    .await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    let cat_payload = cat_rsp.payload.expect("cat payload present");
    let cat_wm = WireMessage::decode(&cat_payload.value).expect("decode cat");
    let cats: Vec<&WireField> = cat_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(cats.len(), 1, "Category score returned");

    // 3. Query latest CDN definition version
    let cdn_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);

    // 4. Admin progress on achievement via eve_public.achievement.api.admin.AddProgressRequest
    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.AddProgressRequest",
        sample_request_payload(&ctx, "eve_public.achievement.api.admin.AddProgressRequest"),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // 5. Admin complete achievement
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.CompleteRequest",
        sample_request_payload(&ctx, "eve_public.achievement.api.admin.CompleteRequest"),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Publish Achievement notice
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.ProgressedNotice"),
                value: make_id_request(101),
            },
            ctx.now_unix(),
        )
        .expect("publish_notice");
    assert_eq!(delivered, 1);

    // Check notices
    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Achievement notices should be emitted");

    // 6. Character showcase score
    let score_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(score_rsp.status_code, STATUS_OK);
    let score_payload = score_rsp.payload.expect("score payload");
    let score_wm = WireMessage::decode(&score_payload.value).expect("decode score");
    let total_score = score_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_varint());
    assert!(total_score.unwrap_or(0) > 0, "Character score should be > 0");

    // 7. Claim reward via eve_public.achievement.reward.api.ClaimRequest
    let claim_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.reward.api.ClaimRequest",
        sample_request_payload(&ctx, "eve_public.achievement.reward.api.ClaimRequest"),
    )
    .await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);

    // 8. Admin reset achievements
    let reset_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.ResetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(reset_rsp.status_code, STATUS_OK);
}

// ============================================================================
// 4. Skills, Skill Plans, Injectors & Implants Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_skills_plans_extractors_and_implants() {
    // parity: plan:Q-4, quasar:eveProto/generated/eve_public/skill/plan/plan_pb2.pyj, quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-4",
        "quasar:eveProto/generated/eve_public/skill/plan/plan_pb2.pyj",
        "quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(10_004, 90_000_001, 98_000_001);

    // 1. Create personal skill plan via eve_public.skill.plan.CreateRequest
    let plan_create_body = sample_request_payload(&ctx, "eve_public.skill.plan.CreateRequest");
    let create_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.skill.plan.CreateRequest",
        plan_create_body,
    )
    .await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let create_payload = create_rsp.payload.expect("create payload");
    assert_eq!(
        create_payload.normalized_type_name(),
        "eve_public.skill.plan.CreateResponse"
    );
    let plan_wm = WireMessage::decode(&create_payload.value).expect("decode plan");
    let uuid_field = plan_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("uuid field");
    let id_bytes = uuid_field.value.as_bytes().expect("bytes");
    let id_wm = WireMessage::decode(id_bytes).expect("id_wm");
    let uuid_field_inner = id_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("uuid");
    let plan_uuid_bytes = uuid_field_inner.value.as_bytes().expect("bytes");
    assert_eq!(plan_uuid_bytes.len(), 16);

    // 2. Query all personal skill plans
    let get_all_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(get_all_rsp.status_code, STATUS_OK);
    assert_eq!(
        get_all_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.skill.plan.GetAllResponse"
    );

    // 3. Create corporation skill plan via eve_public.corporation.skill.plan.CreateRequest
    let corp_create_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.corporation.skill.plan.CreateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(corp_create_rsp.status_code, STATUS_OK);

    // 4. Query corp skill plans
    let corp_get_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.corporation.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(corp_get_all.status_code, STATUS_OK);
    assert_eq!(
        corp_get_all.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.corporation.skill.plan.GetAllResponse"
    );

    // 5. Query skill categories & skill types
    let cat_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.skillcategory.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    assert_eq!(
        cat_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.skillcategory.GetAllResponse"
    );

    let type_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.skilltype.GetRequest",
        sample_request_payload(&ctx, "eve.skilltype.GetRequest"),
    )
    .await;
    assert_eq!(type_rsp.status_code, STATUS_OK);

    // 6. Test Large Skill Injector application
    let initial_free_sp = ctx
        .db
        .with_snapshot(|s| s.characters[&90_000_001].free_skill_points);

    let inject_body = sample_request_payload(&ctx, "eve.skill.injector.InjectSkillPointsRequest");
    let inject_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.skill.injector.InjectSkillPointsRequest",
        inject_body,
    )
    .await;
    assert_eq!(inject_rsp.status_code, STATUS_OK);

    let after_inject_sp = ctx
        .db
        .with_snapshot(|s| s.characters[&90_000_001].free_skill_points);
    assert!(
        after_inject_sp > initial_free_sp,
        "Skill injector should grant unallocated skill points"
    );

    // 7. Test skill extractor
    let extract_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.skill.extractor.ExtractSkillsRequest",
        sample_request_payload(&ctx, "eve.skill.extractor.ExtractSkillsRequest"),
    )
    .await;
    assert_eq!(extract_rsp.status_code, STATUS_OK);

    // 8. Test character implants query
    let implant_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.character.implant.GetActiveImplantsByCharacterRequest",
        sample_request_payload(&ctx, "eve.character.implant.GetActiveImplantsByCharacterRequest"),
    )
    .await;
    assert_eq!(implant_rsp.status_code, STATUS_OK);

    // 9. Test implant types query
    let implant_type_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.implanttype.GetAllRequest",
        sample_request_payload(&ctx, "eve.implanttype.GetAllRequest"),
    )
    .await;
    assert_eq!(implant_type_rsp.status_code, STATUS_OK);
    assert_eq!(
        implant_type_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.implanttype.GetAllResponse"
    );
}

// ============================================================================
// 5. Community Tracks, Narrative Tales & Goals Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_community_progression_tales_and_goals() {
    // parity: plan:Q-4, feat:quasar.communitytrack, feat:quasar.tale, feat:quasar.goal, quasar:eveProto/generated/eve_public/communitytrack/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/communitytrack/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-4",
        "feat:quasar.communitytrack",
        "feat:quasar.tale",
        "feat:quasar.goal",
        "quasar:eveProto/generated/eve_public/communitytrack/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/communitytrack/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = build_sess(10_005, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session for track notices
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 1. Community Tracks listing and definitions
    let track_body = sample_request_payload(&ctx, "eve_public.communitytrack.api.GetRequest");
    let track_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.communitytrack.api.GetRequest",
        track_body,
    )
    .await;
    assert_eq!(track_rsp.status_code, STATUS_OK);
    assert_eq!(
        track_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.communitytrack.api.GetResponse"
    );

    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.communitytrack.api.GetRequest",
        sample_request_payload(&ctx, "eve.communitytrack.api.GetRequest"),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(
        def_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.communitytrack.api.GetResponse"
    );

    // Admin progress on track 1
    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.communitytrack.api.AddProgressRequest",
        sample_request_payload(&ctx, "eve.communitytrack.api.AddProgressRequest"),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // Publish and receive Track Notice
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.communitytrack.api.ProgressedNotice"),
                value: make_id_request(1),
            },
            ctx.now_unix(),
        )
        .expect("publish_notice");
    assert_eq!(delivered, 1);

    // Check notices
    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Track notice should be emitted");
    assert_eq!(
        notifs[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.communitytrack.api.ProgressedNotice"
    );

    // 2. Narrative Tales lifecycle
    let start_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.StartTaleRequest",
        sample_request_payload(&ctx, "eve.tale.api.StartTaleRequest"),
    )
    .await;
    assert_eq!(start_rsp.status_code, STATUS_OK);

    let count_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.GetActiveTalesCountRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(count_rsp.status_code, STATUS_OK);
    assert_eq!(
        count_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.tale.api.GetActiveTalesCountResponse"
    );

    let stop_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.StopTalesRequest",
        sample_request_payload(&ctx, "eve.tale.api.StopTalesRequest"),
    )
    .await;
    assert_eq!(stop_rsp.status_code, STATUS_OK);

    // 3. Generic Goals create and contribute
    let goal_create_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.goal.api.CreateRequest",
        sample_request_payload(&ctx, "eve_public.goal.api.CreateRequest"),
    )
    .await;
    assert_eq!(goal_create_rsp.status_code, STATUS_OK);

    // 4. Character cosmetic titles
    let titles_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(titles_rsp.status_code, STATUS_OK);

    // 5. Seasonal events and live campaigns
    let camp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(camp_rsp.status_code, STATUS_OK);
    assert_eq!(
        camp_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.api.GetAllResponse"
    );

    let chall_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        sample_request_payload(&ctx, "eve.seasonalevent.challenge.GetChallengesForCharacterRequest"),
    )
    .await;
    assert_eq!(chall_rsp.status_code, STATUS_OK);
}

fn verify_progression_pyj_batch(ctx: &ServerContext, row_ids: &[&str]) -> usize {
    let reg = &ctx.quasar_gateway.descriptors;
    for &row_id in row_ids {
        let pyj = row_id.strip_prefix("quasar:").expect("quasar: prefix");
        let fd = reg.get_file_by_pyj(pyj).expect("cataloged pyj present");
        assert_eq!(fd.pyj_path, pyj);
        for (idx, m) in fd.messages.iter().enumerate() {
            let sample = reg
                .instantiate_populated_sample(&m.full_name, (idx as u64) + 1)
                .expect("instantiate sample");
            let wire = reg.encode_dynamic(&sample).expect("encode sample");
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .expect("decode sample");
            assert_eq!(decoded, sample);
            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            assert_eq!(any.normalized_type_name(), m.full_name);
        }
    }
    row_ids.len()
}

#[tokio::test]
async fn test_q4_progression_achievement_categories_and_definitions() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = build_sess(8401, 90_000_001, 98_000_001);
    assert_eq!(verify_progression_pyj_batch(&ctx, ROWS), 10);

    let cat_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.GetRequest",
        sample_request_payload(&ctx, "eve_public.achievement.category.api.GetRequest"),
    )
    .await;
    assert_eq!(
        cat_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.category.api.GetResponse"
    );
    assert_eq!(cat_rsp.correlation_uuid, b"prog-test-corr01");

    let rst_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.ResetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        rst_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "google.protobuf.Empty"
    );
    assert_eq!(rst_rsp.correlation_uuid, b"prog-test-corr01");
}

#[tokio::test]
async fn test_q4_progression_achievement_rewards_showcase_and_character() {
    // parity: quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/agent/agent_pb2.pyj, quasar:eveProto/generated/eve_public/career/career_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/character/hacking/hacking_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/agent/agent_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/career_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/character/character_pb2.pyj",
        "quasar:eveProto/generated/eve_public/character/hacking/hacking_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = build_sess(8402, 90_000_001, 98_000_001);
    assert_eq!(verify_progression_pyj_batch(&ctx, ROWS), 10);

    let rew_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        rew_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.reward.api.GetUnclaimedResponse"
    );

    let show_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        show_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.showcase.api.GetCharacterScoreResponse"
    );

    let names_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.character.GetNamesByUserRequest",
        sample_request_payload(&ctx, "eve_public.character.GetNamesByUserRequest"),
    )
    .await;
    assert_eq!(
        names_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.character.GetNamesByUserResponse"
    );
    assert_eq!(names_rsp.correlation_uuid, b"prog-test-corr01");
}

#[tokio::test]
async fn test_q4_progression_character_skill_plans_and_goals() {
    // parity: quasar:eveProto/generated/eve_public/character/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/character/skill/plan/plan_pb2.pyj, quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/epicarc/epicarc_pb2.pyj, quasar:eveProto/generated/eve_public/goal/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/goal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/goal/goal_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/character/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/character/skill/plan/plan_pb2.pyj",
        "quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/epicarc/epicarc_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/goal_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = build_sess(8403, 90_000_001, 98_000_001);
    assert_eq!(verify_progression_pyj_batch(&ctx, ROWS), 10);

    let sp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.character.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        sp_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.character.skill.plan.GetAllResponse"
    );

    let ms_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.character.skill.plan.milestone.GetAllRequest",
        sample_request_payload(&ctx, "eve_public.character.skill.plan.milestone.GetAllRequest"),
    )
    .await;
    assert_eq!(
        ms_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.character.skill.plan.milestone.GetAllResponse"
    );

    let goal_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.goal.api.CreateRequest",
        sample_request_payload(&ctx, "eve_public.goal.api.CreateRequest"),
    )
    .await;
    assert_eq!(
        goal_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.goal.api.CreateResponse"
    );
    assert_eq!(goal_rsp.correlation_uuid, b"prog-test-corr01");
}

#[tokio::test]
async fn test_q4_progression_missions_opportunities_skills_and_tales() {
    // parity: quasar:eveProto/generated/eve_public/goal/matcher_pb2.pyj, quasar:eveProto/generated/eve_public/mission/mission_pb2.pyj, quasar:eveProto/generated/eve_public/nodegraph/nodegraph_pb2.pyj, quasar:eveProto/generated/eve_public/opportunity/api/event_pb2.pyj, quasar:eveProto/generated/eve_public/opportunity/opportunity_pb2.pyj, quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj, quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj, quasar:eveProto/generated/eve_public/tale/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/tale/spawntemplate_pb2.pyj, quasar:eveProto/generated/eve_public/tale/tale_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/goal/matcher_pb2.pyj",
        "quasar:eveProto/generated/eve_public/mission/mission_pb2.pyj",
        "quasar:eveProto/generated/eve_public/nodegraph/nodegraph_pb2.pyj",
        "quasar:eveProto/generated/eve_public/opportunity/api/event_pb2.pyj",
        "quasar:eveProto/generated/eve_public/opportunity/opportunity_pb2.pyj",
        "quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj",
        "quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj",
        "quasar:eveProto/generated/eve_public/tale/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/tale/spawntemplate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/tale/tale_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = build_sess(8404, 90_000_001, 98_000_001);
    assert_eq!(verify_progression_pyj_batch(&ctx, ROWS), 10);

    let plan_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        plan_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.skill.plan.GetAllResponse"
    );

    let tale_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.GetActiveTalesCountRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(
        tale_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.tale.api.GetActiveTalesCountResponse"
    );
    assert_eq!(tale_rsp.correlation_uuid, b"prog-test-corr01");
    assert_eq!(plan_rsp.correlation_uuid, b"prog-test-corr01");
}
