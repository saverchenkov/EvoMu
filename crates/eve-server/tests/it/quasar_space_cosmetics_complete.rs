//! Acceptance test suite for Quasar Space, Combat & Cosmetics services.
//!
//! Validates:
//! - Space, celestials, solar systems, jump ranges, bubble presence, pirate insurgencies
//!   (corruption/suppression), dynamic bounty systems, NPC ship ownership, and notice broadcasts.
//! - Sovereignty hub, skyhooks, workforce routes, upgrades, fuel management, mercenary dens,
//!   faction warfare leaderboards/stats/systems, and operational recommendations.
//! - Ship cosmetics (logos, emblems, SKINR third-party sequencing, licenses, drafts),
//!   corporation color palettes, character titles, structure paintworks, Quasar config,
//!   and AIR career goals.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_OK};
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

fn encode_id_req(_id: u64) -> Vec<u8> {
    Vec::new()
}

pub(crate) fn call_req(
    ctx: &ServerContext,
    session: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    require_auth: bool,
) -> EvePublicResponse {
    let now = ctx.now_unix();
    ctx.quasar_gateway.handle_public_request(
        session,
        &EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: now as i64,
                nanos: 0,
            }),
            correlation_uuid: b"corr-space-cos01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-space-00001".to_vec(),
            authoritative_context: None,
            trace_context: None,
        },
        None,
        require_auth,
        now,
    )
}

#[tokio::test]
async fn q7_space_celestials_navigation_and_dbs() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let empty_kw = HashMap::new();

    let mut session = SessionState::new(20_001);
    session.user_id = Some(101);
    session.character_id = Some(90_000_101);
    session.corporation_id = Some(98_000_001);
    session.solar_system_id = Some(30_000_142); // Jita

    // 1. eve_public.solarsystem.api.GetRequest
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-space-001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.solarsystem.api.GetRequest"),
            value: encode_id_req(30_000_142),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-space-00001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };

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
        .expect("SendRequest solarsystem");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "solarsystem.api.GetRequest failed: {}", rsp.status_message);
    let payload = rsp.payload.expect("payload present");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.solarsystem.api.GetResponse"
    );

    // 2. Jump range query: eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest
    let jump_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-space-002".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest"
            ),
            value: encode_id_req(30_000_142),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-space-00001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep2 = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(jump_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest jump range");
    let rsp2 = extract_response_proto(&rep2);
    assert_eq!(rsp2.status_code, STATUS_OK);

    // 3. Constellation & Region queries
    let const_rsp = call_req(
        &ctx,
        &session,
        "eve.constellation.GetRequest",
        encode_id_req(20_000_020),
        false,
    );
    assert_eq!(const_rsp.status_code, STATUS_OK);

    let region_rsp = call_req(
        &ctx,
        &session,
        "eve.region.GetRequest",
        encode_id_req(10_000_002),
        false,
    );
    assert_eq!(region_rsp.status_code, STATUS_OK);

    // 4. Dynamic Bounty System (DBS) queries
    let dbs_rsp = call_req(
        &ctx,
        &session,
        "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
        Vec::new(),
        false,
    );
    assert_eq!(dbs_rsp.status_code, STATUS_OK);

    // 5. Dungeon Sovereignty Instances & NPC Ship Owner queries
    let dng_rsp = call_req(
        &ctx,
        &session,
        "eve.dungeon.sovereignty.api.GetInstancesRequest",
        encode_id_req(30_000_142),
        false,
    );
    assert_eq!(dng_rsp.status_code, STATUS_OK);

    let npc_rsp = call_req(
        &ctx,
        &session,
        "eve.npc.ship.api.GetOwnerRequest",
        encode_id_req(90_000_101),
        false,
    );
    assert_eq!(npc_rsp.status_code, STATUS_OK);
    assert_eq!(
        rsp2.payload.as_ref().unwrap().normalized_type_name(),
        "eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse"
    );
    assert_eq!(
        const_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.constellation.GetResponse"
    );
    assert_eq!(
        region_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.region.GetResponse"
    );
    assert_eq!(
        dng_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.dungeon.sovereignty.api.GetInstancesResponse"
    );
}

#[tokio::test]
async fn q7_bubble_presence_and_pirate_insurgency() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_002);
    session.user_id = Some(102);
    // Demo-seed pilot in Jita: system-scoped queries without an explicit system use the
    // authenticated character's current solar system (no fabricated fallback character).
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);

    // 1. Bubble presence queries
    let bubble_rsp = call_req(
        &ctx,
        &session,
        "eve.bubble.presence.api.GetInBubbleRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(bubble_rsp.status_code, STATUS_OK);

    // 2. Pirate Insurgency API
    let ins_rsp = call_req(
        &ctx,
        &session,
        "eve.pirate.insurgency.api.GetRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(ins_rsp.status_code, STATUS_OK);

    // 3. Corruption & Suppression System Info + Stages
    let corr_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.corruption.api.GetSystemInfoRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(corr_rsp.status_code, STATUS_OK);

    let corr_stg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(corr_stg_rsp.status_code, STATUS_OK);

    let supp_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.suppression.api.GetSystemInfoRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(supp_rsp.status_code, STATUS_OK);

    let supp_stg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(supp_stg_rsp.status_code, STATUS_OK);

    let _rx = ctx.quasar_gateway.subscribe_session(
        &session,
        vec![
            "eve_public.pirate.corruption.api.Notice".into(),
            "eve_public.pirate.suppression.api.Notice".into(),
        ],
        ctx.now_unix(),
    );

    let corr_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.corruption.api.GetSystemInfoRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(corr_rsp.status_code, STATUS_OK);

    let corr_stg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(corr_stg_rsp.status_code, STATUS_OK);

    let supp_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.suppression.api.GetSystemInfoRequest",
        encode_id_req(30_000_142),
        true,
    );
    assert_eq!(supp_rsp.status_code, STATUS_OK);

    let supp_stg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(supp_stg_rsp.status_code, STATUS_OK);

    let polled = ctx.quasar_gateway.poll_notices(session.session_id);
    assert_eq!(polled.len(), 0, "Read-only GetSystemInfoRequest must not emit notices");
    assert_eq!(
        corr_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.pirate.corruption.api.GetSystemInfoResponse"
    );
    assert_eq!(
        supp_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.pirate.suppression.api.GetSystemInfoResponse"
    );
}

#[tokio::test]
async fn q8_sovereignty_hub_skyhooks_and_workforce() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_003);
    session.user_id = Some(103);
    session.character_id = Some(90_000_103);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Static definitions and list queries succeed (empty when nothing exists).
    let vuln_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
        Vec::new(),
        true,
    );
    assert_eq!(vuln_rsp.status_code, STATUS_OK);
    assert!(vuln_rsp.payload.as_ref().unwrap().value.is_empty(), "no fabricated vulnerable systems");
    let upg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(upg_rsp.status_code, STATUS_OK);
    let merc_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        Vec::new(),
        true,
    );
    assert_eq!(merc_rsp.status_code, STATUS_OK);

    // 2. State-backed hub / skyhook / den RPCs reject requests without a target identifier
    //    (`encode_id_req` is empty here) instead of fabricating a default structure.
    for req_name in [
        "eve_public.sovereignty.skyhook.api.GetRequest",
        "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
        "eve_public.sovereignty.skyhook.api.ActivateRequest",
        "eve_public.sovereignty.skyhook.api.DeactivateRequest",
        "eve_public.sovereignty.hub.api.GetResourcesRequest",
        "eve_public.sovereignty.hub.fuel.api.GetRequest",
        "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
        "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
        "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
        "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, encode_id_req(1_000_000_010), true);
        assert_eq!(rsp.status_code, STATUS_BAD_REQUEST, "{req_name} must reject a missing identifier");
    }

    assert_eq!(
        upg_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse"
    );
    assert_eq!(
        merc_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse"
    );
}

#[tokio::test]
async fn q8_faction_warfare_and_operations() {
    // parity: quasar:eveProto/generated/eve/factionwarfare/warzone/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/factionwarfare/warzone/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_004);
    session.user_id = Some(104);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);

    // 1. Faction queries & activity scores
    let fac_rsp = call_req(
        &ctx,
        &session,
        "eve.faction.api.GetRequest",
        encode_id_req(500_001),
        false,
    );
    assert_eq!(fac_rsp.status_code, STATUS_OK);

    // 2. Faction Warfare Systems, Leaderboard & Warzone API
    let fws_rsp = call_req(
        &ctx,
        &session,
        "eve.factionwarfare.systems.GetRequest",
        encode_id_req(30_000_142),
        false,
    );
    assert_eq!(fws_rsp.status_code, STATUS_OK);

    let lb_rsp = call_req(
        &ctx,
        &session,
        "eve.factionwarfare.leaderboard.characters.GetRequest",
        Vec::new(),
        false,
    );
    assert_eq!(lb_rsp.status_code, STATUS_OK);

    let get_all_req = "eve.factionwarfare.warzone.api.GetAllRequest";
    let get_part_req = "eve.factionwarfare.warzone.api.GetParticipantsRequest";
    {
        let wz_rsp = call_req(&ctx, &session, get_all_req, Vec::new(), true);
        assert_eq!(wz_rsp.status_code, STATUS_OK);
        let wz_wm = eve_proto::quasar::WireMessage::decode(&wz_rsp.payload.unwrap().value).unwrap();
        let wz_ids: Vec<u64> = wz_wm
            .fields
            .iter()
            .filter(|f| f.field_number == 1)
            .filter_map(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
            .filter_map(|m| m.fields.first()?.value.as_varint())
            .collect();
        assert_eq!(wz_ids, vec![1, 2], "both SDE warzones derived from fw_system_occupations");

        let mut wz1_req = Vec::new();
        let mut wz1_id = Vec::new();
        encode_varint_field(1, 1, &mut wz1_id);
        encode_bytes_field(1, &wz1_id, &mut wz1_req);
        let p1_rsp = call_req(&ctx, &session, get_part_req, wz1_req, true);
        assert_eq!(p1_rsp.status_code, STATUS_OK);
        let p1_wm = eve_proto::quasar::WireMessage::decode(&p1_rsp.payload.unwrap().value).unwrap();
        let part_msg = eve_proto::quasar::WireMessage::decode(
            p1_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap(),
        )
        .unwrap();
        let fac_ids: Vec<u64> = part_msg
            .fields
            .iter()
            .filter(|f| f.field_number == 1)
            .filter_map(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
            .filter_map(|m| m.fields.first()?.value.as_varint())
            .collect();
        let pirate_id = part_msg
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
            .and_then(|m| m.fields.first()?.value.as_varint());
        assert_eq!(fac_ids, vec![500_001, 500_004]);
        assert_eq!(pirate_id, Some(500_010));

        let mut bad_wz = Vec::new();
        let mut bad_id = Vec::new();
        encode_varint_field(1, 99, &mut bad_id);
        encode_bytes_field(1, &bad_id, &mut bad_wz);
        assert_eq!(
            call_req(&ctx, &session, get_part_req, bad_wz, true).status_code,
            eve_server::quasar_gateway::STATUS_NOT_FOUND
        );
    }

    // 3. Operational Recommendations (backed by res:/staticdata/operations.fsdbinary + character completion state)
    let mut unauth = SessionState::new(20_005);
    assert_eq!(
        call_req(&ctx, &unauth, "eve_public.operation.recommendation.GetRequest", Vec::new(), false).status_code,
        eve_server::quasar_gateway::STATUS_UNAUTHORIZED
    );
    let _ = &mut unauth;

    let mut rec_req = Vec::new();
    encode_varint_field(1, 2, &mut rec_req); // amount = 2
    let rec_rsp = call_req(
        &ctx,
        &session,
        "eve_public.operation.recommendation.GetRequest",
        rec_req.clone(),
        true,
    );
    assert_eq!(rec_rsp.status_code, STATUS_OK);
    let rec_payload = rec_rsp.payload.as_ref().unwrap();
    assert_eq!(
        rec_payload.normalized_type_name(),
        "eve_public.operation.recommendation.GetResponse"
    );
    let sde_ops = ctx.sde.operations().expect("SDE operations.fsdbinary");
    let rec_wm = eve_proto::quasar::WireMessage::decode(&rec_payload.value).unwrap();
    let recs: Vec<_> = rec_wm.fields.iter().filter(|f| f.field_number == 4).collect();
    assert_eq!(recs.len(), 2, "requested amount = 2 recommendations");
    let mut rec_op_ids = Vec::new();
    for r in &recs {
        let r_wm = eve_proto::quasar::WireMessage::decode(r.value.as_bytes().unwrap()).unwrap();
        let op_id = r_wm
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
            .and_then(|m| m.fields.first()?.value.as_varint())
            .expect("operation.sequential");
        let journey = r_wm
            .fields
            .iter()
            .find(|f| f.field_number == 3)
            .and_then(|f| f.value.as_bytes())
            .expect("16-byte journey UUID");
        assert_eq!(journey.len(), 16);
        assert!(sde_ops.contains_key(&op_id.to_string()), "recommended op {op_id} must exist in SDE");
        rec_op_ids.push(op_id);
    }
    assert_eq!(rec_op_ids, vec![157, 158]);

    // Completing operation 157 filters it out when allow_old=false, and includes it when allow_old=true.
    ctx.dispatch_rpc(
        &mut session,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[eve_proto::PyRep::Integer(157)],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("activate_operation_in_solar_system");
    ctx.dispatch_rpc(
        &mut session,
        "operationsManager",
        None,
        "process_client_event",
        &[eve_proto::PyRep::String("task_completed".into()), eve_proto::PyRep::Integer(1)],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("process_client_event");
    let rec_after = call_req(
        &ctx,
        &session,
        "eve_public.operation.recommendation.GetRequest",
        rec_req.clone(),
        true,
    );
    let after_wm = eve_proto::quasar::WireMessage::decode(&rec_after.payload.unwrap().value).unwrap();
    let after_ids: Vec<u64> = after_wm
        .fields
        .iter()
        .filter(|f| f.field_number == 1)
        .filter_map(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
        .filter_map(|m| m.fields.first()?.value.as_varint())
        .collect();
    assert_eq!(after_ids, vec![158, 2395], "completed operation 157 excluded when allow_old=false");

    encode_varint_field(2, 1, &mut rec_req); // allow_old = true
    let rec_old = call_req(
        &ctx,
        &session,
        "eve_public.operation.recommendation.GetRequest",
        rec_req,
        true,
    );
    let old_wm = eve_proto::quasar::WireMessage::decode(&rec_old.payload.unwrap().value).unwrap();
    let old_ids: Vec<u64> = old_wm
        .fields
        .iter()
        .filter(|f| f.field_number == 1)
        .filter_map(|f| eve_proto::quasar::WireMessage::decode(f.value.as_bytes()?).ok())
        .filter_map(|m| m.fields.first()?.value.as_varint())
        .collect();
    assert_eq!(old_ids, vec![157, 158], "allow_old=true re-includes completed operation 157");
}


