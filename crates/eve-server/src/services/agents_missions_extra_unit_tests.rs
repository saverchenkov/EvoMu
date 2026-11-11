//! Extra branch-coverage and mutation-killing unit tests for `services::agents_missions`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::MissionTimingAndCargoMeta;
use crate::services::agents_missions::*;
use eve_db::{ActiveMissionRecord, WorldRepository};
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
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
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
        .filter(|a| {
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
        .min_by_key(|a| a.agent_id)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    if let Some(rnd_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mut rnd_sess = char_session(char_id, rnd_ag.solar_system_id, rnd_ag.station_id);
        let details = call_agent_mgr(&ctx, &mut rnd_sess, "GetInfoServiceDetails", &[PyRep::Integer(rnd_ag.agent_id)], &kw)
            .await
            .unwrap();
        let Some(PyRep::List(svcs)) = details.dict_get("services") else { panic!("expected services") };
        assert!(svcs.iter().any(|s| s.dict_get("agentServiceType") == Some(&PyRep::String("research".into()))));
    }

    if let Some(loc_ag) = ctx.sde.agents.values().filter(|a| a.is_locator && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mining_epic_arc_storyline_rnd_colocation_and_remote_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();

    // NoSolarSystem on RemoveOfferFromJournal (`L1457`)
    let mut no_sys = char_session(char_id, None, None);
    assert_eq!(
        call_agent_mgr(&ctx, &mut no_sys, "RemoveOfferFromJournal", &[], &kw).await.unwrap_err().code,
        "NoSolarSystem"
    );

    // CantDoMissionAlphaClone on Lv4 agent (`L746`)
    if let Some(lv4) = ctx.sde.agents.values().filter(|a| a.level == 4 && a.agent_type_id == 2 && a.station_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mid_lv4 = 990_001_u32;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 1;
            }
            snap.active_missions.insert(
                (char_id, mid_lv4),
                make_active_mission(mid_lv4, char_id, lv4.agent_id, lv4.solar_system_id.unwrap_or(sys_id), "Offered", None),
            );
        });
        let mut s4 = char_session(char_id, lv4.solar_system_id, lv4.station_id);
        assert_eq!(
            call_agent_mgr(&ctx, &mut s4, "DoAction", &[PyRep::Integer(3), PyRep::Integer(lv4.agent_id), PyRep::Integer(mid_lv4 as i64)], &kw)
                .await
                .unwrap_err()
                .code,
            "CantDoMissionAlphaClone"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.remove(&(char_id, mid_lv4));
        });
    }

    // In-space agent colocation (`station_id == None`, `L62-75`): docked fails, too far (>2500m) fails, within 2500m succeeds
    if let Some(sp_ag) = ctx.sde.agents.values().filter(|a| a.station_id.is_none() && a.solar_system_id.is_some() && a.level == 1).min_by_key(|a| a.agent_id).cloned() {
        let sp_sys = sp_ag.solar_system_id.unwrap();
        let h = ctx.systems.get_or_spawn(sp_sys);
        let ship_id = 100_000_001_i64;
        let _ = h.spawn_ball_with_meta(
            eve_destiny::Ball {
                id: sp_ag.agent_id, type_id: 3764, position: eve_destiny::DVec3::ZERO,
                velocity: eve_destiny::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e6, radius: 50.0,
                speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        ).await;
        let _ = h.spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id, type_id: 587, position: eve_destiny::DVec3::new(10_000.0, 0.0, 0.0),
                velocity: eve_destiny::DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 30.0,
                speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        ).await;
        let mut docked_s = char_session(char_id, Some(sp_sys), Some(60_003_760));
        assert_eq!(
            call_agent_mgr(&ctx, &mut docked_s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(sp_ag.agent_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "AgentNotCoLocated"
        );
        let mut space_s = char_session(char_id, Some(sp_sys), None);
        assert_eq!(
            call_agent_mgr(&ctx, &mut space_s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(sp_ag.agent_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "AgentNotCoLocated"
        );
    }

    // Mining mission: Offer -> Accept -> GoToLocation spawns asteroid (`L936-951`) -> Incomplete -> Deposit ore -> Complete -> R&D + Epic Arc + Storyline (`L1246-1383`)
    if let Some(min_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 2 && a.division_id == 23 && a.level == 1 && a.station_id.is_some() && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let m_sys = min_ag.solar_system_id.unwrap();
        let m_st = min_ag.station_id.unwrap();
        let mut ms = char_session(char_id, Some(m_sys), Some(m_st));
        call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(2), PyRep::Integer(min_ag.agent_id)], &kw)
            .await
            .unwrap();
        let mid = ctx.db.with_snapshot(|snap| {
            snap.active_missions
                .values()
                .filter(|m| m.character_id == char_id && m.agent_id == min_ag.agent_id && m.state == "Offered")
                .map(|m| m.mission_id)
                .min()
                .unwrap()
        });

        // Offer already offered returns cached briefing (`L621-648`)
        assert!(call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(2), PyRep::Integer(min_ag.agent_id)], &kw).await.is_ok());

        // CannotAcceptRemotely (`L774`) via action_id = 5 from different station
        let mut remote_ms = char_session(char_id, Some(30_000_142), Some(60_003_760));
        if remote_ms.station_id != Some(m_st) {
            assert_eq!(
                call_agent_mgr(&ctx, &mut remote_ms, "DoAction", &[PyRep::Integer(5), PyRep::Integer(min_ag.agent_id), PyRep::Integer(mid as i64)], &kw)
                    .await
                    .unwrap_err()
                    .code,
                "CannotAcceptRemotely"
            );
        }

        call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(3), PyRep::Integer(min_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.unwrap();

        // Deposit required ore if any, or check MissionIncomplete (`L1114`) then complete with ore in hangar
        let (ore_tid, req_qty) = ctx.db.with_snapshot(|snap| {
            let meta = crate::services::agents_missions::agents_standings_arcs::load_mission_meta(snap, char_id, mid);
            let md = crate::services::agents_missions::agents_missions_helpers::resolve_mining_mission_details(&ctx, &ctx.sde.missions[&mid]);
            (
                meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(md.ore_type_id),
                meta.as_ref().map(|mt| mt.required_delivery_qty).filter(|&q| q > 0).unwrap_or(md.required_units),
            )
        });
        assert_eq!(
            call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(6), PyRep::Integer(min_ag.agent_id), PyRep::Integer(mid as i64)], &kw)
                .await
                .unwrap_err()
                .code,
            "MissionIncomplete"
        );
        // Set up R&D research + mission_tally == 15 so storyline triggers (`L1246-1383`)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(m_st);
                ch.solar_system_id = m_sys;
            }
            if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
                m.npcs_remaining = 0;
            }
            snap.items.insert(888_001, eve_db::InventoryItemRecord {
                item_id: 888_001, type_id: ore_tid, owner_id: char_id, location_id: m_st,
                flag: eve_db::InventoryFlag::Hangar, quantity: req_qty.max(10_000) + 50, ..Default::default()
            });
            snap.mission_tally.insert(char_id, 15);
            crate::services::agents_missions::agents_standings_arcs::save_rnd_research_entries(
                snap,
                char_id,
                &[crate::services::agents_missions::agents_standings_arcs::RndResearchEntry {
                    agent_id: min_ag.agent_id,
                    research_type_id: 11433,
                    points_per_day: 50.0,
                    initial_points: 10.0,
                    started_unix: ctx.now_unix().saturating_sub(86_400),
                }],
            );
        });
        assert!(
            call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(6), PyRep::Integer(min_ag.agent_id), PyRep::Integer(mid as i64)], &kw)
                .await
                .is_ok()
        );
    }

    // Epic arc agent completion (`agent_type_id == 7`) & respawn check (`L811-818`, `L1267-1299`)
    if let Some(ep_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 7 && a.station_id.is_some() && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let e_sys = ep_ag.solar_system_id.unwrap();
        let e_st = ep_ag.station_id.unwrap();
        let mut es = char_session(char_id, Some(e_sys), Some(e_st));
        if call_agent_mgr(&ctx, &mut es, "DoAction", &[PyRep::Integer(2), PyRep::Integer(ep_ag.agent_id)], &kw).await.is_ok() {
            let emid = ctx.db.with_snapshot(|snap| {
                snap.active_missions
                    .values()
                    .filter(|m| m.character_id == char_id && m.agent_id == ep_ag.agent_id && m.state == "Offered")
                    .map(|m| m.mission_id)
                    .min()
            });
            if let Some(emid) = emid {
                let _ = call_agent_mgr(&ctx, &mut es, "DoAction", &[PyRep::Integer(3), PyRep::Integer(ep_ag.agent_id), PyRep::Integer(emid as i64)], &kw).await;
                let _ = call_agent_mgr(&ctx, &mut es, "DoAction", &[PyRep::Integer(6), PyRep::Integer(ep_ag.agent_id), PyRep::Integer(emid as i64)], &kw).await;
                assert!(ctx.db.with_snapshot(|s| !s.active_missions.contains_key(&(char_id, emid)) || !crate::services::agents_missions::agents_standings_arcs::load_epic_arc_states(s, char_id).is_empty()));
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_missions_in_space_rnd_epic_arc_trade_heraldry_and_respawn_branches() {
    let ctx = test_ctx();
    let char_id = 980_444_i64;
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().min_by_key(|c| c.character_id).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = 44;
        ch.name = "DeepAgentRunner".into();
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.clone_grade = 2;
        ch.balance = eve_db::IskCents::from_f64_isk(500_000_000.0);
        snap.characters.insert(char_id, ch);
    });

    // 1. In-space agent colocation distance/docked/no-ship/missing-ship checks (`L59-77`)
    if let Some(space_ag) = ctx.sde.agents.values().filter(|a| a.station_id.is_none() && a.solar_system_id.is_some() && a.level == 1).min_by_key(|a| a.agent_id).cloned() {
        let s_sys = space_ag.solar_system_id.unwrap();
        let mut ss = char_session(char_id, Some(s_sys), None);
        if call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(2), PyRep::Integer(space_ag.agent_id)], &kw).await.is_ok() {
            let smid = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().filter(|m| m.character_id == char_id && m.agent_id == space_ag.agent_id && m.state == "Offered").map(|m| m.mission_id).min()
            });
            if let Some(smid) = smid {
                let handle = ctx.systems.get_or_spawn(s_sys);
                let ship_id = 980_999_001_i64;
                {
                    let mut bp = handle.ballpark.write().unwrap();
                    let mk = |eid: i64, x: f64| eve_destiny::Ball {
                        id: eid,
                        type_id: 587,
                        position: eve_destiny::DVec3::new(x, 0.0, 0.0),
                        velocity: eve_destiny::DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 1.0,
                        mass: 1e6,
                        radius: 50.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    };
                    bp.balls.insert(space_ag.agent_id, mk(space_ag.agent_id, 0.0));
                    bp.balls.insert(ship_id, mk(ship_id, 10_000.0));
                }
                // Docked fails (`L62-63`)
                let mut docked_ss = char_session(char_id, Some(s_sys), Some(60_003_760));
                assert_eq!(call_agent_mgr(&ctx, &mut docked_ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(smid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
                // ship_id == None fails (`L67-68`)
                ss.ship_id = None;
                assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(smid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
                // Ship not spawned fails (`L70-71`)
                ss.ship_id = Some(980_999_777);
                assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(smid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
                // Ship > 2500m fails (`L73-75`)
                ss.ship_id = Some(ship_id);
                assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(smid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
                ctx.db.with_mut_snapshot(|snap| {
                    snap.active_missions.remove(&(char_id, smid));
                });
            }
        }
    }

    // 2. Special dialogue unbound args[1] (`L467-475`) + missing respawn check + InvalidMissionState (`L811-822`, `L849-852`)
    let any_ag = ctx.sde.agents.values().filter(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some() && a.level == 1).min_by_key(|a| a.agent_id).unwrap().clone();
    let any_sys = any_ag.solar_system_id.unwrap();
    let any_st = any_ag.station_id.unwrap();
    let mut s_any = char_session(char_id, Some(any_sys), Some(any_st));
    let _ = handle_with_bound(&ctx, &mut s_any, None, "agentMgr", "DoAction", &[PyRep::Integer(14), PyRep::Integer(any_ag.agent_id)], &kw).await;

    let enc_mid = ctx.sde.missions.keys().copied().min().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = any_sys;
            ch.station_id = Some(any_st);
        }
        snap.active_missions.insert((char_id, enc_mid), ActiveMissionRecord {
            mission_id: enc_mid, character_id: char_id, agent_id: any_ag.agent_id,
            solar_system_id: 0, state: "Accepted".into(), current_room_id: 1,
            npcs_remaining: 2, spawned_npc_ball_ids: vec![9_999_881], bookmark_id: None,
        });
        crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
            snap, char_id, &MissionTimingAndCargoMeta {
                mission_id: enc_mid, agent_id: any_ag.agent_id,
                offered_at_unix: 100, offer_expiry_unix: ctx.now_unix() + 10_000,
                accepted_at_unix: Some(100), accept_expiry_unix: Some(ctx.now_unix() + 10_000),
                ..Default::default()
            },
        );
    });
    // Respawn missing NPCs in Accepted mission when solar_system_id == 0 falls back to agent system (`L792-796`, `L811-820`)
    let _ = call_agent_mgr(&ctx, &mut s_any, "DoAction", &[PyRep::Integer(3), PyRep::Integer(any_ag.agent_id), PyRep::Integer(enc_mid as i64)], &kw).await;
    // Re-accept when Accepted & npcs_remaining == 0 -> InvalidMissionState (`L849-852`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, enc_mid)) {
            m.npcs_remaining = 0;
        }
    });
    assert_eq!(
        call_agent_mgr(&ctx, &mut s_any, "DoAction", &[PyRep::Integer(3), PyRep::Integer(any_ag.agent_id), PyRep::Integer(enc_mid as i64)], &kw)
            .await.unwrap_err().code,
        "InvalidMissionState"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.remove(&(char_id, enc_mid));
    });

    // 3. R&D agent (`agent_type_id == 4`) completion (`L1246-1256`) + Heraldry (`L631-635`) + fallback pool (`L643-648`)
    if let Some(rnd_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.level == 1 && a.station_id.is_some() && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let r_sys = rnd_ag.solar_system_id.unwrap();
        let r_st = rnd_ag.station_id.unwrap();
        let mut rs = char_session(char_id, Some(r_sys), Some(r_st));
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = r_sys;
            ch.station_id = Some(r_st);
            crate::services::agents_missions::agents_standings_arcs::save_rnd_research_entries(
                snap, char_id, &[crate::services::agents_missions::agents_standings_arcs::RndResearchEntry {
                    agent_id: rnd_ag.agent_id, research_type_id: 11433, points_per_day: 25.0,
                    initial_points: 5.0, started_unix: ctx.now_unix().saturating_sub(86_400),
                }],
            );
        });
        if call_agent_mgr(&ctx, &mut rs, "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_ag.agent_id)], &kw).await.is_ok() {
            let rmid = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().filter(|m| m.character_id == char_id && m.agent_id == rnd_ag.agent_id && m.state == "Offered").map(|m| m.mission_id).min()
            });
            if let Some(rmid) = rmid {
                let _ = call_agent_mgr(&ctx, &mut rs, "DoAction", &[PyRep::Integer(3), PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(rmid as i64)], &kw).await;
                assert_eq!(call_agent_mgr(&ctx, &mut rs, "DoAction", &[PyRep::Integer(7), PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(rmid as i64)], &kw).await.unwrap_err().code, "CannotCompleteRemotely");
                ctx.db.with_mut_snapshot(|snap| {
                    snap.items.insert(980_881, eve_db::InventoryItemRecord {
                        item_id: 980_881, type_id: 34, owner_id: char_id, location_id: r_st,
                        flag: eve_db::InventoryFlag::Hangar, quantity: 500_000, ..Default::default()
                    });
                });
                assert!(call_agent_mgr(&ctx, &mut rs, "DoAction", &[PyRep::Integer(6), PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(rmid as i64)], &kw).await.is_ok());
            }
        }
    }
    if let Some(her_ag) = ctx.sde.agents.values().filter(|a| (a.agent_type_id == 13 || a.division_id == 37) && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mut hs = char_session(char_id, her_ag.solar_system_id, her_ag.station_id);
        let _ = call_agent_mgr(&ctx, &mut hs, "DoAction", &[PyRep::Integer(2), PyRep::Integer(her_ag.agent_id)], &kw).await;
    }

    // 4. Epic Arc (`agent_type_id == 10`) new + progression + completed reset (`L99-107`, `L1267-1299`)
    if let Some(ea_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 10 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let e_sys = ea_ag.solar_system_id.unwrap();
        let mut es = char_session(char_id, Some(e_sys), ea_ag.station_id);
        let mut ea_mids: Vec<u32> = ctx.sde.missions.keys().copied().collect();
        ea_mids.sort_unstable();
        ea_mids.truncate(4);
        for (idx, &emid) in ea_mids.iter().enumerate() {
            ctx.db.with_mut_snapshot(|snap| {
                let ch = snap.characters.get_mut(&char_id).unwrap();
                ch.solar_system_id = e_sys;
                ch.station_id = ea_ag.station_id;
                if idx == 2 {
                    let mut arcs = crate::services::agents_missions::agents_standings_arcs::load_epic_arc_states(snap, char_id);
                    for a in &mut arcs {
                        a.completed_at_unix = Some(1);
                    }
                    crate::services::agents_missions::agents_standings_arcs::save_epic_arc_states(snap, char_id, &arcs);
                }
                snap.active_missions.insert((char_id, emid), ActiveMissionRecord {
                    mission_id: emid, character_id: char_id, agent_id: ea_ag.agent_id,
                    solar_system_id: e_sys, state: "Accepted".into(), current_room_id: 1,
                    npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
                });
                crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                    snap, char_id, &MissionTimingAndCargoMeta { mission_id: emid, agent_id: ea_ag.agent_id, ..Default::default() },
                );
            });
            let _ = call_agent_mgr(&ctx, &mut es, "DoAction", &[PyRep::Integer(7), PyRep::Integer(ea_ag.agent_id), PyRep::Integer(emid as i64)], &kw).await;
        }
    }

    // 5. Journal info for Mining, Trade, Completed/Declined/Quit (`L390-418`), Trade complete (`L1128-1129`), UnknownAgent (`L1024-1029`)
    let trade_tmpl = ctx.sde.missions.values().filter(|m| m.content_template.as_deref().unwrap_or_default().contains("Trade")).min_by_key(|m| m.mission_id).cloned();
    let mining_tmpl = ctx.sde.missions.values().filter(|m| m.content_template.as_deref().unwrap_or_default().contains("Mining")).min_by_key(|m| m.mission_id).cloned();
    let mut s_j = char_session(char_id, Some(any_sys), Some(any_st));
    for (idx, state_name) in ["Completed", "Declined", "Quit"].into_iter().enumerate() {
        let mid = trade_tmpl.as_ref().map(|m| m.mission_id).unwrap_or(idx as u32 + 1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.insert((char_id, mid), ActiveMissionRecord {
                mission_id: mid, character_id: char_id, agent_id: any_ag.agent_id,
                solar_system_id: any_sys, state: state_name.into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
        });
        assert!(call_agent_mgr(&ctx, &mut s_j, "GetMissionJournalInfo", &[PyRep::Integer(any_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.is_ok());
    }
    if let Some(m_tmpl) = mining_tmpl {
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.insert((char_id, m_tmpl.mission_id), ActiveMissionRecord {
                mission_id: m_tmpl.mission_id, character_id: char_id, agent_id: any_ag.agent_id,
                solar_system_id: any_sys, state: "Accepted".into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
        });
        assert!(call_agent_mgr(&ctx, &mut s_j, "GetMissionJournalInfo", &[PyRep::Integer(any_ag.agent_id), PyRep::Integer(m_tmpl.mission_id as i64)], &kw).await.is_ok());
    }
    if let Some(t_tmpl) = trade_tmpl {
        let td = crate::services::agents_missions::agents_missions_helpers::resolve_trade_mission_details(&ctx, &t_tmpl);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = any_sys;
            ch.station_id = Some(any_st);
            snap.active_missions.insert((char_id, t_tmpl.mission_id), ActiveMissionRecord {
                mission_id: t_tmpl.mission_id, character_id: char_id, agent_id: 99_999_999,
                solar_system_id: any_sys, state: "Accepted".into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
            snap.items.insert(980_882, eve_db::InventoryItemRecord {
                item_id: 980_882, type_id: td.trade_type_id, owner_id: char_id, location_id: any_st,
                flag: eve_db::InventoryFlag::Hangar, quantity: td.trade_quantity + 50, ..Default::default()
            });
        });
        // UnknownAgent on trade completion (`L1024-1029`)
        assert_eq!(call_agent_mgr(&ctx, &mut s_j, "DoAction", &[PyRep::Integer(6), PyRep::Integer(t_tmpl.mission_id as i64)], &kw).await.unwrap_err().code, "UnknownAgent");
        // Co-located fail on complete (`L1016-1022`) + trade completion (`L1128-1129`)
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.get_mut(&(char_id, t_tmpl.mission_id)).unwrap().agent_id = any_ag.agent_id;
        });
        let mut off_s = char_session(char_id, Some(any_sys), Some(60_000_004));
        if off_s.station_id != Some(any_st) {
            ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_000_004); });
            assert_eq!(call_agent_mgr(&ctx, &mut off_s, "DoAction", &[PyRep::Integer(6), PyRep::Integer(any_ag.agent_id), PyRep::Integer(t_tmpl.mission_id as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
            ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().station_id = Some(any_st); });
        }
        let _ = call_agent_mgr(&ctx, &mut s_j, "DoAction", &[PyRep::Integer(6), PyRep::Integer(any_ag.agent_id), PyRep::Integer(t_tmpl.mission_id as i64)], &kw).await;
    }

    // 6. Exhaust has_standing_rewards pool for an agent to hit fallback mission pool (`L643-648`)
    let has_non_sr = ctx.sde.missions.values().any(|m| !m.has_standing_rewards);
    if has_non_sr {
        ctx.db.with_mut_snapshot(|snap| {
            for m in ctx.sde.missions.values().filter(|m| m.has_standing_rewards) {
                snap.active_missions.entry((char_id, m.mission_id)).or_insert(ActiveMissionRecord {
                    mission_id: m.mission_id, character_id: char_id, agent_id: any_ag.agent_id,
                    solar_system_id: any_sys, state: "Completed".into(), current_room_id: 1,
                    npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
                });
            }
        });
        let _ = call_agent_mgr(&ctx, &mut s_j, "DoAction", &[PyRep::Integer(2), PyRep::Integer(any_ag.agent_id)], &kw).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_missions_targeted_region_closer_space_rnd_mining_coloc_and_respawn() {
    let ctx = test_ctx();
    let char_id = 980_555_i64;
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().min_by_key(|c| c.character_id).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = 55;
        ch.name = "RegionCloser".into();
        ch.corporation_id = 98_555_001;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.clone_grade = 2;
        ch.balance = eve_db::IskCents::from_f64_isk(500_000_000.0);
        snap.characters.insert(char_id, ch);
    });

    // 1. In-space agent colocation distance/docked/no-ship/missing-ship checks (`L59-77`)
    if let Some(space_ag) = ctx.sde.agents.values().filter(|a| a.station_id.is_none() && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let s_sys = space_ag.solar_system_id.unwrap();
        let mid = ctx.sde.missions.keys().copied().min().unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.insert((char_id, mid), ActiveMissionRecord {
                mission_id: mid, character_id: char_id, agent_id: space_ag.agent_id,
                solar_system_id: s_sys, state: "Offered".into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
            crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                snap, char_id, &MissionTimingAndCargoMeta {
                    mission_id: mid, agent_id: space_ag.agent_id,
                    offered_at_unix: 100, offer_expiry_unix: ctx.now_unix() + 10_000,
                    ..Default::default()
                },
            );
        });
        let handle = ctx.systems.get_or_spawn(s_sys);
        let ship_id = 985_999_001_i64;
        let mk = |eid: i64, x: f64| eve_destiny::Ball {
            id: eid, type_id: 587, position: eve_destiny::DVec3::new(x, 0.0, 0.0),
            velocity: eve_destiny::DVec3::ZERO, max_velocity: 300.0, inertia: 1.0,
            mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        };
        let _ = handle.spawn_ball_with_combat(mk(space_ag.agent_id, 0.0), Default::default()).await;
        let _ = handle.spawn_ball_with_combat(mk(ship_id, 10_000.0), Default::default()).await;

        let mut docked_ss = char_session(char_id, Some(s_sys), Some(60_003_760));
        assert_eq!(call_agent_mgr(&ctx, &mut docked_ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");

        let mut ss = char_session(char_id, Some(s_sys), None);
        ss.ship_id = None;
        assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
        ss.ship_id = Some(985_999_777);
        assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
        ss.ship_id = Some(ship_id);
        assert_eq!(call_agent_mgr(&ctx, &mut ss, "DoAction", &[PyRep::Integer(3), PyRep::Integer(space_ag.agent_id), PyRep::Integer(mid as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");
    }

    // 2. AgentNotCoLocated on DoAction(3) for station agent (`L756-762`) + can_respawn_missing (`L812-818`)
    let st_ag = ctx.sde.agents.values().filter(|a| a.station_id.is_some() && a.solar_system_id.is_some() && a.level == 1 && a.division_id != 23 && a.agent_type_id == 2).min_by_key(|a| a.agent_id).unwrap().clone();
    let st_sys = st_ag.solar_system_id.unwrap();
    let st_id = st_ag.station_id.unwrap();
    let mid2 = ctx.sde.missions.values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            let lc = ct.to_lowercase();
            let lt = m.title.to_lowercase();
            !m.is_courier
                && !ct.contains("Courier")
                && !ct.contains("Trade")
                && !ct.contains("Heraldry")
                && !lc.contains("mining")
                && !lt.split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "mining" | "mineral" | "minerals" | "ore" | "ores"))
        })
        .min_by_key(|m| m.mission_id)
        .map(|m| m.mission_id)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert((char_id, mid2), ActiveMissionRecord {
            mission_id: mid2, character_id: char_id, agent_id: st_ag.agent_id,
            solar_system_id: st_sys, state: "Offered".into(), current_room_id: 1,
            npcs_remaining: 2, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
        });
        crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
            snap, char_id, &MissionTimingAndCargoMeta {
                mission_id: mid2, agent_id: st_ag.agent_id,
                offered_at_unix: 100, offer_expiry_unix: ctx.now_unix() + 10_000,
                ..Default::default()
            },
        );
    });
    let mut far_s = char_session(char_id, Some(st_sys), Some(st_id + 1));
    assert_eq!(call_agent_mgr(&ctx, &mut far_s, "DoAction", &[PyRep::Integer(3), PyRep::Integer(st_ag.agent_id), PyRep::Integer(mid2 as i64)], &kw).await.unwrap_err().code, "AgentNotCoLocated");

    // Re-accept Accepted mission in same solar system with missing NPC ball IDs (`L812-818`)
    ctx.db.with_mut_snapshot(|snap| {
        let m = snap.active_missions.get_mut(&(char_id, mid2)).unwrap();
        m.state = "Accepted".into();
        m.spawned_npc_ball_ids = vec![9_888_771, 9_888_772];
    });
    let mut near_s = char_session(char_id, Some(st_sys), Some(st_id));
    let _ = call_agent_mgr(&ctx, &mut near_s, "DoAction", &[PyRep::Integer(3), PyRep::Integer(st_ag.agent_id), PyRep::Integer(mid2 as i64)], &kw).await;

    // 3. Direct R&D completion (`L1246-1256`) + Non-div-23 mining completion without meta delivery (`L1103-1109`, `L1125-1126`) + UnknownMission (`L998`) + NoSolarSystem (`L364`)
    let mut no_sys = char_session(char_id, None, None);
    assert_eq!(call_agent_mgr(&ctx, &mut no_sys, "GetMissionBriefingInfo", &[], &kw).await.unwrap_err().code, "NoSolarSystem");
    assert!(call_agent_mgr(&ctx, &mut near_s, "DoAction", &[PyRep::Integer(6), PyRep::Integer(999_888_777)], &kw).await.is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert((char_id, 999_777_666), ActiveMissionRecord {
            mission_id: 999_777_666, character_id: char_id, agent_id: st_ag.agent_id,
            solar_system_id: st_sys, state: "Accepted".into(), current_room_id: 1,
            npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
        });
    });
    assert_eq!(call_agent_mgr(&ctx, &mut near_s, "DoAction", &[PyRep::Integer(6), PyRep::Integer(999_777_666)], &kw).await.unwrap_err().code, "UnknownMission");

    if let Some(m_tmpl) = ctx.sde.missions.values().filter(|m| {
        let c = m.content_template.as_deref().unwrap_or_default().to_lowercase();
        let t = m.title.to_lowercase();
        !c.contains("kill") && !c.contains("encounter") && !c.contains("trade") && !m.is_courier && !c.contains("courier") && c.contains("mining")
            || (!c.contains("kill") && !c.contains("encounter") && !c.contains("trade") && !m.is_courier && !c.contains("courier") && t.contains("ore"))
    }).min_by_key(|m| m.mission_id).cloned() {
        let non23_ag = ctx.sde.agents.values().filter(|a| a.division_id != 23 && a.station_id.is_some() && a.solar_system_id.is_some() && a.agent_type_id == 2).min_by_key(|a| a.agent_id).unwrap().clone();
        let md = crate::services::agents_missions::agents_missions_helpers::resolve_mining_mission_details(&ctx, &m_tmpl);
        let n_sys = non23_ag.solar_system_id.unwrap();
        let n_st = non23_ag.station_id.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = n_sys;
            ch.station_id = Some(n_st);
            snap.active_missions.insert((char_id, m_tmpl.mission_id), ActiveMissionRecord {
                mission_id: m_tmpl.mission_id, character_id: char_id, agent_id: non23_ag.agent_id,
                solar_system_id: n_sys, state: "Accepted".into(), current_room_id: 1,
                npcs_remaining: 1, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
            crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                snap, char_id, &MissionTimingAndCargoMeta {
                    mission_id: m_tmpl.mission_id, agent_id: non23_ag.agent_id,
                    required_delivery_type_id: None, required_delivery_qty: 0,
                    ..Default::default()
                },
            );
            snap.items.insert(985_771, eve_db::InventoryItemRecord {
                item_id: 985_771, type_id: md.ore_type_id, owner_id: char_id, location_id: n_st,
                flag: eve_db::InventoryFlag::Hangar, quantity: md.required_units + 50, ..Default::default()
            });
        });
        let mut ms = char_session(char_id, Some(n_sys), Some(n_st));
        let _ = call_agent_mgr(&ctx, &mut ms, "DoAction", &[PyRep::Integer(6), PyRep::Integer(non23_ag.agent_id), PyRep::Integer(m_tmpl.mission_id as i64)], &kw).await;
    }

    if let Some(rnd_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.station_id.is_some() && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let r_sys = rnd_ag.solar_system_id.unwrap();
        let r_st = rnd_ag.station_id.unwrap();
        let rmid = ctx.sde.missions.values()
            .filter(|m| {
                let ct = m.content_template.as_deref().unwrap_or_default();
                !m.is_courier && !ct.contains("Courier") && !ct.contains("Trade") && !ct.contains("Heraldry")
            })
            .max_by_key(|m| m.mission_id)
            .map(|m| m.mission_id)
            .unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = r_sys;
            ch.station_id = Some(r_st);
            snap.mission_tally.insert(char_id, 15);
            crate::services::agents_missions::agents_standings_arcs::save_rnd_research_entries(
                snap, char_id, &[crate::services::agents_missions::agents_standings_arcs::RndResearchEntry {
                    agent_id: rnd_ag.agent_id, research_type_id: 11433, points_per_day: 25.0,
                    initial_points: 5.0, started_unix: ctx.now_unix().saturating_sub(86_400),
                }],
            );
            snap.active_missions.insert((char_id, rmid), ActiveMissionRecord {
                mission_id: rmid, character_id: char_id, agent_id: rnd_ag.agent_id,
                solar_system_id: r_sys, state: "Accepted".into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
            crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                snap, char_id, &MissionTimingAndCargoMeta {
                    mission_id: rmid, agent_id: rnd_ag.agent_id,
                    required_delivery_type_id: Some(34), required_delivery_qty: 100,
                    ..Default::default()
                },
            );
            snap.items.insert(985_772, eve_db::InventoryItemRecord {
                item_id: 985_772, type_id: 34, owner_id: char_id, location_id: r_st,
                flag: eve_db::InventoryFlag::Hangar, quantity: 500, ..Default::default()
            });
        });
        let mut rs = char_session(char_id, Some(r_sys), Some(r_st));
        assert!(call_agent_mgr(&ctx, &mut rs, "DoAction", &[PyRep::Integer(6), PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(rmid as i64)], &kw).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_missions_push_over_90_regions() {
    let ctx = test_ctx();
    let char_id = 980_666_i64;
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().min_by_key(|c| c.character_id).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = 66;
        ch.name = "RegionPush".into();
        ch.corporation_id = 98_666_001;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.clone_grade = 2; // Omega
        ch.balance = eve_db::IskCents::from_f64_isk(500_000_000.0);
        snap.characters.insert(char_id, ch);
    });

    // 1. DoAction error gates: NoSolarSystem (L450), clamped action (L457), UnknownAgent (L495)
    let mut no_sys = char_session(char_id, None, None);
    assert_eq!(call_agent_mgr(&ctx, &mut no_sys, "DoAction", &[PyRep::Integer(2)], &kw).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_agent_mgr(&ctx, &mut no_sys, "RemoveOfferFromJournal", &[], &kw).await.unwrap_err().code, "NoSolarSystem");
    let mut jita_s = char_session(char_id, Some(30_000_142), Some(60_003_760));
    assert_eq!(call_agent_mgr(&ctx, &mut jita_s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(99_999_888)], &kw).await.unwrap_err().code, "UnknownAgent");
    assert_eq!(call_agent_mgr(&ctx, &mut jita_s, "DoAction", &[PyRep::Integer(99), PyRep::Integer(99_999_888)], &kw).await.unwrap_err().code, "UnknownAgent");
    assert_eq!(call_agent_mgr(&ctx, &mut jita_s, "GetMissionBriefingInfo", &[PyRep::Integer(3_008_416)], &kw).await.unwrap_err().code, "MissionNotActive");

    // Journal unknown mission state arm (L418) + Quit (L1433) + RemoveOfferFromJournal (L1462-1474)
    let any_ag = ctx.sde.agents.values().filter(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some() && a.level == 1).min_by_key(|a| a.agent_id).unwrap().clone();
    let mut sorted_mids: Vec<u32> = ctx.sde.missions.keys().copied().collect();
    sorted_mids.sort_unstable();
    let mid0 = sorted_mids[0];
    let mid1 = sorted_mids[1];
    let mut ag_s = char_session(char_id, any_ag.solar_system_id, any_ag.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert((char_id, mid0), ActiveMissionRecord {
            mission_id: mid0, character_id: char_id, agent_id: any_ag.agent_id,
            solar_system_id: any_ag.solar_system_id.unwrap(), state: "UnknownWeirdState".into(),
            current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
        });
        snap.active_missions.insert((char_id, mid1), ActiveMissionRecord {
            mission_id: mid1, character_id: char_id, agent_id: any_ag.agent_id,
            solar_system_id: any_ag.solar_system_id.unwrap(), state: "Accepted".into(),
            current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
        });
    });
    assert!(call_agent_mgr(&ctx, &mut ag_s, "GetMissionJournalInfo", &[PyRep::Integer(any_ag.agent_id), PyRep::Integer(mid0 as i64)], &kw).await.is_ok());
    assert!(call_agent_mgr(&ctx, &mut ag_s, "DoAction", &[PyRep::Integer(11), PyRep::Integer(any_ag.agent_id), PyRep::Integer(mid1 as i64)], &kw).await.is_ok());
    // RemoveOfferFromJournal with missing mission (L1462-1465) and explicit mission_id
    assert!(call_agent_mgr(&ctx, &mut ag_s, "RemoveOfferFromJournal", &[], &kw).await.is_err());
    // Re-insert Offered and remove via explicit mission_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.get_mut(&(char_id, mid1)).unwrap().state = "Offered".into();
    });
    assert!(call_agent_mgr(&ctx, &mut ag_s, "RemoveOfferFromJournal", &[PyRep::Integer(mid1 as i64)], &kw).await.is_ok());

    // 2. Agent-type gates in DoAction(2): Aura (11, L517), FW (9, L537), EpicArc cooldown (10, L530), COSMOS Expired (8, L563), L5 highsec (L584), R&D no research (4, L597)
    if let Some(aura_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 11 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mut s = char_session(char_id, aura_ag.solar_system_id, aura_ag.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(aura_ag.agent_id)], &kw).await.unwrap_err().code, "UnimplementedRPC");
    }
    if let Some(fw_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 9 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mut s = char_session(char_id, fw_ag.solar_system_id, fw_ag.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(fw_ag.agent_id)], &kw).await.unwrap_err().code, "NotEnlistedInMilitia");
    }
    if let Some(ea_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 10 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let (arc_id, _) = crate::services::agents_missions::agents_standings_arcs::resolve_epic_arc_id_for_agent(&ctx, ea_ag.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            crate::services::agents_missions::agents_standings_arcs::save_epic_arc_states(
                snap, char_id, &[crate::services::agents_missions::agents_standings_arcs::EpicArcStateEntry {
                    epic_arc_id: arc_id, agent_id: ea_ag.agent_id, faction_id: 500_001, chapter: 3,
                    completed_missions: vec![], chosen_branch: None,
                    completed_at_unix: Some(1), cooldown_until_unix: Some(ctx.now_unix() + 10_000),
                }],
            );
        });
        let mut s = char_session(char_id, ea_ag.solar_system_id, ea_ag.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(ea_ag.agent_id)], &kw).await.unwrap_err().code, "EpicArcOnCooldown");
    }
    if let Some(cos_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 8 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mid_exp = sorted_mids[2];
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.insert((char_id, mid_exp), ActiveMissionRecord {
                mission_id: mid_exp, character_id: char_id, agent_id: cos_ag.agent_id,
                solar_system_id: cos_ag.solar_system_id.unwrap(), state: "Expired".into(),
                current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
        });
        let mut s = char_session(char_id, cos_ag.solar_system_id, cos_ag.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(cos_ag.agent_id)], &kw).await.unwrap_err().code, "CosmosMissionExpired");
    }
    if let Some(l5_hs) = ctx.sde.agents.values().filter(|a| a.level == 5 && a.agent_type_id == 2 && a.solar_system_id.is_some_and(|sid| ctx.sde.solar_systems.get(&sid).is_some_and(|sys| sys.security_status >= 0.45))).min_by_key(|a| a.agent_id).cloned() {
        let mut s = char_session(char_id, l5_hs.solar_system_id, l5_hs.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(l5_hs.agent_id)], &kw).await.unwrap_err().code, "CantDoMission");
    }
    if let Some(rnd_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.standings.insert(rnd_ag.agent_id, 10.0);
            ch.standings.insert(rnd_ag.corporation_id, 10.0);
        });
        let mut s = char_session(char_id, rnd_ag.solar_system_id, rnd_ag.station_id);
        assert_eq!(call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_ag.agent_id)], &kw).await.unwrap_err().code, "ResearchNotActive");
    }

    // 3. Paragon corp LP balance path (`L1230-1232`) + Heraldry/DC offers (`L669`, `L683`)
    if let Some(dc_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 5 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.standings.insert(dc_ag.agent_id, 10.0);
            ch.standings.insert(dc_ag.corporation_id, 10.0);
        });
        let mut s = char_session(char_id, dc_ag.solar_system_id, dc_ag.station_id);
        let _ = call_agent_mgr(&ctx, &mut s, "DoAction", &[PyRep::Integer(2), PyRep::Integer(dc_ag.agent_id)], &kw).await;
    }
    if let Some(para_ag) = ctx.sde.agents.values().filter(|a| a.corporation_id == crate::services::lp_store_catalog::PARAGON_CORP_ID && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let p_sys = para_ag.solar_system_id.unwrap();
        let pmid = sorted_mids[4];
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = p_sys;
            ch.station_id = para_ag.station_id;
            snap.active_missions.insert((char_id, pmid), ActiveMissionRecord {
                mission_id: pmid, character_id: char_id, agent_id: para_ag.agent_id,
                solar_system_id: p_sys, state: "Accepted".into(), current_room_id: 1,
                npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            });
            crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                snap, char_id, &MissionTimingAndCargoMeta { mission_id: pmid, agent_id: para_ag.agent_id, ..Default::default() },
            );
        });
        let mut ps = char_session(char_id, Some(p_sys), para_ag.station_id);
        let _ = call_agent_mgr(&ctx, &mut ps, "DoAction", &[PyRep::Integer(7), PyRep::Integer(para_ag.agent_id), PyRep::Integer(pmid as i64)], &kw).await;
    }

    // 4. Heraldry offer via explicit agent (L631-635, L683) + Trade offer (L676-678)
    if let Some(her_ag) = ctx.sde.agents.values().filter(|a| (a.agent_type_id == 13 || a.division_id == 37) && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.standings.insert(her_ag.agent_id, 10.0);
        });
        let mut hs = char_session(char_id, her_ag.solar_system_id, her_ag.station_id);
        let _ = call_agent_mgr(&ctx, &mut hs, "DoAction", &[PyRep::Integer(2), PyRep::Integer(her_ag.agent_id)], &kw).await;
    }
    if let Some(tr_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 2 && a.division_id == 22 && a.level == 1 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let mut ts = char_session(char_id, tr_ag.solar_system_id, tr_ag.station_id);
        let _ = call_agent_mgr(&ctx, &mut ts, "DoAction", &[PyRep::Integer(2), PyRep::Integer(tr_ag.agent_id)], &kw).await;
    }

    // 5. Epic arc (`agent_type_id == 10`) progression + completion + reset (`L1271-1286`), UnknownAgent on GetInfoServiceDetails (`L145`)
    assert!(call_agent_mgr(&ctx, &mut jita_s, "GetInfoServiceDetails", &[PyRep::Integer(99_999_888)], &kw).await.is_ok());
    if let Some(empty_sys) = ctx.sde.solar_systems.keys().copied().filter(|&sid| !ctx.sde.agents.values().any(|a| a.solar_system_id == Some(sid))).min() {
        let mut es_none = char_session(char_id, Some(empty_sys), None);
        assert_eq!(call_agent_mgr(&ctx, &mut es_none, "GetInfoServiceDetails", &[], &kw).await.unwrap_err().code, "UnknownAgent");
    }
    if let Some(ea_ag) = ctx.sde.agents.values().filter(|a| a.agent_type_id == 10 && a.solar_system_id.is_some()).min_by_key(|a| a.agent_id).cloned() {
        let e_sys = ea_ag.solar_system_id.unwrap();
        let mut kill_mids: Vec<u32> = ctx.sde.missions.values().filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default().to_lowercase();
            let t = m.title.to_lowercase();
            !m.is_courier && !ct.contains("courier") && !ct.contains("trade") && !ct.contains("mining") && !t.contains("mining") && !t.contains("ore") && !t.contains("mineral")
        }).map(|m| m.mission_id).collect();
        kill_mids.sort_unstable();
        kill_mids.truncate(6);
        let mut es = char_session(char_id, Some(e_sys), ea_ag.station_id);
        for (idx, &emid) in kill_mids.iter().enumerate() {
            ctx.db.with_mut_snapshot(|snap| {
                let ch = snap.characters.get_mut(&char_id).unwrap();
                ch.solar_system_id = e_sys;
                ch.station_id = ea_ag.station_id;
                if idx == 0 {
                    crate::services::agents_missions::agents_standings_arcs::save_epic_arc_states(snap, char_id, &[]);
                }
                snap.active_missions.insert((char_id, emid), ActiveMissionRecord {
                    mission_id: emid, character_id: char_id, agent_id: ea_ag.agent_id,
                    solar_system_id: e_sys, state: "Accepted".into(), current_room_id: 1,
                    npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
                });
                crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
                    snap, char_id, &MissionTimingAndCargoMeta { mission_id: emid, agent_id: ea_ag.agent_id, ..Default::default() },
                );
            });
            let _ = call_agent_mgr(&ctx, &mut es, "DoAction", &[PyRep::Integer(7), PyRep::Integer(ea_ag.agent_id), PyRep::Integer(emid as i64)], &kw).await;
        }
    }
}
