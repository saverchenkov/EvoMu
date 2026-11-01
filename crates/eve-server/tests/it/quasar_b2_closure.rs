//! Live `publicGatewaySvc` integration tests for Batch 2 Quasar protobuf handlers.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_b2_services::{
    load_state, save_state, PlanetResourceConfig, QuasarB2State, SovClaimRecord,
    StarResourceConfig, StargateLockRecord, StructureRecord,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

fn seed_quasar_b2_fixtures(ctx: &ServerContext) {
    let mut state = QuasarB2State::default();
    state.structures.insert(
        1_030_000_000_001,
        StructureRecord {
            structure_id: 1_030_000_000_001,
            type_id: 35832, // Astrahus
            solar_system_id: 30_000_142,
            corporation_id: 98_000_001,
            name: "Jita Trade Citadel".to_string(),
        },
    );
    state.star_power_production.insert(40_009_041, 4_250);
    state.stargate_locks.insert(
        format!("{}:{}", 2_112_000_001u64, 30_000_142u64),
        StargateLockRecord {
            character_id: 2_112_000_001,
            solar_system_id: 30_000_142,
            gate_id: 50_001_248,
            expiry_unix: 1_800_000_000,
        },
    );
    state.restricted_systems = vec![30_000_142, 30_100_001];
    state.sov_claims.insert(
        30_002_537,
        SovClaimRecord {
            solar_system_id: 30_002_537,
            alliance_id: 99_000_001,
            corporation_id: 98_000_001,
            hub_id: 1_040_000_000_001,
            claimed_at_unix: 1_700_000_000,
            is_capital: true,
            military_index: 5,
            industrial_index: 4,
            strategic_index: 5,
        },
    );
    state.adm_levels.insert(30_002_537, 60);
    state.star_resource_configs = vec![StarResourceConfig {
        star_id: 40_009_041,
        power: 4_250,
    }];
    state.planet_resource_configs = vec![PlanetResourceConfig {
        planet_id: 40_009_042,
        power: 1_200,
        workforce: 8_500,
    }];
    save_state(&ctx.db, &state);
}

static CORR_SEQ: AtomicU64 = AtomicU64::new(1);

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-b2-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-b2".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

#[derive(Debug, Clone)]
enum RawField {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn parse_raw_proto(mut buf: &[u8]) -> Vec<(u32, RawField)> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let mut tag = 0u64;
        let mut shift = 0;
        loop {
            let b = buf[0];
            buf = &buf[1..];
            tag |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let field_num = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u8;
        match wire_type {
            0 => {
                let mut val = 0u64;
                let mut s = 0;
                loop {
                    let b = buf[0];
                    buf = &buf[1..];
                    val |= ((b & 0x7f) as u64) << s;
                    if b & 0x80 == 0 {
                        break;
                    }
                    s += 7;
                }
                out.push((field_num, RawField::Varint(val)));
            }
            2 => {
                let mut len = 0u64;
                let mut s = 0;
                loop {
                    let b = buf[0];
                    buf = &buf[1..];
                    len |= ((b & 0x7f) as u64) << s;
                    if b & 0x80 == 0 {
                        break;
                    }
                    s += 7;
                }
                let len = len as usize;
                out.push((field_num, RawField::Bytes(buf[..len].to_vec())));
                buf = &buf[len..];
            }
            _ => panic!("Unexpected wire type {wire_type}"),
        }
    }
    out
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn first_varint(fields: &[(u32, RawField)], field_num: u32) -> Option<u64> {
    fields.iter().find_map(|(f, v)| match v {
        RawField::Varint(n) if *f == field_num => Some(*n),
        _ => None,
    })
}

fn first_bytes(fields: &[(u32, RawField)], field_num: u32) -> Option<&[u8]> {
    fields.iter().find_map(|(f, v)| match v {
        RawField::Bytes(b) if *f == field_num => Some(b.as_slice()),
        _ => None,
    })
}

#[tokio::test]
async fn test_quasar_b2_ship_unlock_and_starterspace_caster() {
    // parity: quasar:eveProto/generated/eve_public/ship/unlock/requests_pb2.pyj, quasar:eveProto/generated/eve/ship/api/requests_pb2.pyj, quasar:eveProto/generated/eve/starterspace/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(1, 1001, 2_112_000_001, 98_000_001);

    // 1. GetPendingNotificationsRequest returns initial pending ship types [587, 597]
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload");
    assert!(payload
        .type_url
        .ends_with("eve_public.ship.unlock.GetPendingNotificationsResponse"));
    let fields = parse_raw_proto(&payload.value);
    assert_eq!(fields.len(), 2);

    // 2. ClaimRewardRequest removes 587 and persists
    let mut claim_req = Vec::new();
    encode_id_field(1, 587, &mut claim_req);
    let claim_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ship.unlock.ClaimRewardRequest",
        claim_req.clone(),
    )
    .await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);

    // Claiming 587 again returns NOT_FOUND
    let claim_again = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ship.unlock.ClaimRewardRequest",
        claim_req,
    )
    .await;
    assert_eq!(claim_again.status_code, STATUS_NOT_FOUND);

    // 3. CanJumpStarterSpaceShipCasterRequest & CanJumpShipCasterRequest
    let mut caster_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut caster_req);
    let r1 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.ship.api.CanJumpStarterSpaceShipCasterRequest",
        caster_req.clone(),
    )
    .await;
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&r1.payload.unwrap().value), 1),
        Some(1)
    );

    // Character 2_112_000_001 has a stargate lock, so starterspace CanJumpShipCasterRequest returns false (0)
    let r2 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.starterspace.api.CanJumpShipCasterRequest",
        caster_req,
    )
    .await;
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&r2.payload.unwrap().value), 1),
        Some(0)
    );

    // Unauthenticated session is rejected with STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(99);
    let unauth_rsp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);
}

#[tokio::test]
async fn test_quasar_b2_structure_star_resources_and_space_tags() {
    // parity: quasar:eveProto/generated/eve/structure/api/requests_pb2.pyj, quasar:eveProto/generated/eve/star/resources/api/requests_pb2.pyj, quasar:eveProto/generated/eve/space/tag/api/admin/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(2, 1002, 2_112_000_002, 98_000_001);

    // 1. eve.structure.api.GetRequest
    let mut struct_req = Vec::new();
    encode_id_field(1, 1_030_000_000_001, &mut struct_req);
    let srsp = quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", struct_req).await;
    assert_eq!(srsp.status_code, STATUS_OK);
    let sfields = parse_raw_proto(&srsp.payload.unwrap().value);
    let attrs = parse_raw_proto(first_bytes(&sfields, 1).unwrap());
    assert_eq!(
        first_bytes(&attrs, 4),
        Some(b"Jita Trade Citadel".as_slice())
    );

    // Unknown structure returns NOT_FOUND
    let mut bad_struct = Vec::new();
    encode_id_field(1, 9_999_999, &mut bad_struct);
    let bad_srsp = quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", bad_struct).await;
    assert_eq!(bad_srsp.status_code, STATUS_NOT_FOUND);

    // 2. eve.star.resources.api.GetPowerProductionRequest
    let mut star_req = Vec::new();
    encode_id_field(1, 40_009_041, &mut star_req);
    let starrsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.star.resources.api.GetPowerProductionRequest",
        star_req,
    )
    .await;
    assert_eq!(starrsp.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&starrsp.payload.unwrap().value), 1),
        Some(4_250)
    );

    // 3. eve.space.tag.admin.api.ApplyRequest & RemoveRequest
    let mut loc_constellation = Vec::new();
    encode_id_field(2, 30_000_142, &mut loc_constellation);
    let mut loc_id = Vec::new();
    encode_bytes_field(2, &loc_constellation, &mut loc_id);

    let mut tag_id = Vec::new();
    encode_string_field(1, "incursion_active", &mut tag_id);

    let mut apply_req = Vec::new();
    encode_bytes_field(2, &loc_id, &mut apply_req);
    let mut spec = Vec::new();
    encode_bytes_field(1, &tag_id, &mut spec);
    encode_varint_field(2, 1, &mut spec);
    encode_bytes_field(3, &spec, &mut apply_req);
    let apply_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.space.tag.admin.api.ApplyRequest",
        apply_req,
    )
    .await;
    assert_eq!(apply_rsp.status_code, STATUS_OK);
    assert!(load_state(&ctx.db)
        .space_tags
        .get(&30_000_142)
        .unwrap()
        .contains_key("incursion_active"));

    let mut remove_req = Vec::new();
    encode_bytes_field(2, &loc_id, &mut remove_req);
    encode_bytes_field(3, &tag_id, &mut remove_req);
    let remove_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.space.tag.admin.api.RemoveRequest",
        remove_req,
    )
    .await;
    assert_eq!(remove_rsp.status_code, STATUS_OK);
    assert!(!load_state(&ctx.db)
        .space_tags
        .get(&30_000_142)
        .unwrap()
        .contains_key("incursion_active"));
}

#[tokio::test]
async fn test_quasar_b2_stargate_locks_and_delete_command() {
    // parity: quasar:eveProto/generated/eve/stargate/lock/api/requests_pb2.pyj, quasar:eveProto/generated/eve/stargate/lock/api/commands_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(3, 1003, 2_112_000_001, 98_000_001);

    // 1. GetRestrictedSystemsRequest
    let restr_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRestrictedSystemsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(restr_rsp.status_code, STATUS_OK);
    assert_eq!(parse_raw_proto(&restr_rsp.payload.unwrap().value).len(), 2);

    // 2. GetRequest returns active lock on gate 50_001_248
    let mut get_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut get_req);
    encode_id_field(2, 30_000_142, &mut get_req);
    let get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRequest",
        get_req.clone(),
    )
    .await;
    assert_eq!(get_rsp.status_code, STATUS_OK);

    // 3. ValidateJumpRequest on a different gate returns allowed = false (0)
    let mut val_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut val_req);
    encode_id_field(2, 30_000_142, &mut val_req);
    encode_id_field(3, 50_001_999, &mut val_req);
    let val_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.ValidateJumpRequest",
        val_req.clone(),
    )
    .await;
    assert_eq!(val_rsp.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&val_rsp.payload.unwrap().value), 1),
        Some(0)
    );

    // 4. DeleteCommanded requires admin role, then removes the lock, after which ValidateJumpRequest allows jump (1)
    let mut del_req = Vec::new();
    encode_id_field(2, 2_112_000_001, &mut del_req);
    encode_id_field(3, 30_000_142, &mut del_req);

    let mut non_admin = make_session(33, 1033, 2_112_000_001, 98_000_001);
    non_admin.role = 0;
    let unauth_del = quasar_rpc(
        &ctx,
        &mut non_admin,
        "eve.stargate.lock.api.DeleteCommanded",
        del_req.clone(),
    )
    .await;
    assert_eq!(unauth_del.status_code, 403);

    let del_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.DeleteCommanded",
        del_req,
    )
    .await;
    assert_eq!(del_rsp.status_code, STATUS_OK);

    let val_after = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.ValidateJumpRequest",
        val_req,
    )
    .await;
    assert_eq!(
        first_varint(&parse_raw_proto(&val_after.payload.unwrap().value), 1),
        Some(1)
    );
}

#[tokio::test]
async fn test_quasar_b2_sovereignty_claims_adm_and_resources() {
    // parity: quasar:eveProto/generated/eve/sovereignty/api/requests_pb2.pyj, quasar:eveProto/generated/eve/sovereignty/activitydefensemodifier/api/requests_pb2.pyj, quasar:eveProto/generated/eve/sovereignty/resource/star/api/requests_pb2.pyj, quasar:eveProto/generated/eve/sovereignty/resource/planet/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(4, 1004, 2_112_000_004, 98_000_001);

    // 1. GetClaimRequest (claimed system 30_002_537 vs unclaimed 30_000_142)
    let mut claim_req = Vec::new();
    encode_id_field(1, 30_002_537, &mut claim_req);
    let crsp = quasar_rpc(&ctx, &mut sess, "eve.sovereignty.api.GetClaimRequest", claim_req).await;
    assert_eq!(crsp.status_code, STATUS_OK);
    let cfields = parse_raw_proto(&crsp.payload.unwrap().value);
    assert_eq!(first_varint(&cfields, 1), Some(0));
    assert!(first_bytes(&cfields, 2).is_some());

    // GetVersionManifestRequest & GetAllClaimsRequest
    let mrsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.api.GetVersionManifestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(mrsp.status_code, STATUS_OK);

    let all_claims = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.api.GetAllClaimsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(all_claims.status_code, STATUS_OK);

    // 2. GetLevelRequest (ADM)
    let mut adm_req = Vec::new();
    encode_id_field(1, 30_002_537, &mut adm_req);
    let adm_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
        adm_req,
    )
    .await;
    assert_eq!(adm_rsp.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&adm_rsp.payload.unwrap().value), 1),
        Some(60)
    );

    // 3. Star & Planet resource configurations and definitions
    let sres_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(sres_rsp.status_code, STATUS_OK);

    let pcfg_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pcfg_rsp.status_code, STATUS_OK);

    let pdef_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pdef_rsp.status_code, STATUS_OK);

    let pall_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pall_rsp.status_code, STATUS_OK);

    // Bad version request returns STATUS_BAD_REQUEST
    let bad_ver = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bad_ver.status_code, STATUS_BAD_REQUEST);
    let _ = save_state;
}
