//! Integration tests for Quasar campaign, sovereignty resource/mercenary-den activity,
//! faction activity admin, stargate lock, and AI assistant request/response endpoints.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
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

fn make_public_request(
    ctx: &ServerContext,
    type_name: &str,
    payload_bytes: Vec<u8>,
    corr: &[u8],
) -> EvePublicRequest {
    EvePublicRequest {
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
        application_instance_uuid: b"app-feat-test-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

async fn dispatch_and_verify_request(
    ctx: &ServerContext,
    sess: &mut SessionState,
    req_type: &str,
    expected_resp_type: &str,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = make_public_request(ctx, req_type, Vec::new(), corr);
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
        .unwrap_or_else(|e| panic!("SendRequest failed for {req_type}: {e:?}"));
    let rsp = extract_response_proto(&rep);
    assert_eq!(
        rsp.status_code, STATUS_OK,
        "Request failed for {req_type}: {}",
        rsp.status_message
    );
    assert_eq!(rsp.correlation_uuid, corr);
    assert_eq!(rsp.internal_origin, "evomu.quasar.public_gateway");
    let payload = rsp
        .payload
        .as_ref()
        .unwrap_or_else(|| panic!("Missing payload for {req_type}"));
    assert_eq!(payload.normalized_type_name(), expected_resp_type);
    rsp
}

#[tokio::test]
async fn test_quasar_campaign_requests_and_track_progress() {
    // parity: quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = make_session(31_001, 101, 90_000_101, 98_000_001);

    // 1. Campaign API (GetAllRequest, GetRequest)
    let r1 = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.api.GetAllRequest",
        "eve_public.campaign.api.GetAllResponse",
        b"cmp-01",
    )
    .await;
    assert_eq!(r1.payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.api.GetAllResponse");
    assert_eq!(r1.correlation_uuid, b"cmp-01");
    let mut camp_id_req = Vec::new();
    let mut camp_id_inner = Vec::new();
    let mut camp_uuid = [0u8; 16];
    camp_uuid[12..16].copy_from_slice(&1u32.to_be_bytes());
    eve_proto::quasar::encode_bytes_field(1, &camp_uuid, &mut camp_id_inner);
    eve_proto::quasar::encode_bytes_field(1, &camp_id_inner, &mut camp_id_req);
    let r2_req = make_public_request(&ctx, "eve_public.campaign.api.GetRequest", camp_id_req, b"cmp-02");
    let r2_rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(r2_req.encode_proto())], &HashMap::new(), None).await.unwrap();
    let r2 = extract_response_proto(&r2_rep);
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(r2.payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.api.GetResponse");
    assert_eq!(r2.correlation_uuid, b"cmp-02");

    // 2. Contribution method definitions (GetAllRequest)
    let r3 = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        "eve_public.campaign.contributionmethod.definition.api.GetAllResponse",
        b"cmp-03",
    )
    .await;
    assert_eq!(
        r3.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.contributionmethod.definition.api.GetAllResponse"
    );

    // 3. Campaign objective (GetRequest, CommitRequest)
    let mut obj_id_req = Vec::new();
    let mut obj_id_inner = Vec::new();
    let mut obj_uuid = [0u8; 16];
    obj_uuid[12..16].copy_from_slice(&101u32.to_be_bytes());
    eve_proto::quasar::encode_bytes_field(1, &obj_uuid, &mut obj_id_inner);
    eve_proto::quasar::encode_bytes_field(1, &obj_id_inner, &mut obj_id_req);
    let r5_req = make_public_request(&ctx, "eve_public.campaign.objective.api.GetRequest", obj_id_req.clone(), b"cmp-05");
    let r5_rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(r5_req.encode_proto())], &HashMap::new(), None).await.unwrap();
    let r5 = extract_response_proto(&r5_rep);
    assert_eq!(r5.status_code, STATUS_OK);
    assert_eq!(r5.payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.objective.api.GetResponse");
    let before_pred = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse",
        b"cmp-05b",
    )
    .await;
    let initial_pred_bytes = before_pred.payload.unwrap().value;

    let commit_req = make_public_request(&ctx, "eve_public.campaign.objective.api.CommitRequest", obj_id_req, b"cmp-05c");
    let commit_rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(commit_req.encode_proto())], &HashMap::new(), None).await.unwrap();
    let commit_rsp = extract_response_proto(&commit_rep);
    assert_eq!(commit_rsp.status_code, STATUS_OK);

    // 4. Campaign track prediction reflects committed progress
    let pred_rsp = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest",
        "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse",
        b"cmp-06",
    )
    .await;
    assert_eq!(
        pred_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"
    );
    assert_ne!(
        pred_rsp.payload.unwrap().value,
        initial_pred_bytes,
        "Campaign track prediction should advance after CommitRequest"
    );

    // 5. Campaign eventmatch trigger definitions (GetAllRequest)
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse",
        b"cmp-07",
    )
    .await;
}

#[tokio::test]
async fn test_quasar_sovereignty_resources_mercenaryden_stargate_and_admin_requests() {
    // parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    const FW_SYS: u32 = eve_server::services::fw_services::HQ_SYSTEM_AMO;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&90_000_001) {
            ch.solar_system_id = FW_SYS;
        }
        s.fw_system_occupations.insert(FW_SYS, eve_db::snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: FW_SYS,
            owner_faction_id: eve_server::services::fw_services::FACTION_MINMATAR,
            occupier_faction_id: eve_server::services::fw_services::FACTION_MINMATAR,
            victory_points: 0,
            victory_points_threshold: 3000,
        });
    });
    let empty_kw = HashMap::new();
    let mut sess = make_session(31_002, 1, 90_000_001, 98_000_001);
    sess.solar_system_id = Some(FW_SYS);

    // 1. AI Assistant QueryRequest
    let r_ai = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.ai.assistant.chat.api.QueryRequest",
        "eve_public.ai.assistant.chat.api.QueryResponse",
        b"ai-01",
    )
    .await;
    assert_eq!(r_ai.payload.as_ref().unwrap().normalized_type_name(), "eve_public.ai.assistant.chat.api.QueryResponse");
    assert_eq!(r_ai.correlation_uuid, b"ai-01");

    // 2. Mercenary Den Activity API (GetForMercenaryDenRequest, GetAllRequest, StartRequest, GetCapacityRequest)
    let r_md = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse",
        b"mden-01",
    )
    .await;
    assert_eq!(
        r_md.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse"
    );
    assert_eq!(r_md.correlation_uuid, b"mden-01");
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse",
        b"mden-02",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.StartResponse",
        b"mden-03",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse",
        b"mden-04",
    )
    .await;

    // 3. Sovereignty Planet & Star Resources API
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse",
        b"res-p1",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse",
        b"res-p2",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse",
        b"res-p3",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse",
        b"res-s1",
    )
    .await;

    // 4. Stargate Lock API (GetRequest, GetRestrictedSystemsRequest)
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.stargate.lock.api.GetRequest",
        "eve_public.stargate.lock.api.GetResponse",
        b"sg-01",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
        "eve_public.stargate.lock.api.GetRestrictedSystemsResponse",
        b"sg-02",
    )
    .await;

    // 5. Faction Activity Admin & Skyhook Admin (GM succeeds; non-GM gets STATUS_FORBIDDEN)
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        "eve_public.faction.activity.admin.IncrementContributionScoreResponse",
        b"adm-f1",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
        "eve_public.faction.activity.admin.SetContributionScoreResponse",
        b"adm-f2",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
        "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse",
        b"adm-s1",
    )
    .await;
    dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest",
        "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse",
        b"adm-s2",
    )
    .await;

    // Non-GM session must be rejected with STATUS_FORBIDDEN (403)
    sess.role = 0;
    let unpriv_req = make_public_request(
        &ctx,
        "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
        Vec::new(),
        b"adm-deny",
    );
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(unpriv_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    assert_eq!(extract_response_proto(&rep).status_code, STATUS_FORBIDDEN);
}
