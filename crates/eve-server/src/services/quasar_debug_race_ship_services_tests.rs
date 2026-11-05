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
