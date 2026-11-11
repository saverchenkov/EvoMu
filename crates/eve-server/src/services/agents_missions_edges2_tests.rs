//! Additional edge-case and branch coverage tests for `services::agents_missions`.

use super::*;
use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    EpicArcStateEntry, MissionTimingAndCargoMeta, RndResearchEntry, save_epic_arc_states,
    save_mission_meta, save_rnd_research_entries,
};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
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

fn char_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn ensure_char(ctx: &ServerContext, char_id: i64, station_id: Option<i64>, sys_id: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut ch = snap
                .characters
                .values()
                .min_by_key(|c| c.character_id)
                .cloned()
                .expect("seed char");
            ch.character_id = char_id;
            ch.name = format!("Edge Pilot {char_id}");
            ch.corporation_id = 1_000_009;
            ch.alliance_id = None;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            ch.clone_grade = 2;
            snap.characters.insert(char_id, ch);
        }
    });
}

fn find_lvl1_agent(ctx: &ServerContext) -> AgentInfo {
    ctx.sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 24
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("level 1 standard agent")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_space_agent_colocation_and_deterministic_origin_edges() {
    let ctx = test_ctx();
    let char_id = 91_001_001;
    let in_space_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.station_id.is_none() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap_or_else(|| {
            let mut ag = find_lvl1_agent(&ctx);
            ag.station_id = None;
            ag
        });
    let sys_id = in_space_agent.solar_system_id.unwrap();
    ensure_char(&ctx, char_id, None, sys_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: in_space_agent.agent_id,
                type_id: 24,
                position: DVec3::new(1_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;

    // 1. Docked session cannot be co-located with an in-space agent
    let docked_sess = char_session(char_id, sys_id, Some(60_003_760));
    assert!(!is_agent_colocated(&ctx, &docked_sess, sys_id, &in_space_agent).await);

    // 2. Undocked with no ship_id -> false
    let mut no_ship_sess = char_session(char_id, sys_id, None);
    no_ship_sess.ship_id = None;
    assert!(!is_agent_colocated(&ctx, &no_ship_sess, sys_id, &in_space_agent).await);

    // 3. Undocked in wrong solar system -> false
    let wrong_sys = if sys_id == 30_000_142 { 30_002_187 } else { 30_000_142 };
    let wrong_sess = char_session(char_id, wrong_sys, None);
    assert!(!is_agent_colocated(&ctx, &wrong_sess, wrong_sys, &in_space_agent).await);

    // 4. Undocked in same system: place ship ball > 2500m from agent ball, then <= 2500m
    let mut ship_sess = char_session(char_id, sys_id, None);
    let ship_id = 100_999_001;
    ship_sess.ship_id = Some(ship_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1_005_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;
    assert!(!is_agent_colocated(&ctx, &ship_sess, sys_id, &in_space_agent).await);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(1_001_000.0, 0.0, 0.0)).await;
    assert!(is_agent_colocated(&ctx, &ship_sess, sys_id, &in_space_agent).await);

    // deterministic_mission_site_origin determinism & epic_arc_required_missions fallback
    let p1 = deterministic_mission_site_origin(&ctx, sys_id, 1234);
    let p2 = deterministic_mission_site_origin(&ctx, sys_id, 1234);
    assert_eq!(p1, p2);
    assert!(epic_arc_required_missions(&ctx, 9_999_999) >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_info_location_and_mission_metadata_rpcs() {
    let ctx = test_ctx();
    let char_id = 91_001_002;
    let ag = find_lvl1_agent(&ctx);
    let sys_id = ag.solar_system_id.unwrap();
    ensure_char(&ctx, char_id, ag.station_id, sys_id);

    let mut no_sys_sess = char_session(char_id, 0, None);
    no_sys_sess.solar_system_id = None;
    let err_nosys = handle(
        &ctx,
        &mut no_sys_sess,
        "agentMgr",
        "GetInfoServiceDetails",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_nosys.code, "NoSolarSystem");

    let mut sess = char_session(char_id, sys_id, ag.station_id);
    let bound_unknown = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: 999_888_777,
        oid_str: "agentMgr:999888777".into(),
        ref_id: 1,
    };
    let err_unk = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound_unknown),
        "agentMgr",
        "GetInfoServiceDetails",
        &[PyRep::Integer(999_888_777)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_unk.code, "UnknownAgent");

    let info_rep = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetInfoServiceDetails",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(info_rep.dict_get("agentID").is_some() || matches!(info_rep, PyRep::Object { .. }));

    let loc_wrap = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetAgentLocationWrap",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(loc_wrap, PyRep::None);

    // R&D agent info service branch
    if let Some(rnd_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let rnd_info = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetInfoServiceDetails",
            &[PyRep::Integer(rnd_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(rnd_info, PyRep::None);
    }

    // Locator agent info service branch
    if let Some(loc_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.is_locator && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let loc_info = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetInfoServiceDetails",
            &[PyRep::Integer(loc_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(loc_info, PyRep::None);
    }

    // GetSolarSystemOfAgent: arg, bound_entry, unknown, NoSolarSystem
    let err_sys_unk = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[PyRep::Integer(999_888_777)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_sys_unk.code, "UnknownAgent");

    let err_sys_nosys = handle(
        &ctx,
        &mut no_sys_sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_sys_nosys.code, "NoSolarSystem");

    let bound_entry = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: ag.agent_id,
        oid_str: ag.agent_id.to_string(),
        ref_id: 1,
    };
    let bound_sys = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound_entry),
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bound_sys, PyRep::Integer(sys_id as i64));

    // Misc query RPCs
    let _ = handle(&ctx, &mut sess, "agentMgr", "GetAgents", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let _ = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetDungeonShipRestrictions",
        &[PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionKeywords",
        &[PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let sg_unknown = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetStandingGainsForMission",
        &[PyRep::Integer(999_888_777), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(sg_unknown, PyRep::None);

    assert_eq!(
        handle(&ctx, &mut sess, "agentMgr", "IsCheatingWithAgent", &[PyRep::Integer(ag.agent_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &mut sess, "agentMgr", "ShouldAlwaysAllowReplay", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // GetEntryPoint without active mission
    let ep_empty = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetEntryPoint",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = ep_empty;

    // WarpToLocation / GotoLocation / WarpToAgentInSpace
    let _ = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToAgentInSpace",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mission_briefing_objective_journal_state_mapping_and_expiry_sweeps() {
    let ctx = test_ctx();
    let char_id = 91_001_003;
    let ag = find_lvl1_agent(&ctx);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    ensure_char(&ctx, char_id, Some(st_id), sys_id);

    let mut no_sys_sess = char_session(char_id, 0, None);
    no_sys_sess.solar_system_id = None;
    let err_nosys = handle(
        &ctx,
        &mut no_sys_sess,
        "agentMgr",
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_nosys.code, "NoSolarSystem");

    let mut sess = char_session(char_id, sys_id, Some(st_id));
    let err_missing = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(88_888_001)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "MissionNotActive");

    // Seed missions across states and objective types (Trade, Courier, Mining, Combat, unknown state)
    let now = ctx.now_unix() as u64;
    let states = [
        (71_001_u32, "Offered", "Trade", 1),
        (71_002_u32, "Accepted", "Courier", 2),
        (71_003_u32, "Failed", "Mining", 3),
        (71_004_u32, "Completed", "Combat", 4),
        (71_005_u32, "Declined", "Combat", 5),
        (71_006_u32, "Quit", "Combat", 6),
        (71_007_u32, "Expired", "Combat", 3),
        (71_008_u32, "MysteryState", "Courier", 0),
    ];

    ctx.db.with_mut_snapshot(|snap| {
        for &(mid, st_str, _mtype, _) in &states {
            snap.active_missions.insert(
                (char_id, mid),
                ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: mid,
                    agent_id: ag.agent_id,
                    solar_system_id: sys_id,
                    state: st_str.to_string(),
                    current_room_id: 1,
                    npcs_remaining: 3,
                    spawned_npc_ball_ids: Vec::new(),
                    bookmark_id: if mid == 71_002 { Some(9001) } else { None },
                },
            );
            save_mission_meta(
                snap,
                char_id,
                &MissionTimingAndCargoMeta {
                    mission_id: mid,
                    agent_id: ag.agent_id,
                    offered_at_unix: now.saturating_sub(120),
                    offer_expiry_unix: now + 3600,
                    accepted_at_unix: Some(now.saturating_sub(60)),
                    accept_expiry_unix: Some(now + 3600),
                    required_delivery_type_id: Some(34),
                    required_delivery_qty: 10,
                    destination_station_id: Some(st_id),
                    ..Default::default()
                },
            );
        }

        // Add expired Offered and expired Accepted missions to exercise sweep paths
        snap.active_missions.insert(
            (char_id, 71_901),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 71_901,
                agent_id: ag.agent_id,
                solar_system_id: sys_id,
                state: "Offered".to_string(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 71_901,
                agent_id: ag.agent_id,
                offered_at_unix: 1,
                offer_expiry_unix: 1,
                ..Default::default()
            },
        );
        snap.active_missions.insert(
            (char_id, 71_902),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 71_902,
                agent_id: ag.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".to_string(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 71_902,
                agent_id: ag.agent_id,
                offered_at_unix: 1,
                offer_expiry_unix: 100,
                accepted_at_unix: Some(1),
                accept_expiry_unix: Some(1),
                ..Default::default()
            },
        );

        // Add R&D research entry so GetMyJournalDetails accrues RP
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: ag.agent_id,
                research_type_id: 11433,
                points_per_day: 24.0,
                initial_points: 10.0,
                started_unix: now.saturating_sub(7200),
            }],
        );
    });

    for &(mid, _, _, expected_code) in &states {
        let jinfo = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetMissionJournalInfo",
            &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(jinfo.dict_get("missionState"), Some(&PyRep::Integer(expected_code)));

        let obj = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(obj.dict_get("objectives").is_some());
    }

    // GetEntryPoint with bookmark_id None vs Some
    let ep_none = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetEntryPoint",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(71_001)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = ep_none;
    let ep_some = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetEntryPoint",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(71_002)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = ep_some;

    // Trigger expiry sweep & R&D accrual in GetMyJournalDetails
    let journal = handle(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(journal, PyRep::Tuple(_)));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions.get(&(char_id, 71_901)).map(|m| m.state.as_str()),
            Some("Expired")
        );
        assert_eq!(
            snap.active_missions.get(&(char_id, 71_902)).map(|m| m.state.as_str()),
            Some("Failed")
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn do_action_accept_guards_lp_tax_and_remote_complete_rejection() {
    let mut ctx = test_ctx();
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        for (mid_k, title_k) in [(72_003_u32, "Tax Test Mission"), (72_101_u32, "Epic Arc Test Mission")] {
            sde.missions.entry(mid_k).or_insert(eve_sde::MissionTemplate {
                mission_id: mid_k,
                title: title_k.into(),
                is_courier: false,
                lp_alpha: 100,
                lp_omega: 200,
                has_standing_rewards: true,
                content_template: None,
                dungeon_id: None,
            });
        }
    }
    let char_id = 91_001_004;
    let ag = find_lvl1_agent(&ctx);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    ensure_char(&ctx, char_id, Some(st_id), sys_id);

    // 1. Level 5 agent in highsec returns CantDoMission if present
    if let Some(l5_hs) = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 5
                && a.agent_type_id == 2
                && a.solar_system_id.is_some_and(|s| {
                    ctx.sde.solar_systems.get(&s).is_some_and(|sys| sys.security_status >= 0.45)
                })
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let mut s = char_session(char_id, l5_hs.solar_system_id.unwrap(), l5_hs.station_id);
        let err = handle(
            &ctx,
            &mut s,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(l5_hs.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "CantDoMission");
    }

    // 2. Accept guards (action 3 not co-located, action 5 non-storyline cannot accept remotely)
    let mut sess_docked = char_session(char_id, sys_id, Some(st_id));
    let offer = handle(
        &ctx,
        &mut sess_docked,
        "agentMgr",
        "GetOffer",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let mid = offer.dict_get("missionID").unwrap().as_i64().unwrap() as u32;

    let mut wrong_sess = char_session(char_id, sys_id, Some(st_id + 1));
    let err_coloc = handle(
        &ctx,
        &mut wrong_sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_coloc.code, "AgentNotCoLocated");

    let err_remote_acc = handle(
        &ctx,
        &mut wrong_sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(5), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_remote_acc.code, "CannotAcceptRemotely");

    // Alpha clone blocked on L4 accept
    if let Some(l4_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 4
                && a.agent_type_id == 2
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().clone_grade = 1;
            snap.active_missions.insert(
                (char_id, 72_001),
                ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: 72_001,
                    agent_id: l4_ag.agent_id,
                    solar_system_id: l4_ag.solar_system_id.unwrap(),
                    state: "Offered".into(),
                    current_room_id: 1,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: Vec::new(),
                    bookmark_id: None,
                },
            );
        });
        let mut l4_sess = char_session(char_id, l4_ag.solar_system_id.unwrap(), l4_ag.station_id);
        let err_alpha = handle(
            &ctx,
            &mut l4_sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(l4_ag.agent_id), PyRep::Integer(72_001)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_alpha.code, "CantDoMissionAlphaClone");
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        });
    }

    // 3. Action 7 (CompleteRemotely) fails with CannotCompleteRemotely when delivery required
    let mid_deliv = ctx
        .sde
        .missions
        .keys()
        .copied()
        .filter(|&id| id != mid && id != 72_001 && id != 72_003 && id != 72_101)
        .min()
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mid_deliv),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: mid_deliv,
                agent_id: ag.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: mid_deliv,
                agent_id: ag.agent_id,
                required_delivery_type_id: Some(34),
                required_delivery_qty: 1,
                destination_station_id: Some(st_id),
                ..Default::default()
            },
        );
        snap.items.insert(
            990_001,
            InventoryItemRecord {
                item_id: 990_001,
                owner_id: char_id,
                location_id: st_id,
                type_id: 34,
                quantity: 5,
                flag: InventoryFlag::Hangar,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let err_rem_comp = handle(
        &ctx,
        &mut sess_docked,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(7), PyRep::Integer(ag.agent_id), PyRep::Integer(mid_deliv as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_rem_comp.code, "CannotCompleteRemotely");

    // 4. Out-of-range raw_action (e.g. 99) defaults to offer/dialogue without panic when bound, and actions 9..=11 decline/quit
    let bound_ag = crate::router::BoundObjectEntry {
        bound_id: 2,
        session_id: sess_docked.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: ag.agent_id,
        oid_str: format!("agentMgr:{}", ag.agent_id),
        ref_id: 2,
    };
    let raw_99 = handle_with_bound(
        &ctx,
        &mut sess_docked,
        Some(&bound_ag),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(99), PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(raw_99.dict_get("missionID").is_some());

    let declined = handle(
        &ctx,
        &mut sess_docked,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(declined.dict_get("state"), Some(&PyRep::String("Declined".into())));

    // 5. NPC corp LP tax: >= 100_000 ISK deducts 11% LP when char is in non-exempt NPC corp
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.remove(&(char_id, mid_deliv));
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009; // NPC corp, non-exempt
        snap.active_missions.insert(
            (char_id, 72_003),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 72_003,
                agent_id: ag.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 72_003,
                agent_id: ag.agent_id,
                offered_level: Some(4), // >= 100k ISK reward -> 11% NPC corp LP tax applies
                ..Default::default()
            },
        );
    });
    let CharacterMissionRewards {
        isk_reward: isk_r,
        lp_reward: raw_lp,
        ..
    } = compute_character_mission_rewards(&ctx, char_id, ag.agent_id, 72_003, sys_id);
    assert!(isk_r >= 100_000, "expected >= 100k ISK for tax test, got {isk_r}");
    let lp_before = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&ag.corporation_id).copied().unwrap_or(0));
    let comp_tax = handle(
        &ctx,
        &mut sess_docked,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(72_003)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(comp_tax.dict_get("state"), Some(&PyRep::String("Completed".into())));
    let lp_after = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&ag.corporation_id).copied().unwrap_or(0));
    let lp_gained = lp_after - lp_before;
    let expected_taxed_lp = raw_lp - ((raw_lp.saturating_mul(11) + 50) / 100);
    assert_eq!(lp_gained, expected_taxed_lp, "expected 11% NPC corp LP tax ({raw_lp} -> {expected_taxed_lp})");

    // 6. Paragon corp (1_000_437) LP credit & Epic Arc completion/re-start clearing completed_missions
    if let Some(epic_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 10 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let (arc_id, _fac_id) = resolve_epic_arc_id_for_agent(&ctx, epic_ag.agent_id);
        let esys = epic_ag.solar_system_id.unwrap();
        let est = epic_ag.station_id;
        let mut esess = char_session(char_id, esys, est);
        ctx.db.with_mut_snapshot(|snap| {
            let entries = vec![EpicArcStateEntry {
                epic_arc_id: arc_id,
                agent_id: epic_ag.agent_id,
                faction_id: 500001,
                chapter: 1,
                completed_missions: vec![72_100],
                chosen_branch: None,
                completed_at_unix: None,
                cooldown_until_unix: Some(0),
            }];
            save_epic_arc_states(snap, char_id, &entries);
            snap.active_missions.insert(
                (char_id, 72_101),
                ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: 72_101,
                    agent_id: epic_ag.agent_id,
                    solar_system_id: esys,
                    state: "Accepted".into(),
                    current_room_id: 1,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: Vec::new(),
                    bookmark_id: None,
                },
            );
        });
        let comp_arc = handle(
            &ctx,
            &mut esess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(epic_ag.agent_id), PyRep::Integer(72_101)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(comp_arc.dict_get("state"), Some(&PyRep::String("Completed".into())));
    }

    // 7. RemoveOfferFromJournal on unknown mission id
    let rm_missing = handle(
        &ctx,
        &mut sess_docked,
        "agentMgr",
        "RemoveOfferFromJournal",
        &[PyRep::Integer(99_999_999)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(rm_missing.code, "MissionNotActive");
}
