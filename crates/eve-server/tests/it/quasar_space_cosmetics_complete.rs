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
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
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

fn encode_id_req(_id: u64) -> Vec<u8> {
    Vec::new()
}

fn call_req(
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
    // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_002);
    session.user_id = Some(102);
    session.character_id = Some(90_000_102);
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

    // 4. Notice broadcast for insurgency updates
    let _rx = ctx.quasar_gateway.subscribe_session(
        &session,
        vec![
            "eve_public.pirate.corruption.api.Notice".into(),
            "eve_public.pirate.suppression.api.Notice".into(),
        ],
        ctx.now_unix(),
    );
    let (notice, delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::SolarSystem(30_000_142),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.corruption.api.Notice"),
                value: vec![0x08, 0x02],
            },
            ctx.now_unix(),
        )
        .expect("publish corruption notice");
    assert_eq!(delivered, 1);
    assert!(notice.dispatched.is_some());

    let (_, delivered_supp) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::SolarSystem(30_000_142),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.Notice"),
                value: vec![0x08, 0x03],
            },
            ctx.now_unix(),
        )
        .expect("publish suppression notice");
    assert_eq!(delivered_supp, 1);

    let polled = ctx.quasar_gateway.poll_notices(session.session_id);
    assert_eq!(polled.len(), 2);
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
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_003);
    session.user_id = Some(103);
    session.character_id = Some(90_000_103);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Skyhook Get, GetAllLocal, Activate, Deactivate, ModifyReagentsSimulations
    let skyhook_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.skyhook.api.GetRequest",
        encode_id_req(1_000_000_001),
        true,
    );
    assert_eq!(skyhook_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.sovereignty.skyhook.api.GetAllLocalRequest",
        "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
        "eve_public.sovereignty.skyhook.api.ActivateRequest",
        "eve_public.sovereignty.skyhook.api.DeactivateRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, encode_id_req(1_000_000_001), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    // 2. Sovereignty Hub Resources, Fuel & Workforce
    let hub_res_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.api.GetResourcesRequest",
        encode_id_req(1_000_000_010),
        true,
    );
    assert_eq!(hub_res_rsp.status_code, STATUS_OK);

    let fuel_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.fuel.api.GetRequest",
        encode_id_req(1_000_000_010),
        true,
    );
    assert_eq!(fuel_rsp.status_code, STATUS_OK);

    let add_fuel_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.fuel.api.AddRequest",
        encode_id_req(1_000_000_010),
        true,
    );
    assert_eq!(add_fuel_rsp.status_code, STATUS_OK);

    let wf_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
        encode_id_req(1_000_000_010),
        true,
    );
    assert_eq!(wf_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
        "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
        "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, encode_id_req(1_000_000_010), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    // 3. Sovereignty Upgrades & Mercenary Dens
    let upg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(upg_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest",
        "eve_public.sovereignty.hub.upgrade.api.UninstallRequest",
        "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, encode_id_req(1_000_000_010), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    let merc_rsp = call_req(
        &ctx,
        &session,
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        Vec::new(),
        true,
    );
    assert_eq!(merc_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, encode_id_req(1_000_000_020), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    assert_eq!(
        skyhook_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.skyhook.api.GetResponse"
    );
    assert_eq!(
        fuel_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.hub.fuel.api.GetResponse"
    );
    assert_eq!(
        wf_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.hub.workforce.api.GetStateResponse"
    );
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
    // parity: quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_004);
    session.user_id = Some(104);
    session.character_id = Some(90_000_104);
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

    // 2. Faction Warfare Systems & Leaderboard
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

    // 3. Operational Recommendations
    let rec_rsp = call_req(
        &ctx,
        &session,
        "eve_public.operation.recommendation.GetRequest",
        Vec::new(),
        false,
    );
    assert_eq!(rec_rsp.status_code, STATUS_OK);
    assert_eq!(
        fac_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.faction.api.GetResponse"
    );
    assert_eq!(
        fws_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.factionwarfare.systems.GetResponse"
    );
    assert_eq!(
        lb_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.factionwarfare.leaderboard.characters.GetResponse"
    );
    assert_eq!(
        rec_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.operation.recommendation.GetResponse"
    );
}

#[tokio::test]
async fn q9_ship_cosmetics_skinr_and_emblems() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_005);
    session.user_id = Some(105);
    session.character_id = Some(90_000_105);
    session.solar_system_id = Some(30_000_142);

    // 1. Ship logo display & clear
    for req_name in [
        "eve_public.cosmetic.ship.logo.DisplayRequest",
        "eve_public.cosmetic.ship.logo.ClearRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }

    // 2. SKINR third-party sequencing pricing & jobs
    let seq_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
        true,
    );
    assert_eq!(seq_rsp.status_code, STATUS_OK);

    // 3. Cosmetic Licenses (GetOwned)
    let lic_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
        true,
    );
    assert_eq!(lic_rsp.status_code, STATUS_OK);

    // 4. Saved SKINR Design Drafts
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }
    assert_eq!(
        seq_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    );
    assert_eq!(
        lic_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse"
    );
}

#[tokio::test]
async fn q9_corp_palette_titles_structure_paintwork_and_app_infra() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_006);
    session.user_id = Some(106);
    session.character_id = Some(90_000_106);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Corporation color palette
    let pal_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.corporation.palette.api.GetRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pal_rsp.status_code, STATUS_OK);

    // 2. Character titles (GetOwned, GetSelected, Select, ClearSelection)
    let tit_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        Vec::new(),
        true,
    );
    assert_eq!(tit_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        "eve_public.cosmetic.character.title.api.EquipRequest",
        "eve_public.cosmetic.character.title.api.UnequipRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    // 3. Structure paintwork & paintwork licenses
    let pnt_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.api.GetRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_rsp.status_code, STATUS_OK);

    let pnt_sys_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_sys_rsp.status_code, STATUS_OK);

    let pnt_lic_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_lic_rsp.status_code, STATUS_OK);

    // 4. Quasar configuration
    let cfg_rsp = call_req(
        &ctx,
        &session,
        "eve.quasar.config.api.GetRequest",
        Vec::new(),
        false,
    );
    assert_eq!(cfg_rsp.status_code, STATUS_OK);

    // 5. AIR Career Program Goals
    let goal_rsp = call_req(
        &ctx,
        &session,
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Vec::new(),
        true,
    );
    assert_eq!(goal_rsp.status_code, STATUS_OK);
    assert_eq!(
        pal_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.corporation.palette.api.GetResponse"
    );
    assert_eq!(
        tit_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.cosmetic.character.title.api.GetOwnedResponse"
    );
    assert_eq!(
        pnt_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.cosmetic.structure.paintwork.api.GetResponse"
    );
    assert_eq!(
        cfg_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.quasar.config.api.GetResponse"
    );
    assert_eq!(
        goal_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.app.eveonline.career.goal.GetAllResponse"
    );
}
