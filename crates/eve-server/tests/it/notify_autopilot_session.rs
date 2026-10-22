//! Autopilot, Session Change, Destiny Updates, SimClock Rebase, and Mission Change
//! notification emission tests.

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, NotificationTarget, ServerContext};
use glam::DVec3;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;
const SYS_PERIMETER: u32 = 30_000_144;
const GATE_JITA_TO_PERIMETER: i64 = 50_001_249;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs
        .iter()
        .filter(|n| n.event == event)
        .map(|n| n.args.clone())
        .collect()
}

#[tokio::test]
async fn test_autopilot_and_session_notifications() {
    // parity: notify:DoSessionChanging, notify:DoDestinyUpdates, notify:DoSimClockRebase, notify:OnAutoPilotJump
    eve_testkit::attest(&[
        "notify:DoSessionChanging",
        "notify:DoDestinyUpdates",
        "notify:DoSimClockRebase",
        "notify:OnAutoPilotJump",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 810, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 811, "other", "other-secret", P2).await;

    // Colocate P1 and P2 in Jita
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.solar_system_id = SYS_JITA;
            c.station_id = Some(STATION_JITA);
        }
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.solar_system_id = SYS_JITA;
            c.station_id = Some(STATION_JITA);
        }
    });
    s1.solar_system_id = Some(SYS_JITA);
    s1.station_id = Some(STATION_JITA);

    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

    // 1. DoSimClockRebase: via wallet_serverops::set_system_tidi_factor
    eve_server::services::wallet_serverops::set_system_tidi_factor(&ctx, SYS_JITA, 0.75);
    let n1_tidi = ctx.drain_notifications_for_char(P1);
    let n2_tidi = ctx.drain_notifications_for_char(P2);
    let tidi_ev1 = events_from(&n1_tidi, "DoSimClockRebase");
    let tidi_ev2 = events_from(&n2_tidi, "DoSimClockRebase");
    assert_eq!(tidi_ev1.len(), 1, "P1 must receive DoSimClockRebase");
    assert_eq!(tidi_ev2.len(), 1, "P2 must receive DoSimClockRebase");
    assert!(matches!(&tidi_ev1[0][0], PyRep::Tuple(t) if t.len() == 2), "Arg 0 must be blue_ticks tuple");
    assert_eq!(tidi_ev1[0][1], PyRep::Real(0.75), "Arg 1 must be tidi factor");

    // 2. Undock P1: triggers DoSessionChanging via record_session_change_timer
    call(&ctx, &mut s1, "beyonce", "Undock", &[]).await;
    let n1_undock = ctx.drain_notifications_for_char(P1);
    let session_ev = events_from(&n1_undock, "DoSessionChanging");
    assert_eq!(session_ev.len(), 1, "Undock must emit DoSessionChanging");
    assert_eq!(session_ev[0][0], PyRep::Bool(true), "Arg 0 isRemote must be true");
    assert_eq!(session_ev[0][1], PyRep::None, "Arg 1 session must be None");
    let PyRep::Dict(change_entries) = &session_ev[0][2] else {
        panic!("Arg 2 change must be Dict");
    };
    let has_sys = change_entries.iter().any(|(k, v)| {
        k.as_str() == Some("solarsystemid")
            && matches!(v, PyRep::Tuple(t) if t.len() == 2 && t[0].as_i64() == Some(SYS_JITA as i64))
    });
    assert!(has_sys, "Change dict must contain solarsystemid tuple");

    // 3. DoDestinyUpdates: via beyonce.UpdateStateRequest
    let beyonce_id = ctx
        .bind_object(&mut s1, "beyonce", SYS_JITA as i64)
        .expect("bind beyonce");
    ctx.dispatch_rpc(
        &mut s1,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateStateRequest");
    let n1_destiny = ctx.drain_notifications_for_char(P1);
    let destiny_ev = events_from(&n1_destiny, "DoDestinyUpdates");
    assert_eq!(destiny_ev.len(), 1, "UpdateStateRequest must emit DoDestinyUpdates");
    let PyRep::List(batch) = &destiny_ev[0][0] else {
        panic!("DoDestinyUpdates arg 0 must be List batch");
    };
    assert!(!batch.is_empty(), "Batch must not be empty");
    let PyRep::Tuple(first_update) = &batch[0] else {
        panic!("Batch item must be Tuple");
    };
    assert_eq!(first_update[1], PyRep::Bool(false), "waitForBubble must be false");

    // Also test helper notify_destiny_updates directly
    ctx.notify_destiny_updates(NotificationTarget::Character(P1), vec![PyRep::Integer(999)]);
    let n1_helper = ctx.drain_notifications_for_char(P1);
    let helper_ev = events_from(&n1_helper, "DoDestinyUpdates");
    assert_eq!(helper_ev.len(), 1, "notify_destiny_updates must emit DoDestinyUpdates");

    // 4. OnAutoPilotJump: Set waypoint to Perimeter and execute stargate jump
    let ship_id = s1.ship_id.expect("P1 active ship");
    ctx.esi_state.update_autopilot_waypoint(P1, SYS_PERIMETER as i64, false, false);
    assert_eq!(ctx.esi_state.get_autopilot_waypoints(P1), vec![SYS_PERIMETER as i64]);

    // Position ship next to Perimeter gate in Jita
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let (_, gate_ball) = jita_handle
        .get_ball_snapshot(GATE_JITA_TO_PERIMETER)
        .await
        .unwrap()
        .expect("Perimeter gate ball");
    let _ = jita_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: gate_ball.position + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Step 10 ticks to expire the 10s undock session change timer
    for _ in 0..10 {
        let _ = jita_handle.step_manual(1.0).await.expect("step_manual");
    }

    ctx.drain_notifications_for_char(P1);

    // Jump gate with active waypoint
    ctx.dispatch_rpc(
        &mut s1,
        "beyonce",
        Some(beyonce_id),
        "CmdStargateJump",
        &[PyRep::Integer(GATE_JITA_TO_PERIMETER)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStargateJump");

    let n1_jump = ctx.drain_notifications_for_char(P1);
    let ap_jump_ev = events_from(&n1_jump, "OnAutoPilotJump");
    assert_eq!(ap_jump_ev.len(), 1, "CmdStargateJump with waypoint must emit OnAutoPilotJump");
    assert!(ap_jump_ev[0].is_empty(), "OnAutoPilotJump must have empty args");

    // Autopilot route was advanced
    let ap_up_ev = events_from(&n1_jump, "OnAutopilotUpdated");
    assert_eq!(ap_up_ev.len(), 1, "Advancing route on jump must emit OnAutopilotUpdated");
    assert!(ctx.esi_state.get_autopilot_waypoints(P1).is_empty(), "Waypoint consumed");
}

#[tokio::test]
async fn test_autopilot_route_and_mission_notifications() {
    // parity: notify:OnAutopilotUpdated, notify:OnAgentMissionChange
    eve_testkit::attest(&[
        "notify:OnAutopilotUpdated",
        "notify:OnAgentMissionChange",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 812, "demo", "demo-secret", P1).await;

    ctx.drain_notifications_for_char(P1);

    // 1. OnAutopilotUpdated: manual route notification
    ctx.notify(
        NotificationTarget::Character(P1),
        "OnAutopilotUpdated",
        "charid",
        vec![],
    );
    let n_route = ctx.drain_notifications_for_char(P1);
    let route_ev = events_from(&n_route, "OnAutopilotUpdated");
    assert_eq!(route_ev.len(), 1, "OnAutopilotUpdated must be delivered");
    assert!(route_ev[0].is_empty(), "OnAutopilotUpdated args must be empty");

    // 2. OnAgentMissionChange: lifecycle events (offered, offer_accepted, completed, quit)
    // Locate an SDE Level 1 distribution agent for deterministic testing
    let (dist_agent_id, dist_corp_id, dist_station_id, dist_sys_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| {
            (
                a.agent_id,
                a.corporation_id,
                a.station_id.unwrap_or(STATION_JITA),
                a.solar_system_id.unwrap_or(SYS_JITA),
            )
        })
        .expect("SDE distribution agent");

    // Colocate P1 with agent
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
            ch.standings.insert(dist_corp_id, 3.0);
        }
    });
    s1.solar_system_id = Some(dist_sys_id);
    s1.station_id = Some(dist_station_id);

    ctx.drain_notifications_for_char(P1);

    // A. Action 1: Offer mission -> OnAgentMissionChange("offered", agent_id, None)
    let m_offer = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let mission_id = m_offer.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    let n_offer = ctx.drain_notifications_for_char(P1);
    let ev_offered = events_from(&n_offer, "OnAgentMissionChange");
    assert_eq!(
        ev_offered,
        vec![vec![PyRep::String("offered".into()), PyRep::Integer(dist_agent_id), PyRep::None]],
        "Action 1 must emit OnAgentMissionChange(offered, agent_id, None)"
    );

    // B. Action 3: Accept mission -> OnAgentMissionChange("offer_accepted", agent_id, None)
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id)],
    )
    .await;
    let n_accept = ctx.drain_notifications_for_char(P1);
    let ev_accepted = events_from(&n_accept, "OnAgentMissionChange");
    assert_eq!(
        ev_accepted,
        vec![vec![PyRep::String("offer_accepted".into()), PyRep::Integer(dist_agent_id), PyRep::None]],
        "Action 3 must emit OnAgentMissionChange(offer_accepted, agent_id, None)"
    );

    // C. Action 6: Complete mission -> OnAgentMissionChange("completed", agent_id, None)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            920_080,
            InventoryItemRecord {
                item_id: 920_080,
                type_id: 3683,
                owner_id: P1,
                location_id: dist_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id)],
    )
    .await;
    let n_complete = ctx.drain_notifications_for_char(P1);
    let ev_completed = events_from(&n_complete, "OnAgentMissionChange");
    assert_eq!(
        ev_completed,
        vec![vec![PyRep::String("completed".into()), PyRep::Integer(dist_agent_id), PyRep::None]],
        "Action 6 must emit OnAgentMissionChange(completed, agent_id, None)"
    );

    // Clean up active mission record
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.remove(&(P1, mission_id as u32));
    });

    // D. Action 11: Offer, Accept, and Quit mission -> OnAgentMissionChange("quit", agent_id, None)
    let m_offer2 = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let mission_id2 = m_offer2.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID2");
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id2)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);

    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(11), PyRep::Integer(mission_id2)],
    )
    .await;
    let n_quit = ctx.drain_notifications_for_char(P1);
    let ev_quit = events_from(&n_quit, "OnAgentMissionChange");
    assert_eq!(
        ev_quit,
        vec![vec![PyRep::String("quit".into()), PyRep::Integer(dist_agent_id), PyRep::None]],
        "Action 11 must emit OnAgentMissionChange(quit, agent_id, None)"
    );
}
