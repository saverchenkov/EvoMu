//! Unit tests for `quasar_debug_race_ship_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dispatch(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        correlation_uuid: vec![2; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        ..Default::default()
    };
    router.handle_public_request(sess, &req, None, false, now)
}

#[test]
fn debug_admin_races_notices_and_wire_encoders() {
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(reg, db.clone(), shared_sde());
    register_handlers(&router, db, shared_sde());
    let now = 1_750_000_000u64;

    let mut unauth = SessionState::new(1);
    unauth.user_id = None;
    let mut player = SessionState::new(2);
    player.user_id = Some(10);
    player.character_id = Some(90_000_001);
    player.corporation_id = Some(98_000_001);
    player.role = service_roles::ROLE_PLAYER_DEFAULT;
    let mut gm = player.clone();
    gm.role = service_roles::ROLE_GM_DEFAULT;

    // 1. eve_public.debug.Request echoes field 1 bytes
    let mut dbg_req = Vec::new();
    encode_bytes_field(1, b"ping-payload", &mut dbg_req);
    let dbg_rsp = dispatch(&router, &player, "eve_public.debug.Request", dbg_req, now);
    assert_eq!(dbg_rsp.status_code, STATUS_OK);
    assert!(dbg_rsp.payload.as_ref().unwrap().value.ends_with(b"ping-payload"));

    // 2. eve.quasar.admin.IsAuthorizedRequest checks user or admin role
    let auth0 = dispatch(&router, &unauth, "eve.quasar.admin.IsAuthorizedRequest", Vec::new(), now);
    assert_eq!(auth0.status_code, crate::quasar_gateway::STATUS_UNAUTHORIZED);
    let auth1 = dispatch(&router, &gm, "eve.quasar.admin.IsAuthorizedRequest", Vec::new(), now);
    assert_eq!(auth1.payload.as_ref().unwrap().value, vec![0x08, 0x01]);

    // 3. eve.race.GetAllRequest supports both playable_only = false and true
    let all_rsp = dispatch(&router, &player, "eve.race.GetAllRequest", Vec::new(), now);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let mut play_req = Vec::new();
    encode_varint_field(1, 1, &mut play_req);
    let play_rsp = dispatch(&router, &player, "eve.race.GetAllRequest", play_req, now);
    assert_eq!(play_rsp.status_code, STATUS_OK);

    // 4. All 8 notice publishers deliver to subscribed character session
    let mut rx = router.subscribe_session(&player, Vec::new(), now);
    let scope = NoticeScope::Character(90_000_001);
    assert_eq!(publish_debug_notice(&router, scope.clone(), b"u").unwrap(), 1);
    assert_eq!(publish_monolith_event_notice(&router, scope.clone(), b"t").unwrap(), 1);
    assert_eq!(publish_character_route_established(&router, scope.clone(), 90_000_001).unwrap(), 1);
    assert_eq!(publish_raffle_created_notice(&router, scope.clone(), 90_000_001, 11, 8, 500, 100, 60_003_760, true).unwrap(), 1);
    assert_eq!(publish_raffle_created_notice(&router, scope.clone(), 90_000_001, 12, 8, 500, 100, 60_003_760, false).unwrap(), 1);
    assert_eq!(publish_raffle_ticket_bought_notice(&router, scope.clone(), 11, 3, 90_000_001).unwrap(), 1);
    assert_eq!(publish_raffle_finished_notice(&router, scope.clone(), 11, 90_000_001).unwrap(), 1);
    assert_eq!(publish_raffle_expired_notice(&router, scope.clone(), 12).unwrap(), 1);
    assert_eq!(publish_challenge_progressed_notice(&router, scope, 90_000_001, 99, 25).unwrap(), 1);
    let mut received = 0usize;
    while rx.try_recv().is_ok() {
        received += 1;
    }
    assert_eq!(received, 9);

    // 5. Wire encoders & security status roundtrip
    let pos_bytes = encode_security_status_value(4.25);
    assert!((decode_security_status_value(&pos_bytes).unwrap() - 4.25).abs() < 1e-6);
    let neg_bytes = encode_security_status_value(-9.75);
    assert!((decode_security_status_value(&neg_bytes).unwrap() - (-9.75)).abs() < 1e-6);
    let zero_bytes = encode_security_status_value(0.0);
    assert!((decode_security_status_value(&zero_bytes).unwrap() - 0.0).abs() < 1e-6);

    assert_eq!(encode_school_identifier(14), vec![0x08, 14]);
    assert_eq!(encode_ship_identifier(587), vec![0x08, 0xCB, 0x04]);
    assert!(!encode_localized_search_tag("name", 0, "Jita").is_empty());
    assert!(!encode_unlocalized_search_tag("region", "TheForge").is_empty());
    assert!(!encode_ship_attributes(587, 90_000_001).is_empty());
}

#[test]
fn debug_race_auth_notice_failures_and_wire_codec_edges() {
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(reg, db.clone(), shared_sde());
    register_handlers(&router, db, shared_sde());
    let now = 1_750_000_000u64;

    let mut player = SessionState::new(22);
    player.user_id = Some(10);
    player.character_id = Some(90_000_001);
    player.corporation_id = Some(98_000_001);
    player.role = service_roles::ROLE_PLAYER_DEFAULT;

    // 1. debug.Request with empty payload echoes empty bytes field 1
    let dbg_empty = dispatch(&router, &player, "eve_public.debug.Request", Vec::new(), now);
    assert_eq!(dbg_empty.status_code, STATUS_OK);
    let dbg_wm = WireMessage::decode(&dbg_empty.payload.unwrap().value).unwrap();
    assert_eq!(dbg_wm.fields.len(), 1);
    assert_eq!(dbg_wm.fields[0].field_number, 1);
    assert_eq!(dbg_wm.fields[0].value.as_bytes(), Some(&[][..]));

    // 2. eve.quasar.admin.IsAuthorizedRequest: gateway enforces admin role (403 for non-admin),
    // while the handler itself checks admin role or effective_user_id.
    let auth_player = dispatch(&router, &player, "eve.quasar.admin.IsAuthorizedRequest", Vec::new(), now);
    assert_eq!(auth_player.status_code, crate::quasar_gateway::STATUS_FORBIDDEN);
    {
        let handlers = router.handlers.read().unwrap();
        let actx = eve_proto::quasar::AuthoritativeContext::for_session(
            Some(10),
            Some(90_000_001),
            Some(98_000_001),
            0,
            "tranquility",
            vec![],
        );
        let direct_rsp = handlers["eve.quasar.admin.IsAuthorizedRequest"](
            &actx,
            &GoogleAny::default(),
            &eve_proto::quasar::DynamicQuasarMessage::new("Req"),
        )
        .unwrap();
        assert_eq!(direct_rsp.value, vec![0x08, 0x01]);
    }

    // 3. eve.race.GetAllRequest verifies playable vs all races decoded structure
    let all_rsp = dispatch(&router, &player, "eve.race.GetAllRequest", Vec::new(), now);
    let all_wm = WireMessage::decode(&all_rsp.payload.unwrap().value).unwrap();
    let mut play_req = Vec::new();
    encode_varint_field(1, 1, &mut play_req);
    let play_rsp = dispatch(&router, &player, "eve.race.GetAllRequest", play_req, now);
    let play_wm = WireMessage::decode(&play_rsp.payload.unwrap().value).unwrap();
    assert!(all_wm.fields.len() >= play_wm.fields.len());
    assert!(!play_wm.fields.is_empty());
    for f in &play_wm.fields {
        assert_eq!(f.field_number, 1);
        let race_wm = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
        let id_bytes = race_wm
            .fields
            .iter()
            .find(|rf| rf.field_number == 1)
            .and_then(|rf| rf.value.as_bytes())
            .unwrap();
        let rid = WireMessage::decode(id_bytes).unwrap().fields[0]
            .value
            .as_varint()
            .unwrap();
        assert!(matches!(rid, 1 | 2 | 4 | 8));
    }

    // 4. Notice publishers return 0 when no clients are subscribed, and decode cleanly when subscribed
    let unsub_scope = NoticeScope::Character(99_999_123);
    assert_eq!(publish_debug_notice(&router, unsub_scope.clone(), b"u").unwrap(), 0);
    assert_eq!(publish_monolith_event_notice(&router, unsub_scope.clone(), b"t").unwrap(), 0);
    assert_eq!(publish_character_route_established(&router, unsub_scope.clone(), 99).unwrap(), 0);
    assert_eq!(publish_raffle_expired_notice(&router, unsub_scope.clone(), 12).unwrap(), 0);
    assert_eq!(publish_challenge_progressed_notice(&router, unsub_scope, 99, 5, 1).unwrap(), 0);

    let mut rx = router.subscribe_session(&player, Vec::new(), now);
    let scope = NoticeScope::Character(90_000_001);
    publish_raffle_created_notice(&router, scope.clone(), 90_000_001, 44, 16, 250, 777, 60_003_760, false).unwrap();
    let created_notice = rx.try_recv().unwrap();
    let created_wm = WireMessage::decode(&created_notice.payload.unwrap().value).unwrap();
    assert_eq!(
        created_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()),
        Some(44)
    );
    assert_eq!(
        created_wm.fields.iter().find(|f| f.field_number == 7).and_then(|f| f.value.as_varint()),
        Some(0)
    );

    publish_challenge_progressed_notice(&router, scope, 90_000_001, 321, 7).unwrap();
    let chal_notice = rx.try_recv().unwrap();
    let chal_wm = WireMessage::decode(&chal_notice.payload.unwrap().value).unwrap();
    let prog_wm = WireMessage::decode(
        chal_wm
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        prog_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()),
        Some(7)
    );

    // 5. Security status extremes & malformed wire decoding
    for val in [10.0, -10.0, 0.125, -0.125, -5.555555555] {
        let enc = encode_security_status_value(val);
        let dec = decode_security_status_value(&enc).unwrap();
        assert!((dec - val).abs() < 1e-6, "val={val} dec={dec}");
    }
    assert_eq!(decode_security_status_value(&[]).unwrap(), 0.0);
    assert!(decode_security_status_value(&[0x08, 0x80]).is_err());

    // 6. Wire structure checks for school, search tags, and ship attributes
    let loc_wm = WireMessage::decode(&encode_localized_search_tag("name", 1033, "Jita")).unwrap();
    let loc_inner = WireMessage::decode(loc_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(loc_inner.fields[1].value.as_varint(), Some(1033));

    let unloc_wm = WireMessage::decode(&encode_unlocalized_search_tag("region", "Forge")).unwrap();
    assert_eq!(unloc_wm.fields[0].field_number, 2);

    let ship_attr_wm = WireMessage::decode(&encode_ship_attributes(587, 90_000_001)).unwrap();
    assert_eq!(ship_attr_wm.fields[0].field_number, 1);
    assert_eq!(ship_attr_wm.fields[1].field_number, 4);
}
