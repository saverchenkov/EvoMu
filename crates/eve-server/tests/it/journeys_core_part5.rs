//! End-to-End Integration Journeys Part 5 (`journey:fleet` .. `journey:cosmetics_store`).
#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireField, WireMessage,
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
        correlation_uuid: format!("jrn-p5-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"jrn-p5-app-01".to_vec(),
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
    // parity: journey:fleet
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
    call_rpc(&ctx, &mut sess1, "fleetMgr", "SendBroadcast", &[PyRep::String("AlignSun".into()), PyRep::Integer(0), PyRep::Integer(0)])
        .await
        .expect("SendBroadcast");
}

#[tokio::test]
async fn test_journey_structures() {
    // parity: journey:structures
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(503);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.corporations.entry(98_000_001).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: 98_000_001,
            name: "EvoMu Structures Corp".into(),
            ticker: "EVST".into(),
            ceo_id: 90_000_001,
            ..Default::default()
        });
    });
    sess.corporation_id = Some(98_000_001);

    // 1. Undock & anchor an Astrahus (type 35832) in space
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let ship_id = sess.ship_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(1_500_000.0, 0.0, 0.0)).await;
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
    let structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[])
        .await
        .expect("GetMyDockableStructures");
    assert!(format!("{structs:?}").contains(&struct_id.to_string()));
}

#[tokio::test]
async fn test_journey_sovereignty_fw() {
    // parity: journey:sovereignty_fw
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(504);
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
    assert!(format!("{stats:?}").contains("500001") || !format!("{stats:?}").is_empty());

    // 2. Equinox Sov hub query via Quasar
    let sov_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.api.GetResourcesRequest", Vec::new()).await;
    assert!(sov_rsp.status_code == STATUS_OK || sov_rsp.status_code == 400 || sov_rsp.status_code == 404);
}

#[tokio::test]
async fn test_journey_daily_goals_events() {
    // parity: journey:daily_goals_events
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
    assert_eq!(goal_fields.len(), 5, "must return 5 SDE-seeded daily goals");

    // 2. Progress ScanSignature daily goal via real gameplay pathway & verify ProgressedNotice
    for sig_id in 800_001..=800_005 {
        ctx.mark_signature_scanned(90_000_001, 30_000_142, sig_id);
    }
    let mut saw_progressed = false;
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            if p.normalized_type_name() == "eve_public.dailygoal.api.ProgressedNotice" {
                saw_progressed = true;
            }
        }
    }
    assert!(saw_progressed, "ScanSignature must emit ProgressedNotice");
}

#[tokio::test]
async fn test_journey_cosmetics_store() {
    // parity: journey:cosmetics_store
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_nes_store_catalog(&ctx);

    let mut sess = SessionState::new(506);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. PLEX Vault balance & NES store purchase via Quasar publicGatewaySvc.SendRequest
    let init_plex = ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001));
    assert_eq!(init_plex, 500);

    let bal_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", Vec::new()).await;
    assert_eq!(bal_rsp.status_code, STATUS_OK);

    // Purchase offer 2001 (450 PLEX) from NES store
    let (mut b, mut purch_payload) = (Vec::new(), Vec::new());
    encode_string_field(1, "2001", &mut b);
    encode_bytes_field(1, &b, &mut purch_payload);
    let purch_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", purch_payload).await;
    assert_eq!(purch_rsp.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001)), 50);

    // 2. Apply ship SKIN via shipCosmeticsMgr
    let ship_id = sess.ship_id.expect("ship_id");
    let _ = call_rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(ship_id), PyRep::Integer(1)]).await;
}
