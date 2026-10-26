//! Integration tests for Quasar campaign, sovereignty resource/mercenary-den activity,
//! faction activity admin, stargate lock, and AI assistant request/response endpoints.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
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
async fn test_quasar_campaign_definition_requests() {
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = make_session(31_001, 101, 90_000_101, 98_000_001);
    // 2. Contribution method definitions (GetAllRequest)
    let r3 = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        "eve_public.campaign.contributionmethod.definition.api.GetAllResponse",
        b"cmp-03",
    )
    .await;
    let r3_payload = r3.payload.as_ref().unwrap();
    assert_eq!(
        r3_payload.normalized_type_name(),
        "eve_public.campaign.contributionmethod.definition.api.GetAllResponse"
    );
    assert!(!r3_payload.value.is_empty(), "Contribution method definitions must not be empty");
    let cm_wm = WireMessage::decode(&r3_payload.value).expect("decode contribution methods");
    let cm_entries: Vec<_> = cm_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(cm_entries.len(), 22, "Must return 22 authentic contribution methods");

    // 5. Campaign eventmatch trigger definitions (GetAllRequest)
    let r7 = dispatch_and_verify_request(
        &ctx,
        &mut sess,
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse",
        b"cmp-07",
    )
    .await;
    let r7_payload = r7.payload.as_ref().unwrap();
    assert!(!r7_payload.value.is_empty(), "Eventmatch trigger definitions must not be empty");
    let em_wm = WireMessage::decode(&r7_payload.value).expect("decode eventmatch definitions");
    let em_entries: Vec<_> = em_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(em_entries.len(), 10, "Must return authentic eventmatch trigger definitions");
}

#[tokio::test]
async fn test_quasar_sovereignty_resources_mercenaryden_stargate_and_admin_requests() {
    // parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
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

    // 3. Sovereignty Planet Resources API (star resources: quasar_sov_pirate_remediation)
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

    // 5. Faction Activity Admin (GM succeeds; non-GM gets STATUS_FORBIDDEN); skyhook admin
    //    ownership/role checks are covered by quasar_sov_pirate_remediation
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

    // Non-GM session must be rejected with STATUS_FORBIDDEN (403)
    sess.role = 0;
    let unpriv_req = make_public_request(
        &ctx,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
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
