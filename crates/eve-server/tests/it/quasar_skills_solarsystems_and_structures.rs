//! Tests covering Quasar protobuf descriptors, routes, and notifications for:
//! - Skill, SolarSystem, SpaceComponent, Standing, Star, Starbase,
//!   StarterSpace, Station, Structure, and Tag domains.

use std::collections::HashMap;
use eve_db::{TrainedSkillRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::{NoticeScope, STATUS_NOT_IMPLEMENTED, STATUS_OK};
use eve_server::router::ServerContext;

fn make_session(session_id: u64, user_id: u64, char_id: u32, corp_id: u32) -> SessionState {
    let mut sess = SessionState::new(session_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-quasar-domain".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-domain".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now)
}

fn encode_id_msg(field_num: u32, id: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn poll_notice_names(ctx: &ServerContext, sess: &SessionState) -> Vec<String> {
    ctx.quasar_gateway
        .poll_notices(sess.session_id)
        .into_iter()
        .filter_map(|n| n.payload.map(|p| p.normalized_type_name().to_string()))
        .collect()
}

fn get_u64(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field) {
        Some(DynamicValue::Uint64(v)) => Some(*v),
        Some(DynamicValue::Uint32(v)) => Some(*v as u64),
        Some(DynamicValue::Int64(v)) => Some(*v as u64),
        Some(DynamicValue::Int32(v)) => Some(*v as u64),
        Some(DynamicValue::Enum(v)) => Some(*v as u64),
        Some(DynamicValue::Bool(b)) => Some(if *b { 1 } else { 0 }),
        _ => None,
    }
}

// parity: feat:quasar.skill, quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj, quasar:eveProto/generated/eve/skill/character/api/requests_pb2.pyj, quasar:eveProto/generated/eve/skill/plan/plan_pb2.pyj
// parity: feat:quasar.solarsystem, quasar:eveProto/generated/eve_public/solarsystem/api/requests_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/bank_pb2.pyj
#[tokio::test]
async fn test_quasar_skill_and_solarsystem() {
    eve_testkit::attest(&[
        "feat:quasar.skill",
        "feat:quasar.solarsystem",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Skill descriptor verification (21 proto files)
    let skill_files = [
        "eve/skill/character/api/requests.proto",
        "eve/skill/character_attributes.proto",
        "eve/skill/expertsystems/api/events.proto",
        "eve/skill/expertsystems/expertsystems.proto",
        "eve/skill/expertsystems/expertsystems_type.proto",
        "eve/skill/extractor/event.proto",
        "eve/skill/extractor/extractor.proto",
        "eve/skill/extractor/extractor_type.proto",
        "eve/skill/injector/event.proto",
        "eve/skill/injector/injector.proto",
        "eve/skill/injector/injector_type.proto",
        "eve/skill/plan/milestone.proto",
        "eve/skill/plan/plan.proto",
        "eve/skill/points/api/events.proto",
        "eve/skill/points/transaction.proto",
        "eve/skill/skill_category.proto",
        "eve/skill/skill_type.proto",
        "eve_public/skill/plan/certified.proto",
        "eve_public/skill/plan/milestone.proto",
        "eve_public/skill/plan/plan.proto",
        "eve_public/skill/skilltype.proto",
    ];
    for f in skill_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing skill proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("skill").map(|v| v.len()), Some(21));
    assert!(reg.messages_by_full_name.contains_key("eve.skill.character.api.GetLevelRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.skill.character.api.GetLevelResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.skill.plan.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.skill.plan.CreateRequest"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001u32;
    let mut sess = make_session(2001, 1, char_id, 1_000_001);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Seed character skills
    ctx.db.with_mut_snapshot(|s| {
        let mut sk_map = HashMap::new();
        sk_map.insert(3300u32, TrainedSkillRecord {
            type_id: 3300,
            level: 5,
            skill_points: 256_000.0,
        });
        s.skills.insert(char_id as i64, sk_map);
    });

    // Route eve.skill.character.api.GetLevelRequest
    let mut lvl_req = Vec::new();
    let mut char_inner = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_inner);
    encode_bytes_field(1, &char_inner, &mut lvl_req);
    let mut skill_inner = Vec::new();
    encode_varint_field(1, 3300, &mut skill_inner);
    encode_bytes_field(2, &skill_inner, &mut lvl_req);

    let lvl_rsp = quasar_call(&ctx, &sess, "eve.skill.character.api.GetLevelRequest", lvl_req);
    assert_eq!(lvl_rsp.status_code, STATUS_OK);
    let lvl_payload = lvl_rsp.payload.expect("lvl_payload");
    assert_eq!(lvl_payload.normalized_type_name(), "eve.skill.character.api.GetLevelResponse");
    let dyn_lvl = reg.decode_dynamic("eve.skill.character.api.GetLevelResponse", &lvl_payload.value)
        .expect("decode GetLevelResponse");
    assert_eq!(get_u64(&dyn_lvl, 1), Some(5));

    // Route eve_public.skill.plan.GetAllRequest
    let plan_rsp = quasar_call(&ctx, &sess, "eve_public.skill.plan.GetAllRequest", Vec::new());
    assert_eq!(plan_rsp.status_code, STATUS_OK);

    // 2. Quasar SolarSystem descriptor verification (9 proto files)
    let solarsystem_files = [
        "eve/solarsystem/api/requests.proto",
        "eve/solarsystem/bounty/bank/bank.proto",
        "eve/solarsystem/bounty/bank/mainbank.proto",
        "eve/solarsystem/bounty/bank/reservebank.proto",
        "eve/solarsystem/bounty/npc.proto",
        "eve/solarsystem/characters.proto",
        "eve/solarsystem/solarsystem.proto",
        "eve_public/solarsystem/api/requests.proto",
        "eve_public/solarsystem/solarsystem.proto",
    ];
    for f in solarsystem_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing solarsystem proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("solarsystem").map(|v| v.len()), Some(9));

    // Route eve_public.solarsystem.api.GetRequest
    let sys_req = encode_id_msg(1, 30_000_142);
    let sys_rsp = quasar_call(&ctx, &sess, "eve_public.solarsystem.api.GetRequest", sys_req);
    assert_eq!(sys_rsp.status_code, STATUS_OK);
    let sys_payload = sys_rsp.payload.expect("sys_payload");
    assert_eq!(sys_payload.normalized_type_name(), "eve_public.solarsystem.api.GetResponse");

    // Route eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest
    let jump_req = encode_id_msg(1, 30_000_142);
    let jump_rsp = quasar_call(&ctx, &sess, "eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest", jump_req);
    assert_eq!(jump_rsp.status_code, STATUS_OK);

    // Publish NoticeScope::SolarSystem eve.solarsystem.bounty.bank.main.MainBankLinkEstablished
    let mut link_buf = Vec::new();
    encode_varint_field(1, 30_000_142, &mut link_buf); // solar_system
    encode_varint_field(2, char_id as u64, &mut link_buf); // hacker

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.main.MainBankLinkEstablished"),
            value: link_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.solarsystem.bounty.bank.main.MainBankLinkEstablished".to_string()));
}

// parity: feat:quasar.spacecomponent, quasar:eveProto/generated/eve/spacecomponent/deploy/deploy_pb2.pyj, quasar:eveProto/generated/eve/spacecomponent/phaseable/phaseable_pb2.pyj
// parity: feat:quasar.standing, quasar:eveProto/generated/eve_public/standing/standing_pb2.pyj, quasar:eveProto/generated/eve/standing/api/commands_pb2.pyj
#[tokio::test]
async fn test_quasar_spacecomponent_and_standing() {
    eve_testkit::attest(&[
        "feat:quasar.spacecomponent",
        "feat:quasar.standing",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar SpaceComponent descriptor verification (9 proto files)
    let sc_files = [
        "eve/spacecomponent/deploy/api/events.proto",
        "eve/spacecomponent/deploy/deploy.proto",
        "eve/spacecomponent/deploy/deploy_type.proto",
        "eve/spacecomponent/phaseable/api/events.proto",
        "eve/spacecomponent/phaseable/phaseable.proto",
        "eve/spacecomponent/phaseable/phaseable_type.proto",
        "eve/spacecomponent/phasestabilizer/api/events.proto",
        "eve/spacecomponent/phasestabilizer/phasestabilizer.proto",
        "eve/spacecomponent/phasestabilizer/phasestabilizer_type.proto",
    ];
    for f in sc_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing spacecomponent proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("spacecomponent").map(|v| v.len()), Some(9));
    assert!(reg.messages_by_full_name.contains_key("eve.spacecomponent.deploy.api.Deployed"));
    assert!(reg.messages_by_full_name.contains_key("eve.spacecomponent.phaseable.api.PhasedIn"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_202u32;
    let mut sess = make_session(2002, 202, char_id, 1_000_202);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Dynamic encode and decode eve.spacecomponent.deploy.api.Deployed
    let mut deploy_buf = Vec::new();
    let mut dep_inner = Vec::new();
    encode_varint_field(1, 9901, &mut dep_inner);
    encode_bytes_field(1, &dep_inner, &mut deploy_buf); // deployable
    let mut type_inner = Vec::new();
    encode_varint_field(1, 4001, &mut type_inner);
    encode_bytes_field(2, &type_inner, &mut deploy_buf); // deployable_type
    let mut sys_inner = Vec::new();
    encode_varint_field(1, 30_000_142, &mut sys_inner);
    encode_bytes_field(3, &sys_inner, &mut deploy_buf); // solar_system
    let mut c_inner = Vec::new();
    encode_varint_field(1, char_id as u64, &mut c_inner);
    encode_bytes_field(4, &c_inner, &mut deploy_buf); // character

    let dyn_deployed = reg.decode_dynamic("eve.spacecomponent.deploy.api.Deployed", &deploy_buf)
        .expect("decode Deployed event");
    assert!(dyn_deployed.get(1).is_some());
    assert!(dyn_deployed.get(2).is_some());

    // Publish NoticeScope::SolarSystem eve.spacecomponent.deploy.api.Deployed
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.spacecomponent.deploy.api.Deployed"),
            value: deploy_buf,
        },
        ctx.now_unix(),
    );

    // Publish NoticeScope::SolarSystem eve.spacecomponent.phaseable.api.PhasedIn
    let mut phase_buf = Vec::new();
    let mut p_inner = Vec::new();
    encode_varint_field(1, 8801, &mut p_inner);
    encode_bytes_field(1, &p_inner, &mut phase_buf);
    let mut psys_inner = Vec::new();
    encode_varint_field(1, 30_000_142, &mut psys_inner);
    encode_bytes_field(2, &psys_inner, &mut phase_buf);

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.spacecomponent.phaseable.api.PhasedIn"),
            value: phase_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.spacecomponent.deploy.api.Deployed".to_string()));
    assert!(notices.contains(&"eve.spacecomponent.phaseable.api.PhasedIn".to_string()));

    // 2. Quasar Standing descriptor verification (4 proto files)
    let standing_files = [
        "eve/standing/api/commands.proto",
        "eve/standing/api/events.proto",
        "eve/standing/standing.proto",
        "eve_public/standing/standing.proto",
    ];
    for f in standing_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing standing proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("standing").map(|v| v.len()), Some(4));
    assert!(reg.messages_by_full_name.contains_key("eve.standing.api.IncreaseCommanded"));
    assert!(reg.messages_by_full_name.contains_key("eve.standing.api.DecreaseCommanded"));

    // Route eve.standing.api.IncreaseCommanded & DecreaseCommanded
    let inc_rsp = quasar_call(&ctx, &sess, "eve.standing.api.IncreaseCommanded", Vec::new());
    assert_eq!(inc_rsp.status_code, STATUS_OK);
    let dec_rsp = quasar_call(&ctx, &sess, "eve.standing.api.DecreaseCommanded", Vec::new());
    assert_eq!(dec_rsp.status_code, STATUS_OK);
}

// parity: feat:quasar.star, quasar:eveProto/generated/eve_public/star/star_pb2.pyj, quasar:eveProto/generated/eve/star/resources/api/requests_pb2.pyj
// parity: feat:quasar.starbase, quasar:eveProto/generated/eve/starbase/starbase_pb2.pyj
#[tokio::test]
async fn test_quasar_star_and_starbase() {
    eve_testkit::attest(&[
        "feat:quasar.star",
        "feat:quasar.starbase",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Star descriptor verification (4 proto files)
    let star_files = [
        "eve/star/resources/api/requests.proto",
        "eve/star/star.proto",
        "eve/star/star_type.proto",
        "eve_public/star/star.proto",
    ];
    for f in star_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing star proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("star").map(|v| v.len()), Some(4));
    assert!(reg.messages_by_full_name.contains_key("eve.star.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.star.resources.api.GetPowerProductionRequest"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_203u32;
    let sess = make_session(2003, 203, char_id, 1_000_203);

    // Call eve.star.resources.api.GetPowerProductionRequest (returns STATUS_NOT_IMPLEMENTED)
    let power_rsp = quasar_call(&ctx, &sess, "eve.star.resources.api.GetPowerProductionRequest", Vec::new());
    assert_eq!(power_rsp.status_code, STATUS_NOT_IMPLEMENTED);

    // Dynamic encode and decode eve.star.Attributes
    let mut star_buf = Vec::new();
    encode_string_field(1, "Sol Primary", &mut star_buf); // name
    let mut sys_buf = Vec::new();
    encode_varint_field(1, 30_000_142, &mut sys_buf);
    encode_bytes_field(2, &sys_buf, &mut star_buf); // solar_system
    encode_varint_field(3, 4_600_000_000, &mut star_buf); // age
    encode_varint_field(5, 696_340_000, &mut star_buf);   // radius
    encode_string_field(6, "G2 V", &mut star_buf);         // spectral_class
    encode_varint_field(7, 5778, &mut star_buf);          // temperature

    let dyn_star = reg.decode_dynamic("eve.star.Attributes", &star_buf)
        .expect("decode star.Attributes");
    assert_eq!(get_u64(&dyn_star, 3), Some(4_600_000_000));
    assert_eq!(get_u64(&dyn_star, 7), Some(5778));

    // 2. Quasar Starbase descriptor verification (2 proto files)
    let starbase_files = [
        "eve/starbase/starbase.proto",
        "eve/starbase/starbase_type.proto",
    ];
    for f in starbase_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing starbase proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("starbase").map(|v| v.len()), Some(2));
    assert!(reg.messages_by_full_name.contains_key("eve.starbase.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.starbase.State"));
    assert!(reg.messages_by_full_name.contains_key("eve.starbase.Settings"));
    assert!(reg.messages_by_full_name.contains_key("eve.starbase.Fuels"));

    // Dynamic encode and decode eve.starbase.State (flag = 5: STATE_ONLINE)
    let mut state_buf = Vec::new();
    encode_varint_field(1, 5, &mut state_buf); // flag = STATE_ONLINE
    let dyn_state = reg.decode_dynamic("eve.starbase.State", &state_buf)
        .expect("decode starbase.State");
    assert_eq!(get_u64(&dyn_state, 1), Some(5));

    // Dynamic encode and decode eve.starbase.Attributes
    let mut attr_buf = Vec::new();
    let mut type_buf = Vec::new();
    encode_varint_field(1, 16_213, &mut type_buf);
    encode_bytes_field(1, &type_buf, &mut attr_buf); // type
    let mut corp_buf = Vec::new();
    encode_varint_field(1, 1_000_203, &mut corp_buf);
    encode_bytes_field(2, &corp_buf, &mut attr_buf); // corporation
    let mut ssys_buf = Vec::new();
    encode_varint_field(1, 30_000_142, &mut ssys_buf);
    encode_bytes_field(3, &ssys_buf, &mut attr_buf); // solarsystem
    let mut moon_buf = Vec::new();
    encode_varint_field(1, 40_000_001, &mut moon_buf);
    encode_bytes_field(4, &moon_buf, &mut attr_buf); // moon

    let dyn_attrs = reg.decode_dynamic("eve.starbase.Attributes", &attr_buf)
        .expect("decode starbase.Attributes");
    assert!(dyn_attrs.get(1).is_some());
    assert!(dyn_attrs.get(2).is_some());
    assert!(dyn_attrs.get(3).is_some());
}

// parity: feat:quasar.starterspace, quasar:eveProto/generated/eve/starterspace/api/requests_pb2.pyj, quasar:eveProto/generated/eve/starterspace/admin/admin_pb2.pyj
// parity: feat:quasar.station, quasar:eveProto/generated/eve_public/station/api/requests_pb2.pyj, quasar:eveProto/generated/eve/station/station_pb2.pyj
#[tokio::test]
async fn test_quasar_starterspace_and_station() {
    eve_testkit::attest(&[
        "feat:quasar.starterspace",
        "feat:quasar.station",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar StarterSpace descriptor verification (3 proto files)
    let starter_files = [
        "eve/starterspace/admin/admin.proto",
        "eve/starterspace/api/events.proto",
        "eve/starterspace/api/requests.proto",
    ];
    for f in starter_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing starterspace proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("starterspace").map(|v| v.len()), Some(3));
    assert!(reg.messages_by_full_name.contains_key("eve.starterspace.api.CanJumpShipCasterRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.starterspace.api.CanJumpShipCasterResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.starterspace.admin.ResetJumpShipCasterRequest"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_204u32;
    let sess = make_session(2004, 204, char_id, 1_000_204);

    // Route eve.starterspace.api.CanJumpShipCasterRequest
    let jump_req = encode_id_msg(1, char_id as u64);
    let jump_rsp = quasar_call(&ctx, &sess, "eve.starterspace.api.CanJumpShipCasterRequest", jump_req);
    assert_eq!(jump_rsp.status_code, STATUS_OK);
    let jump_payload = jump_rsp.payload.expect("jump_payload");
    assert_eq!(jump_payload.normalized_type_name(), "eve.starterspace.api.CanJumpShipCasterResponse");
    let dyn_jump = reg.decode_dynamic("eve.starterspace.api.CanJumpShipCasterResponse", &jump_payload.value)
        .expect("decode CanJumpShipCasterResponse");
    assert!(dyn_jump.get(1).is_some());

    // 2. Quasar Station descriptor verification (6 proto files)
    let station_files = [
        "eve/station/owner.proto",
        "eve/station/services.proto",
        "eve/station/station.proto",
        "eve/station/station_type.proto",
        "eve_public/station/api/requests.proto",
        "eve_public/station/station.proto",
    ];
    for f in station_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing station proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("station").map(|v| v.len()), Some(6));
    assert!(reg.messages_by_full_name.contains_key("eve_public.station.api.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.station.api.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.station.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.station.GetNameRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.station.GetStationsInSolarSystemRequest"));

    // Route eve_public.station.api.GetRequest for Jita 4-4 (60_003_760)
    let pub_req = encode_id_msg(1, 60_003_760);
    let pub_rsp = quasar_call(&ctx, &sess, "eve_public.station.api.GetRequest", pub_req);
    assert_eq!(pub_rsp.status_code, STATUS_OK);
    let pub_payload = pub_rsp.payload.expect("pub_payload");
    assert_eq!(pub_payload.normalized_type_name(), "eve_public.station.api.GetResponse");

    // Route eve.station.GetRequest
    let st_req = encode_id_msg(1, 60_003_760);
    let st_rsp = quasar_call(&ctx, &sess, "eve.station.GetRequest", st_req);
    assert_eq!(st_rsp.status_code, STATUS_OK);
    let st_payload = st_rsp.payload.expect("st_payload");
    assert_eq!(st_payload.normalized_type_name(), "eve.station.GetResponse");

    // Route eve.station.GetNameRequest
    let name_req = encode_id_msg(1, 60_003_760);
    let name_rsp = quasar_call(&ctx, &sess, "eve.station.GetNameRequest", name_req);
    assert_eq!(name_rsp.status_code, STATUS_OK);

    // Route eve.station.GetStationsInSolarSystemRequest
    let ssys_req = encode_id_msg(1, 30_000_142);
    let ssys_rsp = quasar_call(&ctx, &sess, "eve.station.GetStationsInSolarSystemRequest", ssys_req);
    assert_eq!(ssys_rsp.status_code, STATUS_OK);
}

// parity: feat:quasar.structure, quasar:eveProto/generated/eve_public/structure/structure_pb2.pyj, quasar:eveProto/generated/eve/structure/api/requests_pb2.pyj
// parity: feat:quasar.tag, quasar:eveProto/generated/eve/tag/api/requests_pb2.pyj, quasar:eveProto/generated/eve/tag/tag_pb2.pyj
#[tokio::test]
async fn test_quasar_structure_and_tag() {
    eve_testkit::attest(&[
        "feat:quasar.structure",
        "feat:quasar.tag",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Structure descriptor verification (12 proto files)
    let structure_files = [
        "eve/structure/api/api.proto",
        "eve/structure/api/requests.proto",
        "eve/structure/automoonminer/api/events.proto",
        "eve/structure/group.proto",
        "eve/structure/profile.proto",
        "eve/structure/structure.proto",
        "eve/structure/structure_type.proto",
        "eve/structure/upwell/api/events.proto",
        "eve/structure/upwell/jumpbridge/api/events.proto",
        "eve/structure/upwell/state.proto",
        "eve_public/structure/group.proto",
        "eve_public/structure/structure.proto",
        "eve_public/structure/structure_type.proto",
    ];
    for f in structure_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing structure proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("structure").map(|v| v.len()), Some(13));
    assert!(reg.messages_by_full_name.contains_key("eve.structure.api.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.structure.api.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.structure.upwell.api.StateTransitioned"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001u32;
    let corp_id = 1_000_001u32;
    let struct_id = 1_000_000_050i64;
    let mut sess = make_session(2005, 1, char_id, corp_id);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Seed structure in snapshot
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35_833, // Fortizar
            owner_corp_id: corp_id as i64,
            solar_system_id: 30_000_142,
            name: "Jita Hub Citadel".to_string(),
            position: [1_000.0, 0.0, 0.0],
            state: "Online".to_string(),
            fuel_blocks: 2_400,
            quantum_core_installed: true,
            shield_hp: 1_000_000.0,
            armor_hp: 1_000_000.0,
            hull_hp: 1_000_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
    });

    // Route eve.structure.api.GetRequest
    let str_req = encode_id_msg(1, struct_id as u64);
    let str_rsp = quasar_call(&ctx, &sess, "eve.structure.api.GetRequest", str_req);
    assert_eq!(str_rsp.status_code, STATUS_OK);
    let str_payload = str_rsp.payload.expect("str_payload");
    assert_eq!(str_payload.normalized_type_name(), "eve.structure.api.GetResponse");

    // Publish NoticeScope::SolarSystem eve.structure.upwell.api.StateTransitioned
    let mut trans_buf = Vec::new();
    let mut st_inner = Vec::new();
    encode_varint_field(1, struct_id as u64, &mut st_inner);
    encode_bytes_field(1, &st_inner, &mut trans_buf); // structure
    encode_varint_field(2, 5, &mut trans_buf);         // state = 5

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.structure.upwell.api.StateTransitioned"),
            value: trans_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.structure.upwell.api.StateTransitioned".to_string()));

    // 2. Quasar Tag descriptor verification (5 proto files)
    let tag_files = [
        "eve/tag/api/events.proto",
        "eve/tag/api/requests.proto",
        "eve/tag/tag.proto",
        "eve/tag/version/api/events.proto",
        "eve/tag/version/api/requests.proto",
    ];
    for f in tag_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing tag proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("tag").map(|v| v.len()), Some(5));
    assert!(reg.messages_by_full_name.contains_key("eve.tag.api.RegisterRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.tag.api.RegisterResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.tag.api.ListRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.tag.version.api.CreateRequest"));

    // Route eve.tag.api.RegisterRequest
    let mut tag_id = Vec::new();
    encode_string_field(1, "fleet_vanguard", &mut tag_id);
    let mut reg_req = Vec::new();
    encode_bytes_field(1, &tag_id, &mut reg_req);
    let reg_rsp = quasar_call(&ctx, &sess, "eve.tag.api.RegisterRequest", reg_req);
    assert_eq!(reg_rsp.status_code, STATUS_OK);

    // Route eve.tag.api.ListRequest
    let list_rsp = quasar_call(&ctx, &sess, "eve.tag.api.ListRequest", Vec::new());
    assert_eq!(list_rsp.status_code, STATUS_OK);

    // Route eve.tag.version.api.CreateRequest
    let ver_rsp = quasar_call(&ctx, &sess, "eve.tag.version.api.CreateRequest", Vec::new());
    assert_eq!(ver_rsp.status_code, STATUS_OK);
}
