//! Edge-case tests for `quasar_debug_race_ship_services`: non-GM authorization,
//! invalid/missing wire fields, zero-subscriber notice scopes, and decoder error paths.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DescriptorProto, EvePublicRequest, EvePublicResponse,
    FieldDescriptorProto, FieldLabel, FieldType, GoogleAny, QuasarDescriptorRegistry, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{RaceInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn call_gw(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        correlation_uuid: vec![7; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        ..Default::default()
    };
    router.handle_public_request(sess, &req, None, false, 1_760_000_000)
}

#[test]
fn debug_empty_payload_non_gm_admin_and_non_playable_race_filtering() {
    let mut reg = (*QuasarDescriptorRegistry::open_default_or_empty()).clone();
    reg.messages_by_full_name.insert(
        "eve.race.GetAllRequest".into(),
        DescriptorProto {
            name: "GetAllRequest".into(),
            full_name: "eve.race.GetAllRequest".into(),
            fields: vec![FieldDescriptorProto {
                name: "playable_only".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Bool,
                type_name: String::new(),
                oneof_index: None,
                json_name: "playableOnly".into(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let reg = Arc::new(reg);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut sde = SdeRegistry::with_canonical_seed();
    // Inject a non-playable race (race_id = 16, Jove) and a race without corvette_type_id
    sde.races.insert(
        16,
        RaceInfo {
            race_id: 16,
            name: "Jove".into(),
            description: "Enigmatic empire".into(),
            corvette_type_id: None,
            starter_skills: HashMap::new(),
        },
    );
    let sde = Arc::new(sde);

    let router = PublicGatewayRouter::new(reg, db.clone(), sde.clone());
    register_handlers(&router, db, sde);

    let mut player = SessionState::new(10);
    player.user_id = Some(42);
    player.character_id = Some(90_000_001);
    player.corporation_id = Some(98_000_001);
    player.role = service_roles::ROLE_PLAYER_DEFAULT;

    // 1. eve_public.debug.Request with missing/wrong-type field 1 echoes empty bytes
    let dbg_empty = call_gw(&router, &player, "eve_public.debug.Request", Vec::new());
    assert_eq!(dbg_empty.status_code, STATUS_OK);
    let wm_dbg = WireMessage::decode(&dbg_empty.payload.as_ref().unwrap().value).unwrap();
    assert_eq!(wm_dbg.fields.len(), 1);
    assert_eq!(wm_dbg.fields[0].field_number, 1);

    let mut dbg_wrong_type = Vec::new();
    encode_varint_field(1, 999, &mut dbg_wrong_type);
    let dbg_wrong_rsp = call_gw(&router, &player, "eve_public.debug.Request", dbg_wrong_type);
    assert_eq!(dbg_wrong_rsp.status_code, STATUS_OK);

    // 2. eve.quasar.admin.IsAuthorizedRequest blocked for non-GM player (403) and allowed for GM (200)
    let player_auth = call_gw(&router, &player, "eve.quasar.admin.IsAuthorizedRequest", Vec::new());
    assert_eq!(player_auth.status_code, 403);
    let mut gm = player.clone();
    gm.role = service_roles::ROLE_ADMIN;
    let gm_auth = call_gw(&router, &gm, "eve.quasar.admin.IsAuthorizedRequest", Vec::new());
    assert_eq!(gm_auth.status_code, STATUS_OK);
    assert_eq!(gm_auth.payload.as_ref().unwrap().value, vec![0x08, 0x01]);

    // 3. eve.race.GetAllRequest: all races includes Jove (16), playable_only=true excludes Jove
    let all_rsp = call_gw(&router, &player, "eve.race.GetAllRequest", Vec::new());
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let all_wm = WireMessage::decode(&all_rsp.payload.as_ref().unwrap().value).unwrap();

    let mut play_only_req = Vec::new();
    encode_varint_field(1, 1, &mut play_only_req);
    let play_rsp = call_gw(&router, &player, "eve.race.GetAllRequest", play_only_req);
    assert_eq!(play_rsp.status_code, STATUS_OK);
    let play_wm = WireMessage::decode(&play_rsp.payload.as_ref().unwrap().value).unwrap();
    assert!(all_wm.fields.len() > play_wm.fields.len());
}

#[test]
fn notices_zero_subscribers_wire_decoding_and_security_status_edges() {
    let mut reg = (*QuasarDescriptorRegistry::open_default_or_empty()).clone();
    reg.messages_by_full_name.insert(
        "eve.race.GetAllRequest".into(),
        DescriptorProto {
            name: "GetAllRequest".into(),
            full_name: "eve.race.GetAllRequest".into(),
            fields: vec![FieldDescriptorProto {
                name: "playable_only".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Bool,
                type_name: String::new(),
                oneof_index: None,
                json_name: "playableOnly".into(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let reg = Arc::new(reg);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let router = PublicGatewayRouter::new(reg, db, sde);

    let scope = NoticeScope::Character(90_999_999);
    assert_eq!(publish_debug_notice(&router, scope.clone(), b"abc").unwrap(), 0);
    assert_eq!(publish_monolith_event_notice(&router, scope.clone(), b"tr").unwrap(), 0);
    assert_eq!(publish_character_route_established(&router, scope.clone(), 90_000_001).unwrap(), 0);
    assert_eq!(
        publish_raffle_created_notice(&router, scope.clone(), 1, 2, 16, 1000, 500, 60_003_760, false).unwrap(),
        0
    );
    assert_eq!(publish_raffle_ticket_bought_notice(&router, scope.clone(), 2, 1, 1).unwrap(), 0);
    assert_eq!(publish_raffle_finished_notice(&router, scope.clone(), 2, 1).unwrap(), 0);
    assert_eq!(publish_raffle_expired_notice(&router, scope.clone(), 3).unwrap(), 0);
    assert_eq!(publish_challenge_progressed_notice(&router, scope, 1, 55, 10).unwrap(), 0);

    // Security status encoding omits field 2 when nanos == 0
    let exact_five = encode_security_status_value(5.0);
    let wm_five = WireMessage::decode(&exact_five).unwrap();
    assert_eq!(wm_five.fields.len(), 1);
    assert!((decode_security_status_value(&exact_five).unwrap() - 5.0).abs() < 1e-9);

    let exact_neg = encode_security_status_value(-10.0);
    assert!((decode_security_status_value(&exact_neg).unwrap() - (-10.0)).abs() < 1e-9);

    // Decode with unknown field 99 and non-varint fields 1 and 2 falls back to 0.0
    let mut non_varint = Vec::new();
    encode_bytes_field(1, b"not_varint", &mut non_varint);
    encode_bytes_field(2, b"not_varint", &mut non_varint);
    encode_varint_field(99, 123, &mut non_varint);
    assert!((decode_security_status_value(&non_varint).unwrap() - 0.0).abs() < 1e-9);

    // Invalid wire bytes fail to decode
    assert!(decode_security_status_value(&[0xFF, 0xFF]).is_err());

    // Verify wire structures of search tags & ship attributes
    let loc_wm = WireMessage::decode(&encode_localized_search_tag("item", 1033, "Tritanium")).unwrap();
    assert_eq!(loc_wm.fields[0].field_number, 1);
    let unloc_wm = WireMessage::decode(&encode_unlocalized_search_tag("cat", "Ship")).unwrap();
    assert_eq!(unloc_wm.fields[0].field_number, 2);
    let ship_wm = WireMessage::decode(&encode_ship_attributes(587, 90_000_001)).unwrap();
    assert_eq!(ship_wm.fields.len(), 2);
}
