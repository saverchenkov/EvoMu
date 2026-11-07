//! Extra branch-coverage and mutation-killing unit tests for `services::agents_missions`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    MissionTimingAndCargoMeta, RndResearchEntry, save_mission_meta, save_rnd_research_entries,
};
use crate::services::agents_missions::*;
use eve_db::{
    ActiveMissionRecord, BookmarkRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AgentInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "agents_missions_actions_and_completion_unit_tests.rs"]
mod agents_missions_actions_and_completion_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn char_session(char_id: i64, sys_id: Option<u32>, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: sys_id,
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

pub(super) async fn call_agent_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, "agentMgr", method, args, kwargs)
        .await
        .expect("agentMgr method must be dispatched")
}

pub(super) fn rowset_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.as_slice(),
        _ => panic!("expected rowset, got {rep:?}"),
    }
}

pub(super) fn find_standard_agent(ctx: &ServerContext, level: u8) -> AgentInfo {
    ctx.sde
        .agents
        .values()
        .find(|a| {
            a.level == level
                && a.agent_type_id == 2
                && a.division_id == 24
                && a.solar_system_id.is_some_and(|sid| {
                    ctx.sde
                        .solar_systems
                        .get(&sid)
                        .is_some_and(|s| s.security_status >= 0.5)
                })
                && a.station_id.is_some()
        })
        .cloned()
        .expect("SDE must contain standard security agent")
}

pub(super) fn make_meta(mid: u32, agent_id: i64, offer_exp: u64, accept_exp: Option<u64>) -> MissionTimingAndCargoMeta {
    MissionTimingAndCargoMeta {
        mission_id: mid,
        agent_id,
        offered_at_unix: 100,
        offer_expiry_unix: offer_exp,
        accepted_at_unix: accept_exp.map(|_| 100),
        accept_expiry_unix: accept_exp,
        bonus_expiry_unix: None,
        completed_at_unix: None,
        required_delivery_type_id: None,
        required_delivery_qty: 0,
        granted_courier_item_id: None,
        granted_courier_qty: 0,
        is_anomic_burner: false,
        chosen_branch: None,
        destination_station_id: None,
        origin_station_id: None,
        offered_level: Some(1),
    }
}

pub(super) fn make_active_mission(
    mid: u32,
    char_id: i64,
    agent_id: i64,
    sys_id: u32,
    state: &str,
    bm_id: Option<i64>,
) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id: mid,
        character_id: char_id,
        agent_id,
        solar_system_id: sys_id,
        state: state.into(),
        current_room_id: 1,
        npcs_remaining: 0,
        spawned_npc_ball_ids: Vec::new(),
        bookmark_id: bm_id,
    }
}

#[tokio::test]
async fn get_agents_positional_and_security_filters_and_standing_access_gates() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let l1 = find_standard_agent(&ctx, 1);
    let l2 = find_standard_agent(&ctx, 2);
    let faction_id = ctx
        .sde
        .npc_corporations
        .get(&l1.corporation_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(500_001) as i64;
    let kw = HashMap::new();

    // Unbound character: hasAccess is true only for level <= 1
    let mut unbound = char_session(char_id, l1.solar_system_id, l1.station_id);
    unbound.character_id = None;
    let l1_rows = call_agent_mgr(
        &ctx,
        &mut unbound,
        "GetAgents",
        &[
            PyRep::Integer(1),
            PyRep::Integer(l1.division_id as i64),
            PyRep::Integer(faction_id),
            PyRep::Integer(l1.corporation_id),
            PyRep::Integer(l1.agent_type_id as i64),
            PyRep::Real(0.5),
        ],
        &kw,
    )
    .await
    .unwrap();
    assert!(!rowset_rows(&l1_rows).is_empty());
    assert_eq!(rowset_rows(&l1_rows)[0][9], PyRep::Bool(true));

    let l2_rows_unbound = call_agent_mgr(
        &ctx,
        &mut unbound,
        "GetAgents",
        &[
            PyRep::Integer(2),
            PyRep::Integer(l2.division_id as i64),
            PyRep::None,
            PyRep::Integer(l2.corporation_id),
            PyRep::Integer(l2.agent_type_id as i64),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(rowset_rows(&l2_rows_unbound)[0][9], PyRep::Bool(false));

    // Bound character: test L2 threshold (1.0) + string numeric security filter
    let mut sess = char_session(char_id, l2.solar_system_id, l2.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.standings.insert(l2.agent_id, 0.95);
    });
    let mut sec_kw = HashMap::from([
        ("level".into(), PyRep::Integer(2)),
        ("corporation".into(), PyRep::Integer(l2.corporation_id)),
        ("division".into(), PyRep::Integer(l2.division_id as i64)),
        ("agentType".into(), PyRep::Integer(l2.agent_type_id as i64)),
        ("security".into(), PyRep::String("0.5".into())),
    ]);
    let rows_below = call_agent_mgr(&ctx, &mut sess, "GetAgents", &[], &sec_kw).await.unwrap();
    let row_l2_below = rowset_rows(&rows_below)
        .iter()
        .find(|r| r[0] == PyRep::Integer(l2.agent_id))
        .unwrap();
    assert_eq!(row_l2_below[9], PyRep::Bool(false));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(l2.agent_id, 1.0);
    });
    let rows_at = call_agent_mgr(&ctx, &mut sess, "GetAgents", &[], &sec_kw).await.unwrap();
    let row_l2_at = rowset_rows(&rows_at)
        .iter()
        .find(|r| r[0] == PyRep::Integer(l2.agent_id))
        .unwrap();
    assert_eq!(row_l2_at[9], PyRep::Bool(true));

    // Lowsec filter excludes highsec agent
    sec_kw.insert("security".into(), PyRep::String("lowsec".into()));
    let rows_low = call_agent_mgr(&ctx, &mut sess, "GetAgents", &[], &sec_kw).await.unwrap();
    assert!(!rowset_rows(&rows_low).iter().any(|r| r[0] == PyRep::Integer(l2.agent_id)));
}

#[tokio::test]
async fn info_service_details_entry_point_and_simple_rpc_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    // NoSolarSystem error on GetInfoServiceDetails and GetSolarSystemOfAgent
    let mut no_sys = char_session(char_id, None, None);
    assert_eq!(
        call_agent_mgr(&ctx, &mut no_sys, "GetInfoServiceDetails", &[], &kw).await.unwrap_err().code,
        "NoSolarSystem"
    );
    assert_eq!(
        call_agent_mgr(&ctx, &mut no_sys, "GetSolarSystemOfAgent", &[], &kw).await.unwrap_err().code,
        "NoSolarSystem"
    );

    let mut sess = char_session(char_id, Some(30_000_142), Some(60_003_760));
    assert_eq!(
        call_agent_mgr(&ctx, &mut sess, "GetSolarSystemOfAgent", &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap_err()
            .code,
        "UnknownAgent"
    );

    // R&D agent (agent_type_id == 4) and Locator agent service list population
    if let Some(rnd_ag) = ctx.sde.agents.values().find(|a| a.agent_type_id == 4 && a.solar_system_id.is_some()).cloned() {
        let mut rnd_sess = char_session(char_id, rnd_ag.solar_system_id, rnd_ag.station_id);
        let details = call_agent_mgr(&ctx, &mut rnd_sess, "GetInfoServiceDetails", &[PyRep::Integer(rnd_ag.agent_id)], &kw)
            .await
            .unwrap();
        let Some(PyRep::List(svcs)) = details.dict_get("services") else { panic!("expected services") };
        assert!(svcs.iter().any(|s| s.dict_get("agentServiceType") == Some(&PyRep::String("research".into()))));
    }

    if let Some(loc_ag) = ctx.sde.agents.values().find(|a| a.is_locator && a.solar_system_id.is_some()).cloned() {
        let mut loc_sess = char_session(char_id, loc_ag.solar_system_id, loc_ag.station_id);
        let details = call_agent_mgr(&ctx, &mut loc_sess, "GetInfoServiceDetails", &[PyRep::Integer(loc_ag.agent_id)], &kw)
            .await
            .unwrap();
        assert_eq!(details.dict_get("isLocator"), Some(&PyRep::Bool(true)));
        let Some(PyRep::List(svcs)) = details.dict_get("services") else { panic!("expected services") };
        assert!(svcs.iter().any(|s| s.dict_get("agentServiceType") == Some(&PyRep::String("locator".into()))));
    }

    // GetEntryPoint with active mission having bookmark_id = Some(777)
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 5001),
            make_active_mission(5001, char_id, 3_008_416, 30_002_187, "Accepted", Some(777)),
        );
    });
    let ep = call_agent_mgr(&ctx, &mut sess, "GetEntryPoint", &[], &kw).await.unwrap();
    assert_eq!(ep.dict_get("solarSystemID"), Some(&PyRep::Integer(30_002_187)));
    assert_eq!(ep.dict_get("bookmarkID"), Some(&PyRep::Integer(777)));
    assert_eq!(call_agent_mgr(&ctx, &mut sess, "ShouldAlwaysAllowReplay", &[], &kw).await.unwrap(), PyRep::Bool(false));
    assert_eq!(call_agent_mgr(&ctx, &mut sess, "WarpToAgentInSpace", &[], &kw).await.unwrap(), PyRep::Bool(true));
    assert_eq!(call_agent_mgr(&ctx, &mut sess, "WarpToLocation", &[], &kw).await.unwrap(), PyRep::Bool(true));
}
