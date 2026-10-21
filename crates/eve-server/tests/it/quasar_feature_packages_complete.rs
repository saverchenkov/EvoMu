//! End-to-End Parity Acceptance Tests for Quasar Feature Packages (`feat:quasar.*`).
//!
//! Validates schema integrity, dynamic wire roundtripping, gateway request routing via `publicGatewaySvc`,
//! response envelope validation, and notice fan-out across the Quasar public feature packages:
//! `ai`, `app`, `campaign`, `character`, `cosmetic`, `entitlement`, `faction`, `operation`,
//! `opportunity`, `payment`, `pirate`, `plex`, `sovereignty`, `space`, `stargate`, `store`, `user`.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, STATUS_OK};
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
    expected_resp_type: Option<&str>,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let payload = if let Ok(sample) = ctx.quasar_gateway.descriptors.instantiate_populated_sample(req_type, 42) {
        ctx.quasar_gateway.descriptors.encode_dynamic(&sample).unwrap_or_default()
    } else {
        Vec::new()
    };
    let req = make_public_request(ctx, req_type, payload, corr);
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
    assert_eq!(rsp.status_code, STATUS_OK, "Request failed for {req_type}: {}", rsp.status_message);
    assert_eq!(rsp.correlation_uuid, corr);
    assert_eq!(rsp.internal_origin, "evomu.quasar.public_gateway");
    if let Some(expected_type) = expected_resp_type {
        let payload = rsp.payload.as_ref().unwrap_or_else(|| panic!("Missing payload for {req_type}"));
        assert_eq!(payload.normalized_type_name(), expected_type);
    }
    rsp
}

fn verify_event_notice_delivery(
    ctx: &ServerContext,
    sess: &SessionState,
    event_type: &str,
) {
    let desc = &ctx.quasar_gateway.descriptors;
    let sample = desc
        .instantiate_populated_sample(event_type, 2002)
        .unwrap_or_else(|e| panic!("instantiate event {event_type}: {e}"));
    let wire = desc
        .encode_dynamic(&sample)
        .unwrap_or_else(|e| panic!("encode event {event_type}: {e}"));
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{event_type}"),
        value: wire,
    };
    let char_id = sess.character_id.expect("character_id required") as u32;
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(char_id),
        any,
        ctx.now_unix(),
    );
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        !notices.is_empty(),
        "Session {} should receive notices after publishing {}",
        sess.session_id,
        event_type
    );
    let found = notices.iter().any(|n| {
        n.payload
            .as_ref()
            .map(|p| p.normalized_type_name() == event_type)
            .unwrap_or(false)
    });
    assert!(
        found,
        "Notice with payload type '{}' should be delivered to subscriber",
        event_type
    );
}

#[tokio::test]
async fn test_quasar_service_routing_batch1() {
    // parity: feat:quasar.ai, feat:quasar.app, feat:quasar.campaign, feat:quasar.character, feat:quasar.cosmetic, feat:quasar.entitlement, feat:quasar.faction, feat:quasar.operation, feat:quasar.opportunity
    eve_testkit::attest(&[
        "feat:quasar.ai",
        "feat:quasar.app",
        "feat:quasar.campaign",
        "feat:quasar.character",
        "feat:quasar.cosmetic",
        "feat:quasar.entitlement",
        "feat:quasar.faction",
        "feat:quasar.operation",
        "feat:quasar.opportunity",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_001, 101, 90_000_101, 98_000_001);

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 1. Ai (2 files)
    assert_eq!(reg.files_by_domain.get("ai").unwrap().len(), 2);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.ai.assistant.chat.api.QueryRequest", Some("eve_public.ai.assistant.chat.api.QueryResponse"), b"ai-01").await;

    // 2. App (70 files)
    assert_eq!(reg.files_by_domain.get("app").unwrap().len(), 70);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.app.eveonline.career.goal.GetAllRequest", Some("eve_public.app.eveonline.career.goal.GetAllResponse"), b"app-01").await;

    // 3. Campaign (63 files)
    assert_eq!(reg.files_by_domain.get("campaign").unwrap().len(), 63);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Some("eve_public.campaign.api.GetAllResponse"), b"camp-01").await;

    // 4. Character (94 files)
    assert_eq!(reg.files_by_domain.get("character").unwrap().len(), 94);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.character.GetNamesByUserRequest", Some("eve_public.character.GetNamesByUserResponse"), b"char-01").await;

    // 5. Cosmetic (138 files)
    assert_eq!(reg.files_by_domain.get("cosmetic").unwrap().len(), 138);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Some("eve_public.cosmetic.character.title.api.GetOwnedResponse"), b"cosm-01").await;

    // 6. Entitlement (17 files)
    assert_eq!(reg.files_by_domain.get("entitlement").unwrap().len(), 17);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.entitlement.user.api.GetRequest", Some("eve_public.entitlement.user.api.GetResponse"), b"ent-01").await;

    // 7. Faction (9 files)
    assert_eq!(reg.files_by_domain.get("faction").unwrap().len(), 9);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", Some("eve_public.faction.activity.admin.IncrementContributionScoreResponse"), b"fact-01").await;

    // 8. Operation (4 files)
    assert_eq!(reg.files_by_domain.get("operation").unwrap().len(), 4);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.operation.recommendation.GetRequest", Some("eve_public.operation.recommendation.GetResponse"), b"oper-01").await;

    // 9. Opportunity (2 files)
    assert_eq!(reg.files_by_domain.get("opportunity").unwrap().len(), 2);
    verify_event_notice_delivery(&ctx, &sess, "eve_public.opportunity.api.Completed");
}

#[tokio::test]
async fn test_quasar_service_routing_batch2() {
    // parity: feat:quasar.payment, feat:quasar.pirate, feat:quasar.plex, feat:quasar.sovereignty, feat:quasar.space, feat:quasar.stargate, feat:quasar.store, feat:quasar.user
    eve_testkit::attest(&[
        "feat:quasar.payment",
        "feat:quasar.pirate",
        "feat:quasar.plex",
        "feat:quasar.sovereignty",
        "feat:quasar.space",
        "feat:quasar.stargate",
        "feat:quasar.store",
        "feat:quasar.user",
    ]);

    let ctx = ServerContext::new();
    let reg = &ctx.quasar_gateway.descriptors;
    let mut sess = make_session(30_002, 102, 90_000_102, 98_000_002);

    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // 10. Payment (28 files)
    assert_eq!(reg.files_by_domain.get("payment").unwrap().len(), 28);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.payment.token.api.GetQuickPayRequest", Some("eve_public.payment.token.api.GetQuickPayResponse"), b"pay-01").await;

    // 11. Pirate (13 files)
    assert_eq!(reg.files_by_domain.get("pirate").unwrap().len(), 13);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", Some("eve_public.pirate.suppression.api.GetSystemInfoResponse"), b"pir-01").await;

    // 12. Plex (18 files)
    assert_eq!(reg.files_by_domain.get("plex").unwrap().len(), 18);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", Some("eve_public.plex.vault.api.BalanceResponse"), b"plex-01").await;

    // 13. Sovereignty (93 files)
    assert_eq!(reg.files_by_domain.get("sovereignty").unwrap().len(), 93);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Some("eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse"), b"sov-01").await;

    // 14. Space (17 files)
    assert_eq!(reg.files_by_domain.get("space").unwrap().len(), 17);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.space.api.cdn.GetUpdateRequest", Some("eve_public.space.api.cdn.GetUpdateResponse"), b"spc-01").await;

    // 15. Stargate (11 files)
    assert_eq!(reg.files_by_domain.get("stargate").unwrap().len(), 11);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.stargate.lock.api.GetRequest", Some("eve_public.stargate.lock.api.GetResponse"), b"sg-01").await;

    // 16. Store (60 files)
    assert_eq!(reg.files_by_domain.get("store").unwrap().len(), 60);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.store.offer.api.GetAllRequest", Some("eve_public.store.offer.api.GetAllResponse"), b"store-01").await;

    // 17. User (50 files)
    assert_eq!(reg.files_by_domain.get("user").unwrap().len(), 50);
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Some("eve_public.user.license.api.GetResponse"), b"user-01").await;
}

fn verify_pyj_batch(reg: &QuasarDescriptorRegistry, row_ids: &[&str]) -> usize {
    for &row_id in row_ids {
        let pyj = row_id.strip_prefix("quasar:").expect("quasar: prefix");
        let fd = reg.get_file_by_pyj(pyj).expect("cataloged pyj present");
        assert_eq!(fd.pyj_path, pyj);
        for (idx, m) in fd.messages.iter().enumerate() {
            let sample = reg
                .instantiate_populated_sample(&m.full_name, (idx as u64) + 1)
                .expect("instantiate sample");
            let wire = reg.encode_dynamic(&sample).expect("encode sample");
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .expect("decode sample");
            assert_eq!(decoded, sample);
            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            assert_eq!(any.normalized_type_name(), m.full_name);
        }
    }
    row_ids.len()
}

#[tokio::test]
async fn test_quasar_campaign_objectives_and_contributions_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_001, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Some("eve_public.campaign.api.GetAllResponse"), b"cmp-b1-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Some("eve_public.campaign.contributionmethod.definition.api.GetAllResponse"), b"cmp-b1-2").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", Some("eve_public.campaign.objective.api.GetResponse"), b"cmp-b1-3").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/campaign_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj").unwrap().domain, "campaign");
}

#[tokio::test]
async fn test_quasar_campaign_tracks_and_triggers_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_002, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", Some("eve_public.campaign.track.api.GetTickingContributionsPredictionResponse"), b"cmp-b2-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Some("eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"), b"cmp-b2-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj").unwrap().domain, "campaign");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj").unwrap().domain, "campaign");
}

#[tokio::test]
async fn test_quasar_deadspace_faction_math_ships_and_sov_hub_batch() {
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj, quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj, quasar:eveProto/generated/eve_public/deadspace/archetype_pb2.pyj, quasar:eveProto/generated/eve_public/deadspace/signaturetype_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj, quasar:eveProto/generated/eve_public/math/fraction_pb2.pyj, quasar:eveProto/generated/eve_public/math/vector_pb2.pyj, quasar:eveProto/generated/eve_public/ship/classification_pb2.pyj, quasar:eveProto/generated/eve_public/ship/ship_pb2.pyj, quasar:eveProto/generated/eve_public/ship/ship_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj",
        "quasar:eveProto/generated/eve_public/deadspace/archetype_pb2.pyj",
        "quasar:eveProto/generated/eve_public/deadspace/signaturetype_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/math/fraction_pb2.pyj",
        "quasar:eveProto/generated/eve_public/math/vector_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/classification_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/ship_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ship/ship_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_003, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", Some("eve_public.faction.activity.admin.IncrementContributionScoreResponse"), b"sov-b3-1").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/ship/ship_pb2.pyj").unwrap().domain, "ship");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/math/vector_pb2.pyj").unwrap().domain, "math");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/hub_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_sovereignty_hub_upgrades_workforce_and_mercenaryden_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activitytemplate/activitytemplate_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorph/itemtype/identifier_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activity_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/activitytemplate/activitytemplate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorph/itemtype/identifier_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_004, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Some("eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse"), b"sov-b4-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Some("eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse"), b"sov-b4-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/upgrade/upgrade_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/workforce/workforce_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/mercenaryden/evolution_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_sovereignty_resources_and_skyhooks_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorphs_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_type_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent/itemtype/identifier_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/infomorphs_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/mercenaryden_type_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent/itemtype/identifier_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/reagent_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_005, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 12);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Some("eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"), b"sov-b5-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Some("eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"), b"sov-b5-2").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", Some("eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse"), b"sov-b5-3").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/resource/planet/planet_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/resource/star/star_pb2.pyj").unwrap().domain, "sovereignty");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/skyhook/theftvulnerability_pb2.pyj").unwrap().domain, "sovereignty");
}

#[tokio::test]
async fn test_quasar_stars_stargates_stations_and_structures_batch() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/workforce_pb2.pyj, quasar:eveProto/generated/eve_public/star/star_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/stargate/stargate_pb2.pyj, quasar:eveProto/generated/eve_public/station/station_pb2.pyj, quasar:eveProto/generated/eve_public/structure/group_pb2.pyj, quasar:eveProto/generated/eve_public/structure/structure_pb2.pyj, quasar:eveProto/generated/eve_public/structure/structure_type_pb2.pyj
    const ROWS: &[&str] = &[
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/workforce_pb2.pyj",
        "quasar:eveProto/generated/eve_public/star/star_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/stargate_pb2.pyj",
        "quasar:eveProto/generated/eve_public/station/station_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/group_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/structure_pb2.pyj",
        "quasar:eveProto/generated/eve_public/structure/structure_type_pb2.pyj",
    ];
    eve_testkit::attest(ROWS);

    let ctx = ServerContext::new();
    let mut sess = make_session(31_006, 101, 90_000_101, 98_000_001);
    assert_eq!(verify_pyj_batch(&ctx.quasar_gateway.descriptors, ROWS), 9);

    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.stargate.lock.api.GetRequest", Some("eve_public.stargate.lock.api.GetResponse"), b"sov-b6-1").await;
    dispatch_and_verify_request(&ctx, &mut sess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Some("eve_public.stargate.lock.api.GetRestrictedSystemsResponse"), b"sov-b6-2").await;
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/star/star_pb2.pyj").unwrap().domain, "star");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/stargate/stargate_pb2.pyj").unwrap().domain, "stargate");
    assert_eq!(ctx.quasar_gateway.descriptors.get_file_by_pyj("eveProto/generated/eve_public/structure/structure_pb2.pyj").unwrap().domain, "structure");
}
