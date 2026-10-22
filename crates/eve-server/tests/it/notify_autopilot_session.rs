use crate::common;

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};

const P1: i64 = 90_000_001;
const SYS_JITA: u32 = 30000142;
const STATION_JITA: i64 = 60003760;

fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs
        .iter()
        .filter(|n| n.event == event)
        .map(|n| n.args.clone())
        .collect()
}

async fn select(
    ctx: &ServerContext,
    client_id: u64,
    user_name: &str,
    secret: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(client_id);
    common::login_pilot(ctx, &mut sess, user_name, secret).await;
    call(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await;
    sess
}

async fn call(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

#[tokio::test]
async fn test_agent_mission_change_notifications() {
    // parity: notify:OnAgentMissionChange
    eve_testkit::attest(&[
        "notify:OnAgentMissionChange",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 812, "demo", "demo-secret", P1).await;

    ctx.drain_notifications_for_char(P1);

    // OnAgentMissionChange: lifecycle events (offered, offer_accepted, completed, quit)
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
