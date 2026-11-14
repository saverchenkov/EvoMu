use super::*;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    AuthoritativeContext, DescriptorProto, EvePublicEvent, EvePublicRequest, FieldDescriptorProto,
    FieldLabel, FieldType, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, WireField,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn build_gateway_edges_router(reg: QuasarDescriptorRegistry) -> PublicGatewayRouter {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::default());
    PublicGatewayRouter::new(Arc::new(reg), db, sde)
}

fn make_gw_session(
    session_id: u64,
    user_id: Option<u64>,
    char_id: Option<i64>,
    corp_id: Option<i64>,
    alliance_id: Option<i64>,
    solar_system_id: Option<u32>,
    fleet_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = user_id;
    s.character_id = char_id;
    s.corporation_id = corp_id;
    s.alliance_id = alliance_id;
    s.solar_system_id = solar_system_id;
    s.fleet_id = fleet_id;
    s.role = role;
    s
}

fn make_req(
    type_name: &str,
    value: Vec<u8>,
    app_uuid: Vec<u8>,
    auth_ctx: Option<AuthoritativeContext>,
) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_700_000_000,
            nanos: 0,
        }),
        correlation_uuid: vec![0xBB; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "edge-test".into(),
        application_instance_uuid: app_uuid,
        authoritative_context: auth_ctx,
        trace_context: None,
    }
}

#[test]
fn notice_scope_routing_session_changes_streams_and_unsubscribe() {
    let router = build_gateway_edges_router(QuasarDescriptorRegistry::empty_with_builtins());
    let now = 1_700_000_000u64;

    let s1 = make_gw_session(101, Some(10), Some(90_001), Some(98_001), Some(99_001), Some(30_000_142), Some(7001), 0);
    let mut s2 = make_gw_session(102, Some(11), Some(90_002), Some(98_001), Some(99_002), Some(30_000_142), Some(7001), 0);
    let s3_no_char = make_gw_session(103, Some(12), None, None, None, Some(30_000_142), Some(7001), 0);

    let mut rx1 = router.subscribe_session(&s1, vec!["eve_public.debug.Notice".into()], now);
    let rx2 = router.subscribe_session(&s2, vec![], now);
    let _rx3 = router.subscribe_session(&s3_no_char, vec![], now);

    assert!(router.has_subscriber(101));
    assert!(router.has_subscriber(102));
    assert_eq!(PublicGatewayRouter::default_app_instance_uuid(101).len(), 16);

    assert!(router.take_stream_receiver(101).is_none());
    // Drop rx2 to exercise closed-channel send branch without panicking
    drop(rx2);

    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
        value: vec![1, 2, 3],
    };

    // Corporation targets s1 & s2
    let _ = router.publish_notice(NoticeScope::Corporation(98_001), any.clone(), now);
    assert!(rx1.try_recv().is_ok());

    // Alliance targets only s1
    let _ = router.publish_notice(NoticeScope::Alliance(99_001), any.clone(), now);
    assert!(rx1.try_recv().is_ok());

    // SolarSystem targets s1, s2, s3
    let _ = router.publish_notice(NoticeScope::SolarSystem(30_000_142), any.clone(), now);
    assert!(rx1.try_recv().is_ok());

    // User targets s1
    let _ = router.publish_notice(NoticeScope::User(10), any.clone(), now);
    assert!(rx1.try_recv().is_ok());

    // Fleet targets s1 & s2 (skips s3 which has character_id == None)
    let _ = router.publish_notice(NoticeScope::Fleet(7001), any.clone(), now);
    assert!(rx1.try_recv().is_ok());

    // ApplicationInstance matching app_uuid vs unmatched BubbleInstance
    let _ = router.publish_notice(
        NoticeScope::ApplicationInstance(PublicGatewayRouter::default_app_instance_uuid(101)),
        any.clone(),
        now,
    );
    assert!(rx1.try_recv().is_ok());
    let _ = router.publish_notice(NoticeScope::BubbleInstance(vec![1, 2, 3]), any.clone(), now);
    assert!(rx1.try_recv().is_err());

    // Move s2 to another system via on_session_changed
    s2.solar_system_id = Some(30_002_187);
    s2.alliance_id = Some(99_001);
    router.on_session_changed(&s2, now);
    let _ = router.publish_notice(NoticeScope::Alliance(99_001), any.clone(), now);
    assert_eq!(router.poll_notices(102).len(), 4);
    router.unsubscribe_session(102);
    assert!(!router.has_subscriber(102));
}

#[test]
fn handle_public_request_lazy_char_upgrade_gm_role_empty_ok_explicit_idem_and_nan_latency() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "eve.idempotency.Token".into(),
        DescriptorProto {
            name: "Token".into(),
            full_name: "eve.idempotency.Token".into(),
            fields: vec![FieldDescriptorProto {
                name: "uuid".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Bytes,
                type_name: String::new(),
                oneof_index: None,
                json_name: String::new(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    reg.messages_by_full_name.insert(
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest".into(),
        DescriptorProto {
            name: "GetAllRequest".into(),
            full_name: "eve_public.cosmetic.market.skin.listing.api.GetAllRequest".into(),
            fields: vec![FieldDescriptorProto {
                name: "token".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Message,
                type_name: ".eve.idempotency.Token".into(),
                oneof_index: None,
                json_name: String::new(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    let router = Arc::new(build_gateway_edges_router(reg));
    router.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().unwrap().clone();
        let mut c1 = base.clone();
        c1.character_id = 94_000_001;
        c1.user_id = 10;
        c1.name = "Owned Pilot".into();
        let mut c2 = base;
        c2.character_id = 94_000_002;
        c2.user_id = 99;
        c2.name = "Other Pilot".into();
        snap.characters.insert(c1.character_id, c1);
        snap.characters.insert(c2.character_id, c2);
    });

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cl = Arc::clone(&calls);
    router.register_handler(
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            calls_cl.fetch_add(1, Ordering::SeqCst);
            Ok(GoogleAny::default())
        }),
    );

    let no_char_session = make_gw_session(201, Some(10), None, None, None, None, None, service_roles::ROLE_PLAYER_DEFAULT);
    let now = 2_000_000u64;

    // 1. Lazy character upgrade with UNOWNED character -> stays None -> character-scoped request returns 403
    let unowned_ctx = AuthoritativeContext::for_session(Some(10), Some(94_000_002), None, 0, "tranquility", vec![]);
    let rsp_unowned = router.handle_public_request(
        &no_char_session,
        &make_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), vec![7; 16], Some(unowned_ctx)),
        None,
        false,
        now,
    );
    assert_eq!(rsp_unowned.status_code, STATUS_FORBIDDEN);

    // 2. Lazy character upgrade with OWNED character -> upgrades -> passes policy & executes handler!
    let empty_tok_inner = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(Vec::new()),
        }],
    }
    .encode();
    let wire_empty_tok = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(empty_tok_inner),
        }],
    }
    .encode();

    let owned_ctx = AuthoritativeContext::for_session(Some(10), Some(94_000_001), None, 0, "tranquility", vec![]);
    let rsp_owned = router.handle_public_request(
        &no_char_session,
        &make_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", wire_empty_tok.clone(), vec![9; 16], Some(owned_ctx.clone())),
        None,
        false,
        now,
    );
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    assert!(rsp_owned.payload.is_none(), "Ok(GoogleAny::default()) yields None payload");
    assert_eq!(rsp_owned.application_instance_uuid, vec![9; 16]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Re-send with empty token -> NOT cached, runs handler again
    let _ = router.handle_public_request(
        &no_char_session,
        &make_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", wire_empty_tok, vec![9; 16], Some(owned_ctx.clone())),
        None,
        false,
        now + 1,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // 3. Explicit idempotency key caches within TTL
    let _ = router.handle_public_request(
        &no_char_session,
        &make_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), vec![], Some(owned_ctx.clone())),
        Some(b"explicit-key-abc".to_vec()),
        false,
        now + 10,
    );
    let _ = router.handle_public_request(
        &no_char_session,
        &make_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), vec![], Some(owned_ctx)),
        Some(b"explicit-key-abc".to_vec()),
        false,
        now + 20,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    // 4. GM role allowed on admin-scoped request (`eve.fulfillment.api.DeliverItemRequest`)
    router.register_handler(
        "eve.fulfillment.api.DeliverItemRequest",
        Arc::new(|_ctx, _any, _dyn| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.fulfillment.api.DeliverItemResponse"),
                value: vec![1],
            })
        }),
    );
    let gm_session = make_gw_session(202, Some(10), Some(94_000_001), None, None, None, None, service_roles::ROLE_GM_MASK);
    let gm_rsp = router.handle_public_request(
        &gm_session,
        &make_req("eve.fulfillment.api.DeliverItemRequest", Vec::new(), vec![], None),
        None,
        false,
        now,
    );
    assert_eq!(gm_rsp.status_code, STATUS_OK);

    // 5. Server hijack mode with NaN/negative latency does NOT corrupt dispatched timestamp
    router.set_server_hijack_mode(true, None, Some(f64::NAN));
    let nan_rsp = router.handle_public_request(
        &gm_session,
        &make_req("google.protobuf.Empty", Vec::new(), vec![], None),
        None,
        false,
        now,
    );
    assert_eq!(nan_rsp.status_code, STATUS_OK);
    assert_eq!(nan_rsp.dispatched.unwrap().seconds, now as i64);
    router.set_server_hijack_mode(false, None, None);

    // 6. publish_event with pre-set `occurred`, `payload: None`, and `type_url` without slash
    let ev = EvePublicEvent {
        uuid: vec![1, 2],
        occurred: Some(GoogleTimestamp { seconds: 123_456, nanos: 789 }),
        payload: None,
        external_origin: "custom".into(),
        ..Default::default()
    };
    router.publish_event(&gm_session, ev, now);
    let last_ev = router.published_events.read().unwrap().back().unwrap().clone();
    assert_eq!(last_ev.occurred.unwrap().seconds, 123_456);

    // 7. Additional MachoNet gateway RPC edge branches
    let kw = HashMap::new();
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::String("TimeoutException".into()), PyRep::Real(0.5)],
            &kw,
        )
        .unwrap()
        .is_ok());
    router.set_server_hijack_mode(false, None, None);

    assert!(router
        .dispatch_public_gateway_rpc(&gm_session, "PublishEventPayload", &[PyRep::Bytes(vec![0xFF])], &kw, now)
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_public_gateway_rpc(&gm_session, "SendRequest", &[], &kw, now)
        .is_none());
    let bad_rsp = router
        .dispatch_public_gateway_rpc(&gm_session, "SendRequest", &[PyRep::Bytes(vec![0xFF])], &kw, now)
        .unwrap()
        .unwrap();
    assert!(!matches!(bad_rsp, PyRep::None));
    let raw_ev = EvePublicEvent {
        uuid: vec![5],
        payload: Some(GoogleAny {
            type_url: "eve_public.app.launcher.Start".into(),
            value: Vec::new(),
        }),
        ..Default::default()
    }
    .encode_proto();
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(&gm_session, "PublishEvent", &[PyRep::Bytes(raw_ev)], &kw, now)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(matches!(
        router
            .dispatch_public_gateway_rpc(&gm_session, "PollNotices", &[], &kw, now)
            .unwrap()
            .unwrap(),
        PyRep::List(_)
    ));
    assert!(router
        .dispatch_public_gateway_rpc(&gm_session, "NonExistentGatewayMethod", &[], &kw, now)
        .is_none());
}
