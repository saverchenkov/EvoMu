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
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj, quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj
    eve_testkit::attest(&[
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
    // V24 `eve.membership.AllMembership`: 1 = corporation, 2 = alliance membership, 3 = faction membership
    // (no clone-state tier; Omega is checked through entitlements and clone grade below).
    let inner_field1 = |attr: &eve_proto::quasar::WireMessage, num: u32| -> Option<u64> {
        let b = attr.fields.iter().find(|f| f.field_number == num)?.value.as_bytes()?;
        eve_proto::quasar::WireMessage::decode(b).ok()?.fields.iter().find(|f| f.field_number == 1)?.value.as_varint()
    };
    assert_eq!((inner_field1(&pre_mem_attr, 2), inner_field1(&pre_mem_attr, 3)), (Some(1), Some(1)));

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
        PyRep::Integer(0)
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

    // 2. Verify caller user 1 has 'alliance_tournament_xx' but NOT 'omega', while target user 101 has deduplicated 'omega' (clone grade 1)
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
    assert_eq!((inner_field1(&u1_mem_attr, 2), inner_field1(&u1_mem_attr, 3)), (Some(1), Some(1)));

    let u101_rep = ctx.dispatch_rpc(&mut sess_101, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(get_ent_req.encode_proto())], &empty_kw, None).await.expect("GetRequest user 101");
    let u101_ents = decode_ent_names(&u101_rep);
    assert_eq!(u101_ents.iter().filter(|s| s.as_str() == "omega").count(), 1, "Target user 101 must receive deduplicated omega entitlement");

    let mem_rep = ctx.dispatch_rpc(&mut sess_101, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(mem_req.encode_proto())], &empty_kw, None).await.expect("membership user 101");
    let mem_wm = eve_proto::quasar::WireMessage::decode(&extract_response_proto(&mem_rep).payload.unwrap().value).unwrap();
    let mem_attr = eve_proto::quasar::WireMessage::decode(mem_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let corp_101 = ctx.db.with_snapshot(|s| s.characters[&90_000_101].corporation_id) as u64;
    assert_eq!(inner_field1(&mem_attr, 1), Some(corp_101), "AllMembership.corporation");
    assert_eq!(inner_field1(&mem_attr, 2), Some(1));
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_101, "loginRewardFacilities", None, "get_my_clone_grade", &[], &empty_kw, None).await.expect("clone grade post"),
        PyRep::Integer(1)
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


#[allow(dead_code, unused_imports)]
pub(crate) mod quasar_identity_ext {
    use super::*;

//  Acceptance tests for Quasar Payment, PLEX Vault, Store, Vendors & Legal (Part 2)
//  over `publicGatewaySvc`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_FORBIDDEN, STATUS_NOT_IMPLEMENTED, STATUS_OK,
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

async fn dispatch_req(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> EvePublicResponse {
    let req = build_public_request(ctx.now_unix(), type_name, payload);
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn make_varint_id_request(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    let mut out = Vec::new();
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_str_id_request(s: &str) -> Vec<u8> {
    let mut b = Vec::new();
    encode_string_field(1, s, &mut b);
    let mut out = Vec::new();
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_ta_offer_request(offer_id: u32) -> Vec<u8> {
    let mut entry = Vec::new();
    encode_string_field(1, &offer_id.to_string(), &mut entry);
    let mut id_msg = Vec::new();
    encode_bytes_field(2, &entry, &mut id_msg);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_msg, &mut out);
    out
}

#[tokio::test]
async fn test_q3_payment_purchase_and_schedules() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_006);
    session.user_id = Some(1);

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

    // 1. Payment code redeem
    let mut code_inner = Vec::new();
    encode_string_field(1, "EVE-OMEGA-30D-PROMO", &mut code_inner);
    let mut code_payload = Vec::new();
    encode_bytes_field(1, &code_inner, &mut code_payload);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.code.api.RedeemRequest",
        code_payload,
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
        .expect("RedeemRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // 2. Billing schedule query
    let uid = session.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        let schedules = vec![eve_server::services::quasar_payment_store_types::PaymentScheduleRecord {
            id: 101,
            user_id: uid,
            schedule_type: 1,
            next_payment_unix: ctx.now_unix() + 30 * 86400,
            offer_id: "1001".to_string(),
            interval_days: 30,
            created_unix: ctx.now_unix(),
        }];
        snap.service_state_blobs.insert("payment_schedules".to_string(), serde_json::to_string(&schedules).unwrap());
    });
    let mut sched_inner = Vec::new();
    encode_varint_field(1, 101, &mut sched_inner);
    let mut sched_payload = Vec::new();
    encode_bytes_field(1, &sched_inner, &mut sched_payload);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.schedule.api.GetRequest",
        sched_payload,
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
        .expect("GetScheduleRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));

    // 3. Add payment token and query it
    let tok_rec = eve_server::services::quasar_payment_store_types::PaymentTokenRecord {
        id: 101,
        user_id: 1,
        token_str: "tok_101".into(),
        is_quick_pay: true,
        consumed: false,
    };
    eve_server::services::quasar_payment_store_types::add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        1,
        tok_rec,
    );

    // Verify token query returns token
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.payment.token.api.GetQuickPayRequest",
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
        .expect("GetQuickPayRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetQuickPayResponse"));

    // Poll notices
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected payment schedule or token notices");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("QuickPayAddedNotice"));
}

#[tokio::test]
async fn test_q3_plex_vault_and_isk_wallets() {
    // parity: quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_007);
    session.user_id = Some(1);
    session.character_id = Some(1001);

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

    // 1. Initial balance check
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.plex.vault.api.BalanceRequest",
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
        .expect("BalanceRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("BalanceResponse"));

    // 2. Deposit PLEX -> unprivileged must fail with 403, GM succeeds
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 250 * 100, &mut curr_msg);
    let mut dep_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut dep_body);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        dep_body.clone(),
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
        .expect("DepositRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // Give GM role to session
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

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
        .expect("DepositRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    // 3. Withdraw PLEX
    let mut curr_msg = Vec::new();
    encode_varint_field(1, 50 * 100, &mut curr_msg);
    let mut with_body = Vec::new();
    encode_bytes_field(3, &curr_msg, &mut with_body);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        with_body,
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
        .expect("WithdrawRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    // 4. Poll notices -> BalanceChangedNotice present
    let notices = ctx.quasar_gateway.poll_notices(session.session_id);
    assert!(!notices.is_empty(), "Expected BalanceChangedNotice in inbox");
    assert!(notices[0]
        .payload
        .as_ref()
        .unwrap()
        .type_url
        .contains("plex"));
}

#[tokio::test]
async fn test_q3_store_categories_and_products() {
    // DATA GAP: quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj
    // (NES store catalog is live Quasar backend data not in V24 client FSD)
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut session = SessionState::new(2_008);
    session.user_id = Some(1);

    // 1. Get all categories
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.category.api.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get category with explicit ID 1
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.category.api.GetRequest", make_varint_id_request(1)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p_cat = rsp.payload.unwrap();
    assert!(!p_cat.value.is_empty());
    assert!(p_cat.type_url.ends_with("GetResponse"));

    // 3. Get product with explicit ID 101
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.product.api.GetRequest", make_varint_id_request(101)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(!p2.value.is_empty());
    assert!(p2.type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_store_offers_and_targetaudience() {
    // DATA GAP: requires SDE/live store catalog import (quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj, offer/api_pb2.pyj)
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut session = SessionState::new(2_009);
    session.user_id = Some(1);

    // 1. Get all offers
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p1 = rsp.payload.unwrap();
    assert!(!p1.value.is_empty());
    assert!(p1.type_url.ends_with("GetAllResponse"));

    // 2. Get offer with explicit ID 1001
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetRequest", make_str_id_request("1001")).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let p_off = rsp.payload.unwrap();
    assert!(!p_off.value.is_empty());
    assert!(p_off.type_url.ends_with("GetResponse"));

    // 3. Targeted offers
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetAllTargetedResponse"));

    // 4. Store targetaudience list
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.targetaudience.offer.api.ListRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("ListResponse"));

    // 5. Store targetaudience get with explicit ID 2001
    let rsp = dispatch_req(&ctx, &mut session, "eve_public.store.targetaudience.offer.api.GetRequest", make_ta_offer_request(2001)).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetResponse"));
}

#[tokio::test]
async fn test_q3_vendors_and_fulfillment() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_010);
    session.user_id = Some(1);
    session.character_id = Some(1001);
    ctx.db.with_mut_snapshot(|snap| {
        let mut rec = snap.characters.get(&90_000_001).cloned().unwrap();
        rec.character_id = 1001;
        snap.characters.insert(1001, rec);
    });

    // 1. Deliver item via fulfillment
    let mut item_type = Vec::new();
    encode_varint_field(1, 44992, &mut item_type);
    let mut fulfill_req = Vec::new();
    encode_bytes_field(2, &item_type, &mut fulfill_req);
    encode_varint_field(3, 10, &mut fulfill_req);

    let req = build_public_request(
        ctx.now_unix(),
        "eve.fulfillment.api.DeliverItemRequest",
        fulfill_req.clone(),
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
        .expect("DeliverItemRequest unauth");
    assert_eq!(extract_response_proto(&rep_unauth).status_code, STATUS_FORBIDDEN);

    // With GM role, DeliverItemRequest succeeds
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

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
        .expect("DeliverItemRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let items = ctx.db.get_items_for_character(1001);
    assert!(
        items.iter().any(|it| it.type_id == 44992),
        "Delivered item should exist in DB inventory"
    );

    // 2. Steam auth and RSA public key queries
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.steam.AuthenticateSteamRequest",
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
        .expect("AuthenticateSteamRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);

    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.auth.crypto.rsa.GetPublicKeyRequest",
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
        .expect("GetPublicKeyRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_OK);
}

#[tokio::test]
async fn test_q3_legal_pii_cdn_localization() {
    // parity: quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = std::sync::Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();
    let mut session = SessionState::new(2_011);
    session.user_id = Some(1);
    session.character_id = Some(1001);

    // 1. Achievement definition CDN GetLatestRequest
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
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
        .expect("achievement.definition.api.cdn.GetLatestRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.unwrap();
    assert!(payload.type_url.ends_with("GetLatestResponse"));

    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default().expect("registry");
    let dyn_cdn = reg
        .decode_dynamic("eve_public.achievement.definition.api.cdn.GetLatestResponse", &payload.value)
        .expect("decode GetLatestResponse");
    if let Some(eve_proto::quasar::DynamicValue::Message(cp)) = dyn_cdn.get(1) {
        let Some(eve_proto::quasar::DynamicValue::String(url)) = cp.get(1) else {
            panic!("Expected url string in checkpoint");
        };
        let path = url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
        assert!(path.starts_with("/cdn/achievements/v24.1.60/"), "unexpected path {path}");
        let esi = eve_server::esi::router::EsiRouter::new(std::sync::Arc::clone(&ctx));
        let cdn_http = esi.handle_request(&eve_server::esi::EsiHttpRequest::new("GET", path));
        assert_eq!(cdn_http.status_code, 200, "EsiRouter must serve achievement JSON bundle");
        assert_eq!(cdn_http.body, ctx.quasar_gateway.progression.achievements.sde_definitions_json);
        if let Some(eve_proto::quasar::DynamicValue::Message(ver)) = cp.get(2) {
            match ver.get(3) {
                Some(eve_proto::quasar::DynamicValue::Uint32(p)) => assert_eq!(*p, 60),
                Some(eve_proto::quasar::DynamicValue::Uint64(p)) => assert_eq!(*p, 60),
                other => panic!("Expected patch 60 in version, got {other:?}"),
            }
        } else {
            panic!("Expected version in checkpoint");
        }
    } else {
        panic!("Expected checkpoint message");
    }

    // 2. Space CDN GetUpdateRequest (MapTagProvider flatbuffer checkpoint)
    let bad_req = build_public_request(
        ctx.now_unix(),
        "eve_public.space.api.cdn.GetUpdateRequest",
        Vec::new(),
    );
    let bad_rep = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(bad_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("space.api.cdn.GetUpdateRequest empty");
    assert_eq!(extract_response_proto(&bad_rep).status_code, 400);

    let mut no_local_buf = Vec::new();
    eve_proto::quasar::encode_varint_field(2, 1, &mut no_local_buf);
    let req = build_public_request(
        ctx.now_unix(),
        "eve_public.space.api.cdn.GetUpdateRequest",
        no_local_buf,
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
        .expect("space.api.cdn.GetUpdateRequest");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK);
    let p2 = rsp.payload.unwrap();
    assert!(p2.type_url.ends_with("GetUpdateResponse"));
    let dyn_space_cdn = reg
        .decode_dynamic("eve_public.space.api.cdn.GetUpdateResponse", &p2.value)
        .expect("decode space GetUpdateResponse");
    let expected_patch = ctx.sde.solar_systems.len() as u64;
    if let Some(eve_proto::quasar::DynamicValue::Message(cp)) = dyn_space_cdn.get(1) {
        if let Some(eve_proto::quasar::DynamicValue::String(url)) = cp.get(1) {
            assert!(url.contains("maptags/v24.1.") && url.ends_with("/universe.fb"), "unexpected url {url}");
        } else {
            panic!("Expected url in space CDN checkpoint");
        }
        if let Some(eve_proto::quasar::DynamicValue::Message(ver)) = cp.get(2) {
            match ver.get(3) {
                Some(eve_proto::quasar::DynamicValue::Uint32(p)) => assert_eq!(*p as u64, expected_patch),
                Some(eve_proto::quasar::DynamicValue::Uint64(p)) => assert_eq!(*p, expected_patch),
                other => panic!("Expected patch {expected_patch}, got {other:?}"),
            }
        } else {
            panic!("Expected version in space CDN checkpoint");
        }
        assert!(cp.get(3).is_some(), "Expected crc checksum in space CDN checkpoint");
    } else {
        panic!("Expected checkpoint in space GetUpdateResponse");
    }
}

}
