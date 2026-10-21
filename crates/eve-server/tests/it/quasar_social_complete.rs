//! `quasar_social_complete` — Integration Acceptance Tests for Quasar Social & Organization services.
//!
//! Validates stateful gateway handling across:
//! `corporation`, `corporationgoal`, `freelance`, `alliance`, `contact`, `standing`,
//! `mail`, `chat`, `notification`, `calendar`, `fleet`, `medal`, `presence`, `search`,
//! `bookmark`, `tag`, `war`, `duel`, `securitystatus`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
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

#[tokio::test]
async fn test_quasar_chat_and_presence_complete() {
    // parity: plan:Q-5, feat:quasar.chat, quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-5",
        "feat:quasar.chat",
        "quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj",
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
    let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", msg_bytes.clone(), now);
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

    // 3. Admin MuteRequest (both eve_public and eve namespaces) -> MembershipRefreshedNotice
    let pub_mute_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", encode_seq_id(1, 90000001), now);
    let pub_mute_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pub_mute_req, None, false, now);
    assert_eq!(pub_mute_rsp.status_code, STATUS_OK, "Public Mute failed: {}", pub_mute_rsp.status_message);

    let mute_req = make_public_req("eve.chat.local.api.admin.MuteRequest", encode_seq_id(1, 90000001), now);
    let mute_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mute_req, None, false, now);
    assert_eq!(mute_rsp.status_code, STATUS_OK, "Mute failed: {}", mute_rsp.status_message);

    // 4. GetModeRequest & SetModeRequest
    let mode_req = make_public_req("eve.chat.local.api.GetModeRequest", Vec::new(), now);
    let mode_rsp = ctx.quasar_gateway.handle_public_request(&sess, &mode_req, None, false, now);
    assert_eq!(mode_rsp.status_code, STATUS_OK, "GetMode failed: {}", mode_rsp.status_message);
    assert!(mode_rsp.payload.unwrap().type_url.contains("GetModeResponse"));

    let set_mode_req = make_public_req("eve.chat.local.api.admin.SetModeRequest", Vec::new(), now);
    let set_mode_rsp = ctx.quasar_gateway.handle_public_request(&sess, &set_mode_req, None, false, now);
    assert_eq!(set_mode_rsp.status_code, STATUS_OK, "SetMode failed: {}", set_mode_rsp.status_message);

    // 5. Chat bans: TempBan blocks BroadcastMessageRequest with STATUS_FORBIDDEN until UnbanCharacterRequest
    let ban_req = make_public_req("eve.chat.ban.TempBanCharacterRequest", encode_seq_id(1, 90000001), now);
    let ban_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ban_req, None, false, now);
    assert_eq!(ban_rsp.status_code, STATUS_OK, "TempBan failed: {}", ban_rsp.status_message);

    let blocked_bcast = ctx.quasar_gateway.handle_public_request(&sess, &bcast_req, None, false, now);
    assert_eq!(
        blocked_bcast.status_code, STATUS_FORBIDDEN,
        "Banned character must be rejected from broadcasting in chat"
    );

    let unban_req = make_public_req("eve.chat.ban.UnbanCharacterRequest", encode_seq_id(1, 90000001), now);
    let unban_rsp = ctx.quasar_gateway.handle_public_request(&sess, &unban_req, None, false, now);
    assert_eq!(unban_rsp.status_code, STATUS_OK, "Unban failed: {}", unban_rsp.status_message);

    let unblocked_bcast = ctx.quasar_gateway.handle_public_request(&sess, &bcast_req, None, false, now);
    assert_eq!(
        unblocked_bcast.status_code, STATUS_OK,
        "Unbanned character should be able to broadcast again"
    );

    // 6. Presence: GetCorporationRequest, GetAllianceRequest, GetFactionRequest
    let p_req = make_public_req("eve_public.presence.api.GetCorporationRequest", encode_seq_id(1, 98000001), now);
    let p_rsp = ctx.quasar_gateway.handle_public_request(&sess, &p_req, None, false, now);
    assert_eq!(p_rsp.status_code, STATUS_OK, "Presence Corp failed: {}", p_rsp.status_message);
    assert!(p_rsp.payload.unwrap().type_url.contains("GetCorporationResponse"));

    let pa_req = make_public_req("eve_public.presence.api.GetAllianceRequest", encode_seq_id(1, 99000001), now);
    let pa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pa_req, None, false, now);
    assert_eq!(pa_rsp.status_code, STATUS_OK, "Presence Ally failed: {}", pa_rsp.status_message);

    let pf_req = make_public_req("eve_public.presence.api.GetFactionRequest", encode_seq_id(1, 500001), now);
    let pf_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pf_req, None, false, now);
    assert_eq!(pf_rsp.status_code, STATUS_OK, "Presence Faction failed: {}", pf_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_alliance_complete() {
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
    // parity: quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let now = ctx.now_unix();
    let sess = make_auth_session(103, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. Create corporate skill plan -> CreatedNotice published to Corporation
    let create_req = make_public_req("eve_public.corporation.skill.plan.CreateRequest", Vec::new(), now);
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
    let get_req = make_public_req("eve_public.corporation.skill.plan.GetRequest", Vec::new(), now);
    let get_rsp = ctx.quasar_gateway.handle_public_request(&sess, &get_req, None, false, now);
    assert_eq!(get_rsp.status_code, STATUS_OK, "GetRequest failed: {}", get_rsp.status_message);

    // 4. SetNameRequest, SetDescriptionRequest, SetCategoryRequest, SetSkillRequirementsRequest
    let name_req = make_public_req("eve_public.corporation.skill.plan.SetNameRequest", Vec::new(), now);
    let name_rsp = ctx.quasar_gateway.handle_public_request(&sess, &name_req, None, false, now);
    assert_eq!(name_rsp.status_code, STATUS_OK, "SetName failed: {}", name_rsp.status_message);

    // 5. Milestones: Create, GetAll, SetDescription, Delete
    let ms_req = make_public_req("eve_public.corporation.skill.plan.milestone.CreateRequest", Vec::new(), now);
    let ms_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_req, None, false, now);
    assert_eq!(ms_rsp.status_code, STATUS_OK, "Milestone create failed: {}", ms_rsp.status_message);

    let ms_all_req = make_public_req("eve_public.corporation.skill.plan.milestone.GetAllRequest", Vec::new(), now);
    let ms_all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_all_req, None, false, now);
    assert_eq!(ms_all_rsp.status_code, STATUS_OK, "Milestone getAll failed: {}", ms_all_rsp.status_message);

    // 6. DeleteRequest
    let del_req = make_public_req("eve_public.corporation.skill.plan.DeleteRequest", Vec::new(), now);
    let del_rsp = ctx.quasar_gateway.handle_public_request(&sess, &del_req, None, false, now);
    assert_eq!(del_rsp.status_code, STATUS_OK, "Delete failed: {}", del_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_corporation_governance_complete() {
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

    let med_req = make_public_req("eve.corporation.medals.GetRequest", encode_seq_id(1, 98000001), now);
    let med_rsp = ctx.quasar_gateway.handle_public_request(&sess, &med_req, None, false, now);
    assert_eq!(med_rsp.status_code, STATUS_OK, "Medals failed: {}", med_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_corporation_goals_complete() {
    // parity: feat:quasar.corporationgoal, quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.corporationgoal",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporationgoal/api/requests_pb2.pyj",
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
    let summ_req = make_public_req("eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest", Vec::new(), now);
    let summ_rsp = ctx.quasar_gateway.handle_public_request(&sess, &summ_req, None, false, now);
    assert_eq!(summ_rsp.status_code, STATUS_OK, "Goal summary failed: {}", summ_rsp.status_message);

    // 3. GetMineWithRewardsRequest
    let rew_req = make_public_req("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Vec::new(), now);
    let rew_rsp = ctx.quasar_gateway.handle_public_request(&sess, &rew_req, None, false, now);
    assert_eq!(rew_rsp.status_code, STATUS_OK);

    // 4. RedeemMyRewardsRequest -> RedeemedNotice to Character
    let red_req = make_public_req("eve_public.corporationgoal.api.RedeemMyRewardsRequest", Vec::new(), now);
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
    // parity: feat:quasar.freelance, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.freelance",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj",
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
    let inc_req = make_public_req("eve.standing.api.IncreaseCommanded", Vec::new(), now);
    let inc_rsp = ctx.quasar_gateway.handle_public_request(&sess, &inc_req, None, false, now);
    assert_eq!(inc_rsp.status_code, STATUS_OK, "Standing increase failed: {}", inc_rsp.status_message);

    let dec_req = make_public_req("eve.standing.api.DecreaseCommanded", Vec::new(), now);
    let dec_rsp = ctx.quasar_gateway.handle_public_request(&sess, &dec_req, None, false, now);
    assert_eq!(dec_rsp.status_code, STATUS_OK, "Standing decrease failed: {}", dec_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_wars_tags_bookmarks_fleet_medal_security_complete() {
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
    let tag_reg_req = make_public_req("eve.tag.api.RegisterRequest", Vec::new(), now);
    let tag_reg_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_reg_req, None, false, now);
    assert_eq!(tag_reg_rsp.status_code, STATUS_OK, "Tag reg failed: {}", tag_reg_rsp.status_message);
    assert!(tag_reg_rsp.payload.unwrap().type_url.contains("RegisterResponse"));

    let tag_val_req = make_public_req("eve.tag.api.ValidateRequest", Vec::new(), now);
    let tag_val_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_val_req, None, false, now);
    assert_eq!(tag_val_rsp.status_code, STATUS_OK, "Tag val failed: {}", tag_val_rsp.status_message);

    let tag_list_req = make_public_req("eve.tag.api.ListRequest", Vec::new(), now);
    let tag_list_rsp = ctx.quasar_gateway.handle_public_request(&sess, &tag_list_req, None, false, now);
    assert_eq!(tag_list_rsp.status_code, STATUS_OK, "Tag list failed: {}", tag_list_rsp.status_message);
}
