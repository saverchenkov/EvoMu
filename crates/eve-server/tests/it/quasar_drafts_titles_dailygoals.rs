//! Integration test suite for Quasar SKINR drafts, cosmetic titles, and daily goals.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireField,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_daily_goals::DailyGoalTemplate;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

pub fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
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
    sess.corp_role = 0x01;
    sess
}

fn build_player_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(2);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = 0; // Standard non-GM role
    sess.corp_role = 0;
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    static CORR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
    let seq = CORR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut corr = b"draft-test-".to_vec();
    corr.extend_from_slice(&seq.to_be_bytes());
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr,
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-draft-inst-01".to_vec(),
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

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_id_request(id: u32) -> Vec<u8> {
    let (mut out, mut id_bytes, mut uuid_bytes) = (Vec::new(), Vec::new(), [0u8; 16]);
    uuid_bytes[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_character_id_request(char_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut char_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(1, &char_msg, &mut out);
    out
}

fn make_admin_grant_char_and_title(char_id: u32, title_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut char_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(1, &char_msg, &mut out);

    let mut title_msg = Vec::new();
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes[12..16].copy_from_slice(&title_id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut title_msg);
    encode_bytes_field(2, &title_msg, &mut out);
    out
}

fn make_admin_progress_with_char(uuid: [u8; 16], target_char: u32, delta: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);

    let mut char_msg = Vec::new();
    encode_varint_field(1, target_char as u64, &mut char_msg);
    encode_bytes_field(2, &char_msg, &mut out);

    encode_varint_field(3, delta, &mut out);
    out
}

fn make_skinr_save_request(name: &str, line: &str, slot_ids: &[u32], pattern_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, name, &mut out);
    encode_string_field(2, line, &mut out);

    let mut layout_bytes = Vec::new();
    for sid in slot_ids {
        let mut slot_msg = Vec::new();
        let mut id_msg = Vec::new();
        encode_varint_field(1, *sid as u64, &mut id_msg);
        encode_bytes_field(1, &id_msg, &mut slot_msg);
        encode_bytes_field(1, &slot_msg, &mut layout_bytes);
    }
    encode_varint_field(3, pattern_id as u64, &mut layout_bytes);
    encode_bytes_field(3, &layout_bytes, &mut out);
    out
}

#[tokio::test]
async fn quasar_skinr_drafts_and_titles_full_lifecycle() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = build_sess(10_101, 90_000_101, 98_000_001);
    let mut sess2 = build_sess(10_102, 90_000_102, 98_000_001);
    let mut player_sess = build_player_sess(10_103, 90_000_103, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session 1 for notices
    ctx.dispatch_rpc(&mut sess1, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess1.session_id)
        .expect("take_stream_receiver");

    // ========================================================================
    // PART 1: SKINR Draft Lifecycle, Design Parameters & Validation
    // ========================================================================

    // 1. Save initial draft with custom design parameters for char 90_000_101
    let save_req = make_skinr_save_request("Rifter Custom Blaze", "Solar Flare", &[501, 502], 7);
    let save_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", save_req).await;
    assert_eq!(save_rsp.status_code, STATUS_OK);
    let save_payload = save_rsp.payload.expect("SaveResponse payload");
    assert!(save_payload.type_url.ends_with("SaveResponse"));

    // Decode SaveResponse: field 1 is id (Identifier), field 2 is attributes (Attributes)
    let save_wm = WireMessage::decode(&save_payload.value).expect("decode SaveResponse");
    let id_field = save_wm.fields.iter().find(|f| f.field_number == 1).expect("draft id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().unwrap()).expect("decode id_wm");
    let draft_uuid = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    assert_eq!(draft_uuid.len(), 16, "Draft UUID must be 16 bytes");
    let mut draft_uuid_bytes = [0u8; 16];
    draft_uuid_bytes.copy_from_slice(draft_uuid);

    let attr_field = save_wm.fields.iter().find(|f| f.field_number == 2).expect("attributes field");
    assert!(!attr_field.value.as_bytes().unwrap().is_empty(), "Attributes must be encoded");

    // 2. Query draft via GetRequest
    let get_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", make_uuid_request(draft_uuid_bytes)).await;
    assert_eq!(get_rsp.status_code, STATUS_OK);
    let get_wm = WireMessage::decode(&get_rsp.payload.unwrap().value).expect("decode GetResponse");
    assert!(get_wm.fields.iter().any(|f| f.field_number == 2), "GetResponse must contain field 2: draft Attributes");

    // Query non-existent draft -> must return 404 (STATUS_NOT_FOUND)
    let missing_uuid = [0x99u8; 16];
    let miss_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", make_uuid_request(missing_uuid)).await;
    assert_eq!(miss_rsp.status_code, STATUS_NOT_FOUND, "Non-existent draft must return 404");

    // 3. Query all saved drafts via GetAllSavedRequest
    let all_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let all_wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllSavedResponse");
    assert!(all_wm.fields.iter().any(|f| f.field_number == 1), "GetAllSavedResponse must have field 1: rep drafts");

    // 4. Update draft
    let mut update_req = Vec::new();
    let mut id_sub = Vec::new();
    encode_bytes_field(1, &draft_uuid_bytes, &mut id_sub);
    encode_bytes_field(1, &id_sub, &mut update_req);
    encode_string_field(2, "Rifter Custom Supernova", &mut update_req);
    let update_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest", update_req).await;
    assert_eq!(update_rsp.status_code, STATUS_OK);

    // 5. Cross-character ownership isolation: char 90_000_102 cannot update or delete char 90_000_101's draft
    let mut unauth_update = Vec::new();
    encode_bytes_field(1, &id_sub, &mut unauth_update);
    encode_string_field(2, "Hacked Draft", &mut unauth_update);
    let unauth_update_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest", unauth_update).await;
    assert_eq!(unauth_update_rsp.status_code, STATUS_FORBIDDEN, "Other character cannot update draft");

    let unauth_del_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", make_uuid_request(draft_uuid_bytes)).await;
    assert_eq!(unauth_del_rsp.status_code, STATUS_FORBIDDEN, "Other character cannot delete draft");

    // 6. Capacity check: capacity is 20
    let cap_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest", Vec::new()).await;
    assert_eq!(cap_rsp.status_code, STATUS_OK);
    let cap_wm = WireMessage::decode(&cap_rsp.payload.unwrap().value).unwrap();
    assert_eq!(cap_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint(), Some(20));

    // Save drafts up to capacity (currently 1 saved, save 19 more)
    for i in 2..=20 {
        let mut req = Vec::new();
        encode_string_field(1, &format!("Draft #{i}"), &mut req);
        assert_eq!(quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", req).await.status_code, STATUS_OK);
    }
    // 21st draft must be rejected with 400 Bad Request
    let overflow_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", Vec::new()).await;
    assert_eq!(overflow_rsp.status_code, STATUS_BAD_REQUEST, "Exceeding 20 draft capacity must be rejected");

    // Delete first draft -> frees a slot
    let del_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", make_uuid_request(draft_uuid_bytes)).await;
    assert_eq!(del_rsp.status_code, STATUS_OK);

    // ========================================================================
    // PART 2: Cosmetic Titles Lifecycle & Ownership Validation
    // ========================================================================

    // 1. GetOwnedRequest -> initially character owns 0 titles
    let owned_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    let owned_wm = WireMessage::decode(&owned_rsp.payload.unwrap().value).expect("decode GetOwnedResponse");
    assert!(owned_wm.fields.is_empty(), "Character must initially have no titles");

    // 2. Try to equip title 101 without ownership -> rejected with STATUS_BAD_REQUEST
    let unowned_eq = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.EquipRequest", make_id_request(101)).await;
    assert_eq!(unowned_eq.status_code, STATUS_BAD_REQUEST, "Equipping unowned title must fail");

    // 3. Non-GM user attempting GrantRequest -> 403 Forbidden
    let unauth_grant = quasar_rpc(&ctx, &mut player_sess, "eve.cosmetic.character.title.api.admin.GrantRequest", make_admin_grant_char_and_title(90_000_103, 101)).await;
    assert_eq!(unauth_grant.status_code, STATUS_FORBIDDEN, "Non-GM cannot call GrantRequest");

    // 4. GM grants title 101 to character 90_000_101
    drain_notices(&mut stream_rx);
    let grant_req = make_admin_grant_char_and_title(90_000_101, 101);
    let grant_rsp = quasar_rpc(&ctx, &mut sess1, "eve.cosmetic.character.title.api.admin.GrantRequest", grant_req).await;
    assert_eq!(grant_rsp.status_code, STATUS_OK);
    assert_eq!(grant_rsp.payload.unwrap().normalized_type_name(), "eve.cosmetic.character.title.api.admin.GrantResponse");

    // Verify GrantedNotice received
    let grant_notices = drain_notices(&mut stream_rx);
    let granted_notice = grant_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("GrantedNotice")).unwrap_or(false));
    assert!(granted_notice.is_some(), "GrantedNotice must be emitted upon GrantRequest");

    // 5. Now character equips title 101 -> success, SetNotice emitted
    let eq_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.EquipRequest", make_id_request(101)).await;
    assert_eq!(eq_rsp.status_code, STATUS_OK);
    assert_eq!(eq_rsp.payload.unwrap().normalized_type_name(), "eve_public.cosmetic.character.title.api.EquipResponse");

    let eq_notices = drain_notices(&mut stream_rx);
    let set_notice = eq_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("SetNotice")).unwrap_or(false));
    assert!(set_notice.is_some(), "SetNotice must be emitted upon EquipRequest");

    // Verify GetEquippedForCharacterRequest reports title 101 (no_title = false)
    let eq_check_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest", make_character_id_request(90_000_101)).await;
    let eq_check_wm = WireMessage::decode(&eq_check_rsp.payload.unwrap().value).unwrap();
    assert_eq!(eq_check_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(0), "no_title must be false");

    // 6. Admin RevokeRequest -> revokes title 101, character unequipped and receives RevokedNotice
    let revoke_req = make_admin_grant_char_and_title(90_000_101, 101);
    let revoke_rsp = quasar_rpc(&ctx, &mut sess1, "eve.cosmetic.character.title.api.admin.RevokeRequest", revoke_req).await;
    assert_eq!(revoke_rsp.status_code, STATUS_OK);
    assert_eq!(revoke_rsp.payload.unwrap().normalized_type_name(), "eve.cosmetic.character.title.api.admin.RevokeResponse");

    let rev_notices = drain_notices(&mut stream_rx);
    let rev_notice = rev_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("RevokedNotice")).unwrap_or(false));
    assert!(rev_notice.is_some(), "RevokedNotice must be emitted upon RevokeRequest");

    // Equipping title 101 now fails because it was revoked
    let re_equip = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.EquipRequest", make_id_request(101)).await;
    assert_eq!(re_equip.status_code, STATUS_BAD_REQUEST, "Equipping revoked title must fail");

    // 7. Verify public admin GrantRequest / RevokeRequest as well
    let pub_grant = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.admin.GrantRequest", make_id_request(102)).await;
    assert_eq!(pub_grant.status_code, STATUS_OK);
    assert_eq!(pub_grant.payload.unwrap().normalized_type_name(), "eve_public.cosmetic.character.title.api.admin.GrantResponse");

    let pub_revoke = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.admin.RevokeRequest", make_id_request(102)).await;
    assert_eq!(pub_revoke.status_code, STATUS_OK);
    assert_eq!(pub_revoke.payload.unwrap().normalized_type_name(), "eve_public.cosmetic.character.title.api.admin.RevokeResponse");

    // 8. UnequipRequest with no title equipped still returns STATUS_OK
    let uneq_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.character.title.api.UnequipRequest", Vec::new()).await;
    assert_eq!(uneq_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn quasar_daily_goals_configurable_and_admin_targeting() {
    // parity: quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = build_sess(20_101, 90_000_001, 98_000_001);
    let mut sess2 = build_sess(20_102, 90_000_002, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session 2 for notices (target char 90_000_002)
    ctx.dispatch_rpc(&mut sess2, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx2 = ctx
        .quasar_gateway
        .take_stream_receiver(sess2.session_id)
        .expect("take_stream_receiver 2");

    // Subscribe session 1 for notices
    ctx.dispatch_rpc(&mut sess1, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx1 = ctx
        .quasar_gateway
        .take_stream_receiver(sess1.session_id)
        .expect("take_stream_receiver 1");

    // ========================================================================
    // 1. Configurable Daily Goals: Dynamically Configure Template with Custom Costs
    // ========================================================================
    let custom_uuid: [u8; 16] = [0x44, 0x47, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 99];
    let custom_tpl = DailyGoalTemplate {
        uuid: custom_uuid,
        name_message: "ui_daily_goal_abyssal_runs".to_string(),
        description_message: "ui_daily_goal_abyssal_runs_desc".to_string(),
        category: 1, // Combat
        career: 1,   // Enforcer
        target: 3,
        isk_reward: 1_000_000,
        sp_reward: 10_000.0,
        completion_cost_isk: Some(50_000),
        completion_cost_evermarks: Some(1_000),
    };
    ctx.quasar_gateway.progression.daily_goals.configure_template(custom_tpl);

    // Query current daily goals for session 2 -> verify custom goal template is present
    let current_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(current_rsp.status_code, STATUS_OK);
    let current_wm = WireMessage::decode(&current_rsp.payload.unwrap().value).unwrap();
    let goal_items: Vec<&WireField> = current_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(
        goal_items.iter().any(|item| {
            let item_wm = WireMessage::decode(item.value.as_bytes().unwrap()).unwrap();
            let id_wm = WireMessage::decode(item_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
            let uuid = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
            uuid == &custom_uuid
        }),
        "Configured custom goal template must appear in GetAllCurrentResponse"
    );

    // ========================================================================
    // 2. Admin Progress with Character Targeting (field 2)
    // ========================================================================
    drain_notices(&mut stream_rx2);
    drain_notices(&mut stream_rx1);

    // GM advances progress for target character 90_000_002 by delta 2 (target = 3)
    let prog_req1 = make_admin_progress_with_char(custom_uuid, 90_000_002, 2);
    let prog_rsp1 = quasar_rpc(&ctx, &mut sess1, "eve_public.dailygoal.api.admin.ProgressRequest", prog_req1).await;
    assert_eq!(prog_rsp1.status_code, STATUS_OK);
    assert_eq!(prog_rsp1.payload.unwrap().normalized_type_name(), "eve_public.dailygoal.api.admin.ProgressResponse");

    // Target char (session 2) receives ProgressedNotice with progress = 2
    let notices2 = drain_notices(&mut stream_rx2);
    let prog_notice = notices2.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("ProgressedNotice")).unwrap_or(false));
    assert!(prog_notice.is_some(), "Target character must receive ProgressedNotice");

    // Session 1 should NOT have received notices for char 2's goal
    let notices1 = drain_notices(&mut stream_rx1);
    assert!(notices1.is_empty(), "Non-target character must not receive target character's notice");

    // Advance 1 more progress to hit target 3 -> completed!
    let prog_req2 = make_admin_progress_with_char(custom_uuid, 90_000_002, 1);
    let prog_rsp2 = quasar_rpc(&ctx, &mut sess1, "eve_public.dailygoal.api.admin.ProgressRequest", prog_req2).await;
    assert_eq!(prog_rsp2.status_code, STATUS_OK);

    let notices2_complete = drain_notices(&mut stream_rx2);
    let comp_notice = notices2_complete.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("CompletedNotice")).unwrap_or(false));
    assert!(comp_notice.is_some(), "Target character must receive CompletedNotice upon completion");

    // ========================================================================
    // 3. Pay-for-Completion with Real Notice Emissions & Custom Cost
    // ========================================================================
    drain_notices(&mut stream_rx1);

    // Char 90_000_001 pays for completion of custom goal
    let pay_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.dailygoal.api.PayForCompletionRequest", make_uuid_request(custom_uuid)).await;
    assert_eq!(pay_rsp.status_code, STATUS_OK);
    assert_eq!(pay_rsp.payload.unwrap().normalized_type_name(), "eve_public.dailygoal.api.PayForCompletionResponse");

    let pay_notices = drain_notices(&mut stream_rx1);
    let pay_prog = pay_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("ProgressedNotice")).unwrap_or(false));
    let pay_comp = pay_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("CompletedNotice")).unwrap_or(false));
    assert!(pay_prog.is_some(), "Pay-for-completion must emit ProgressedNotice");
    assert!(pay_comp.is_some(), "Pay-for-completion must emit CompletedNotice");

    // Paying again must fail because it's already completed
    let double_pay = quasar_rpc(&ctx, &mut sess1, "eve_public.dailygoal.api.PayForCompletionRequest", make_uuid_request(custom_uuid)).await;
    assert_eq!(double_pay.status_code, STATUS_BAD_REQUEST, "Double pay for completion must fail");

    // ========================================================================
    // 4. Persistence across Server Reload
    // ========================================================================
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "quasar_dg_cfg");
    let reloaded_goals = ctx_reloaded.quasar_gateway.progression.daily_goals.get_or_init_goals(90_000_001);
    let reloaded_custom = reloaded_goals.iter().find(|g| g.uuid == custom_uuid).expect("custom goal preserved across reload");
    assert!(reloaded_custom.completed, "Completed state must survive SQLite reload");
    assert!(reloaded_custom.paid_completion, "Paid completion flag must survive SQLite reload");
}
