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
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn make_auth_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30000142);
    s.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    s.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
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

fn encode_mute_req(char_id: u64, duration_secs: u64, reason: &str) -> Vec<u8> {
    let mut out = encode_seq_id(1, char_id);
    let mut dur = Vec::new();
    encode_varint_field(1, duration_secs, &mut dur);
    encode_bytes_field(2, &dur, &mut out);
    encode_string_field(3, reason, &mut out);
    out
}

fn decode_id(bytes: &[u8]) -> u64 {
    let wm = eve_proto::quasar::WireMessage::decode(bytes).expect("decode id");
    match wm.fields.iter().find(|f| f.field_number == 1).unwrap().value {
        eve_proto::quasar::WireValue::Varint(v) => v,
        _ => panic!("Expected varint in Identifier"),
    }
}

fn field_ld<'a>(wm: &'a eve_proto::quasar::WireMessage, num: u32) -> &'a [u8] {
    match &wm.fields.iter().find(|f| f.field_number == num).unwrap().value {
        eve_proto::quasar::WireValue::LengthDelimited(b) => b.as_slice(),
        _ => panic!("field not length-delimited"),
    }
}

#[tokio::test]
async fn test_quasar_chat_and_presence_complete() {
    // parity: feat:quasar.chat, quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.chat",
        "quasar:eveProto/generated/eve_public/chat/local/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/chat/local/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let mut sess = make_auth_session(101, 1, 90000001, 98000001);

    // Seed corp 98000001, alliance & faction for metadata and corp membership verification
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&90000001) {
            ch.corporation_id = 98000001;
            ch.alliance_id = Some(99000001);
            ch.war_faction_id = Some(500001);
        }
        if let Some(ch) = s.characters.get_mut(&90000002) {
            ch.corporation_id = 98000001;
            ch.alliance_id = Some(99000001);
        }
        s.corporations.insert(98000001, eve_db::CorporationRecord {
            corporation_id: 98000001, name: "Quasar Test Corp".into(), ticker: "QTC".into(),
            ceo_id: 90000001, alliance_id: Some(99000001), tax_rate: 0.05,
            master_wallet: eve_db::IskCents(0), members: std::collections::HashSet::from([90000001, 90000002]),
            member_roles: std::collections::HashMap::from([(90000001, u64::MAX), (90000002, 0)]),
            applications: std::collections::HashMap::new(), wallet_divisions: vec![],
            description: String::new(), url: String::new(), hq_station_id: Some(60003760),
        });
    });

    // Subscribe session to receive broadcast notices
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);
    let sess2 = make_auth_session(102, 2, 90000002, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess2, vec![], now);

    // 1. GetMembershipListRequest returns multi-character solar-system membership list with metadata
    let req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
    let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
    assert_eq!(rsp.status_code, STATUS_OK, "GetMembershipListRequest status");
    assert!(rsp.payload.is_some(), "Membership list payload must be present");
    let p = rsp.payload.unwrap();
    assert!(p.type_url.contains("GetMembershipListResponse"));
    assert!(!p.value.is_empty(), "Membership payload non-empty");
    // Verify response decodes multiple members (at least 90000001 and 90000002 in Jita)
    let wm = eve_proto::quasar::WireMessage::decode(&p.value).expect("decode membership");
    let member_fields: Vec<_> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(member_fields.len() >= 2, "Membership list must include all characters in system");
    let member_cids: Vec<u64> = member_fields
        .iter()
        .map(|f| {
            let m_wm = eve_proto::quasar::WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
            decode_id(field_ld(&m_wm, 1))
        })
        .collect();
    assert!(member_cids.contains(&90000001));
    assert!(member_cids.contains(&90000002));

    // 2. BroadcastMessageRequest -> MessageBroadcastNotice received by subscriber
    let mut msg_bytes = Vec::new();
    encode_string_field(1, "o7 Local chat channel broadcast", &mut msg_bytes);
    let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", msg_bytes.clone(), now);
    let bcast_rsp = ctx.quasar_gateway.handle_public_request(&sess, &bcast_req, None, false, now);
    assert_eq!(bcast_rsp.status_code, STATUS_OK, "BroadcastMessageRequest status");
    assert!(bcast_rsp.payload.unwrap().type_url.contains("BroadcastMessageResponse"));

    // Poll notices from subscriber inbox
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    let bcast_notice = notices.iter().find(|n| {
        n.payload.as_ref().map(|p| p.type_url.contains("MessageBroadcastNotice")).unwrap_or(false)
    }).expect("MessageBroadcastNotice");
    let b_wm = eve_proto::quasar::WireMessage::decode(&bcast_notice.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(decode_id(field_ld(&b_wm, 1)), 90000001);
    assert_eq!(b_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap(), "o7 Local chat channel broadcast");

    // 3. Admin MuteRequest: RBAC check, target validation, and mute persistence
    let mut non_admin_sess = make_auth_session(109, 3, 90000003, 98000001);
    non_admin_sess.role = 0;
    non_admin_sess.corp_role = 0;

    let mute_target_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", encode_mute_req(90000002, 300, "Spam"), now);
    let unauth_mute_rsp = ctx.quasar_gateway.handle_public_request(&non_admin_sess, &mute_target_req, None, false, now);
    assert_eq!(unauth_mute_rsp.status_code, STATUS_FORBIDDEN);

    // Mute without duration returns 400; non-existent char returns 404
    let no_dur = make_public_req("eve_public.chat.local.api.admin.MuteRequest", encode_seq_id(1, 90000002), now);
    assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &no_dur, None, false, now).status_code, 400);

    let missing_char = make_public_req("eve_public.chat.local.api.admin.MuteRequest", encode_mute_req(99999999, 300, "x"), now);
    assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &missing_char, None, false, now).status_code, 404);

    let pub_mute_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", encode_mute_req(90000002, 300, "Spam"), now);
    let pub_mute_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pub_mute_req, None, false, now);
    assert_eq!(pub_mute_rsp.status_code, STATUS_OK);

    // Verify mute saved in quasar_state_blobs and rejects broadcast
    let persisted_mutes = ctx.db.with_snapshot(|s| s.quasar_state_blobs.get("quasar_chat_mutes").cloned()).unwrap();
    assert!(persisted_mutes.contains("90000002"));
    let muted_bcast = ctx.quasar_gateway.handle_public_request(&sess2, &bcast_req, None, false, now);
    assert_eq!(muted_bcast.status_code, STATUS_FORBIDDEN);

    // Advance clock past mute duration (300s) and verify broadcast succeeds
    ctx.advance_secs(301);
    let now = ctx.now_unix();
    let unmuted_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", msg_bytes.clone(), now);
    let unmuted_bcast = ctx.quasar_gateway.handle_public_request(&sess2, &unmuted_req, None, false, now);
    assert_eq!(unmuted_bcast.status_code, STATUS_OK, "Broadcast after mute expiration must succeed");

    // Verify MembershipRefreshedNotice on real corporation change via RPC KickOutMembers
    let _ = ctx.quasar_gateway.poll_notices(sess.session_id);
    let kick = ctx.dispatch_rpc(&mut sess, "corpRegistry", None, "KickOutMembers", &[PyRep::Integer(90000002)], &HashMap::new(), None).await.expect("Kick");
    let PyRep::KeyVal(kkv) = &kick else { panic!("Expected KeyVal") };
    assert_eq!(kkv["characterID"], PyRep::Integer(90000002));

    let ref_notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    let ref_notice = ref_notices.into_iter().find(|n| {
        n.payload.as_ref().map(|p| p.type_url.ends_with("MembershipRefreshedNotice")).unwrap_or(false)
    }).expect("MembershipRefreshedNotice");
    let wm = eve_proto::quasar::WireMessage::decode(&ref_notice.payload.unwrap().value).unwrap();
    let m_wm = eve_proto::quasar::WireMessage::decode(field_ld(&wm, 1)).unwrap();
    assert_eq!(decode_id(field_ld(&m_wm, 1)), 90000002);
    assert_eq!(decode_id(field_ld(&m_wm, 2)), 1_000_006);
    assert_eq!(decode_id(field_ld(&wm, 2)), 30000142);
    assert!(wm.fields.iter().all(|f| f.field_number != 3));

    // 4. Local chat stargate jump transition: LeaveNotice, MembershipListNotice via CmdStargateJump
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &HashMap::new(), None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal from Undock") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == 30000142)
        .unwrap()
        .clone();
    let jita_handle = ctx.systems.get_or_spawn(30000142);
    assert!(jita_handle.set_ball_position(ship_id, gate.position).await);

    let _ = ctx.quasar_gateway.poll_notices(sess.session_id);
    let _ = ctx.quasar_gateway.poll_notices(sess2.session_id);

    let jump_res = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_id), "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)], &HashMap::new(), None,
    ).await.expect("CmdStargateJump");
    let PyRep::KeyVal(jump_kv) = &jump_res else { panic!("Expected KeyVal from jump") };
    assert_eq!(jump_kv["solarSystemID"].as_i64(), Some(gate.to_solar_system_id as i64));
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));

    let sess2_notices = ctx.quasar_gateway.poll_notices(sess2.session_id);
    let leave_n = sess2_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("LeaveNotice")).unwrap_or(false)).expect("LeaveNotice");
    let leave_wm = eve_proto::quasar::WireMessage::decode(&leave_n.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(decode_id(field_ld(&leave_wm, 1)), 90000001);
    assert_eq!(decode_id(field_ld(&leave_wm, 2)), 30000142);

    let sess_notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    let ml_n = sess_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("MembershipListNotice")).unwrap_or(false)).expect("MembershipListNotice");
    let ml_wm = eve_proto::quasar::WireMessage::decode(&ml_n.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(decode_id(field_ld(&ml_wm, 2)), gate.to_solar_system_id as u64);

    // 4b. Local chat login and logout transitions: MembershipListNotice, JoinNotice, LeaveNotice
    ctx.db.with_mut_snapshot(|s| {
        if let Some(base) = s.characters.get(&90000001).cloned() {
            let mut c3 = base;
            c3.character_id = 90000003;
            c3.user_id = 3;
            c3.name = "Pilot Three".into();
            c3.solar_system_id = 30000142;
            s.characters.insert(90000003, c3);
        }
    });

    let mut login_sess = SessionState::new(103);
    login_sess.user_id = Some(3);
    ctx.quasar_gateway.subscribe_session(&login_sess, vec![], now);
    let _ = ctx.quasar_gateway.poll_notices(login_sess.session_id);
    let _ = ctx.quasar_gateway.poll_notices(sess2.session_id);

    // Login pilot 90000003 into Jita via SelectCharacterID
    let _ = ctx.dispatch_rpc(
        &mut login_sess, "charUnboundMgr", None, "SelectCharacterID",
        &[PyRep::Integer(90000003)], &HashMap::new(), None,
    ).await.expect("SelectCharacterID");

    let login_notices = ctx.quasar_gateway.poll_notices(login_sess.session_id);
    let l_ml = login_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("MembershipListNotice")).unwrap_or(false)).expect("login MembershipListNotice");
    let l_ml_wm = eve_proto::quasar::WireMessage::decode(&l_ml.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(decode_id(field_ld(&l_ml_wm, 2)), 30000142);

    let s2_in = ctx.quasar_gateway.poll_notices(sess2.session_id);
    let j_n = s2_in.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("JoinNotice")).unwrap_or(false)).expect("login JoinNotice");
    let j_wm = eve_proto::quasar::WireMessage::decode(&j_n.payload.as_ref().unwrap().value).unwrap();
    let j_m = eve_proto::quasar::WireMessage::decode(field_ld(&j_wm, 1)).unwrap();
    assert_eq!(decode_id(field_ld(&j_m, 1)), 90000003);
    assert_eq!(decode_id(field_ld(&j_wm, 2)), 30000142);

    // Logout pilot 90000003 via authentication.Logout
    let _ = ctx.dispatch_rpc(&mut login_sess, "authentication", None, "Logout", &[], &HashMap::new(), None).await.expect("Logout");
    let s2_out = ctx.quasar_gateway.poll_notices(sess2.session_id);
    let l_out = s2_out.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("LeaveNotice")).unwrap_or(false)).expect("logout LeaveNotice");
    let l_out_wm = eve_proto::quasar::WireMessage::decode(&l_out.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(decode_id(field_ld(&l_out_wm, 1)), 90000003);
    assert_eq!(decode_id(field_ld(&l_out_wm, 2)), 30000142);

    // 4c. Wormhole delayed local chat (solar_system_id >= 31_000_000)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(base) = s.characters.get(&90000001).cloned() {
            let mut c4 = base;
            c4.character_id = 90000004;
            c4.user_id = 4;
            c4.name = "WH Pilot".into();
            c4.solar_system_id = 31000001;
            s.characters.insert(90000004, c4);
        }
    });
    let mut wh_sess = make_auth_session(110, 4, 90000004, 98000001);
    wh_sess.solar_system_id = Some(31000001);
    ctx.quasar_gateway.subscribe_session(&wh_sess, vec![], now);
    let _ = ctx.quasar_gateway.poll_notices(wh_sess.session_id);

    // Pilot enters WH: JoinNotice skipped; MembershipListNotice returned with 0 members
    ctx.sync_character_local_chat(90000004, Some(30000142), 31000001);
    let wh_n1 = ctx.quasar_gateway.poll_notices(wh_sess.session_id);
    assert!(!wh_n1.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("JoinNotice")).unwrap_or(false)));
    let wh_ml = wh_n1.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.contains("MembershipListNotice")).unwrap_or(false)).expect("WH MembershipListNotice");
    let wh_ml_wm = eve_proto::quasar::WireMessage::decode(&wh_ml.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(wh_ml_wm.fields.iter().filter(|f| f.field_number == 1).count(), 0);

    // GetMembershipListRequest returns 0 members
    let wh_req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
    let wh_rsp = ctx.quasar_gateway.handle_public_request(&wh_sess, &wh_req, None, false, now);
    assert_eq!(wh_rsp.status_code, STATUS_OK);
    let wh_wm = eve_proto::quasar::WireMessage::decode(&wh_rsp.payload.unwrap().value).unwrap();
    assert_eq!(wh_wm.fields.iter().filter(|f| f.field_number == 1).count(), 0);

    // Pilot speaks: BroadcastMessageRequest triggers JoinNotice + MessageBroadcastNotice
    let mut wh_msg = Vec::new();
    encode_string_field(1, "Broadcast in WH", &mut wh_msg);
    let wh_b_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", wh_msg, now);
    let wh_b_rsp = ctx.quasar_gateway.handle_public_request(&wh_sess, &wh_b_req, None, false, now);
    assert_eq!(wh_b_rsp.status_code, STATUS_OK);

    let wh_n2 = ctx.quasar_gateway.poll_notices(wh_sess.session_id);
    assert!(wh_n2.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("JoinNotice")).unwrap_or(false)));
    assert!(wh_n2.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("MessageBroadcastNotice")).unwrap_or(false)));

    // GetMembershipListRequest now includes author 90000004
    let wh_rsp2 = ctx.quasar_gateway.handle_public_request(&wh_sess, &wh_req, None, false, now);
    let wh_wm2 = eve_proto::quasar::WireMessage::decode(&wh_rsp2.payload.unwrap().value).unwrap();
    let mems: Vec<_> = wh_wm2.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(mems.len(), 1);
    let m2_wm = eve_proto::quasar::WireMessage::decode(mems[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(decode_id(field_ld(&m2_wm, 1)), 90000004);

    // Pilot leaves WH: LeaveNotice emitted because revealed
    let wh_left = eve_server::services::quasar_social_chat::on_character_leave_local_chat(&ctx.quasar_gateway, &ctx.db, 90000004, 31000001, now).unwrap();
    assert!(wh_left > 0);
    let wh_n3 = ctx.quasar_gateway.poll_notices(wh_sess.session_id);
    assert!(wh_n3.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("LeaveNotice")).unwrap_or(false)));

    // 5. Presence: GetCorporationRequest, GetAllianceRequest, GetFactionRequest
    let p_req = make_public_req("eve_public.presence.api.GetCorporationRequest", encode_seq_id(1, 90000001), now);
    let p_rsp = ctx.quasar_gateway.handle_public_request(&sess, &p_req, None, false, now);
    assert_eq!(p_rsp.status_code, STATUS_OK, "Presence Corp failed: {}", p_rsp.status_message);
    assert!(p_rsp.payload.unwrap().type_url.contains("GetCorporationResponse"));

    let pa_req = make_public_req("eve_public.presence.api.GetAllianceRequest", encode_seq_id(1, 90000001), now);
    let pa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pa_req, None, false, now);
    assert_eq!(pa_rsp.status_code, STATUS_OK, "Presence Ally failed: {}", pa_rsp.status_message);

    let pf_req = make_public_req("eve_public.presence.api.GetFactionRequest", encode_seq_id(1, 90000001), now);
    let pf_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pf_req, None, false, now);
    assert_eq!(pf_rsp.status_code, STATUS_OK, "Presence Faction failed: {}", pf_rsp.status_message);
    let pf_wire = eve_proto::quasar::WireMessage::decode(&pf_rsp.payload.unwrap().value).expect("decode faction");
    assert!(pf_wire.fields.iter().any(|f| f.field_number == 2), "Faction present must encode field 2");

    let pf_none_req = make_public_req("eve_public.presence.api.GetFactionRequest", encode_seq_id(1, 90000002), now);
    let pf_none_rsp = ctx.quasar_gateway.handle_public_request(&sess, &pf_none_req, None, false, now);
    assert_eq!(pf_none_rsp.status_code, STATUS_OK);
    let pf_none_wire = eve_proto::quasar::WireMessage::decode(&pf_none_rsp.payload.unwrap().value).expect("decode");
    assert!(pf_none_wire.fields.iter().any(|f| f.field_number == 1), "Faction absent must encode field 1 (unaligned)");
}

#[tokio::test]
async fn test_quasar_alliance_complete() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(102, 1, 90000001, 98000001);

    // Unknown alliance returns STATUS_NOT_FOUND before seeding
    let miss_req = make_public_req("eve.alliance.GetRequest", encode_seq_id(1, 99000001), now);
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&sess, &miss_req, None, false, now)
            .status_code,
        eve_server::quasar_gateway::STATUS_NOT_FOUND
    );

    ctx.db.with_mut_snapshot(|s| {
        s.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "EvoMu Parity Coalition".into(),
                short_name: "EVOMU".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: [98_000_001, 98_000_002].into_iter().collect(),
            },
        );
    });

    let mut search_bytes = Vec::new();
    encode_string_field(1, "EvoMu", &mut search_bytes);
    for (url, payload, want) in [
        ("eve.alliance.GetRequest", encode_seq_id(1, 99000001), "GetResponse"),
        ("eve.alliance.GetAllRequest", Vec::new(), "GetAllResponse"),
        ("eve.alliance.SearchRequest", search_bytes, "SearchResponse"),
        ("eve.alliance.corporation.GetMembersRequest", encode_seq_id(1, 99000001), "GetMembersResponse"),
    ] {
        let req = make_public_req(url, payload, now);
        let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
        assert_eq!(rsp.status_code, STATUS_OK, "failed: {url}");
        assert!(rsp.payload.unwrap().type_url.contains(want));
    }
}

#[tokio::test]
async fn test_quasar_corporation_skill_plans_complete() {
    // parity: feat:quasar.corporation, quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.corporation",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(103, 1, 90000001, 98000001);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    let bad_req = make_public_req("eve_public.corporation.skill.plan.CreateRequest", Vec::new(), now);
    assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &bad_req, None, false, now).status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);

    let (mut sp_attr, mut create_val) = (Vec::new(), Vec::new());
    encode_string_field(1, "Corp Fleet Plan", &mut sp_attr);
    encode_string_field(2, "Fleet doctrine", &mut sp_attr);
    encode_bytes_field(1, &sp_attr, &mut create_val);
    let create_req = make_public_req("eve_public.corporation.skill.plan.CreateRequest", create_val, now);
    let create_rsp = ctx.quasar_gateway.handle_public_request(&sess, &create_req, None, false, now);
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let plan_id_val = create_rsp.payload.unwrap().value;

    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("CreatedNotice")).unwrap_or(false)));

    let all_req = make_public_req("eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), now);
    let all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &all_req, None, false, now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(all_rsp.payload.unwrap().type_url.contains("GetAllResponse"));

    let get_req = make_public_req("eve_public.corporation.skill.plan.GetRequest", plan_id_val.clone(), now);
    let get_rsp = ctx.quasar_gateway.handle_public_request(&sess, &get_req, None, false, now);
    assert_eq!(get_rsp.status_code, STATUS_OK, "GetRequest failed: {}", get_rsp.status_message);

    let mut name_val = plan_id_val.clone();
    encode_string_field(2, "Updated Fleet Plan", &mut name_val);
    let name_req = make_public_req("eve_public.corporation.skill.plan.SetNameRequest", name_val, now);
    let name_rsp = ctx.quasar_gateway.handle_public_request(&sess, &name_req, None, false, now);
    assert_eq!(name_rsp.status_code, STATUS_OK, "SetName failed: {}", name_rsp.status_message);

    // 5. Milestones: Create, GetAll, SetDescription, authority checks, Delete
    let mut ms_attr = plan_id_val.clone();
    encode_string_field(4, "Milestone Alpha", &mut ms_attr);
    let mut ms_val = Vec::new();
    encode_bytes_field(1, &ms_attr, &mut ms_val);

    // Non-director member without roles must be rejected with STATUS_FORBIDDEN
    let mut non_admin_sess = make_auth_session(199, 2, 90000002, 98000001);
    non_admin_sess.role = 0;
    non_admin_sess.corp_role = 0;
    let unauth_ms_req = make_public_req("eve_public.corporation.skill.plan.milestone.CreateRequest", ms_val.clone(), now);
    let unauth_ms_rsp = ctx.quasar_gateway.handle_public_request(&non_admin_sess, &unauth_ms_req, None, false, now);
    assert_eq!(unauth_ms_rsp.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

    let ms_req = make_public_req("eve_public.corporation.skill.plan.milestone.CreateRequest", ms_val, now);
    let ms_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_req, None, false, now);
    assert_eq!(ms_rsp.status_code, STATUS_OK, "Milestone create failed: {}", ms_rsp.status_message);
    let ms_id_val = ms_rsp.payload.expect("milestone CreateResponse payload").value;
    assert!(!ms_id_val.is_empty());

    let ms_all_req = make_public_req("eve_public.corporation.skill.plan.milestone.GetAllRequest", plan_id_val.clone(), now);
    let ms_all_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_all_req, None, false, now);
    assert_eq!(ms_all_rsp.status_code, STATUS_OK, "Milestone getAll failed: {}", ms_all_rsp.status_message);
    let ms_all_wm = eve_proto::quasar::WireMessage::decode(&ms_all_rsp.payload.unwrap().value).expect("decode milestone GetAllResponse");
    assert_eq!(ms_all_wm.fields.len(), 1);
    let ms_entry_wm = eve_proto::quasar::WireMessage::decode(ms_all_wm.fields[0].value.as_bytes().unwrap()).expect("decode milestone entry");
    assert_eq!(ms_entry_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap(), Some("Milestone Alpha"));

    let mut ms_desc_val = ms_id_val.clone();
    encode_string_field(2, "Milestone Beta", &mut ms_desc_val);
    let ms_desc_req = make_public_req("eve_public.corporation.skill.plan.milestone.SetDescriptionRequest", ms_desc_val, now);
    let ms_desc_rsp = ctx.quasar_gateway.handle_public_request(&sess, &ms_desc_req, None, false, now);
    assert_eq!(ms_desc_rsp.status_code, STATUS_OK);

    let ms_all_rsp2 = ctx.quasar_gateway.handle_public_request(&sess, &ms_all_req, None, false, now);
    let ms_all_wm2 = eve_proto::quasar::WireMessage::decode(&ms_all_rsp2.payload.unwrap().value).unwrap();
    let ms_entry_wm2 = eve_proto::quasar::WireMessage::decode(ms_all_wm2.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(ms_entry_wm2.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap(), Some("Milestone Beta"));

    let ms_del_req = make_public_req("eve_public.corporation.skill.plan.milestone.DeleteRequest", ms_id_val, now);
    assert_eq!(ctx.quasar_gateway.handle_public_request(&non_admin_sess, &ms_del_req, None, false, now).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
    assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &ms_del_req, None, false, now).status_code, STATUS_OK);
    let ms_all_rsp3 = ctx.quasar_gateway.handle_public_request(&sess, &ms_all_req, None, false, now);
    assert!(ms_all_rsp3.payload.unwrap().value.is_empty(), "Deleted milestone must be removed from GetAll");

    // 6. DeleteRequest
    let del_req = make_public_req("eve_public.corporation.skill.plan.DeleteRequest", plan_id_val, now);
    let del_rsp = ctx.quasar_gateway.handle_public_request(&sess, &del_req, None, false, now);
    assert_eq!(del_rsp.status_code, STATUS_OK, "Delete failed: {}", del_rsp.status_message);
}

#[tokio::test]
async fn test_quasar_corporation_governance_complete() {
    // parity: feat:quasar.corporation
    eve_testkit::attest(&["feat:quasar.corporation"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sess = make_auth_session(104, 1, 90000001, 98000001);

    // Seed authoritative corporation state across all 7 sub-domains:
    // accounting, divisions, home station, industry, members, alliance membership, and ALSC logs.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.members.insert(90_000_001);
            corp.alliance_id = Some(99_000_001);
            corp.hq_station_id = Some(60_003_760);
            corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(250_000_00));
            corp.wallet_divisions[0].name = "Ops Treasury".into();
            corp.member_roles.insert(90_000_001, 0x01);
        }
        snap.corp_hangar_divisions.entry(98_000_001).or_default().insert(1, "Capital Bay".into());
        snap.corp_member_limits.insert(98_000_001, 640);
        snap.corp_alliance_history.insert(98_000_001, vec![eve_db::snapshot_ext::CorpAllianceHistoryEntry {
            record_id: 701, alliance_id: Some(99_000_001), start_date_unix: now - 3600, is_deleted: Some(false),
        }]);
        snap.corp_container_logs.insert(98_000_001, vec![eve_db::snapshot_ext::CorpContainerLogEntry {
            logged_at_unix: now - 60, container_id: 700_001, container_type_id: 17_366, character_id: 90_000_001,
            location_id: 60_003_760, location_flag: "CorpSAG1".into(), action: "Add".into(), password_type: None,
            type_id: Some(34), quantity: Some(500), old_config_bitmask: None, new_config_bitmask: None,
        }]);
        snap.industry_jobs.insert(880_001, eve_db::IndustryJobRecord {
            job_id: 880_001, installer_id: 90_000_001, facility_id: 60_003_760, blueprint_item_id: 880_002,
            blueprint_type_id: 688, activity_id: 1, runs: 4, product_type_id: 587, status: "Active".into(),
            duration_seconds: 1800, start_unix: now,
        });
        snap.industry_jobs.insert(880_003, eve_db::IndustryJobRecord {
            job_id: 880_003, installer_id: 90_000_001, facility_id: 60_003_760, blueprint_item_id: 880_002,
            blueprint_type_id: 688, activity_id: 3, runs: 2, product_type_id: 688, status: "Active".into(),
            duration_seconds: 900, start_unix: now,
        });
        snap.items.insert(880_002, eve_db::InventoryItemRecord {
            item_id: 880_002, type_id: 688, owner_id: 98_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: std::collections::HashMap::from([(1112, 8.0), (1113, 16.0)]), ..Default::default()
        });
        snap.items.insert(770_001, eve_db::InventoryItemRecord {
            item_id: 770_001, type_id: 2233, owner_id: 98_000_001, location_id: 30_000_142,
            flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.corp_titles.insert(98_000_001, vec![eve_db::snapshot_ext::CorpTitleEntry { title_id: 1, name: "Fleet Admiral".into(), roles: vec!["Director".into()], ..Default::default() }]);
        snap.corp_member_titles.insert((98_000_001, 90_000_001), vec![1]);
        snap.corp_role_history.insert(98_000_001, vec![eve_db::snapshot_ext::CorpRoleHistoryEntry { character_id: 90_000_001, changed_at_unix: now - 30, issuer_id: 90_000_001, role_type: "roles".into(), old_roles: Vec::new(), new_roles: vec!["Director".into()] }]);
        snap.corp_vote_cases.insert(501, eve_db::snapshot_ext::CorpVoteCaseRecord { case_id: 501, corporation_id: 98_000_001, vote_type: 1, text: "CEO Vote".into(), description: "Election".into(), parameter: 0, start_ts: now - 100, end_ts: now + 3600, executed: false, options: Vec::new() });
        snap.starbase_settings.insert(1_035_000_001, eve_db::snapshot_ext::StarbaseDetailSettings::default());
        snap.corp_wallet_transactions.insert((98_000_001, 1), vec![eve_db::snapshot_ext::WalletTransactionRecord { transaction_id: 990_001, date_unix: now - 10, location_id: 60_003_760, type_id: 34, unit_price: 5.5, quantity: 100, client_id: 90_000_001, is_buy: true, is_personal: false, journal_ref_id: 1 }]);
        snap.corp_contacts.insert(98_000_001, vec![eve_db::snapshot_ext::SocialContactRecord { contact_id: 500_001, contact_type: "faction".into(), standing: 4.5, is_watched: false, is_blocked: false, label_ids: Vec::new() }]);
        snap.corp_customs_offices.insert(98_000_001, vec![eve_db::snapshot_ext::CustomsOfficeRecord { office_id: 770_001, system_id: 30_000_142, reinforce_exit_start: 18, reinforce_exit_end: 20, allow_alliance_access: true, allow_access_with_standings: false, standing_level: None, corporation_tax_rate: Some(0.05), alliance_tax_rate: Some(0.08), excellent_standing_tax_rate: None, good_standing_tax_rate: None, neutral_standing_tax_rate: None, bad_standing_tax_rate: None, terrible_standing_tax_rate: None }]);
        snap.structures.insert(1_035_000_001, eve_db::UpwellStructureRecord { structure_id: 1_035_000_001, type_id: 35_835, owner_corp_id: 98_000_001, solar_system_id: 30_000_142, name: "Corp Athanor".into(), position: [0.0, 0.0, 0.0], state: "Online".into(), quantum_core_installed: true, fuel_blocks: 100, shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None });
        snap.observer_mining_ledger.insert(1_035_000_001, Vec::new());
    });

    let call = |url: &str, payload: Vec<u8>| -> eve_proto::quasar::WireMessage {
        let req = make_public_req(url, payload, now);
        let rsp = ctx.quasar_gateway.handle_public_request(&sess, &req, None, false, now);
        assert_eq!(rsp.status_code, STATUS_OK, "failed: {url}: {}", rsp.status_message);
        eve_proto::quasar::WireMessage::decode(&rsp.payload.as_ref().expect("payload").value).expect("decode wm")
    };
    let field_varint = |wm: &eve_proto::quasar::WireMessage, num: u32| -> u64 {
        wm.fields.iter().find(|f| f.field_number == num).and_then(|f| f.value.as_varint()).expect("varint")
    };

    // 1. Accounting & Divisions & Home station
    let acct = call("eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest", encode_seq_id(2, 98_000_001));
    let div1 = eve_proto::quasar::WireMessage::decode(field_ld(&acct, 1)).unwrap();
    assert_eq!(field_varint(&div1, 1), 1000);
    let bal1 = eve_proto::quasar::WireMessage::decode(field_ld(&div1, 2)).unwrap();
    assert_eq!(field_varint(&bal1, 1), 500_000);
    let divs = call("eve.corporation.divisions.GetRequest", encode_seq_id(1, 98_000_001));
    let h1 = eve_proto::quasar::WireMessage::decode(field_ld(&divs, 1)).unwrap();
    assert_eq!(std::str::from_utf8(field_ld(&h1, 3)).unwrap(), "Capital Bay");
    let w1 = eve_proto::quasar::WireMessage::decode(field_ld(&divs, 2)).unwrap();
    assert_eq!(std::str::from_utf8(field_ld(&w1, 3)).unwrap(), "Ops Treasury");
    let hs = call("eve.corporation.homestation.GetRequest", encode_seq_id(1, 98_000_001));
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&hs, 1)).unwrap(), 1), 60_003_760);

    // 4. Industry: active jobs (manufacturing at field 4 + TE research at field 6), blueprints, facilities, and observers
    let jobs = call("eve.corporation.industry.job.GetActiveRequest", encode_seq_id(1, 98_000_001));
    let job_entries: Vec<_> = jobs.fields.iter().filter(|f| f.field_number == 1).map(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes().unwrap()).unwrap()).collect();
    assert_eq!(job_entries.len(), 2);
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&job_entries[0], 1)).unwrap(), 1), 880_001);
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&job_entries[0], 4)).unwrap(), 8), 4);
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&job_entries[1], 1)).unwrap(), 1), 880_003);
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&job_entries[1], 6)).unwrap(), 8), 2, "TE research encoded at field 6");
    let bps = call("eve.corporation.industry.blueprints.GetRequest", encode_seq_id(1, 98_000_001));
    let bp_e = eve_proto::quasar::WireMessage::decode(field_ld(&bps, 1)).unwrap();
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&bp_e, 1)).unwrap(), 1), 880_002);
    let bp_attr = eve_proto::quasar::WireMessage::decode(field_ld(&bp_e, 2)).unwrap();
    assert_eq!((field_varint(&bp_attr, 2), field_varint(&bp_attr, 3)), (8, 16), "blueprint dynamic ME=8, TE=16");
    let facs = call("eve.corporation.industry.facilties.GetRequest", encode_seq_id(1, 98_000_001));
    let fac_e = eve_proto::quasar::WireMessage::decode(field_ld(&facs, 1)).unwrap();
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&fac_e, 1)).unwrap(), 1), 1_035_000_001);
    let obs = call("eve.corporation.industry.observer.GetAllDetailsRequest", encode_seq_id(1, 98_000_001));
    let obs_e = eve_proto::quasar::WireMessage::decode(field_ld(&obs, 1)).unwrap();
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&obs_e, 1)).unwrap(), 1), 1_035_000_001);

    // 5. Members, titles, role auditlog, logo, vote, starbase, market entries, NPC standings, and PI offices
    let mems = call("eve.corporation.members.GetRequest", Vec::new());
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&mems, 1)).unwrap(), 1), 90_000_001);
    assert_eq!(field_varint(&call("eve.corporation.members.GetLimitRequest", Vec::new()), 1), 640);
    let roles = call("eve.corporation.members.roles.GetRequest", Vec::new());
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&roles, 1)).unwrap(), 2), 0x01);
    assert!(!call("eve.corporation.members.titles.GetRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.title.GetAllRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.members.roles.auditlog.GetRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.logo.api.GetRequest", encode_seq_id(1, 98_000_001)).fields.is_empty());
    assert!(!call("eve.corporation.vote.GetRequest", encode_seq_id(1, 501)).fields.is_empty());
    assert!(!call("eve.corporation.starbase.GetAllRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.starbase.GetSettingsAndFuelsRequest", encode_seq_id(3, 1_035_000_001)).fields.is_empty());
    assert!(!call("eve.corporation.market.entry.GetEntriesRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.standing.npc.GetAllRequest", Vec::new()).fields.is_empty());
    assert!(!call("eve.corporation.planetinteraction.office.GetRequest", Vec::new()).fields.is_empty());

    // 6. Alliance membership & ALSC container audit logs
    let cur_al = call("eve.corporation.alliance.api.GetCurrentMembershipRequest", encode_seq_id(1, 98_000_001));
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&cur_al, 1)).unwrap(), 1), 99_000_001);
    let al_hist = call("eve.corporation.alliancemembershiprecord.GetAllRequest", encode_seq_id(1, 98_000_001));
    let rec_e = eve_proto::quasar::WireMessage::decode(field_ld(&al_hist, 1)).unwrap();
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&rec_e, 1)).unwrap(), 1), 701);
    let alsc = call("eve.corporation.alsclogs.GetRequest", encode_seq_id(1, 98_000_001));
    let log_e = eve_proto::quasar::WireMessage::decode(field_ld(&alsc, 1)).unwrap();
    assert_eq!(field_varint(&eve_proto::quasar::WireMessage::decode(field_ld(&log_e, 2)).unwrap(), 1), 700_001);
    assert_eq!(field_varint(&log_e, 5), 116, "CorpSAG1 flag == 116");

    // Cross-corporation query by non-admin pilot in another corp must return STATUS_FORBIDDEN
    let mut foreign_sess = make_auth_session(105, 2, 90000002, 98000002);
    foreign_sess.role = 0;
    foreign_sess.corp_role = 0;
    for url in [
        "eve.corporation.members.GetRequest",
        "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
        "eve.corporation.alsclogs.GetRequest",
        "eve.corporation.alliance.api.GetCurrentMembershipRequest",
    ] {
        let cross_corp_req = make_public_req(url, encode_seq_id(1, 98000001), now);
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(&foreign_sess, &cross_corp_req, None, false, now).status_code,
            eve_server::quasar_gateway::STATUS_FORBIDDEN,
            "cross-corp {url} must be forbidden"
        );
    }
}
