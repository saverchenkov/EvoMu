//! Integration tests for Quasar Debug, Core Quasar Admin/Routes,
//! Race, Raffle, Region, School, Search, SeasonalEvent, SecurityStatus, and Ship domains.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicEvent,
    EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, STATUS_NOT_IMPLEMENTED, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_debug_race_ship_services::{
    decode_security_status_value, encode_localized_search_tag, encode_school_identifier,
    encode_security_status_value, encode_ship_attributes, encode_ship_identifier,
    encode_unlocalized_search_tag, publish_challenge_progressed_notice,
    publish_character_route_established, publish_debug_notice, publish_monolith_event_notice,
    publish_raffle_created_notice, publish_raffle_expired_notice, publish_raffle_finished_notice,
    publish_raffle_ticket_bought_notice,
};

fn make_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32, is_admin: bool) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30_000_142);
    s.role = if is_admin {
        eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_GM_DEFAULT
    } else {
        eve_machonet::service_roles::ROLE_PLAYER
    };
    s.corp_role = 0;
    s
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> eve_proto::quasar::EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-quasar-test".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now)
}

fn subscribe_notices(ctx: &ServerContext, sess: &SessionState, notice_types: &[&str]) {
    let types = notice_types.iter().map(|s| s.to_string()).collect();
    let _ = ctx
        .quasar_gateway
        .subscribe_session(sess, types, ctx.now_unix());
}

fn poll_notice_types(ctx: &ServerContext, sess: &SessionState) -> Vec<(String, Vec<u8>)> {
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    notices
        .into_iter()
        .filter_map(|n| {
            n.payload
                .map(|p| (p.normalized_type_name().to_string(), p.value))
        })
        .collect()
}

// parity: feat:quasar.debug, quasar:eveProto/generated/eve_public/debug/debug_pb2.pyj, quasar:eveProto/generated/eve/debug/monolith/event_pb2.pyj
// parity: feat:quasar.quasar, quasar:eveProto/generated/eve/quasar/routes_pb2.pyj, quasar:eveProto/generated/eve/quasar/config/config_pb2.pyj, quasar:eveProto/generated/eve/quasar/admin/admin_pb2.pyj
#[tokio::test]
async fn test_quasar_debug_and_quasar() {
    eve_testkit::attest(&[
        "feat:quasar.debug",
        "feat:quasar.quasar",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Debug: descriptor verification & gateway request/response/notice
    let dbg_public_idx = reg
        .files_by_proto_name
        .get("eve_public/debug/debug.proto")
        .expect("debug.proto");
    assert_eq!(reg.files[*dbg_public_idx].package, "eve_public.debug");
    assert!(reg.messages_by_full_name.contains_key("eve_public.debug.Notice"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.debug.Event"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.debug.Request"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.debug.Response"));

    let dbg_mono_idx = reg
        .files_by_proto_name
        .get("eve/debug/monolith/event.proto")
        .expect("event.proto");
    assert_eq!(reg.files[*dbg_mono_idx].package, "eve.debug.monolith.event");
    assert!(reg.messages_by_full_name.contains_key("eve.debug.monolith.event.EventEmitted"));

    let ctx = ServerContext::new();
    let sess = make_session(101, 1001, 90_000_001, 1_000_001, true);

    // Call eve_public.debug.Request (echo data)
    let ping_payload = b"ping-echo-42".to_vec();
    let mut dbg_req = Vec::new();
    encode_bytes_field(1, &ping_payload, &mut dbg_req);
    let dbg_rsp = quasar_call(&ctx, &sess, "eve_public.debug.Request", dbg_req);
    assert_eq!(dbg_rsp.status_code, STATUS_OK);
    let p = dbg_rsp.payload.expect("debug response payload");
    assert_eq!(p.normalized_type_name(), "eve_public.debug.Response");
    let wm = WireMessage::decode(&p.value).expect("decode debug response");
    let echoed = wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    assert_eq!(echoed, ping_payload.as_slice());

    // Debug notices: eve_public.debug.Notice and eve.debug.monolith.event.EventEmitted
    subscribe_notices(
        &ctx,
        &sess,
        &[
            "eve_public.debug.Notice",
            "eve.debug.monolith.event.EventEmitted",
            "eve.quasar.CharacterRouteEstablished",
        ],
    );
    let pub_dbg = publish_debug_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        b"trace-uuid-1234",
    )
    .expect("publish debug notice");
    assert!(pub_dbg >= 1);

    let pub_mono = publish_monolith_event_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        b"monolith-tracer-999",
    )
    .expect("publish monolith notice");
    assert!(pub_mono >= 1);

    // Client PublishEvent for eve_public.debug.Event
    let mut ev_body = Vec::new();
    encode_bytes_field(1, b"client-telemetry-data", &mut ev_body);
    let ev = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Event"),
            value: ev_body,
        }),
        ..Default::default()
    };
    let mut sess_mut = sess.clone();
    let rep = ctx
        .dispatch_rpc(
            &mut sess_mut,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[PyRep::Bytes(ev.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("PublishEvent");
    assert_eq!(rep, PyRep::Bool(true));

    // 2. Quasar Core: routes, config, and admin
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.CharacterRouteEstablished"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.SolarSystemRouteEstablished"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.ApplicationRouteEstablished"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.config.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.config.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.admin.IsAuthorizedRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.quasar.admin.IsAuthorizedResponse"));

    // Call eve.quasar.admin.IsAuthorizedRequest
    let mut auth_req = Vec::new();
    encode_string_field(2, "eve_public.admin.SuperUserOp", &mut auth_req);
    let auth_rsp = quasar_call(&ctx, &sess, "eve.quasar.admin.IsAuthorizedRequest", auth_req);
    assert_eq!(auth_rsp.status_code, STATUS_OK);
    let auth_p = auth_rsp.payload.expect("auth payload");
    assert_eq!(auth_p.normalized_type_name(), "eve.quasar.admin.IsAuthorizedResponse");
    let auth_wm = WireMessage::decode(&auth_p.value).expect("decode auth");
    assert_eq!(auth_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint(), Some(1));

    // Call eve.quasar.config.api.GetRequest
    let cfg_rsp = quasar_call(&ctx, &sess, "eve.quasar.config.api.GetRequest", Vec::new());
    assert_eq!(cfg_rsp.status_code, STATUS_OK);

    // Publish CharacterRouteEstablished notice
    let pub_route = publish_character_route_established(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
    )
    .expect("publish route");
    assert!(pub_route >= 1);

    let polled = poll_notice_types(&ctx, &sess);
    assert!(polled.iter().any(|(t, _)| t == "eve_public.debug.Notice"));
    assert!(polled.iter().any(|(t, _)| t == "eve.debug.monolith.event.EventEmitted"));
    assert!(polled.iter().any(|(t, _)| t == "eve.quasar.CharacterRouteEstablished"));
}

// parity: feat:quasar.race, quasar:eveProto/generated/eve/race/race_pb2.pyj
#[tokio::test]
async fn test_quasar_race() {
    eve_testkit::attest(&[
        "feat:quasar.race",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");
    let ctx = ServerContext::new();
    let sess = make_session(101, 1001, 90_000_001, 1_000_001, true);

    // Quasar Race: GetAllRequest for playable and all races
    assert!(reg.messages_by_full_name.contains_key("eve.race.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.race.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.race.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.race.GetAllResponse"));

    // Playable races: Caldari (1), Minmatar (2), Amarr (4), Gallente (8)
    let mut play_req = Vec::new();
    encode_varint_field(1, 1, &mut play_req); // playable = true
    let play_rsp = quasar_call(&ctx, &sess, "eve.race.GetAllRequest", play_req);
    assert_eq!(play_rsp.status_code, STATUS_OK);
    let play_p = play_rsp.payload.expect("race payload");
    assert_eq!(play_p.normalized_type_name(), "eve.race.GetAllResponse");
    let play_wm = WireMessage::decode(&play_p.value).expect("decode play_wm");
    let race_subs: Vec<_> = play_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(race_subs.len(), 4, "Playable races count must be 4");

    // All races: 11 SDE races
    let all_rsp = quasar_call(&ctx, &sess, "eve.race.GetAllRequest", Vec::new());
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let all_p = all_rsp.payload.expect("all races payload");
    let all_wm = WireMessage::decode(&all_p.value).expect("decode all_wm");
    let all_race_subs: Vec<_> = all_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(all_race_subs.len(), 11, "Total SDE races count must be 11");
}

// parity: feat:quasar.raffle, quasar:eveProto/generated/eve/raffle/raffle_pb2.pyj
// parity: feat:quasar.region, quasar:eveProto/generated/eve_public/region/region_pb2.pyj, quasar:eveProto/generated/eve/region/region_pb2.pyj
#[tokio::test]
async fn test_quasar_raffle_and_region() {
    eve_testkit::attest(&[
        "feat:quasar.raffle",
        "feat:quasar.region",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Raffle: descriptor verification & lifecycle notices
    assert!(reg.messages_by_full_name.contains_key("eve.raffle.ItemRaffleCreated"));
    assert!(reg.messages_by_full_name.contains_key("eve.raffle.TicketBought"));
    assert!(reg.messages_by_full_name.contains_key("eve.raffle.Finished"));
    assert!(reg.messages_by_full_name.contains_key("eve.raffle.Expired"));

    let ctx = ServerContext::new();
    let sess = make_session(102, 1002, 90_000_002, 1_000_002, false);

    subscribe_notices(
        &ctx,
        &sess,
        &[
            "eve.raffle.ItemRaffleCreated",
            "eve.raffle.TicketBought",
            "eve.raffle.Finished",
            "eve.raffle.Expired",
        ],
    );

    let n1 = publish_raffle_created_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_002),
        90_000_002,
        5001,
        100,
        1_000_000,
        587,
        60_003_760,
        false,
    )
    .expect("publish raffle created");
    assert!(n1 >= 1);

    let n2 = publish_raffle_ticket_bought_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_002),
        5001,
        7,
        90_000_002,
    )
    .expect("publish ticket bought");
    assert!(n2 >= 1);

    let n3 = publish_raffle_finished_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_002),
        5001,
        90_000_002,
    )
    .expect("publish finished");
    assert!(n3 >= 1);

    let n4 = publish_raffle_expired_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_002),
        5002,
    )
    .expect("publish expired");
    assert!(n4 >= 1);

    let polled = poll_notice_types(&ctx, &sess);
    assert!(polled.iter().any(|(t, _)| t == "eve.raffle.ItemRaffleCreated"));
    assert!(polled.iter().any(|(t, _)| t == "eve.raffle.TicketBought"));
    assert!(polled.iter().any(|(t, _)| t == "eve.raffle.Finished"));
    assert!(polled.iter().any(|(t, _)| t == "eve.raffle.Expired"));

    // 2. Quasar Region: GetRequest, GetAllRequest, SearchRequest, Identifier
    assert!(reg.messages_by_full_name.contains_key("eve_public.region.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.GetAllResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.SearchRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.region.SearchResponse"));

    // GetRequest for The Forge (10000002)
    let mut reg_req = Vec::new();
    let mut id_sub = Vec::new();
    encode_varint_field(1, 10_000_002, &mut id_sub);
    encode_bytes_field(1, &id_sub, &mut reg_req);
    let reg_rsp = quasar_call(&ctx, &sess, "eve.region.GetRequest", reg_req);
    assert_eq!(reg_rsp.status_code, STATUS_OK);
    let reg_p = reg_rsp.payload.expect("region payload");
    assert_eq!(reg_p.normalized_type_name(), "eve.region.GetResponse");

    // GetAllRequest
    let all_reg_rsp = quasar_call(&ctx, &sess, "eve.region.GetAllRequest", Vec::new());
    assert_eq!(all_reg_rsp.status_code, STATUS_OK);

    // SearchRequest for "Forge"
    let mut search_req = Vec::new();
    let mut query_sub = Vec::new();
    encode_string_field(1, "Forge", &mut query_sub);
    encode_bytes_field(1, &query_sub, &mut search_req);
    let s_rsp = quasar_call(&ctx, &sess, "eve.region.SearchRequest", search_req);
    assert_eq!(
        s_rsp.status_code,
        STATUS_OK,
        "SearchRequest failed: {} (code {})",
        s_rsp.status_message,
        s_rsp.status_code
    );
}

// parity: feat:quasar.school, quasar:eveProto/generated/eve/school/school_pb2.pyj
// parity: feat:quasar.search, quasar:eveProto/generated/eve/search/tag_pb2.pyj
#[tokio::test]
async fn test_quasar_school_and_search() {
    eve_testkit::attest(&[
        "feat:quasar.school",
        "feat:quasar.search",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");
    let ctx = ServerContext::new();

    // 1. Quasar School: Identifier & SDE school mapping
    assert!(reg.messages_by_full_name.contains_key("eve.school.Identifier"));
    assert!(ctx.sde.schools.len() >= 12, "SDE contains starter schools");
    let school_id = 11u64; // Center for Advanced Studies
    let school_bytes = encode_school_identifier(school_id);
    let school_msg = reg
        .decode_dynamic("eve.school.Identifier", &school_bytes)
        .expect("decode school.Identifier");
    assert_eq!(
        school_msg.get(1),
        Some(&eve_proto::quasar::DynamicValue::Uint32(11))
    );
    let sch_entry = ctx.sde.schools.get(&(school_id as u32)).expect("school in SDE");
    assert_eq!(sch_entry.school_id, 11);
    assert_eq!(sch_entry.race_id, 4);
    assert_eq!(sch_entry.corporation_id, 1000166);

    // 2. Quasar Search: Tag with localized and unlocalized oneof variants
    assert!(reg.messages_by_full_name.contains_key("eve.search.Tag"));
    assert!(reg.messages_by_full_name.contains_key("eve.search.Tag.LocalizedKeyValue"));
    assert!(reg.messages_by_full_name.contains_key("eve.search.Tag.KeyValue"));

    let loc_bytes = encode_localized_search_tag("description", 0, "Ore Harvesting frigate"); // 0 = EN
    let loc_msg = reg
        .decode_dynamic("eve.search.Tag", &loc_bytes)
        .expect("decode localized search.Tag");
    if let Some(eve_proto::quasar::DynamicValue::Message(sub)) = loc_msg.get(1) {
        assert_eq!(
            sub.get(1),
            Some(&eve_proto::quasar::DynamicValue::String("description".to_string()))
        );
        assert_eq!(
            sub.get(2),
            Some(&eve_proto::quasar::DynamicValue::Enum(0))
        );
        assert_eq!(
            sub.get(3),
            Some(&eve_proto::quasar::DynamicValue::String("Ore Harvesting frigate".to_string()))
        );
    } else {
        panic!("expected LocalizedKeyValue message");
    }

    let unloc_bytes = encode_unlocalized_search_tag("category", "mining");
    let unloc_msg = reg
        .decode_dynamic("eve.search.Tag", &unloc_bytes)
        .expect("decode unlocalized search.Tag");
    if let Some(eve_proto::quasar::DynamicValue::Message(sub)) = unloc_msg.get(2) {
        assert_eq!(
            sub.get(1),
            Some(&eve_proto::quasar::DynamicValue::String("category".to_string()))
        );
        assert_eq!(
            sub.get(5),
            Some(&eve_proto::quasar::DynamicValue::String("mining".to_string()))
        );
    } else {
        panic!("expected KeyValue message");
    }
}

// parity: feat:quasar.seasonalevent, quasar:eveProto/generated/eve/seasonalevent/challenge_pb2.pyj
// parity: feat:quasar.securitystatus, quasar:eveProto/generated/eve_public/securitystatus/securitystatus_pb2.pyj, quasar:eveProto/generated/eve/securitystatus/securitystatus_pb2.pyj
// parity: feat:quasar.ship, quasar:eveProto/generated/eve_public/ship/ship_pb2.pyj, quasar:eveProto/generated/eve/ship/ship_pb2.pyj, quasar:eveProto/generated/eve_public/ship/unlock/requests_pb2.pyj
#[tokio::test]
async fn test_quasar_seasonalevent_securitystatus_and_ship() {
    eve_testkit::attest(&[
        "feat:quasar.seasonalevent",
        "feat:quasar.securitystatus",
        "feat:quasar.ship",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar SeasonalEvent: GetChallengesForCharacterRequest & notice delivery
    assert!(reg.messages_by_full_name.contains_key("eve.seasonalevent.challenge.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.seasonalevent.challenge.ChallengeProgress"));
    assert!(reg.messages_by_full_name.contains_key("eve.seasonalevent.challenge.GetChallengesForCharacterRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.seasonalevent.challenge.GetChallengesForCharacterResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.seasonalevent.challenge.ChallengeProgressed"));

    let ctx = ServerContext::new();
    let sess = make_session(103, 1003, 90_000_003, 1_000_003, false);

    // Call GetChallengesForCharacterRequest
    let mut chal_req = Vec::new();
    let mut c_sub = Vec::new();
    encode_varint_field(1, 90_000_003, &mut c_sub);
    encode_bytes_field(1, &c_sub, &mut chal_req);
    let chal_rsp = quasar_call(
        &ctx,
        &sess,
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        chal_req,
    );
    assert_eq!(chal_rsp.status_code, STATUS_OK);
    let chal_p = chal_rsp.payload.expect("seasonal payload");
    assert_eq!(
        chal_p.normalized_type_name(),
        "eve.seasonalevent.challenge.GetChallengesForCharacterResponse"
    );

    // Subscribe to ChallengeProgressed notice
    subscribe_notices(
        &ctx,
        &sess,
        &["eve.seasonalevent.challenge.ChallengeProgressed"],
    );
    let np = publish_challenge_progressed_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_003),
        90_000_003,
        2001,
        75,
    )
    .expect("publish challenge progress");
    assert!(np >= 1);
    let polled = poll_notice_types(&ctx, &sess);
    assert!(polled.iter().any(|(t, _)| t == "eve.seasonalevent.challenge.ChallengeProgressed"));

    // 2. Quasar SecurityStatus: Value codec across positive, zero, and negative values
    assert!(reg.messages_by_full_name.contains_key("eve_public.securitystatus.Value"));
    assert!(reg.messages_by_full_name.contains_key("eve.securitystatus.Value"));

    for &sec in &[5.0, 2.5, 0.0, -1.5, -4.8, -10.0] {
        let sec_bytes = encode_security_status_value(sec);
        let decoded = decode_security_status_value(&sec_bytes).expect("decode security status");
        assert!((decoded - sec).abs() < 1e-4, "decoded {decoded} must match {sec}");

        let dyn_val = reg
            .decode_dynamic("eve_public.securitystatus.Value", &sec_bytes)
            .expect("decode dynamic eve_public.securitystatus.Value");
        assert!(dyn_val.get(1).is_some());
    }

    // 3. Quasar Ship: unlock requests, identifiers, and attributes
    assert!(reg.messages_by_full_name.contains_key("eve_public.ship.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.ship.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.ship.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ship.unlock.GetPendingNotificationsRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ship.unlock.GetPendingNotificationsResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ship.unlock.ClaimRewardRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ship.unlock.ClaimRewardResponse"));

    // Call GetPendingNotificationsRequest -> returns STATUS_NOT_IMPLEMENTED (handled data gap)
    let unlock_rsp = quasar_call(
        &ctx,
        &sess,
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Vec::new(),
    );
    assert_eq!(unlock_rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // Call ClaimRewardRequest -> returns STATUS_NOT_IMPLEMENTED
    let mut claim_req = Vec::new();
    let mut ship_type_id = Vec::new();
    encode_varint_field(1, 587, &mut ship_type_id);
    encode_bytes_field(1, &ship_type_id, &mut claim_req);
    let claim_rsp = quasar_call(
        &ctx,
        &sess,
        "eve_public.ship.unlock.ClaimRewardRequest",
        claim_req,
    );
    assert_eq!(claim_rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // Ship Identifier & Attributes
    let ship_id = 100_000_001u64;
    let ship_id_bytes = encode_ship_identifier(ship_id);
    let dyn_ship_id = reg
        .decode_dynamic("eve.ship.Identifier", &ship_id_bytes)
        .expect("decode ship.Identifier");
    assert_eq!(
        dyn_ship_id.get(1),
        Some(&eve_proto::quasar::DynamicValue::Uint64(100_000_001))
    );

    let ship_attr_bytes = encode_ship_attributes(587, 90_000_003); // Rifter, owner 90000003
    let dyn_ship_attr = reg
        .decode_dynamic("eve.ship.Attributes", &ship_attr_bytes)
        .expect("decode ship.Attributes");
    assert!(dyn_ship_attr.get(1).is_some(), "type present");
    assert!(dyn_ship_attr.get(4).is_some(), "owner present");
}
