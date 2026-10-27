//! Acceptance tests for Quasar Identity, Accounts, Entitlements, Payment, PLEX Vault & Store
//! over `publicGatewaySvc`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    NoticeScope, STATUS_FORBIDDEN, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;

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

fn build_public_request(now_unix: u64, type_name: &str, payload_value: Vec<u8>) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-q3-test-uuid".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_value,
        }),
        external_origin: "eve-v24-client".into(),
        application_instance_uuid: b"app-q3-test-inst".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

#[tokio::test]
async fn test_q3_auth_login_device() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();

    // 1. Unauthenticated session -> 401 STATUS_UNAUTHORIZED
    let mut unauth_sess = SessionState::new(2_001);
    let ping_req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.AuthenticateUsernameRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ping_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_UNAUTHORIZED);
    assert_eq!(rsp.correlation_uuid, b"corr-q3-test-uuid");

    // Seed account with known credentials
    ctx.db.provision_player_account("test_pilot", "test_password_123");

    // 2. Wrong password -> 401 STATUS_UNAUTHORIZED
    let mut user_sess = SessionState::new(2_002);
    user_sess.user_id = Some(1);

    let mut bad_enc = Vec::new();
    encode_bytes_field(2, b"wrong_password", &mut bad_enc);
    let mut bad_creds = Vec::new();
    encode_string_field(1, "test_pilot", &mut bad_creds);
    encode_bytes_field(2, &bad_enc, &mut bad_creds);
    let mut bad_req_body = Vec::new();
    encode_bytes_field(1, &bad_creds, &mut bad_req_body);

    let bad_auth_req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.AuthenticateUsernameRequest",
        bad_req_body,
    );
    let rep = ctx
        .dispatch_rpc(
            &mut user_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(bad_auth_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_UNAUTHORIZED, "Wrong password failed with {}: {}", rsp.status_code, rsp.status_message);

    // 3. Valid credentials -> AuthenticateUsernameRequest succeeds
    let mut good_enc = Vec::new();
    encode_bytes_field(2, b"test_password_123", &mut good_enc);
    let mut creds = Vec::new();
    encode_string_field(1, "test_pilot", &mut creds);
    encode_bytes_field(2, &good_enc, &mut creds);
    let mut req_body = Vec::new();
    encode_bytes_field(1, &creds, &mut req_body);

    let auth_req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.AuthenticateUsernameRequest",
        req_body,
    );
    let rep = ctx
        .dispatch_rpc(
            &mut user_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(auth_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("AuthenticateUsernameResponse payload");
    assert_eq!(
        payload.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.AuthenticateUsernameResponse")
    );
    assert!(!payload.value.is_empty());
}

#[tokio::test]
async fn test_q3_user_accounts_lifecycle() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_003);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. eve_public.character.GetNamesByUserRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.character.GetNamesByUserRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetNamesByUserResponse payload");
    assert_eq!(
        payload.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserResponse")
    );

    // 2. eve_public.user.license.api.GetRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.user.license.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_user_license_and_subscription() {
    // parity: quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_004);
    session.user_id = Some(1);

    // Subscribe to notices
    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("Subscribe");

    // Publish ActivatedNotice and poll notices
    let mut notice_body = Vec::new();
    encode_varint_field(1, 1, &mut notice_body);
    let (_, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Broadcast,
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"
                ),
                value: notice_body,
            },
            ctx.now_unix(),
        )
        .expect("publish ActivatedNotice");
    assert_eq!(delivered, 1);

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected ActivatedNotice in inbox");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("user.license"));

    // Query license
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.user.license.api.GetRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GetRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("GetResponse payload");
    assert!(payload.type_url.ends_with("GetResponse"));
    assert!(!payload.value.is_empty());
}

#[tokio::test]
async fn test_q3_entitlements_omega_and_membership() {
    // parity: feat:quasar.entitlement, quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.entitlement",
        "quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_005);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. Grant entitlement -> enqueues GrantedNotice
    let mut ent_bytes = Vec::new();
    encode_string_field(3, "alliance_tournament_xx", &mut ent_bytes);
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &ent_bytes, &mut req_bytes);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.entitlement.user.api.GrantRequest",
        req_bytes.clone(),
    );
    let rep_unauth = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("GrantRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // Before granting Omega, membership must report inactive Alpha tier
    let mem_req = build_public_request(
        ctx.now_unix(),
        "eve.membership.GetCharacterAllRequest",
        Vec::new(),
    );
    let pre_mem_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(mem_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("pre-grant membership.GetCharacterAllRequest");
    let pre_mem_wm = eve_proto::quasar::WireMessage::decode(
        &extract_response_proto(&pre_mem_rep).payload.unwrap().value,
    )
    .unwrap();
    let pre_mem_attr = eve_proto::quasar::WireMessage::decode(
        pre_mem_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap(),
    )
    .unwrap();
    assert_eq!(
        pre_mem_attr.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).unwrap_or(0),
        0,
        "Membership before Omega grant must have active == 0"
    );
    assert_eq!(
        pre_mem_attr.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()),
        Some(b"alpha".as_slice())
    );

    // With GM role (user 1), GrantRequest without user ID grants to caller (user 1), while GrantRequest with sint32 user 101 grants to user 101 only
    ctx.db.with_mut_snapshot(|s| {
        if let Some(base) = s.characters.get(&90_000_001).cloned() {
            let mut c101 = base;
            c101.character_id = 90_000_101;
            c101.user_id = 101;
            c101.clone_grade = 1;
            s.characters.insert(90_000_101, c101);
        }
    });
    let mut sess_101 = SessionState::new(2_006);
    sess_101.user_id = Some(101);
    sess_101.character_id = Some(90_000_101);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_101, "loginRewardFacilities", None, "get_my_clone_grade", &[], &empty_kw, None).await.expect("clone grade pre"),
        PyRep::Integer(1)
    );

    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let rep = ctx.dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("GrantRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let (mut uid_101, mut omega_ent_bytes, mut omega_req_bytes) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, eve_proto::quasar::encode_zigzag32(101) as u64, &mut uid_101);
    encode_bytes_field(2, &uid_101, &mut omega_ent_bytes);
    encode_string_field(3, "omega", &mut omega_ent_bytes);
    encode_bytes_field(1, &omega_ent_bytes, &mut omega_req_bytes);
    let omega_grant_req = build_public_request(ctx.now_unix(), "eve.entitlement.user.api.GrantRequest", omega_req_bytes);
    for _ in 0..2 {
        let rep_omega = ctx.dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(omega_grant_req.encode_proto())], &empty_kw, None).await.expect("GrantRequest omega");
        assert_eq!(extract_response_proto(&rep_omega).status_code, STATUS_OK);
    }
    session.role = 0;

    // 2. Verify caller user 1 has 'alliance_tournament_xx' but NOT 'omega' (and remains Alpha), while target user 101 has deduplicated 'omega' and is Omega
    let get_ent_req = build_public_request(ctx.now_unix(), "eve_public.entitlement.user.api.GetRequest", Vec::new());
    let decode_ent_names = |rep: &PyRep| -> Vec<String> {
        let rsp = extract_response_proto(rep);
        assert_eq!(rsp.status_code, STATUS_OK);
        assert_eq!(rsp.correlation_uuid, b"corr-q3-test-uuid");
        let p = rsp.payload.unwrap();
        assert_eq!(p.normalized_type_name(), "eve_public.entitlement.user.api.GetResponse");
        let wm = eve_proto::quasar::WireMessage::decode(&p.value).expect("decode user GetResponse");
        wm.fields.iter().filter(|f| f.field_number == 1).filter_map(|f| f.value.as_bytes()).filter_map(|b| eve_proto::quasar::WireMessage::decode(b).ok()).filter_map(|sub| {
            sub.fields.iter().find(|sf| sf.field_number == 1).and_then(|sf| sf.value.as_bytes()).and_then(|sb| std::str::from_utf8(sb).ok()).map(|s| s.to_string())
        }).collect()
    };
    let u1_rep = ctx.dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(get_ent_req.encode_proto())], &empty_kw, None).await.expect("GetRequest user 1");
    let u1_ents = decode_ent_names(&u1_rep);
    assert!(u1_ents.contains(&"alliance_tournament_xx".to_string()));
    assert!(!u1_ents.contains(&"omega".to_string()), "Caller user 1 must NOT receive target user 101's omega grant");

    let u1_mem_rep = ctx.dispatch_rpc(&mut session, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(mem_req.encode_proto())], &empty_kw, None).await.expect("membership user 1");
    let u1_mem_wm = eve_proto::quasar::WireMessage::decode(&extract_response_proto(&u1_mem_rep).payload.unwrap().value).unwrap();
    let u1_mem_attr = eve_proto::quasar::WireMessage::decode(u1_mem_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(u1_mem_attr.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).unwrap_or(0), 0);
    assert_eq!(u1_mem_attr.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()), Some(b"alpha".as_slice()));

    let u101_rep = ctx.dispatch_rpc(&mut sess_101, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(get_ent_req.encode_proto())], &empty_kw, None).await.expect("GetRequest user 101");
    let u101_ents = decode_ent_names(&u101_rep);
    assert_eq!(u101_ents.iter().filter(|s| s.as_str() == "omega").count(), 1, "Target user 101 must receive deduplicated omega entitlement");

    let mem_rep = ctx.dispatch_rpc(&mut sess_101, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(mem_req.encode_proto())], &empty_kw, None).await.expect("membership user 101");
    let mem_wm = eve_proto::quasar::WireMessage::decode(&extract_response_proto(&mem_rep).payload.unwrap().value).unwrap();
    let mem_attr = eve_proto::quasar::WireMessage::decode(mem_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(mem_attr.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1));
    assert_eq!(mem_attr.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()), Some(b"omega".as_slice()));
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_101, "loginRewardFacilities", None, "get_my_clone_grade", &[], &empty_kw, None).await.expect("clone grade post"),
        PyRep::Integer(2)
    );

    let char_ent_req = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.character.GetAllRequest",
        Vec::new(),
    );
    let char_ent_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(char_ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("character.GetAllRequest");
    let char_ent_rsp = extract_response_proto(&char_ent_rep);
    assert_eq!(char_ent_rsp.status_code, STATUS_OK);
    assert_eq!(
        char_ent_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.entitlement.character.GetAllResponse"
    );
    assert!(
        char_ent_rsp.payload.as_ref().unwrap().value.is_empty(),
        "Un-granted logo entitlements must be empty"
    );

    // 4. Admin Grant/Revoke on ship alliancelogo and corplogo (non-GM forbidden, GM allowed)
    let _ = ctx
        .dispatch_rpc(&mut session, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let _ = ctx.quasar_gateway.poll_notices(session.session_id);

    let unpriv = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
        Vec::new(),
    );
    let rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(unpriv.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("unpriv admin GrantRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_FORBIDDEN);

    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let make_logo_req_bytes = |cid: u64, stid: u64| -> Vec<u8> {
        let ident = eve_server::services::quasar_entitlement_membership::encode_logo_identifier(cid, stid);
        let mut req_b = Vec::new();
        encode_bytes_field(1, &ident, &mut req_b);
        req_b
    };
    for (admin_req, stid) in [
        ("eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest", 17_715_u64),
        ("eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", 17_720_u64),
        ("eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", 17_722_u64),
        ("eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest", 17_722_u64),
    ] {
        let req = build_public_request(ctx.now_unix(), admin_req, make_logo_req_bytes(1001, stid));
        let rep = ctx
            .dispatch_rpc(
                &mut session,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect(admin_req);
        assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
    }

    // VerifyGetAllRequest returns both corp logo (17720) and alliance logo (17715), and revoking corplogo (17722) kept alliancelogo (17715)
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default().expect("QuasarDescriptorRegistry");
    for pyj in [
        "eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj",
        "eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj",
    ] {
        let fd = reg.get_file_by_pyj(pyj).expect(pyj);
        let msg_names: Vec<&str> = fd.messages.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(msg_names, vec!["Identifier", "GrantedNotice", "RevokedNotice"]);
    }
    let char_ent_after_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(char_ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("character.GetAllRequest after grants");
    let char_ent_after = extract_response_proto(&char_ent_after_rep).payload.unwrap();
    let decoded_ents = reg
        .decode_dynamic("eve_public.entitlement.character.GetAllResponse", &char_ent_after.value)
        .expect("decode GetAllResponse");
    let Some(eve_proto::quasar::DynamicValue::Repeated(ent_list)) = decoded_ents.get(1) else {
        panic!("Expected repeated entitlements in GetAllResponse, got {decoded_ents:?}");
    };
    assert_eq!(ent_list.len(), 2, "Expected 1 corp logo + 1 alliance logo entitlement after revoking 17722");
    let mut saw_corp_17720 = false;
    let mut saw_alliance_17715 = false;
    for item in ent_list {
        if let eve_proto::quasar::DynamicValue::Message(m) = item {
            if let Some(eve_proto::quasar::DynamicValue::Message(id_msg)) = m.get(1) {
                if let Some(eve_proto::quasar::DynamicValue::Message(ship_msg)) = id_msg.get(2) {
                    if ship_msg.get(1) == Some(&eve_proto::quasar::DynamicValue::Uint64(17_720)) {
                        saw_corp_17720 = true;
                    }
                }
            }
            if let Some(eve_proto::quasar::DynamicValue::Message(id_msg)) = m.get(2) {
                if let Some(eve_proto::quasar::DynamicValue::Message(ship_msg)) = id_msg.get(2) {
                    if ship_msg.get(1) == Some(&eve_proto::quasar::DynamicValue::Uint64(17_715)) {
                        saw_alliance_17715 = true;
                    }
                }
            }
        }
    }
    assert!(saw_corp_17720, "Must include granted corp logo ship 17720");
    assert!(saw_alliance_17715, "Revoking corp logo must preserve alliance logo ship 17715");

    // Also revoke alliance logo 17715 and poll/verify GrantedNotice and RevokedNotice payloads
    let revoke_alliance = build_public_request(
        ctx.now_unix(),
        "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
        make_logo_req_bytes(1001, 17_715),
    );
    let rep_rev = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(revoke_alliance.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("alliancelogo.RevokeRequest");
    assert_eq!(extract_response_proto(&rep_rev).status_code, STATUS_OK);

    let logo_notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert_eq!(logo_notices.len(), 5, "Expected 3 GrantedNotice + 2 RevokedNotice");
    let mut notice_types = Vec::new();
    for n in &logo_notices {
        let p = n.payload.as_ref().expect("notice payload");
        let tname = p.normalized_type_name();
        let dyn_n = reg.decode_dynamic(tname, &p.value).expect("decode logo notice");
        assert!(dyn_n.get(1).is_some(), "Notice {tname} must populate field 1");
        notice_types.push(tname.to_string());
    }
    assert!(notice_types.contains(&"eve_public.entitlement.character.ship.alliancelogo.GrantedNotice".to_string()));
    assert!(notice_types.contains(&"eve_public.entitlement.character.ship.corplogo.GrantedNotice".to_string()));
    assert!(notice_types.contains(&"eve_public.entitlement.character.ship.corplogo.RevokedNotice".to_string()));
    assert!(notice_types.contains(&"eve_public.entitlement.character.ship.alliancelogo.RevokedNotice".to_string()));
}

