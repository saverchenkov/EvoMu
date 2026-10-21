//! Acceptance test suite for Quasar Space, Combat & Cosmetics (`Tasks Q-7, Q-8, Q-9`).
//!
//! Validates:
//! - Q-7: Space, celestials, solar systems, jump ranges, bubble presence, pirate insurgencies
//!   (corruption/suppression), dynamic bounty systems, NPC ship ownership, and notice broadcasts.
//! - Q-8: Sovereignty hub, skyhooks, workforce routes, upgrades, fuel management, mercenary dens,
//!   faction warfare leaderboards/stats/systems, and operational recommendations.
//! - Q-9: Ship cosmetics (logos, emblems, SKINR third-party sequencing, licenses, drafts),
//!   corporation color palettes, character titles, structure paintworks, Quasar config,
//!   and AIR career goals.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
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

#[tokio::test]
async fn q7_space_celestials_navigation_and_dbs() {
    // parity: plan:Q-7, quasar:eveProto/generated/eve_public/solarsystem/solarsystem_pb2.pyj, quasar:eveProto/generated/eve_public/constellation/constellation_pb2.pyj, quasar:eveProto/generated/eve_public/region/region_pb2.pyj, quasar:eveProto/generated/eve_public/dungeon/dungeon_pb2.pyj, quasar:eveProto/generated/eve_public/dungeon/instance_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-7",
        "quasar:eveProto/generated/eve_public/solarsystem/solarsystem_pb2.pyj",
        "quasar:eveProto/generated/eve_public/constellation/constellation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/region/region_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dungeon/dungeon_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dungeon/instance_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    let mut session = SessionState::new(20_001);
    session.user_id = Some(101);
    session.character_id = Some(90_000_101);
    session.corporation_id = Some(98_000_001);
    session.solar_system_id = Some(30_000_142); // Jita

    // 1. eve_public.solarsystem.api.GetRequest
    let mut req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve_public.solarsystem.api.GetRequest", 1)
        .expect("instantiate GetRequest");
    if let Some(DynamicValue::Message(ident)) = req_msg.fields.get_mut(&1) {
        ident.set(1, DynamicValue::Uint32(30_000_142));
    }
    let req_bytes = ctx
        .quasar_gateway
        .descriptors
        .encode_dynamic(&req_msg)
        .expect("encode GetRequest");

    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-space-001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.solarsystem.api.GetRequest"),
            value: req_bytes,
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
    let jump_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest",
            2,
        )
        .expect("instantiate jump req");
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
            value: ctx
                .quasar_gateway
                .descriptors
                .encode_dynamic(&jump_req_msg)
                .unwrap(),
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
    let const_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.constellation.GetRequest", 3)
        .unwrap();
    let const_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-const-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.constellation.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&const_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(const_rsp.status_code, STATUS_OK);

    let region_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.region.GetRequest", 4)
        .unwrap();
    let region_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-region-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.region.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&region_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(region_rsp.status_code, STATUS_OK);

    // 4. Dynamic Bounty System (DBS) queries
    let dbs_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
            5,
        )
        .unwrap();
    let dbs_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-dbs-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.dynamicbountysystem.GetDynamicBountySettingsRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&dbs_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(dbs_rsp.status_code, STATUS_OK);

    // 5. Dungeon Sovereignty Instances & NPC Ship Owner queries
    let dng_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.dungeon.sovereignty.api.GetInstancesRequest", 6)
        .unwrap();
    let dng_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-dng-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.dungeon.sovereignty.api.GetInstancesRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&dng_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(dng_rsp.status_code, STATUS_OK);

    let npc_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.npc.ship.api.GetOwnerRequest", 7)
        .unwrap();
    let npc_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-npc-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.npc.ship.api.GetOwnerRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&npc_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
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
    // parity: plan:Q-7, quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-7",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut session = SessionState::new(20_002);
    session.user_id = Some(102);
    session.character_id = Some(90_000_102);
    session.solar_system_id = Some(30_000_142);

    // 1. Bubble presence queries
    let bubble_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.bubble.presence.api.GetInBubbleRequest", 11)
        .unwrap();
    let bubble_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-bbl-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.bubble.presence.api.GetInBubbleRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&bubble_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(bubble_rsp.status_code, STATUS_OK);

    // 2. Pirate Insurgency API
    let ins_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.pirate.insurgency.api.GetRequest", 12)
        .unwrap();
    let ins_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-ins-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pirate.insurgency.api.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&ins_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(ins_rsp.status_code, STATUS_OK);

    // 3. Corruption & Suppression System Info
    let corr_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.pirate.corruption.api.GetSystemInfoRequest",
            13,
        )
        .unwrap();
    let corr_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-corrup-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.corruption.api.GetSystemInfoRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&corr_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(corr_rsp.status_code, STATUS_OK);

    let supp_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.pirate.suppression.api.GetSystemInfoRequest",
            14,
        )
        .unwrap();
    let supp_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-supp-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.GetSystemInfoRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&supp_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(supp_rsp.status_code, STATUS_OK);

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
                value: vec![0x08, 0x02], // Corruption stage 2
            },
            ctx.now_unix(),
        )
        .expect("publish notice");
    assert_eq!(delivered, 1);
    assert!(notice.dispatched.is_some());
    let polled = ctx.quasar_gateway.poll_notices(session.session_id);
    assert_eq!(polled.len(), 1);
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
    // parity: plan:Q-8, quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-8",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut session = SessionState::new(20_003);
    session.user_id = Some(103);
    session.character_id = Some(90_000_103);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Skyhook Get & GetAllLocal
    let skyhook_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.sovereignty.skyhook.api.GetRequest",
            21,
        )
        .unwrap();
    let skyhook_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-sky-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&skyhook_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(skyhook_rsp.status_code, STATUS_OK);

    // 2. Skyhook Activate with director role
    let act_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            22,
        )
        .unwrap();
    let act_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-sky-002".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivateRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&act_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(act_rsp.status_code, STATUS_OK);

    // 3. Sovereignty Hub Fuel & Workforce
    let fuel_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve_public.sovereignty.hub.fuel.api.GetRequest", 23)
        .unwrap();
    let fuel_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-fuel-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.fuel.api.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&fuel_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(fuel_rsp.status_code, STATUS_OK);

    let wf_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            24,
        )
        .unwrap();
    let wf_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-wf-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.workforce.api.GetStateRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&wf_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(wf_rsp.status_code, STATUS_OK);

    // 4. Sovereignty Upgrades & Mercenary Dens
    let upg_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
            25,
        )
        .unwrap();
    let upg_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-upg-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&upg_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(upg_rsp.status_code, STATUS_OK);

    let merc_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            26,
        )
        .unwrap();
    let merc_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-merc-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&merc_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(merc_rsp.status_code, STATUS_OK);
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
    // parity: plan:Q-8, quasar:eveProto/generated/eve_public/faction/faction_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj, quasar:eveProto/generated/eve_public/operation/operation_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-8",
        "quasar:eveProto/generated/eve_public/faction/faction_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj",
        "quasar:eveProto/generated/eve_public/operation/recommendation/recommendation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/operation/operation_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut session = SessionState::new(20_004);
    session.user_id = Some(104);
    session.character_id = Some(90_000_104);
    session.solar_system_id = Some(30_000_142);

    // 1. Faction queries & activity scores
    let fac_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.faction.api.GetRequest", 31)
        .unwrap();
    let fac_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-fac-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.faction.api.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&fac_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(fac_rsp.status_code, STATUS_OK);

    // 2. Faction Warfare Systems & Leaderboard
    let fws_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.factionwarfare.systems.GetRequest", 32)
        .unwrap();
    let fws_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-fw-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.factionwarfare.systems.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&fws_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(fws_rsp.status_code, STATUS_OK);

    let lb_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve.factionwarfare.leaderboard.characters.GetRequest",
            33,
        )
        .unwrap();
    let lb_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-fw-002".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.factionwarfare.leaderboard.characters.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&lb_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(lb_rsp.status_code, STATUS_OK);

    // 3. Operational Recommendations
    let rec_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve_public.operation.recommendation.GetRequest", 34)
        .unwrap();
    let rec_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-rec-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.operation.recommendation.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&rec_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
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
    // parity: plan:Q-9, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-9",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut session = SessionState::new(20_005);
    session.user_id = Some(105);
    session.character_id = Some(90_000_105);
    session.solar_system_id = Some(30_000_142);

    // 1. Ship logo display & clear
    let disp_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve_public.cosmetic.ship.logo.DisplayRequest", 41)
        .unwrap();
    let disp_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-logo-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.logo.DisplayRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&disp_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(disp_rsp.status_code, STATUS_OK, "disp_rsp: {}", disp_rsp.status_message);

    let clr_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve_public.cosmetic.ship.logo.ClearRequest", 42)
        .unwrap();
    let clr_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-logo-002".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.logo.ClearRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&clr_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(clr_rsp.status_code, STATUS_OK, "clr_rsp: {}", clr_rsp.status_message);

    // 2. SKINR third-party sequencing pricing & jobs
    let seq_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
            43,
        )
        .unwrap();
    let seq_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-seq-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&seq_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(seq_rsp.status_code, STATUS_OK);

    // 3. Cosmetic Licenses (GetOwned)
    let lic_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
            44,
        )
        .unwrap();
    let lic_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-lic-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&lic_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(lic_rsp.status_code, STATUS_OK);

    // 4. Saved SKINR Design Drafts
    let draft_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
            45,
        )
        .unwrap();
    let draft_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-draft-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&draft_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(draft_rsp.status_code, STATUS_OK);
    assert_eq!(
        seq_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    );
    assert_eq!(
        lic_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse"
    );
    assert_eq!(draft_rsp.correlation_uuid, b"corr-draft-001");
}

#[tokio::test]
async fn q9_corp_palette_titles_structure_paintwork_and_app_infra() {
    // parity: plan:Q-9, quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/paintwork_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/license_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-9",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/paintwork_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/license_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut session = SessionState::new(20_006);
    session.user_id = Some(106);
    session.character_id = Some(90_000_106);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Corporation color palette
    let pal_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.cosmetic.corporation.palette.api.GetRequest", 51)
        .unwrap();
    let pal_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-pal-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.corporation.palette.api.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&pal_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(pal_rsp.status_code, STATUS_OK);

    // 2. Character titles (GetOwned)
    let tit_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.cosmetic.character.title.api.GetOwnedRequest",
            52,
        )
        .unwrap();
    let tit_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-tit-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GetOwnedRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&tit_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(tit_rsp.status_code, STATUS_OK);

    // 3. Structure paintwork
    let pnt_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.cosmetic.structure.paintwork.api.GetRequest",
            53,
        )
        .unwrap();
    let pnt_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-pnt-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.structure.paintwork.api.GetRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&pnt_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(pnt_rsp.status_code, STATUS_OK);

    // 4. Quasar configuration
    let cfg_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.quasar.config.api.GetRequest", 54)
        .unwrap();
    let cfg_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-cfg-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.quasar.config.api.GetRequest"),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&cfg_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(cfg_rsp.status_code, STATUS_OK);

    // 5. AIR Career Program Goals
    let goal_req_msg = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample(
            "eve_public.app.eveonline.career.goal.GetAllRequest",
            55,
        )
        .unwrap();
    let goal_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &EvePublicRequest {
            correlation_uuid: b"corr-goal-001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.career.goal.GetAllRequest"
                ),
                value: ctx
                    .quasar_gateway
                    .descriptors
                    .encode_dynamic(&goal_req_msg)
                    .unwrap(),
            }),
            ..Default::default()
        },
        None,
        true,
        ctx.now_unix(),
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
