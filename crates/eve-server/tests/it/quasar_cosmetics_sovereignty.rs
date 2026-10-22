//! Integration test suite for Quasar Cosmetics, SKINR, Sovereignty, Entitlements,
//! Goals, Faction Activity, Freelance, Payment, and Navigation Endpoints (`Q-9` closure).

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{NoticeScope, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_space_services::{encode_domain_message, set_domain_attributes};
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

fn build_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess.corp_role = 0x01; // Director role
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr_id: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr_id.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-final".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
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

// ============================================================================
// 1. Quasar Cosmetics Titles, Palette & Market Request Endpoints
// ============================================================================

#[tokio::test]
async fn q_final_cosmetics_titles_palette_market() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/request_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/character/title/api/admin/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.character.title.api.admin");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.character.title.definition.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/corporation/palette/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.corporation.palette.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj").expect("fd").package, "eve_public.cosmetic.market.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj").expect("fd").package, "eve_public.cosmetic.market.component.listing.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.market.skin.listing.api.admin");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj").expect("fd").package, "eve_public.cosmetic.market.skin.listing.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/api/request_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj").expect("fd").package, "eve_public.cosmetic.structure.paintwork.license.api.admin");

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(40_001, 90_000_201, 98_000_001);

    let endpoints = [
        (
            "eve_public.cosmetic.character.title.api.admin.GrantRequest",
            "eve_public.cosmetic.character.title.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
            "eve_public.cosmetic.character.title.definition.api.GetLatestResponse",
        ),
        (
            "eve_public.cosmetic.corporation.palette.api.GetRequest",
            "eve_public.cosmetic.corporation.palette.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
            "eve_public.cosmetic.market.api.GetBrokerFeesResponse",
        ),
        (
            "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
            "eve_public.cosmetic.market.component.listing.api.GetAllResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
            "eve_public.cosmetic.market.skin.listing.api.GetAllResponse",
        ),
        (
            "eve_public.cosmetic.ship.api.GetRequest",
            "eve_public.cosmetic.ship.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest",
            "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueResponse",
        ),
    ];

    for (i, (req_type, expected_resp)) in endpoints.iter().enumerate() {
        let corr = format!("corr-cosm-1-{i}").into_bytes();
        let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new(), &corr).await;
        assert_eq!(rsp.status_code, STATUS_OK, "{req_type} status");
        assert_eq!(rsp.correlation_uuid, corr, "{req_type} corr");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(payload.normalized_type_name(), *expected_resp, "{req_type} type");
    }
}

// ============================================================================
// 2. Quasar Cosmetics Licenses & Sequencing Request Endpoints
// ============================================================================

#[tokio::test]
async fn q_final_cosmetics_licenses_and_sequencing() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/admin/admin_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/finite/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/admin/admin_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/infinite/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/item/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/admin/admin_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.license.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api");

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(40_002, 90_000_202, 98_000_001);

    let endpoints = [
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveResponse",
        ),
    ];

    for (i, (req_type, expected_resp)) in endpoints.iter().enumerate() {
        let corr = format!("corr-cosm-2-{i}").into_bytes();
        let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new(), &corr).await;
        assert_eq!(rsp.status_code, STATUS_OK, "{req_type} status");
        assert_eq!(rsp.correlation_uuid, corr, "{req_type} corr");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(payload.normalized_type_name(), *expected_resp, "{req_type} type");
    }
}

// ============================================================================
// 3. Entitlements, Faction, Freelance, Payment & Stargate Requests
// ============================================================================

#[tokio::test]
async fn q_final_entitlements_faction_freelance_payment_stargate() {
    // parity: quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/faction/activity/activity_pb2.pyj").expect("fd").package, "eve_public.faction.activity");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj").expect("fd").package, "eve_public.freelance.contributionmethod.definition.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/payment/purchase/api_pb2.pyj").expect("fd").package, "eve_public.payment.purchase.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj").expect("fd").package, "eve_public.plex.vault.transaction.api");

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    eve_server::services::fw_services::add_victory_points(&ctx, 30_000_142, 2_500);
    let mut sess = build_sess(40_003, 90_000_203, 98_000_001);

    let endpoints = [
        (
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            "eve_public.faction.activity.GetSolarSystemScoresResponse",
        ),
        (
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest",
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse",
        ),
        (
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorResponse",
        ),
        (
            "eve_public.payment.purchase.api.TokenRequest",
            "eve_public.payment.purchase.api.TokenResponse",
        ),
        (
            "eve_public.payment.purchase.api.CostRequest",
            "eve_public.payment.purchase.api.CostResponse",
        ),
        (
            "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
            "eve_public.plex.vault.transaction.api.GetAllLoggedForUserResponse",
        ),
        (
            "eve_public.plex.vault.transaction.api.GetLogRequest",
            "eve_public.plex.vault.transaction.api.GetLogResponse",
        ),
        (
            "eve_public.plex.vault.transaction.api.GetStatisticsRequest",
            "eve_public.plex.vault.transaction.api.GetStatisticsResponse",
        ),
    ];

    for (i, (req_type, expected_resp)) in endpoints.iter().enumerate() {
        let corr = format!("corr-fac-pay-{i}").into_bytes();
        let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new(), &corr).await;
        assert_eq!(rsp.status_code, STATUS_OK, "{req_type} status");
        assert_eq!(rsp.correlation_uuid, corr, "{req_type} corr");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(payload.normalized_type_name(), *expected_resp, "{req_type} type");
        if *req_type == "eve_public.faction.activity.GetSolarSystemScoresRequest" {
            // Verify FW live score resolution from fw_services runtime
            let dyn_msg = reg
                .decode_dynamic("eve_public.faction.activity.GetSolarSystemScoresResponse", &payload.value)
                .expect("decode dynamic GetSolarSystemScoresResponse");
            let f1 = dyn_msg.get(1).expect("solar_system field 1");
            let f2 = dyn_msg.get(2).expect("scores field 2");
            assert!(matches!(f1, eve_proto::quasar::DynamicValue::Message(m) if m.get(1) == Some(&eve_proto::quasar::DynamicValue::Uint64(30_000_142))));
            assert!(matches!(f2, eve_proto::quasar::DynamicValue::Repeated(list) if !list.is_empty()));
        }
    }

    // Stargate lock notice validation (loopback infrastructure test without closing open notice row)
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    for sg_notice in [
        "eve_public.stargate.lock.api.RestrictedSystemsNotice",
        "eve_public.stargate.lock.api.ActivatedNotice",
        "eve_public.stargate.lock.api.DeactivatedNotice",
    ] {
        let payload = encode_domain_message(&reg, sg_notice, |r_reg, resp| {
            set_domain_attributes(r_reg, resp, 30_000_142, 50_000_001, "gate_lock");
        })
        .expect("encode stargate lock notice");
        ctx.quasar_gateway
            .publish_notice(NoticeScope::Broadcast, payload, ctx.now_unix())
            .expect("publish_notice");
        let recvd = stream_rx.recv().await.expect("recv notice");
        assert_eq!(recvd.payload.unwrap().normalized_type_name(), sg_notice);
        assert_eq!(recvd.internal_origin, "evomu.quasar.notice_publisher");
    }

    let sg1 = eve_server::services::quasar_sov_services::publish_stargate_lock_activated_notice(
        &ctx.quasar_gateway, 30_000_142, 50_000_001, ctx.now_unix() + 3600, ctx.now_unix(),
    ).expect("stargate lock notice");
    assert_eq!(sg1, 1);
    let sgr1 = stream_rx.recv().await.expect("recv sgr1");
    assert!(sgr1.payload.unwrap().type_url.ends_with("ActivatedNotice"));
}

// ============================================================================
// 4. Cosmetic, SKINR & Tale Notices Fan-out
// ============================================================================

#[tokio::test]
async fn q_final_cosmetic_skinr_and_tale_notices() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/notice_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/tale/api/notices_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/character/title/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.character.title.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/character/title/definition/version/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.character.title.definition.version.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.market.transaction.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/api/notice_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/component/license/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/license/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.license.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj").expect("fd").package, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/tale/api/notices_pb2.pyj").expect("fd").package, "eve_public.tale.api");

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(40_004, 90_000_204, 98_000_001);

    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let notice_types = [
        "eve_public.cosmetic.character.title.api.GrantedNotice",
        "eve_public.cosmetic.character.title.api.RevokedNotice",
        "eve_public.cosmetic.character.title.api.SetNotice",
        "eve_public.cosmetic.character.title.api.SetAllInBubbleNotice",
        "eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice",
        "eve_public.cosmetic.market.transaction.api.TransactionFailedNotice",
        "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice",
        "eve_public.cosmetic.ship.api.SetNotice",
        "eve_public.cosmetic.ship.api.SetAllInBubbleNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.UpdatedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.FailedNotice",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice",
        "eve_public.tale.api.StartedNotice",
        "eve_public.tale.api.StoppedNotice",
    ];

    for ntype in &notice_types {
        let payload = encode_domain_message(&reg, ntype, |r_reg, resp| {
            set_domain_attributes(r_reg, resp, 90_000_204, 98_000_001, "cosmetic_notice");
        })
        .unwrap_or_else(|e| panic!("Failed to encode notice {ntype}: {e:?}"));

        let (published, count) = ctx
            .quasar_gateway
            .publish_notice(NoticeScope::Broadcast, payload, ctx.now_unix())
            .expect("publish_notice");
        assert_eq!(count, 1, "Broadcast notice delivered to 1 subscriber");
        assert_eq!(published.internal_origin, "evomu.quasar.notice_publisher");

        let recvd = stream_rx.recv().await.expect("recv notice from stream");
        let recvd_payload = recvd.payload.expect("received payload present");
        assert_eq!(recvd_payload.normalized_type_name(), *ntype);
    }

    // Direct domain notice helper verification for cosmetics, SKINR, and tales
    let n1 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_title_granted_notice(
        &ctx.quasar_gateway, 90_000_204, 101, ctx.now_unix(),
    ).expect("title granted notice");
    assert_eq!(n1, 1);
    let r1 = stream_rx.recv().await.expect("recv r1");
    assert!(r1.payload.unwrap().type_url.ends_with("GrantedNotice"));

    let n2 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_title_version_notice(
        &ctx.quasar_gateway, 90_000_204, 24, 1, 0, ctx.now_unix(),
    ).expect("title version notice");
    assert_eq!(n2, 1);
    let r2 = stream_rx.recv().await.expect("recv r2");
    assert!(r2.payload.unwrap().type_url.ends_with("CheckVersionNotice"));

    let n3 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_market_transaction_notice(
        &ctx.quasar_gateway, 90_000_204, 8801, ctx.now_unix(),
    ).expect("market tx notice");
    assert_eq!(n3, 1);
    let r3 = stream_rx.recv().await.expect("recv r3");
    assert!(r3.payload.unwrap().type_url.ends_with("TransactionSucceededNotice"));

    let n4 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_ship_set_notice(
        &ctx.quasar_gateway, 90_000_204, 601, 701, ctx.now_unix(),
    ).expect("ship set notice");
    assert_eq!(n4, 1);
    let r4 = stream_rx.recv().await.expect("recv r4");
    assert!(r4.payload.unwrap().type_url.ends_with("SetNotice"));

    let n5 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_component_license_granted_notice(
        &ctx.quasar_gateway, 90_000_204, 401, ctx.now_unix(),
    ).expect("comp license notice");
    assert_eq!(n5, 1);
    let r5 = stream_rx.recv().await.expect("recv r5");
    assert!(r5.payload.unwrap().type_url.ends_with("GrantedNotice"));

    let n6 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_license_added_notice(
        &ctx.quasar_gateway, 90_000_204, 501, ctx.now_unix(),
    ).expect("license added notice");
    assert_eq!(n6, 1);
    let r6 = stream_rx.recv().await.expect("recv r6");
    assert!(r6.payload.unwrap().type_url.ends_with("AddedNotice"));

    let n7 = eve_server::services::quasar_cosmetics_services::publish_cosmetic_sequencing_job_started_notice(
        &ctx.quasar_gateway, 90_000_204, 1001, ctx.now_unix(),
    ).expect("job started notice");
    assert_eq!(n7, 1);
    let r7 = stream_rx.recv().await.expect("recv r7");
    assert!(r7.payload.unwrap().type_url.ends_with("StartedNotice"));

    let n8 = eve_server::services::quasar_daily_goals::publish_tale_started_notice(
        &ctx.quasar_gateway, 30_000_142, 901, 1001, ctx.now_unix(),
    ).expect("tale started notice");
    assert_eq!(n8, 1);
    let r8 = stream_rx.recv().await.expect("recv r8");
    assert!(r8.payload.unwrap().type_url.ends_with("StartedNotice"));
}

// ============================================================================
// 5. Sovereignty & Goals Notices Fan-out
// ============================================================================

#[tokio::test]
async fn q_final_sovereignty_and_goals_notices() {
    // parity: quasar:eveProto/generated/eve_public/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/goal/api/notices_pb2.pyj").expect("fd").package, "eve_public.goal.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj").expect("fd").package, "eve_public.goal.contribution.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.hub.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.hub.upgrade.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.resource.transfer.workforce.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.mercenaryden.activity.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.mercenaryden.api");
    assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj").expect("fd").package, "eve_public.sovereignty.skyhook.api");

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(40_005, 90_000_205, 98_000_001);

    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let notice_types = [
        "eve_public.goal.api.CreatedNotice",
        "eve_public.goal.api.DeletedNotice",
        "eve_public.goal.api.ClosedNotice",
        "eve_public.goal.api.CompletedNotice",
        "eve_public.goal.api.ProgressedNotice",
        "eve_public.goal.api.NameChangedNotice",
        "eve_public.goal.api.DescriptionChangedNotice",
        "eve_public.goal.contribution.api.ContributedNotice",
        "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice",
        "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice",
        "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice",
        "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
        "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice",
        "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice",
        "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice",
        "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice",
        "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice",
        "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice",
        "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice",
        "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice",
        "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice",
        "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice",
        "eve_public.sovereignty.skyhook.api.ReagentDefinitionsNotice",
        "eve_public.sovereignty.skyhook.api.ReagentConfigurationsNotice",
        "eve_public.sovereignty.skyhook.api.ActivationNotice",
        "eve_public.sovereignty.skyhook.api.WorkforceChangedNotice",
        "eve_public.sovereignty.skyhook.api.AllInSolarSystemNotice",
        "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice",
        "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowStartedNotice",
        "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowEndedNotice",
    ];

    for ntype in &notice_types {
        let payload = encode_domain_message(&reg, ntype, |r_reg, resp| {
            set_domain_attributes(r_reg, resp, 30_000_142, 90_000_205, "sov_notice");
        })
        .unwrap_or_else(|e| panic!("Failed to encode notice {ntype}: {e:?}"));

        let (published, count) = ctx
            .quasar_gateway
            .publish_notice(NoticeScope::Broadcast, payload, ctx.now_unix())
            .expect("publish_notice");
        assert_eq!(count, 1, "Broadcast notice delivered to 1 subscriber");
        assert_eq!(published.internal_origin, "evomu.quasar.notice_publisher");

        let recvd = stream_rx.recv().await.expect("recv notice from stream");
        let recvd_payload = recvd.payload.expect("received payload present");
        assert_eq!(recvd_payload.normalized_type_name(), *ntype);
    }

    // Direct domain notice helper verification for goals and sovereignty
    let g1 = eve_server::services::quasar_daily_goals::publish_goal_created_notice(
        &ctx.quasar_gateway, 90_000_205, 5501, ctx.now_unix(),
    ).expect("goal created notice");
    assert_eq!(g1, 1);
    let gr1 = stream_rx.recv().await.expect("recv gr1");
    assert!(gr1.payload.unwrap().type_url.ends_with("CreatedNotice"));

    let g2 = eve_server::services::quasar_daily_goals::publish_goal_contributed_notice(
        &ctx.quasar_gateway, 90_000_205, 5501, 0, 10, ctx.now_unix(),
    ).expect("goal contributed notice");
    assert_eq!(g2, 1);
    let gr2 = stream_rx.recv().await.expect("recv gr2");
    assert!(gr2.payload.unwrap().type_url.ends_with("ContributedNotice"));

    let s1 = eve_server::services::quasar_sov_services::publish_sov_hub_resources_simulated_notice(
        &ctx.quasar_gateway, 30_000_142, 60_000_001, 5000, 200, ctx.now_unix(),
    ).expect("sov hub resources notice");
    assert_eq!(s1, 1);
    let sr1 = stream_rx.recv().await.expect("recv sr1");
    assert!(sr1.payload.unwrap().type_url.ends_with("ResourcesSimulatedNotice"));

    let s2 = eve_server::services::quasar_sov_services::publish_sov_hub_upgrade_notice(
        &ctx.quasar_gateway, 30_000_142, 60_000_001, ctx.now_unix(),
    ).expect("sov hub upgrade notice");
    assert_eq!(s2, 1);
    let sr2 = stream_rx.recv().await.expect("recv sr2");
    assert!(sr2.payload.unwrap().type_url.ends_with("HubUpgradesConfiguredNotice"));

    let s3 = eve_server::services::quasar_sov_services::publish_sov_hub_workforce_configured_notice(
        &ctx.quasar_gateway, 30_000_142, 60_000_001, ctx.now_unix(),
    ).expect("sov hub workforce notice");
    assert_eq!(s3, 1);
    let sr3 = stream_rx.recv().await.expect("recv sr3");
    assert!(sr3.payload.unwrap().type_url.ends_with("ConfiguredNotice"));

    let s4 = eve_server::services::quasar_sov_services::publish_sov_mercenary_den_activity_notice(
        &ctx.quasar_gateway, 30_000_142, 70_000_001, ctx.now_unix(),
    ).expect("mercenary den activity notice");
    assert_eq!(s4, 1);
    let sr4 = stream_rx.recv().await.expect("recv sr4");
    assert!(sr4.payload.unwrap().type_url.ends_with("SimulatedNotice"));

    let s5 = eve_server::services::quasar_sov_services::publish_sov_mercenary_den_simulation_notice(
        &ctx.quasar_gateway, 30_000_142, 70_000_001, ctx.now_unix(),
    ).expect("mercenary den simulation notice");
    assert_eq!(s5, 1);
    let sr5 = stream_rx.recv().await.expect("recv sr5");
    assert!(sr5.payload.unwrap().type_url.ends_with("SimulationNotice"));

    let s6 = eve_server::services::quasar_sov_services::publish_sov_skyhook_activation_notice(
        &ctx.quasar_gateway, 30_000_142, 40_000_001, 80_000_001, true, ctx.now_unix(),
    ).expect("skyhook activation notice");
    assert_eq!(s6, 1);
    let sr6 = stream_rx.recv().await.expect("recv sr6");
    assert!(sr6.payload.unwrap().type_url.ends_with("ActivationNotice"));
}
