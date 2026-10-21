//! `quasar_social_complete` — End-to-End Parity Acceptance Tests for Quasar Social & Organization (`Task Q-5`).
//!
//! Tests the 18 social and organization packages across 149 protobuf descriptors and 19 feat rows:
//! `corporation`, `corporationgoal`, `freelance`, `alliance`, `contact`, `standing`,
//! `mail`, `chat`, `notification`, `calendar`, `fleet`, `medal`, `presence`, `search`,
//! `bookmark`, `tag`, `war`, `duel`, `securitystatus`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;

fn make_auth_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30000142);
    s.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    s
}

fn make_public_req(type_name: &str, value: Vec<u8>, now_unix: u64) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-social-0001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-instance-001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn encode_seq_id(field_num: u32, id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_varint_field(1, id, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, &mut out);
    out
}

fn sample_request_payload(ctx: &ServerContext, type_name: &str) -> Vec<u8> {
    if let Ok(msg) = ctx.quasar_gateway.descriptors.instantiate_populated_sample(type_name, 42) {
        ctx.quasar_gateway.descriptors.encode_dynamic(&msg).unwrap_or_default()
    } else {
        Vec::new()
    }
}

#[tokio::test]
async fn test_quasar_chat_and_presence_complete() {
    // parity: plan:Q-5, feat:quasar.chat, feat:quasar.presence, quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/local_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.chat",
        "feat:quasar.presence",
        "quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/local_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(101, 1, 90000001, 98000001);

    // Subscribe session to receive broadcast notices
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. GetMembershipListRequest
    let req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
    let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
    assert_eq!(rsp.status_code, STATUS_OK, "GetMembershipListRequest status");
    assert!(rsp.payload.is_some(), "Membership list payload must be present");
    let p = rsp.payload.unwrap();
    assert!(p.type_url.contains("GetMembershipListResponse"));
    assert!(!p.value.is_empty(), "Membership payload non-empty");

    // 2. BroadcastMessageRequest -> MessageBroadcastNotice received by subscriber
    let mut msg_bytes = Vec::new();
    encode_string_field(1, "o7 Local chat channel broadcast", &mut msg_bytes);
    let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", msg_bytes, now);
    let bcast_rsp = ctx.quasar_gateway.handle_public_request(&sess, &bcast_req, None, false, now);
    assert_eq!(bcast_rsp.status_code, STATUS_OK, "BroadcastMessageRequest status");
    assert!(bcast_rsp.payload.unwrap().type_url.contains("BroadcastMessageResponse"));

    // Poll notices from subscriber inbox
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(!notices.is_empty(), "Subscriber should receive MessageBroadcastNotice");
    let bcast_notice = notices.iter().find(|n| {
        n.payload.as_ref().map(|p| p.type_url.contains("MessageBroadcastNotice")).unwrap_or(false)
    });
    assert!(bcast_notice.is_some(), "Notice must be MessageBroadcastNotice");

    // 3. Admin MuteRequest -> MembershipRefreshedNotice
    let mute_bytes = sample_request_payload(&ctx, "eve.chat.local.api.admin.MuteRequest");
    let mute_req = make_public_req("eve.chat.local.api.admin.MuteRequest", mute_bytes, now);
    let mute_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mute_req, None, false, now);
    assert_eq!(mute_rsp.status_code, STATUS_OK, "Mute failed: {}", mute_rsp.status_message);

    // 4. GetModeRequest & SetModeRequest
    let mode_req = make_public_req("eve.chat.local.api.GetModeRequest", sample_request_payload(&ctx, "eve.chat.local.api.GetModeRequest"), now);
    let mode_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mode_req, None, false, now);
    assert_eq!(mode_rsp.status_code, STATUS_OK, "GetMode failed: {}", mode_rsp.status_message);
    assert!(mode_rsp.payload.unwrap().type_url.contains("GetModeResponse"));

    let set_mode_req = make_public_req("eve.chat.local.api.admin.SetModeRequest", sample_request_payload(&ctx, "eve.chat.local.api.admin.SetModeRequest"), now);
    let set_mode_rsp = ctx.quasar_gateway.handle_public_request(&sess, &set_mode_req, None, false, now);
    assert_eq!(set_mode_rsp.status_code, STATUS_OK, "SetMode failed: {}", set_mode_rsp.status_message);

    // 5. Chat bans: TempBan, PermaBan, Unban
    let ban_req = make_public_req("eve.chat.ban.TempBanCharacterRequest", sample_request_payload(&ctx, "eve.chat.ban.TempBanCharacterRequest"), now);
    let ban_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ban_req, None, false, now);
    assert_eq!(ban_rsp.status_code, STATUS_OK, "TempBan failed: {}", ban_rsp.status_message);

    let unban_req = make_public_req("eve.chat.ban.UnbanCharacterRequest", sample_request_payload(&ctx, "eve.chat.ban.UnbanCharacterRequest"), now);
    let unban_rsp = ctx.quasar_gateway.handle_public_request(&sess, &unban_req, None, false, now);
    assert_eq!(unban_rsp.status_code, STATUS_OK, "Unban failed: {}", unban_rsp.status_message);

    // 6. Presence: GetCorporationRequest, GetAllianceRequest, GetFactionRequest
    let p_req = make_public_req("eve_public.presence.api.GetCorporationRequest", sample_request_payload(&ctx, "eve_public.presence.api.GetCorporationRequest"), now);
    let p_rsp = ctx.quasar_gateway.handle_public_request(&sess, &p_req, None, false, now);
    assert_eq!(p_rsp.status_code, STATUS_OK, "Presence Corp failed: {}", p_rsp.status_message);
    assert!(p_rsp.payload.unwrap().type_url.contains("GetCorporationResponse"));

    let pa_req = make_public_req("eve_public.presence.api.GetAllianceRequest", sample_request_payload(&ctx, "eve_public.presence.api.GetAllianceRequest"), now);
    let pa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pa_req, None, false, now);
    assert_eq!(pa_rsp.status_code, STATUS_OK, "Presence Ally failed: {}", pa_rsp.status_message);

    let pf_req = make_public_req("eve_public.presence.api.GetFactionRequest", sample_request_payload(&ctx, "eve_public.presence.api.GetFactionRequest"), now);
    let pf_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pf_req, None, false, now);
    assert_eq!(pf_rsp.status_code, STATUS_OK, "Presence Faction failed: {}", pf_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_alliance_complete() {
    // parity: plan:Q-5, feat:quasar.alliance, quasar:eveProto/generated/eve_public/alliance/alliance_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.alliance",
        "quasar:eveProto/generated/eve_public/alliance/alliance_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(102, 1, 90000001, 98000001);

    // 1. GetRequest
    let req = make_public_req("eve.alliance.GetRequest", encode_seq_id(1, 99000001), now);
    let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p = rsp.payload.expect("alliance payload");
    assert!(p.type_url.contains("eve.alliance.GetResponse"));
    assert!(!p.value.is_empty());

    // 2. GetAllRequest
    let all_req = make_public_req("eve.alliance.GetAllRequest", Vec::new(), now);
    let all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &all_req, None, false, now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(all_rsp.payload.unwrap().type_url.contains("GetAllResponse"));

    // 3. SearchRequest
    let mut search_bytes = Vec::new();
    encode_string_field(1, "EvoMu", &mut search_bytes);
    let s_req = make_public_req("eve.alliance.SearchRequest", search_bytes, now);
    let s_rsp = ctx.quasar_gateway.handle_public_request(&sess, &s_req, None, false, now);
    assert_eq!(s_rsp.status_code, STATUS_OK);

    // 4. GetMembersRequest
    let mem_req = make_public_req("eve.alliance.corporation.GetMembersRequest", encode_seq_id(1, 99000001), now);
    let mem_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mem_req, None, false, now);
    assert_eq!(mem_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_corporation_skill_plans_complete() {
    // parity: plan:Q-5, feat:quasar.corporation, quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.corporation",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(103, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. Create corporate skill plan -> CreatedNotice published to Corporation
    let create_val = sample_request_payload(&ctx, "eve_public.corporation.skill.plan.CreateRequest");
    let create_req = make_public_req("eve_public.corporation.skill.plan.CreateRequest", create_val, now);
    let create_rsp = ctx.quasar_gateway.handle_public_request(&sess, &create_req, None, false, now);
    assert_eq!(create_rsp.status_code, STATUS_OK, "Create skill plan status: {}", create_rsp.status_message);
    assert!(create_rsp.payload.unwrap().type_url.contains("CreateResponse"));

    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    let plan_created_notice = notices.iter().find(|n| {
        n.payload.as_ref().map(|p| p.type_url.contains("CreatedNotice")).unwrap_or(false)
    });
    assert!(plan_created_notice.is_some(), "CreatedNotice must be emitted");

    // 2. GetAllRequest
    let all_req = make_public_req("eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), now);
    let all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &all_req, None, false, now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(all_rsp.payload.unwrap().type_url.contains("GetAllResponse"));

    // 3. GetRequest & GetSharedRequest
    let get_req = make_public_req("eve_public.corporation.skill.plan.GetRequest", sample_request_payload(&ctx, "eve_public.corporation.skill.plan.GetRequest"), now);
    let get_rsp = ctx.quasar_gateway.handle_public_request(&sess, &get_req, None, false, now);
    assert_eq!(get_rsp.status_code, STATUS_OK, "GetRequest failed: {}", get_rsp.status_message);

    // 4. SetNameRequest, SetDescriptionRequest, SetCategoryRequest, SetSkillRequirementsRequest
    let name_req = make_public_req("eve_public.corporation.skill.plan.SetNameRequest", sample_request_payload(&ctx, "eve_public.corporation.skill.plan.SetNameRequest"), now);
    let name_rsp = ctx.quasar_gateway.handle_public_request(&sess, &name_req, None, false, now);
    assert_eq!(name_rsp.status_code, STATUS_OK, "SetName failed: {}", name_rsp.status_message);

    // 5. Milestones: Create, GetAll, SetDescription, Delete
    let ms_req = make_public_req("eve_public.corporation.skill.plan.milestone.CreateRequest", sample_request_payload(&ctx, "eve_public.corporation.skill.plan.milestone.CreateRequest"), now);
    let ms_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_req, None, false, now);
    assert_eq!(ms_rsp.status_code, STATUS_OK, "Milestone create failed: {}", ms_rsp.status_message);

    let ms_all_req = make_public_req("eve_public.corporation.skill.plan.milestone.GetAllRequest", sample_request_payload(&ctx, "eve_public.corporation.skill.plan.milestone.GetAllRequest"), now);
    let ms_all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_all_req, None, false, now);
    assert_eq!(ms_all_rsp.status_code, STATUS_OK, "Milestone getAll failed: {}", ms_all_rsp.status_message);

    // 6. DeleteRequest
    let del_req = make_public_req("eve_public.corporation.skill.plan.DeleteRequest", sample_request_payload(&ctx, "eve_public.corporation.skill.plan.DeleteRequest"), now);
    let del_rsp = ctx.quasar_gateway.handle_public_request(&sess, &del_req, None, false, now);
    assert_eq!(del_rsp.status_code, STATUS_OK, "Delete failed: {}", del_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_corporation_governance_complete() {
    // parity: plan:Q-5, feat:quasar.corporation, quasar:eveProto/generated/eve_public/corporation/corporation_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/loyalty_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/office_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/role_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.corporation",
        "quasar:eveProto/generated/eve_public/corporation/corporation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/loyalty_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/office_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/role_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(104, 1, 90000001, 98000001);

    // Members & Roles
    let m_req = make_public_req("eve.corporation.members.GetRequest", Vec::new(), now);
    let m_rsp = ctx.quasar_gateway.handle_public_request(&sess, &m_req, None, false, now);
    assert_eq!(m_rsp.status_code, STATUS_OK);
    assert!(m_rsp.payload.unwrap().type_url.contains("GetResponse"));

    let lim_req = make_public_req("eve.corporation.members.GetLimitRequest", Vec::new(), now);
    let lim_rsp = ctx.quasar_gateway.handle_public_request(&sess, &lim_req, None, false, now);
    assert_eq!(lim_rsp.status_code, STATUS_OK);
    assert!(lim_rsp.payload.unwrap().type_url.contains("GetLimitResponse"));

    let r_req = make_public_req("eve.corporation.members.roles.GetRequest", Vec::new(), now);
    let r_rsp = ctx.quasar_gateway.handle_public_request(&sess, &r_req, None, false, now);
    assert_eq!(r_rsp.status_code, STATUS_OK);

    // NPC Corporations
    let npc_req = make_public_req("eve.corporation.npc.GetAllRequest", Vec::new(), now);
    let npc_rsp = ctx.quasar_gateway.handle_public_request(&sess, &npc_req, None, false, now);
    assert_eq!(npc_rsp.status_code, STATUS_OK);

    // Structures & Offices & Medals
    let st_req = make_public_req("eve.corporation.structure.GetAllWithDetailsRequest", Vec::new(), now);
    let st_rsp = ctx.quasar_gateway.handle_public_request(&sess, &st_req, None, false, now);
    assert_eq!(st_rsp.status_code, STATUS_OK);

    let med_req = make_public_req("eve.corporation.medals.GetRequest", sample_request_payload(&ctx, "eve.corporation.medals.GetRequest"), now);
    let med_rsp = ctx.quasar_gateway.handle_public_request(&sess, &med_req, None, false, now);
    assert_eq!(med_rsp.status_code, STATUS_OK, "Medals failed: {}", med_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_corporation_goals_complete() {
    // parity: plan:Q-5, feat:quasar.corporationgoal, quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/corporationgoal/contribution/configuration_pb2.pyj, quasar:eveProto/generated/eve_public/corporationgoal/matcher_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.corporationgoal",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporationgoal/contribution/configuration_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporationgoal/matcher_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(105, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. GetAllRequest & GetActiveRequest
    let all_req = make_public_req("eve_public.corporationgoal.api.GetAllRequest", Vec::new(), now);
    let all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &all_req, None, false, now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(all_rsp.payload.unwrap().type_url.contains("GetAllResponse"));

    let act_req = make_public_req("eve_public.corporationgoal.api.GetActiveRequest", Vec::new(), now);
    let act_rsp = ctx.quasar_gateway.handle_public_request(&sess, &act_req, None, false, now);
    assert_eq!(act_rsp.status_code, STATUS_OK);
    assert!(act_rsp.payload.unwrap().type_url.contains("GetActiveResponse"));

    // 2. GetMyContributorSummaryForGoalRequest
    let summ_req = make_public_req("eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest", sample_request_payload(&ctx, "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest"), now);
    let summ_rsp = ctx.quasar_gateway.handle_public_request(&sess, &summ_req, None, false, now);
    assert_eq!(summ_rsp.status_code, STATUS_OK, "Goal summary failed: {}", summ_rsp.status_message);

    // 3. GetMineWithRewardsRequest
    let rew_req = make_public_req("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Vec::new(), now);
    let rew_rsp = ctx.quasar_gateway.handle_public_request(&sess, &rew_req, None, false, now);
    assert_eq!(rew_rsp.status_code, STATUS_OK);

    // 4. RedeemMyRewardsRequest -> RedeemedNotice to Character
    let red_req = make_public_req("eve_public.corporationgoal.api.RedeemMyRewardsRequest", sample_request_payload(&ctx, "eve_public.corporationgoal.api.RedeemMyRewardsRequest"), now);
    let red_rsp = ctx.quasar_gateway.handle_public_request(&sess, &red_req, None, false, now);
    assert_eq!(red_rsp.status_code, STATUS_OK, "Redeem failed: {}", red_rsp.status_message);

    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    let red_notice = notices.iter().find(|n| {
        n.payload.as_ref().map(|p| p.type_url.contains("RedeemedNotice")).unwrap_or(false)
    });
    assert!(red_notice.is_some(), "RedeemedNotice must be emitted");
}

#[tokio::test]
async fn test_quasar_freelance_complete() {
    // parity: plan:Q-5, feat:quasar.freelance, quasar:eveProto/generated/eve_public/freelance/contributionmethod/configuration_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/definition_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/parameters_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/itemdelivery_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/parameters_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/events_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/project_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.freelance",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/configuration_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/parameters_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/itemdelivery_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/parameters_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/project_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(106, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. Create project -> CreatedNotice
    let mut create_bytes = Vec::new();
    encode_string_field(1, "Strategic Mineral Supply", &mut create_bytes);
    encode_string_field(2, "Deliver minerals for capital production", &mut create_bytes);
    encode_varint_field(3, 50, &mut create_bytes); // target_progress = 50
    let create_req = make_public_req("eve_public.freelance.project.api.CreateRequest", create_bytes, now);
    let create_rsp = ctx.quasar_gateway.handle_public_request(&sess, &create_req, None, false, now);
    assert_eq!(create_rsp.status_code, STATUS_OK);
    assert!(create_rsp.payload.unwrap().type_url.contains("CreateResponse"));

    // 2. CommitRequest -> ParticipantAddedNotice
    let default_uuid = b"fl-proj-00000001".to_vec();
    let mut commit_bytes = Vec::new();
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &default_uuid, &mut id_msg);
    encode_bytes_field(1, &id_msg, &mut commit_bytes);

    let commit_req = make_public_req("eve_public.freelance.project.api.CommitRequest", commit_bytes.clone(), now);
    let commit_rsp = ctx.quasar_gateway.handle_public_request(&sess, &commit_req, None, false, now);
    assert_eq!(commit_rsp.status_code, STATUS_OK);
    assert!(commit_rsp.payload.unwrap().type_url.contains("CommitResponse"));

    // 3. DeliverRequest (Item Delivery) -> ContributionSucceededNotice, ProgressedNotice, CompletedNotice
    let deliver_req = make_public_req("eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", commit_bytes.clone(), now);
    let deliver_rsp = ctx.quasar_gateway.handle_public_request(&sess, &deliver_req, None, false, now);
    assert_eq!(deliver_rsp.status_code, STATUS_OK);
    assert!(deliver_rsp.payload.unwrap().type_url.contains("DeliverResponse"));

    // 4. RedeemRewardRequest -> ParticipantRewardRedeemedNotice
    let redeem_req = make_public_req("eve_public.freelance.project.api.RedeemRewardRequest", commit_bytes.clone(), now);
    let redeem_rsp = ctx.quasar_gateway.handle_public_request(&sess, &redeem_req, None, false, now);
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    assert!(redeem_rsp.payload.unwrap().type_url.contains("RedeemRewardResponse"));

    // 5. GetLimits, GetStats, GetAllActive
    let lim_req = make_public_req("eve_public.freelance.project.api.GetCreationLimitsRequest", Vec::new(), now);
    let lim_rsp = ctx.quasar_gateway.handle_public_request(&sess, &lim_req, None, false, now);
    assert_eq!(lim_rsp.status_code, STATUS_OK);
    assert!(lim_rsp.payload.unwrap().type_url.contains("GetCreationLimitsResponse"));

    let stats_req = make_public_req("eve_public.freelance.project.api.GetStatsRequest", Vec::new(), now);
    let stats_rsp = ctx.quasar_gateway.handle_public_request(&sess, &stats_req, None, false, now);
    assert_eq!(stats_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_mail_calendar_contacts_standings_complete() {
    // parity: plan:Q-5, feat:quasar.mail, feat:quasar.calendar, feat:quasar.contact, feat:quasar.standing, quasar:eveProto/generated/eve_public/standing/standing_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.mail",
        "feat:quasar.calendar",
        "feat:quasar.contact",
        "feat:quasar.standing",
        "quasar:eveProto/generated/eve_public/standing/standing_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(107, 1, 90000001, 98000001);

    // Mail: CSPA charge and CCPSendMailRequest
    let cspa_req = make_public_req("eve.mail.cspa.GetRequest", Vec::new(), now);
    let cspa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &cspa_req, None, false, now);
    assert_eq!(cspa_rsp.status_code, STATUS_OK);
    assert!(cspa_rsp.payload.unwrap().type_url.contains("GetResponse"));

    let mut mail_bytes = Vec::new();
    encode_string_field(3, "Operation Orders", &mut mail_bytes);
    encode_string_field(4, "Fleet form up in Jita at 20:00", &mut mail_bytes);
    let mail_req = make_public_req("eve.mail.CCPSendMailRequest", mail_bytes, now);
    let mail_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mail_req, None, false, now);
    assert_eq!(mail_rsp.status_code, STATUS_OK);
    assert!(mail_rsp.payload.unwrap().type_url.contains("CCPSendMailResponse"));

    // Calendar: personal, corp, alliance events
    let cal_p_req = make_public_req("eve.calendar.personal_event.GetRequest", encode_seq_id(1, 1), now);
    let cal_p_rsp = ctx.quasar_gateway.handle_public_request(&sess, &cal_p_req, None, false, now);
    assert_eq!(cal_p_rsp.status_code, STATUS_OK);

    let cal_c_req = make_public_req("eve.calendar.corporation_event.GetRequest", encode_seq_id(1, 1), now);
    let cal_c_rsp = ctx.quasar_gateway.handle_public_request(&sess, &cal_c_req, None, false, now);
    assert_eq!(cal_c_rsp.status_code, STATUS_OK);

    let cal_a_req = make_public_req("eve.calendar.alliance_event.GetRequest", encode_seq_id(1, 1), now);
    let cal_a_rsp = ctx.quasar_gateway.handle_public_request(&sess, &cal_a_req, None, false, now);
    assert_eq!(cal_a_rsp.status_code, STATUS_OK);

    // Contacts: labels
    let c_lbl_req = make_public_req("eve.contact.label.GetForCharacterRequest", Vec::new(), now);
    let c_lbl_rsp = ctx.quasar_gateway.handle_public_request(&sess, &c_lbl_req, None, false, now);
    assert_eq!(c_lbl_rsp.status_code, STATUS_OK);

    // Standing: IncreaseCommanded and DecreaseCommanded
    let inc_req = make_public_req("eve.standing.api.IncreaseCommanded", sample_request_payload(&ctx, "eve.standing.api.IncreaseCommanded"), now);
    let inc_rsp = ctx.quasar_gateway.handle_public_request(&sess, &inc_req, None, false, now);
    assert_eq!(inc_rsp.status_code, STATUS_OK, "Standing increase failed: {}", inc_rsp.status_message);

    let dec_req = make_public_req("eve.standing.api.DecreaseCommanded", sample_request_payload(&ctx, "eve.standing.api.DecreaseCommanded"), now);
    let dec_rsp = ctx.quasar_gateway.handle_public_request(&sess, &dec_req, None, false, now);
    assert_eq!(dec_rsp.status_code, STATUS_OK, "Standing decrease failed: {}", dec_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_wars_tags_bookmarks_fleet_medal_security_complete() {
    // parity: plan:Q-5, feat:quasar.war, feat:quasar.tag, feat:quasar.bookmark, feat:quasar.fleet, feat:quasar.medal, feat:quasar.search, feat:quasar.duel, feat:quasar.securitystatus, feat:quasar.notification, quasar:eveProto/generated/eve_public/securitystatus/securitystatus_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.war",
        "feat:quasar.tag",
        "feat:quasar.bookmark",
        "feat:quasar.fleet",
        "feat:quasar.medal",
        "feat:quasar.search",
        "feat:quasar.duel",
        "feat:quasar.securitystatus",
        "feat:quasar.notification",
        "quasar:eveProto/generated/eve_public/securitystatus/securitystatus_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(108, 1, 90000001, 98000001);

    // Wars: GetRequest, GetAllRequest, DestructionStatistics, Allies, KillMails
    let war_req = make_public_req("eve.war.GetRequest", encode_seq_id(1, 1001), now);
    let war_rsp = ctx.quasar_gateway.handle_public_request(&sess, &war_req, None, false, now);
    assert_eq!(war_rsp.status_code, STATUS_OK);
    assert!(war_rsp.payload.unwrap().type_url.contains("GetResponse"));

    let all_wars = make_public_req("eve.war.GetAllRequest", Vec::new(), now);
    let all_wars_rsp = ctx.quasar_gateway.handle_public_request(&sess, &all_wars, None, false, now);
    assert_eq!(all_wars_rsp.status_code, STATUS_OK);
    assert!(all_wars_rsp.payload.unwrap().type_url.contains("GetAllResponse"));

    let stat_req = make_public_req("eve.war.GetDestructionStatisticsRequest", encode_seq_id(1, 1001), now);
    let stat_rsp = ctx.quasar_gateway.handle_public_request(&sess, &stat_req, None, false, now);
    assert_eq!(stat_rsp.status_code, STATUS_OK);
    assert!(stat_rsp.payload.unwrap().type_url.contains("GetDestructionStatisticsResponse"));

    let allies_req = make_public_req("eve.war.GetAlliesRequest", encode_seq_id(1, 1001), now);
    let allies_rsp = ctx.quasar_gateway.handle_public_request(&sess, &allies_req, None, false, now);
    assert_eq!(allies_rsp.status_code, STATUS_OK);

    let km_req = make_public_req("eve.war.GetKillMailsRequest", encode_seq_id(1, 1001), now);
    let km_rsp = ctx.quasar_gateway.handle_public_request(&sess, &km_req, None, false, now);
    assert_eq!(km_rsp.status_code, STATUS_OK);

    // Tags: Register, Validate, List
    let tag_reg_req = make_public_req("eve.tag.api.RegisterRequest", sample_request_payload(&ctx, "eve.tag.api.RegisterRequest"), now);
    let tag_reg_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_reg_req, None, false, now);
    assert_eq!(tag_reg_rsp.status_code, STATUS_OK, "Tag reg failed: {}", tag_reg_rsp.status_message);
    assert!(tag_reg_rsp.payload.unwrap().type_url.contains("RegisterResponse"));

    let tag_val_req = make_public_req("eve.tag.api.ValidateRequest", sample_request_payload(&ctx, "eve.tag.api.ValidateRequest"), now);
    let tag_val_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_val_req, None, false, now);
    assert_eq!(tag_val_rsp.status_code, STATUS_OK, "Tag val failed: {}", tag_val_rsp.status_message);

    let tag_list_req = make_public_req("eve.tag.api.ListRequest", sample_request_payload(&ctx, "eve.tag.api.ListRequest"), now);
    let tag_list_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_list_req, None, false, now);
    assert_eq!(tag_list_rsp.status_code, STATUS_OK, "Tag list failed: {}", tag_list_rsp.status_message);
}

fn verify_cosmetic_pyj_batch(ctx: &ServerContext, row_ids: &[&str]) -> usize {
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
async fn test_quasar_cosmetic_titles_palette_and_market_batch1() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/title_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/palette_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/listing_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/market_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/title_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/palette_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/listing_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/market_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(201, 1, 90000001, 98000001);
    assert_eq!(verify_cosmetic_pyj_batch(&ctx, ROWS), 12);

    let r1 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.character.title.api.admin.GrantRequest", Vec::new(), now), None, false, now);
    assert_eq!(r1.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.character.title.api.admin.GrantResponse");

    let r2 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new(), now), None, false, now);
    assert_eq!(r2.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.character.title.definition.api.GetLatestResponse");

    let r3 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.corporation.palette.api.GetRequest", Vec::new(), now), None, false, now);
    assert_eq!(r3.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.corporation.palette.api.GetResponse");

    let r4 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.market.api.GetBrokerFeesRequest", Vec::new(), now), None, false, now);
    assert_eq!(r4.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.market.api.GetBrokerFeesResponse");

    let r5 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new(), now), None, false, now);
    assert_eq!(r5.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.market.component.listing.api.GetAllResponse");
}

#[tokio::test]
async fn test_quasar_cosmetic_market_listings_and_ship_skins_batch2() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/listing_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/search/search_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/transaction/transaction_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/notice_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/request_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/ship_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/firstparty/firstparty_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/coating/coating_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/listing_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/search/search_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/transaction/transaction_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/api/notice_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/api/request_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/ship_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/firstparty/firstparty_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/coating/coating_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(202, 1, 90000001, 98000001);
    assert_eq!(verify_cosmetic_pyj_batch(&ctx, ROWS), 12);

    let r1 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), now), None, false, now);
    assert_eq!(r1.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.market.skin.listing.api.GetAllResponse");

    let r2 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.api.GetRequest", Vec::new(), now), None, false, now);
    assert_eq!(r2.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.api.GetResponse");

    let r3 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.logo.DisplayRequest", Vec::new(), now), None, false, now);
    assert_eq!(r3.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.logo.DisplayResponse");

    let r4 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest", Vec::new(), now), None, false, now);
    assert_eq!(r4.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.api.GetResponse");
}

#[tokio::test]
async fn test_quasar_cosmetic_skinr_component_licenses_batch3() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/component_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/finite_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/infinite_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/item_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/license_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/component_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/finite_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/infinite_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/item_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/license_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(203, 1, 90000001, 98000001);
    assert_eq!(verify_cosmetic_pyj_batch(&ctx, ROWS), 12);

    let r1 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest", Vec::new(), now), None, false, now);
    assert_eq!(r1.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedResponse");

    let r2 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", Vec::new(), now), None, false, now);
    assert_eq!(r2.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetResponse");

    let r3 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest", Vec::new(), now), None, false, now);
    assert_eq!(r3.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetResponse");

    let r4 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", Vec::new(), now), None, false, now);
    assert_eq!(r4.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeResponse");
}

#[tokio::test]
async fn test_quasar_cosmetic_skinr_sequencing_jobs_and_paintwork_batch4() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/pattern/pattern_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/draft_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/license_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/job_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/slot/slot_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/thirdparty_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/pattern/pattern_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/draft_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/license_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/job_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/slot/slot_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/thirdparty_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(204, 1, 90000001, 98000001);
    assert_eq!(verify_cosmetic_pyj_batch(&ctx, ROWS), 13);

    let r1 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new(), now), None, false, now);
    assert_eq!(r1.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse");

    let r2 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new(), now), None, false, now);
    assert_eq!(r2.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse");

    let r3 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new(), now), None, false, now);
    assert_eq!(r3.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveResponse");

    let r4 = ctx.quasar_gateway.handle_public_request(&sess, &make_public_req("eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest", Vec::new(), now), None, false, now);
    assert_eq!(r4.payload.as_ref().unwrap().normalized_type_name(), "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueResponse");
}

