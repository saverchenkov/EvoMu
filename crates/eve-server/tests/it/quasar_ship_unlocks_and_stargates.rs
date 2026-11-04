//! Live `publicGatewaySvc` integration tests for Batch 2 Quasar protobuf handlers.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

fn seed_quasar_b2_fixtures(ctx: &ServerContext) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_030_000_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_030_000_000_001,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Jita Trade Citadel".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.zarzakh_gate_locks
            .insert(2_112_000_001, (50_001_248, now + 3_600));
        snap.system_states.insert(
            30_002_537,
            eve_db::SystemDynamicState {
                solar_system_id: 30_002_537,
                dbs_multiplier: 1.0,
                sov_alliance_id: Some(99_000_001),
                military_index: 5,
                industrial_index: 4,
                strategic_index: 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_040_000_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_040_000_000_001,
                type_id: 32458, // Sovereignty Hub
                owner_corp_id: 98_000_001,
                solar_system_id: 30_002_537,
                name: "Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            1_040_000_000_001,
            eve_db::PersistedSovHubData {
                hub_id: 1_040_000_000_001,
                ..Default::default()
            },
        );
    });
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

fn decode_nested_id(fields: &[(u32, RawField)], field_num: u32) -> Option<u64> {
    first_bytes(fields, field_num).and_then(|b| first_varint(&parse_raw_proto(b), 1))
}

#[tokio::test]
async fn test_quasar_b2_ship_unlock_and_starterspace_caster() {
    // parity: quasar:eveProto/generated/eve/ship/api/requests_pb2.pyj, quasar:eveProto/generated/eve/starterspace/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(1, 1001, 2_112_000_001, 98_000_001);

    // 1. eve_public.ship.unlock.* -> STATUS_NOT_IMPLEMENTED (DATA GAP)
    for req_type in [
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        "eve_public.ship.unlock.ClaimRewardRequest",
    ] {
        let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new()).await;
        assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED, "{req_type}");
    }

    // 3. CanJumpStarterSpaceShipCasterRequest & CanJumpShipCasterRequest
    // Seed CHAR_A in space (30_000_142) + starter_launchers; while zarzakh_gate_lock is active, both return false (0)
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.values().next().cloned() {
            ch.character_id = 2_112_000_001;
            ch.user_id = 1001;
            ch.corporation_id = 98_000_001;
            ch.solar_system_id = 30_000_142;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = 1_010_000_001;
            ch.jump_cooldown_until_unix = 0;
            snap.characters.insert(2_112_000_001, ch);
        }
        snap.items.insert(1_010_000_001, eve_db::InventoryItemRecord {
            item_id: 1_010_000_001, type_id: 606, owner_id: 2_112_000_001,
            location_id: 30_000_142, quantity: 1, singleton: true, ..Default::default()
        });
        snap.service_state_blobs.insert("cluster_misc_state".to_string(), r#"{"starter_launchers":[1]}"#.to_string());
    });
    let mut caster_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut caster_req);
    let r1 = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req.clone()).await;
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(first_varint(&parse_raw_proto(&r1.payload.unwrap().value), 1), Some(0));

    let r2 = quasar_rpc(&ctx, &mut sess, "eve.starterspace.api.CanJumpShipCasterRequest", caster_req.clone()).await;
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(first_varint(&parse_raw_proto(&r2.payload.unwrap().value), 1), Some(0));

    // Clearing zarzakh_gate_lock enables both ShipCaster endpoints (allowed == 1)
    ctx.db.with_mut_snapshot(|snap| { snap.zarzakh_gate_locks.remove(&2_112_000_001); });
    let r1_ok = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req.clone()).await;
    assert_eq!(r1_ok.status_code, STATUS_OK);
    assert_eq!(first_varint(&parse_raw_proto(&r1_ok.payload.unwrap().value), 1), Some(1));
    let r2_ok = quasar_rpc(&ctx, &mut sess, "eve.starterspace.api.CanJumpShipCasterRequest", caster_req.clone()).await;
    assert_eq!(r2_ok.status_code, STATUS_OK);
    assert_eq!(first_varint(&parse_raw_proto(&r2_ok.payload.unwrap().value), 1), Some(1));

    // Emptying starter_launchers in cluster_misc_state disables CanJumpStarterSpaceShipCasterRequest
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("cluster_misc_state".to_string(), r#"{"starter_launchers":[]}"#.to_string());
    });
    let r3 = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req).await;
    assert_eq!(r3.status_code, STATUS_OK);
    assert_eq!(first_varint(&parse_raw_proto(&r3.payload.unwrap().value), 1), Some(0));

    // Unauthenticated session is rejected with STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(99);
    let unauth_rsp = quasar_rpc(&ctx, &mut unauth, "eve_public.ship.unlock.GetPendingNotificationsRequest", Vec::new()).await;
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);
}

#[tokio::test]
async fn test_quasar_b2_structure_star_resources_and_space_tags() {
    // parity: quasar:eveProto/generated/eve/structure/api/requests_pb2.pyj, quasar:eveProto/generated/eve/space/tag/api/admin/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(2, 1002, 2_112_000_002, 98_000_001);

    // 1. eve.structure.api.GetRequest -> wired to snap.structures
    let mut struct_req = Vec::new();
    encode_id_field(1, 1_030_000_000_001, &mut struct_req);
    let srsp = quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", struct_req).await;
    assert_eq!(srsp.status_code, STATUS_OK);
    let sfields = parse_raw_proto(&srsp.payload.unwrap().value);
    let attrs = parse_raw_proto(first_bytes(&sfields, 1).unwrap());
    assert_eq!(decode_nested_id(&attrs, 1), Some(35832));
    assert_eq!(decode_nested_id(&attrs, 2), Some(30_000_142));
    assert_eq!(decode_nested_id(&attrs, 3), Some(98_000_001));
    assert_eq!(
        first_bytes(&attrs, 4),
        Some(b"Jita Trade Citadel".as_slice())
    );

    // Unknown, unauthorized private, and destroyed structures return NOT_FOUND
    let mut bad_struct = Vec::new();
    encode_id_field(1, 9_999_999, &mut bad_struct);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", bad_struct).await.status_code, STATUS_NOT_FOUND);
    let mut outsider = make_session(20, 1020, 2_112_000_020, 98_000_999);
    outsider.role = 0;
    let mut priv_req = Vec::new();
    encode_id_field(1, 1_030_000_000_001, &mut priv_req);
    assert_eq!(quasar_rpc(&ctx, &mut outsider, "eve.structure.api.GetRequest", priv_req.clone()).await.status_code, STATUS_NOT_FOUND);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&1_030_000_000_001) { st.state = "Destroyed".to_string(); }
    });
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", priv_req).await.status_code, STATUS_NOT_FOUND);

    // 2. eve.star.resources.api.GetPowerProductionRequest -> STATUS_NOT_IMPLEMENTED (DATA GAP)
    let mut star_req = Vec::new();
    encode_id_field(1, 40_009_041, &mut star_req);
    let starrsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.star.resources.api.GetPowerProductionRequest",
        star_req,
    )
    .await;
    assert_eq!(starrsp.status_code, STATUS_NOT_IMPLEMENTED);

    // 3. eve.space.tag.admin.api.ApplyRequest & RemoveRequest (handled by quasar_space_domain)
    let jita = ctx.sde.solar_systems.get(&30_000_142).expect("Jita in SDE");
    let mut loc_id = Vec::new();
    encode_id_field(1, jita.region_id as u64, &mut loc_id);

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
    assert_eq!(apply_rsp.status_code, STATUS_OK, "{apply_rsp:?}");
    let mut get_locs_req = Vec::new();
    encode_bytes_field(1, &tag_id, &mut get_locs_req);
    let get_locs_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.space.tag.api.GetLocationsRequest",
        get_locs_req.clone(),
    )
    .await;
    assert_eq!(get_locs_rsp.status_code, STATUS_OK);
    let locs_bytes = get_locs_rsp.payload.unwrap().value;
    assert!(locs_bytes.windows(loc_id.len()).any(|w| w == loc_id.as_slice()));

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
    let get_locs_after = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.space.tag.api.GetLocationsRequest",
        get_locs_req,
    )
    .await;
    assert_eq!(get_locs_after.status_code, STATUS_OK);
    let locs_after_bytes = get_locs_after.payload.unwrap().value;
    assert!(!locs_after_bytes.windows(loc_id.len()).any(|w| w == loc_id.as_slice()));
}

#[tokio::test]
async fn test_quasar_b2_stargate_locks_and_delete_command() {
    // parity: quasar:eveProto/generated/eve/stargate/lock/api/requests_pb2.pyj, quasar:eveProto/generated/eve/stargate/lock/api/commands_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(3, 1003, 2_112_000_001, 98_000_001);

    // 1. GetRestrictedSystemsRequest -> Zarzakh (30_100_000)
    let restr_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRestrictedSystemsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(restr_rsp.status_code, STATUS_OK);
    let restr_fields = parse_raw_proto(&restr_rsp.payload.unwrap().value);
    assert_eq!(decode_nested_id(&restr_fields, 1), Some(30_100_000));

    // 2. GetRequest on non-restricted Jita (30_000_142) returns STATUS_NOT_FOUND; Zarzakh (30_100_000) returns active lock
    let mut jita_get_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut jita_get_req);
    encode_id_field(2, 30_000_142, &mut jita_get_req);
    let jita_get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRequest",
        jita_get_req,
    )
    .await;
    assert_eq!(jita_get_rsp.status_code, STATUS_NOT_FOUND);

    let mut mismatch_get_req = Vec::new();
    encode_id_field(1, 2_112_999_999, &mut mismatch_get_req);
    encode_id_field(2, 30_100_000, &mut mismatch_get_req);
    let mismatch_get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRequest",
        mismatch_get_req,
    )
    .await;
    assert_eq!(mismatch_get_rsp.status_code, 403);

    let mut get_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut get_req);
    encode_id_field(2, 30_100_000, &mut get_req);
    let get_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRequest",
        get_req.clone(),
    )
    .await;
    assert_eq!(get_rsp.status_code, STATUS_OK);
    let gfields = parse_raw_proto(&get_rsp.payload.unwrap().value);
    assert_eq!(decode_nested_id(&gfields, 1), Some(50_001_248));
    assert!(decode_nested_id(&gfields, 2).unwrap_or(0) > ctx.now_unix());

    // 3. ValidateJumpRequest on Jita (30_000_142) returns allowed = true (1); Zarzakh (30_100_000) on a different gate returns allowed = false (0) + lock details
    let mut jita_val_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut jita_val_req);
    encode_id_field(2, 30_000_142, &mut jita_val_req);
    encode_id_field(3, 50_001_999, &mut jita_val_req);
    let jita_val_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.ValidateJumpRequest",
        jita_val_req,
    )
    .await;
    assert_eq!(jita_val_rsp.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&jita_val_rsp.payload.unwrap().value), 1),
        Some(1)
    );

    let mut val_req = Vec::new();
    encode_id_field(1, 2_112_000_001, &mut val_req);
    encode_id_field(2, 30_100_000, &mut val_req);
    encode_id_field(3, 50_001_999, &mut val_req);
    let val_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.ValidateJumpRequest",
        val_req.clone(),
    )
    .await;
    assert_eq!(val_rsp.status_code, STATUS_OK);
    let vfields = parse_raw_proto(&val_rsp.payload.unwrap().value);
    assert_eq!(first_varint(&vfields, 1), Some(0));
    let lock_sub = parse_raw_proto(first_bytes(&vfields, 2).unwrap());
    assert_eq!(decode_nested_id(&lock_sub, 1), Some(50_001_248));

    // 4. DeleteCommanded requires admin role, returns STATUS_NOT_FOUND for Jita, then removes Zarzakh lock
    let mut jita_del_req = Vec::new();
    encode_id_field(2, 2_112_000_001, &mut jita_del_req);
    encode_id_field(3, 30_000_142, &mut jita_del_req);
    let jita_del_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.DeleteCommanded",
        jita_del_req,
    )
    .await;
    assert_eq!(jita_del_rsp.status_code, STATUS_NOT_FOUND);

    let mut del_req = Vec::new();
    encode_id_field(2, 2_112_000_001, &mut del_req);
    encode_id_field(3, 30_100_000, &mut del_req);

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
        val_req.clone(),
    )
    .await;
    assert_eq!(val_after.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&val_after.payload.unwrap().value), 1),
        Some(1)
    );

    // 5. Expired stargate lock is treated as inactive by GetRequest and ValidateJumpRequest
    seed_quasar_b2_fixtures(&ctx);
    ctx.advance_time_seconds(3_601);
    let expired_get = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.GetRequest",
        get_req,
    )
    .await;
    assert_eq!(expired_get.status_code, STATUS_NOT_FOUND);
    let expired_val = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.stargate.lock.api.ValidateJumpRequest",
        val_req,
    )
    .await;
    assert_eq!(expired_val.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&expired_val.payload.unwrap().value), 1),
        Some(1)
    );
}

#[tokio::test]
async fn test_quasar_b2_sovereignty_claims_adm_and_resources() {
    // parity: quasar:eveProto/generated/eve/sovereignty/api/requests_pb2.pyj, quasar:eveProto/generated/eve/sovereignty/activitydefensemodifier/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    seed_quasar_b2_fixtures(&ctx);
    let mut sess = make_session(4, 1004, 2_112_000_004, 98_000_001);

    // 1. GetClaimRequest (claimed system 30_002_537)
    let mut claim_req = Vec::new();
    encode_id_field(1, 30_002_537, &mut claim_req);
    let crsp = quasar_rpc(&ctx, &mut sess, "eve.sovereignty.api.GetClaimRequest", claim_req).await;
    assert_eq!(crsp.status_code, STATUS_OK);
    let cfields = parse_raw_proto(&crsp.payload.unwrap().value);
    assert_eq!(first_varint(&cfields, 1), Some(0));
    let cbuf = parse_raw_proto(first_bytes(&cfields, 2).unwrap());
    assert_eq!(decode_nested_id(&cbuf, 1), Some(99_000_001));
    assert_eq!(decode_nested_id(&cbuf, 3), Some(1_040_000_000_001));
    assert_eq!(decode_nested_id(&cbuf, 4), Some(98_000_001));

    // GetVersionManifestRequest
    let mrsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.api.GetVersionManifestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(mrsp.status_code, STATUS_OK);
    let mfields = parse_raw_proto(&mrsp.payload.unwrap().value);
    let manifest = parse_raw_proto(first_bytes(&mfields, 1).unwrap());
    assert_eq!(manifest.len(), 6);

    // GetAllClaimsRequest
    let all_claims = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.api.GetAllClaimsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(all_claims.status_code, STATUS_OK);
    let all_fields = parse_raw_proto(&all_claims.payload.unwrap().value);
    let sc = parse_raw_proto(first_bytes(&all_fields, 1).unwrap());
    assert_eq!(decode_nested_id(&sc, 1), Some(30_002_537));
    let ac = parse_raw_proto(first_bytes(&sc, 3).unwrap());
    assert_eq!(decode_nested_id(&ac, 1), Some(99_000_001));
    assert_eq!(decode_nested_id(&ac, 2), Some(98_000_001));

    // 2. GetLevelRequest (ADM) -> wired to snap.system_states via calculate_adm
    let mut adm_req = Vec::new();
    encode_id_field(1, 30_002_537, &mut adm_req);
    let adm_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
        adm_req.clone(),
    )
    .await;
    assert_eq!(adm_rsp.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&adm_rsp.payload.unwrap().value), 1),
        Some(60)
    );

    // Non-saturated index combination (1, 2, 1) -> 1.0 + 0.6 + 1.2 + 0.4 = 3.2 -> 32
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&30_002_537).unwrap();
        st.military_index = 1;
        st.industrial_index = 2;
        st.strategic_index = 1;
    });
    let adm_non_sat = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
        adm_req,
    )
    .await;
    assert_eq!(adm_non_sat.status_code, STATUS_OK);
    assert_eq!(
        first_varint(&parse_raw_proto(&adm_non_sat.payload.unwrap().value), 1),
        Some(32)
    );

    // 3. Equinox star & planet resource handlers -> STATUS_NOT_IMPLEMENTED (DATA GAP)
    for req_type in [
        "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
        "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
    ] {
        let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new()).await;
        assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED, "{req_type}");
    }
    let mut overflow_sys = Vec::new();
    encode_id_field(1, 30_002_537 + (1u64 << 32), &mut overflow_sys);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.sovereignty.api.GetClaimRequest", overflow_sys).await.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);
}
