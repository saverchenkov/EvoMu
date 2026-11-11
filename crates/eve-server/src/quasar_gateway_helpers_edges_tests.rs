//! Behavioral edge-case unit tests for `quasar_gateway_helpers`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::{corp_roles, CorpMemberRoleAssignment, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    EvePublicEvent, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleEmpty,
    GoogleTimestamp, NoticeTargetGroup, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::default())))
}

fn new_router() -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::new(WorldRepository::with_demo_seed()),
        shared_sde(),
    )
}

fn kv_field<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
        _ => None,
    }
}

#[test]
fn response_and_notice_to_pyrep_and_wire_validation_and_admin_guard() {
    let rsp_with_payload = EvePublicResponse {
        status_code: 200,
        status_message: "OK".into(),
        correlation_uuid: vec![1, 2, 3],
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.test.PingResponse".into(),
            value: vec![0xAA, 0xBB],
        }),
        ..Default::default()
    };
    let rep1 = response_to_pyrep(&rsp_with_payload);
    assert_eq!(kv_field(&rep1, "status_code"), Some(&PyRep::Integer(200)));
    assert_eq!(
        kv_field(&rep1, "status_message"),
        Some(&PyRep::String("OK".into()))
    );
    assert_eq!(
        kv_field(&rep1, "correlation_uuid"),
        Some(&PyRep::Bytes(vec![1, 2, 3]))
    );
    assert_eq!(
        kv_field(&rep1, "type_url"),
        Some(&PyRep::String(
            "type.googleapis.com/eve_public.test.PingResponse".into()
        ))
    );
    assert_eq!(
        kv_field(&rep1, "payload"),
        Some(&PyRep::Bytes(vec![0xAA, 0xBB]))
    );
    let raw_rsp = match kv_field(&rep1, "response_proto") {
        Some(PyRep::Bytes(b)) => b.clone(),
        other => panic!("expected response_proto bytes, got {other:?}"),
    };
    assert_eq!(
        EvePublicResponse::decode_proto(&raw_rsp).unwrap().status_code,
        200
    );

    let rsp_empty = EvePublicResponse {
        status_code: 404,
        status_message: "NotFound".into(),
        ..Default::default()
    };
    let rep2 = response_to_pyrep(&rsp_empty);
    assert_eq!(kv_field(&rep2, "type_url"), Some(&PyRep::String("".into())));
    assert_eq!(kv_field(&rep2, "payload"), Some(&PyRep::Bytes(vec![])));

    let notice_full = EvePublicNotice {
        uuid: vec![9, 8, 7],
        tenant: "tranquility".into(),
        target_group: Some(NoticeTargetGroup::Character(90_000_001)),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.test.Notice".into(),
            value: vec![1, 2],
        }),
        ..Default::default()
    };
    let nrep1 = notice_to_pyrep(&notice_full);
    assert_eq!(kv_field(&nrep1, "uuid"), Some(&PyRep::Bytes(vec![9, 8, 7])));
    assert_eq!(
        kv_field(&nrep1, "tenant"),
        Some(&PyRep::String("tranquility".into()))
    );
    assert_eq!(
        kv_field(&nrep1, "target_group"),
        Some(&PyRep::String("character".into()))
    );
    assert_eq!(
        kv_field(&nrep1, "type_url"),
        Some(&PyRep::String(
            "type.googleapis.com/eve_public.test.Notice".into()
        ))
    );
    let raw_notice = match kv_field(&nrep1, "notice_proto") {
        Some(PyRep::Bytes(b)) => b.clone(),
        other => panic!("expected notice_proto bytes, got {other:?}"),
    };
    assert_eq!(
        EvePublicNotice::decode_proto(&raw_notice).unwrap().tenant,
        "tranquility"
    );

    let notice_empty = EvePublicNotice::default();
    let nrep2 = notice_to_pyrep(&notice_empty);
    assert_eq!(
        kv_field(&nrep2, "target_group"),
        Some(&PyRep::String("".into()))
    );
    assert_eq!(kv_field(&nrep2, "payload"), Some(&PyRep::Bytes(vec![])));

    // require_admin_role
    let non_gm = AuthoritativeContext {
        service_role_mask: 0,
        ..Default::default()
    };
    assert_eq!(
        require_admin_role(&non_gm),
        Err((STATUS_FORBIDDEN, "Admin/GM role required".into()))
    );
    let gm = AuthoritativeContext {
        service_role_mask: service_roles::ROLE_GM_MASK,
        ..Default::default()
    };
    assert_eq!(require_admin_role(&gm), Ok(()));

    // validate_raw_wire_frame
    let mut buf = Vec::new();
    encode_varint_field(1, 1, &mut buf);
    let decoded_wire = validate_raw_wire_frame(&buf).unwrap();
    assert_eq!(decoded_wire.fields.len(), 1);
    assert!(validate_raw_wire_frame(&[0xFF, 0xFF, 0xFF]).is_err());
}

#[test]
fn resolve_live_session_corp_and_roles_handles_npc_player_ceo_and_missing_char() {
    let router = new_router();
    let now = 1_700_000_000;

    // 1. No character_id -> falls back to session.corporation_id with 0 roles
    let mut s_no_char = SessionState::new(1);
    assert_eq!(router.resolve_live_session_corp_and_roles(&s_no_char, now), (None, 0));
    s_no_char.corporation_id = Some(98_000_123);
    assert_eq!(
        router.resolve_live_session_corp_and_roles(&s_no_char, now),
        (Some(98_000_123), 0)
    );

    // 2. Character not in DB -> falls back to session.corporation_id (or None)
    let mut s_ghost = SessionState::new(2);
    s_ghost.character_id = Some(999_999_999);
    assert_eq!(router.resolve_live_session_corp_and_roles(&s_ghost, now), (None, 0));
    s_ghost.corporation_id = Some(98_000_555);
    assert_eq!(
        router.resolve_live_session_corp_and_roles(&s_ghost, now),
        (Some(98_000_555), 0)
    );

    // 3. Character in NPC corp (1_000_009) with session in player corp where char is CEO -> uses sc and u64::MAX
    let (npc_char_id, player_corp_id, other_char_id) = router.db.with_mut_snapshot(|snap| {
        let mut chars: Vec<i64> = snap.characters.keys().copied().collect();
        chars.sort_unstable();
        let c1 = chars[0];
        let c2 = chars.get(1).copied().unwrap_or(c1 + 1);
        snap.characters.get_mut(&c1).unwrap().corporation_id = 1_000_009;
        let pcorp = 98_000_777;
        snap.corporations.insert(
            pcorp,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = pcorp; __corp.name = "Test Corp".into(); __corp.ticker = "TSTC".into(); __corp.ceo_id = c1; __corp.members = HashSet::from([c1, c2]); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corp_role_assignments.insert(
            (pcorp, c2),
            CorpMemberRoleAssignment {
                character_id: c2,
                roles: corp_roles::ROLE_DIRECTOR,
                grantable_roles: 0,
                roles_at_hq: 0,
                roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_hq: 0,
                grantable_roles_at_base: 0,
                grantable_roles_at_other: 0,
            },
        );
        (c1, pcorp, c2)
    });

    let mut s_ceo = SessionState::new(3);
    s_ceo.character_id = Some(npc_char_id);
    s_ceo.corporation_id = Some(player_corp_id);
    assert_eq!(
        router.resolve_live_session_corp_and_roles(&s_ceo, now),
        (Some(player_corp_id as u32), u64::MAX)
    );

    // 4. Character in NPC corp with session pointing to a corp where char is NOT CEO/member -> keeps DB NPC corp
    router.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_888,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = 98_000_888; __corp.name = "Foreign Corp".into(); __corp.ticker = "FGN".into(); __corp.ceo_id = other_char_id; __corp.members = HashSet::from([other_char_id]); __corp.ensure_wallet_divisions(); __corp },
        );
    });
    s_ceo.corporation_id = Some(98_000_888);
    assert_eq!(
        router.resolve_live_session_corp_and_roles(&s_ceo, now),
        (Some(1_000_009), 0)
    );

    // 5. Character in player corp (not NPC range) with different session corp -> uses DB player corp and roles
    router.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&other_char_id) {
            ch.corporation_id = player_corp_id;
        }
    });
    let mut s_member = SessionState::new(4);
    s_member.character_id = Some(other_char_id);
    s_member.corporation_id = Some(98_000_888);
    let (resolved_corp, resolved_roles) =
        router.resolve_live_session_corp_and_roles(&s_member, now);
    assert_eq!(resolved_corp, Some(player_corp_id as u32));
    assert_ne!(resolved_roles & corp_roles::ROLE_DIRECTOR, 0);
}

#[test]
fn service_gateway_mgr_rpc_hijack_args_and_state_transitions() {
    let router = new_router();
    let empty_kw = HashMap::new();

    // Unknown method returns None
    assert!(router
        .dispatch_service_gateway_mgr_rpc("unknown", &[], &empty_kw)
        .is_none());

    // Initially disabled
    assert_eq!(
        router
            .dispatch_service_gateway_mgr_rpc("qa_is_hijack_mode_enabled", &[], &empty_kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // Enable with string/token error options & real/int latency
    for (err_rep, lat_rep) in [
        (PyRep::None, PyRep::None),
        (PyRep::String("None".into()), PyRep::Real(0.0)),
        (PyRep::String("".into()), PyRep::Integer(0)),
        (PyRep::String("TimeoutException".into()), PyRep::Real(1.5)),
        (PyRep::Token("UnpackException".into()), PyRep::Integer(300)),
        (PyRep::Integer(400), PyRep::Real(300.0)),
        (PyRep::Integer(503), PyRep::None),
    ] {
        let res = router
            .dispatch_service_gateway_mgr_rpc(
                "qa_enable_hijack_mode",
                &[err_rep, lat_rep],
                &empty_kw,
            )
            .unwrap();
        assert_eq!(res.unwrap(), PyRep::None);
        assert_eq!(
            router
                .dispatch_service_gateway_mgr_rpc("qa_is_hijack_mode_enabled", &[], &empty_kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
    }

    // Kwargs path
    let mut kw = HashMap::new();
    kw.insert("error".to_string(), PyRep::Integer(412));
    kw.insert("latency".to_string(), PyRep::Integer(2));
    assert_eq!(
        router
            .dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Disable
    assert_eq!(
        router
            .dispatch_service_gateway_mgr_rpc("qa_disable_hijack_mode", &[], &empty_kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        router
            .dispatch_service_gateway_mgr_rpc("qa_is_hijack_mode_enabled", &[], &empty_kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // Invalid error options
    for bad_err in [
        PyRep::String("InvalidErr".into()),
        PyRep::Integer(418),
        PyRep::Bool(true),
    ] {
        let err = router
            .dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[bad_err], &empty_kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    // Invalid latency options
    for bad_lat in [
        PyRep::Real(-0.1),
        PyRep::Real(300.1),
        PyRep::Real(f64::NAN),
        PyRep::Integer(-1),
        PyRep::Integer(301),
        PyRep::String("fast".into()),
    ] {
        let err = router
            .dispatch_service_gateway_mgr_rpc(
                "qa_enable_hijack_mode",
                &[PyRep::None, bad_lat],
                &empty_kw,
            )
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }
}

#[test]
fn public_gateway_rpc_subscribe_poll_publish_and_send_request_edges() {
    let router = new_router();
    let now = 1_700_000_500;
    let empty_kw = HashMap::new();

    let unauth = SessionState::new(50);
    let mut auth = SessionState::new(51);
    auth.user_id = Some(1001);
    auth.character_id = Some(90_000_001);

    // Unknown method
    assert!(router
        .dispatch_public_gateway_rpc(&auth, "UnknownMethod", &[], &empty_kw, now)
        .is_none());

    // Subscribe unauthenticated -> AuthenticationRequired
    let sub_unauth = router
        .dispatch_public_gateway_rpc(&unauth, "Subscribe", &[], &empty_kw, now)
        .unwrap()
        .unwrap_err();
    assert_eq!(sub_unauth.code, "AuthenticationRequired");

    // Subscribe / SubscribeToNotice / ConsumeNotices authenticated with String, List, Tuple
    let sub_rep = router
        .dispatch_public_gateway_rpc(
            &auth,
            "SubscribeToNotice",
            &[
                PyRep::String("eve_public.test.NoticeA".into()),
                PyRep::List(vec![PyRep::String("eve_public.test.NoticeB".into())]),
                PyRep::Tuple(vec![PyRep::String("eve_public.test.NoticeC".into())]),
                PyRep::Integer(123),
            ],
            &empty_kw,
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(kv_field(&sub_rep, "subscribed"), Some(&PyRep::Bool(true)));
    assert!(matches!(
        kv_field(&sub_rep, "application_instance_uuid"),
        Some(PyRep::Bytes(b)) if !b.is_empty()
    ));

    // PollNotices initially empty
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(&auth, "PollNotices", &[], &empty_kw, now)
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );

    // PublishEvent missing arg -> None
    assert!(router
        .dispatch_public_gateway_rpc(&auth, "PublishEvent", &[], &empty_kw, now)
        .is_none());

    // PublishEvent unauthenticated -> AuthenticationRequired
    let valid_event_bytes = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.PingEvent"),
            value: vec![1, 2, 3],
        }),
        ..Default::default()
    }
    .encode_proto();
    let pub_unauth = router
        .dispatch_public_gateway_rpc(
            &unauth,
            "PublishEvent",
            &[PyRep::Bytes(valid_event_bytes.clone())],
            &empty_kw,
            now,
        )
        .unwrap()
        .unwrap_err();
    assert_eq!(pub_unauth.code, "AuthenticationRequired");

    // PublishEvent invalid protobuf -> InvalidProtobuf
    let pub_bad = router
        .dispatch_public_gateway_rpc(
            &auth,
            "PublishEvent",
            &[PyRep::Bytes(vec![0xFF, 0xFF, 0xFF])],
            &empty_kw,
            now,
        )
        .unwrap()
        .unwrap_err();
    assert_eq!(pub_bad.code, "InvalidProtobuf");

    // PublishEvent valid
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(
                &auth,
                "PublishEvent",
                &[PyRep::Buffer(valid_event_bytes.clone())],
                &empty_kw,
                now,
            )
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // PublishEventPayload with GoogleAny (with quasar prefix), fallback EvePublicEvent, and invalid bytes
    let mut any_bytes = Vec::new();
    let ping_type_url = format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.PingEvent");
    encode_string_field(1, &ping_type_url, &mut any_bytes);
    encode_bytes_field(2, &[0x08, 0x01], &mut any_bytes);
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(
                &auth,
                "PublishEventPayload",
                &[PyRep::Bytes(any_bytes)],
                &empty_kw,
                now,
            )
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(
                &auth,
                "PublishEventPayload",
                &[PyRep::Bytes(valid_event_bytes)],
                &empty_kw,
                now,
            )
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let pub_payload_bad = router
        .dispatch_public_gateway_rpc(
            &auth,
            "PublishEventPayload",
            &[PyRep::Bytes(vec![0xFF, 0xFF, 0xFF])],
            &empty_kw,
            now,
        )
        .unwrap()
        .unwrap_err();
    assert_eq!(pub_payload_bad.code, "InvalidProtobuf");

    // SendRequest missing request -> None
    assert!(router
        .dispatch_public_gateway_rpc(&auth, "SendRequest", &[], &empty_kw, now)
        .is_none());

    // SendRequest malformed wire frame -> status_code 400
    let bad_req_rep = router
        .dispatch_public_gateway_rpc(
            &auth,
            "CallQuasar",
            &[PyRep::Bytes(vec![0xFF, 0xFF, 0xFF])],
            &empty_kw,
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        kv_field(&bad_req_rep, "status_code"),
        Some(&PyRep::Integer(i64::from(STATUS_BAD_REQUEST)))
    );

    // Send via kwargs["request"] + kwargs["idempotency_key"] (String / Bytes)
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![0x11; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: GoogleEmpty.encode_proto(),
        }),
        external_origin: "client".into(),
        ..Default::default()
    };
    let mut send_kw = HashMap::new();
    send_kw.insert("request".into(), PyRep::Buffer(req.encode_proto()));
    send_kw.insert("idempotency_key".into(), PyRep::String("idem-1".into()));
    let ok_rep = router
        .dispatch_public_gateway_rpc(&auth, "Send", &[], &send_kw, now)
        .unwrap()
        .unwrap();
    assert!(matches!(
        kv_field(&ok_rep, "status_code"),
        Some(PyRep::Integer(_))
    ));
}
