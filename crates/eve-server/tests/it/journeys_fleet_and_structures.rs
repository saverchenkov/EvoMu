//! End-to-End Integration Journeys: Fleet, Structures, Sovereignty/FW, and Daily Goals (Journeys 21..=24).
#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireField, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal with response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("jrn-fs-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"jrn-fs-app-01".to_vec(),
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
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_journey_fleet() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess1 = SessionState::new(501);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID 1");

    let mut sess2 = SessionState::new(502);
    crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
        .await
        .expect("SelectCharacterID 2");

    // 1. Form a fleet, invite & accept second player, set MOTD & broadcast
    let fleet = call_rpc(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = fleet.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or(1);
    call_rpc(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(90_000_002), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)])
        .await
        .expect("fleetMgr.Invite");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
    call_rpc(&ctx, &mut sess2, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fleet_id)])
        .await
        .expect("fleetMgr.AcceptInvite");
    let fleet_bind = call_rpc(&ctx, &mut sess1, "fleetObjectHandler", "MachoBindObject", &[PyRep::Integer(fleet_id), PyRep::None])
        .await
        .expect("fleetObjectHandler bind");
    let fleet_bid = match fleet_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", Some(fleet_bid), "SetMotdEx", &[PyRep::String("Fleet Ops Ready".into())], &HashMap::new(), None)
        .await
        .expect("SetMotdEx");
    let fleet_bind2 = call_rpc(&ctx, &mut sess2, "fleetObjectHandler", "MachoBindObject", &[PyRep::Integer(fleet_id), PyRep::None])
        .await
        .expect("fleetObjectHandler bind 2");
    let fleet_bid2 = match fleet_bind2 { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
    let motd = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", Some(fleet_bid2), "GetMotd", &[], &HashMap::new(), None)
        .await
        .expect("GetMotd");
    assert_eq!(motd.as_str(), Some("Fleet Ops Ready"));

    // 2. Broadcast & fleet warp
    call_rpc(&ctx, &mut sess1, "ship", "Undock", &[]).await.expect("ship.Undock 1");
    call_rpc(&ctx, &mut sess2, "ship", "Undock", &[]).await.expect("ship.Undock 2");
    let mut rx_bcast = ctx.notify_tx.subscribe();
    call_rpc(&ctx, &mut sess1, "fleetMgr", "SendBroadcast", &[PyRep::String("AlignSun".into()), PyRep::Integer(0), PyRep::Integer(0)])
        .await
        .expect("SendBroadcast");
    let mut saw_bcast = false;
    for _ in 0..32 {
        let Ok(env) = rx_bcast.try_recv() else { break };
        let dbg = format!("{env:?}");
        if dbg.contains("OnFleetBroadcast") && dbg.contains("AlignSun") {
            saw_bcast = true;
            break;
        }
    }
    assert!(saw_bcast, "SendBroadcast must emit OnFleetBroadcast with AlignSun");
}

#[tokio::test]
async fn test_journey_structures() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(503);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let created = call_rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("EvoMu Structures Corp".into()), PyRep::String("EVST".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    assert_eq!(sess.corporation_id, Some(corp_id));
    ctx.advance_secs(8 * 86_400);

    // 1. Undock, jump to Perimeter (30_000_144), & anchor an Astrahus (type 35832) in space
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let ship_id = sess.ship_id.unwrap();
    let PyRep::Tuple(b_tup) = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap() else { panic!("beyonce bind") };
    let bey_bid = b_tup[0].as_i64().unwrap() as u64;
    let sg = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == 30_000_142 && g.to_solar_system_id == 30_000_144).unwrap();
    let _ = ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship_id, sg.position).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_bid), "CmdStargateJump", &[PyRep::Integer(sg.stargate_id as i64)], &HashMap::new(), None).await.expect("CmdStargateJump to Perimeter");
    let sys_h = ctx.systems.get_or_spawn(30_000_144);
    let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(0.0, 5.0e9, 0.0)).await;
    let cit_item = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 35832, // Astrahus
                owner_id: sess.corporation_id.unwrap_or(1_000_035),
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    });

    let anchored = call_rpc(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(cit_item), PyRep::String("EvoMu Citadel".into()), PyRep::Integer(1)],
    )
    .await
    .expect("structureDeployment.Anchor");
    let struct_id = anchored.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(cit_item);
    let corp_structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyCorporationStructures", &[])
        .await
        .expect("GetMyCorporationStructures");
    assert!(format!("{corp_structs:?}").contains(&struct_id.to_string()));

    // 2. Complete anchoring, dock, insert Quantum Core (56201 -> flag 172), undock, and unanchor
    let core_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 56_201, 1).unwrap().item_id;
    ctx.advance_secs(25 * 3_600);
    let structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[])
        .await
        .expect("GetMyDockableStructures");
    assert!(format!("{structs:?}").contains(&struct_id.to_string()));
    let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(60_000.0, 5.0e9, 0.0)).await;
    call_rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
        .await
        .expect("structureDocking.Dock");
    assert_eq!(sess.structure_id, Some(struct_id));
    call_rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(struct_id), PyRep::Integer(172)])
        .await
        .expect("Insert Quantum Core");
    ctx.advance_secs(12);
    call_rpc(&ctx, &mut sess, "structureDocking", "Undock", &[])
        .await
        .expect("structureDocking.Undock");
    assert!(sess.structure_id.is_none());
    ctx.advance_secs(1_800);
    let _ = call_rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[]).await;
    let mut rx_unanchor = ctx.notify_tx.subscribe();
    call_rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)])
        .await
        .expect("structureDeployment.Unanchor");
    let mut saw_unanchoring = false;
    for _ in 0..32 {
        let Ok(env) = rx_unanchor.try_recv() else { break };
        let dbg = format!("{env:?}");
        if dbg.contains("OnStructureStateChanged") && dbg.contains("Unanchoring") {
            saw_unanchoring = true;
            break;
        }
    }
    assert!(saw_unanchoring, "Unanchor must emit OnStructureStateChanged(Unanchoring)");
}

#[tokio::test]
async fn test_journey_sovereignty_fw() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(504);
    ctx.db
        .set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT)
        .expect("set_account_role");
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Enlist in Faction Warfare (Caldari State = 500001) & check stats
    call_rpc(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
        .await
        .expect("JoinFactionAsCharacter");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_001].war_faction_id), Some(500_001));
    let stats = call_rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[])
        .await
        .expect("GetStats_Character");
    let PyRep::Dict(ref stat_entries) = stats else { panic!("expected Dict from GetStats_Character") };
    assert_eq!(stat_entries.len(), 6);

    // 2. Anchor a Sovereignty Hub & query resources via Quasar
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let created = call_rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("EvoMu Sov Corp".into()), PyRep::String("EVSV".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.set_division_balance(1, eve_db::IskCents::from_isk(2_000_000_000));
            c.set_division_balance(1, eve_db::IskCents::from_isk(2_000_000_000));
        }
    });
    call_rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "CreateAlliance",
        &[PyRep::String("EvoMu Sov Alliance".into()), PyRep::String("EVSA".into()), PyRep::String("Sov".into()), PyRep::String("https://evomu.local".into())],
    )
    .await
    .expect("CreateAlliance");
    let nullsec_sys = ctx
        .sde
        .solar_systems
        .keys().copied()
        .filter(|&id| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, id))
        .min()
        .unwrap_or(30_004_759);
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/tr {nullsec_sys}"))])
        .await
        .expect("/tr nullsec");
    let ship_id = sess.ship_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(nullsec_sys);
    let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(2_500_000.0, 0.0, 0.0)).await;
    let hub_item = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 32458, // Sovereignty Hub
                owner_id: corp_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    });
    let anchored = call_rpc(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(hub_item), PyRep::String("EvoMu Sov Hub".into()), PyRep::Integer(1)],
    )
    .await
    .expect("structureDeployment.Anchor Sov Hub");
    let hub_id = anchored.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(hub_item);
    let mut hub_msg = Vec::new();
    encode_varint_field(1, hub_id as u64, &mut hub_msg);
    let mut sov_req = Vec::new();
    encode_bytes_field(1, &hub_msg, &mut sov_req);
    let sov_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.api.GetResourcesRequest", sov_req).await;
    assert_eq!(sov_rsp.status_code, STATUS_OK);
    let sov_wm = WireMessage::decode(&sov_rsp.payload.unwrap().value).expect("decode GetResourcesResponse");
    assert!(sov_wm.fields.iter().any(|f| f.field_number == 1), "GetResourcesResponse must include power field");
}

#[tokio::test]
async fn test_journey_daily_goals_events() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(505);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Subscribe to publicGatewaySvc notices & fetch all current daily goals via Quasar
    call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[])
        .await
        .expect("publicGatewaySvc.Subscribe");
    let mut rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream receiver");

    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllCurrentResponse");
    let goal_fields: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(goal_fields.len(), 5, "must return 5 SDE-seeded daily goals (4 rotating daily + 1 daily bonus)");

    // 2. Advance 1 day so ScanSignature is in today's rotating 4 goals & verify ProgressedNotice
    ctx.advance_virtual_clock(86_400);
    for sig_id in 800_001..=800_005 {
        ctx.mark_signature_scanned(90_000_001, 30_000_142, sig_id);
    }
    let mut saw_progressed = false;
    for _ in 0..64 {
        let Ok(n) = rx.try_recv() else { break };
        if let Some(p) = n.payload {
            if p.normalized_type_name() == "eve_public.dailygoal.api.ProgressedNotice" {
                saw_progressed = true;
            }
        }
    }
    assert!(saw_progressed, "ScanSignature must emit ProgressedNotice");
}
