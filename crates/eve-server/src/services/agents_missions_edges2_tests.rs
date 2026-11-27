//! Additional edge-case and branch coverage tests for `services::agents_missions`.

use super::*;
use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    EpicArcStateEntry, RndResearchEntry, save_epic_arc_states, save_rnd_research_entries,
};
use crate::services::mission_fixture::{self, MissionFixture};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
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

    // mission_site_origin_for_instance determinism & distinctness + epic_arc_required_missions fallback
    let p1 = mission_site_origin_for_instance(&ctx, sys_id, 1234);
    let p2 = mission_site_origin_for_instance(&ctx, sys_id, 1234);
    let p3 = mission_site_origin_for_instance(&ctx, sys_id, 1235);
    assert_eq!(p1, p2);
    assert!(p1.distance(p3) >= 250_000.0);
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

    // WarpToLocation / GotoLocation / WarpToAgentInSpace reject docked callers (#1367)
    let goto_err = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(goto_err.code, "ShipDocked");
    let warp_agent_err = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToAgentInSpace",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(warp_agent_err.code, "ShipDocked");
    let warp_loc_err = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_DUNGEON.into()), PyRep::Integer(0), PyRep::Real(0.0)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(warp_loc_err.code, "ShipDocked");
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
    let none_missing = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(88_888_001)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(none_missing, PyRep::None);
    let err_missing = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionJournalInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(88_888_001)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "MissionNotActive");

    // Seed missions across valid typed MissionState variants and objective types
    let now = ctx.now_unix() as u64;
    assert!(
        eve_db::MissionState::parse("MysteryState").is_none(),
        "unrecognized mission state must not parse into MissionState"
    );
    let states = [
        (71_001_u32, "Offered", "Trade", 1),
        (71_002_u32, "Accepted", "Courier", 2),
        (71_003_u32, "Failed", "Mining", 3),
        (71_004_u32, "Completed", "Combat", 4),
        (71_005_u32, "Declined", "Combat", 5),
        (71_006_u32, "Quit", "Combat", 6),
        (71_007_u32, "Expired", "Combat", 3),
    ];

    ctx.db.with_mut_snapshot(|snap| {
        for &(mid, st_str, _mtype, _) in &states {
            mission_fixture::insert(
                snap,
                MissionFixture::new(char_id, mid, ag.agent_id, sys_id, st_str)
                    .current_room_id(1)
                    .npcs_remaining(3)
                    .bookmark_id(if mid == 71_002 { Some(9001) } else { None })
                    .offered_at_unix(now.saturating_sub(120))
                    .offer_expiry_unix(now + 3600)
                    .accepted_at_unix(Some(now.saturating_sub(60)))
                    .accept_expiry_unix(Some(now + 3600))
                    .required_delivery_type_id(Some(34))
                    .required_delivery_qty(10)
                    .destination_station_id(Some(st_id)),
            );
        }

        // Add expired Offered and expired Accepted missions to exercise sweep paths
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 71_901, ag.agent_id, sys_id)
                .current_room_id(1)
                .offered_at_unix(1)
                .offer_expiry_unix(1),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 71_902, ag.agent_id, sys_id)
                .current_room_id(1)
                .offered_at_unix(1)
                .offer_expiry_unix(100)
                .accepted_at_unix(Some(1))
                .accept_expiry_unix(Some(1)),
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
        assert!(mission_fixture::get(snap, char_id, 71_901).is_none());
        assert_eq!(
            mission_fixture::latest_history(snap, char_id, 71_901).as_deref(),
            Some("Expired")
        );
        assert!(mission_fixture::get(snap, char_id, 71_902).is_none());
        assert_eq!(
            mission_fixture::latest_history(snap, char_id, 71_902).as_deref(),
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
            mission_fixture::insert(
                snap,
                MissionFixture::offered(char_id, 72_001, l4_ag.agent_id, l4_ag.solar_system_id.unwrap())
                    .current_room_id(1),
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
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_deliv, ag.agent_id, sys_id)
                .current_room_id(1)
                .required_delivery_type_id(Some(34))
                .required_delivery_qty(1)
                .destination_station_id(Some(st_id)),
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

    // 5. NPC corp tax: 0% LP tax; 11% ISK tax independently on >= 100_000 ISK rewards in non-exempt NPC corp
    let l4_tax_ag = ctx
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
        .expect("SDE must contain an L4 basic agent");
    let l4_sys_id = l4_tax_ag.solar_system_id.unwrap();
    let mut sess_l4_tax = char_session(char_id, l4_sys_id, l4_tax_ag.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::remove(snap, char_id, mid_deliv);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = 1_000_009; // NPC corp, non-exempt
        ch.solar_system_id = l4_sys_id;
        ch.station_id = l4_tax_ag.station_id;
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 72_003, l4_tax_ag.agent_id, l4_sys_id)
                .current_room_id(1)
                .offered_level(Some(4)), // >= 100k ISK reward -> 11% NPC corp ISK tax applies; 0% LP tax
        );
        snap.items.insert(
            990_002,
            InventoryItemRecord {
                item_id: 990_002,
                owner_id: char_id,
                location_id: l4_tax_ag.station_id.unwrap(),
                type_id: 20,
                quantity: 250,
                flag: InventoryFlag::Hangar,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let CharacterMissionRewards {
        isk_reward: isk_r,
        bonus_reward: bonus_r,
        lp_reward: raw_lp,
    } = compute_character_mission_rewards(&ctx, char_id, l4_tax_ag.agent_id, 72_003, l4_sys_id);
    assert!(isk_r >= 100_000, "expected >= 100k ISK for tax test, got {isk_r}");
    let (bal_before, lp_before, j_start) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].balance.raw_cents(),
            s.characters[&char_id].loyalty_points.get(&l4_tax_ag.corporation_id).copied().unwrap_or(0),
            s.wallet_journal.len(),
        )
    });
    let comp_tax = handle(
        &ctx,
        &mut sess_l4_tax,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(l4_tax_ag.agent_id), PyRep::Integer(72_003)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(comp_tax.dict_get("state"), Some(&PyRep::String("Completed".into())));
    let bonus_paid = comp_tax.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(bonus_paid, bonus_r);
    let tax_for = |isk: i64| -> i64 {
        if isk >= 100_000 {
            ((isk * 100).saturating_mul(11) + 50) / 100
        } else {
            0
        }
    };
    let expected_isk_tax_cents = tax_for(isk_r);
    let expected_bonus_tax_cents = tax_for(bonus_paid);
    let expected_net_delta_cents =
        (isk_r * 100 - expected_isk_tax_cents) + (bonus_paid * 100 - expected_bonus_tax_cents);
    ctx.db.with_snapshot(|s| {
        let lp_after = s.characters[&char_id].loyalty_points.get(&l4_tax_ag.corporation_id).copied().unwrap_or(0);
        let lp_gained = lp_after - lp_before;
        assert_eq!(lp_gained, raw_lp, "NPC corps charge 0% LP tax ({raw_lp})");

        let bal_after = s.characters[&char_id].balance.raw_cents();
        assert_eq!(
            bal_after - bal_before,
            expected_net_delta_cents,
            "11% NPC corp ISK tax must be deducted independently from >= 100k ISK rewards"
        );
        let new_entries: Vec<_> = s.wallet_journal.iter().skip(j_start).collect();
        assert!(
            new_entries.iter().any(|e| {
                e.character_id == char_id
                    && e.entry_type == "agent_mission_reward_corporation_tax"
                    && e.amount.raw_cents() == -expected_isk_tax_cents
            }),
            "expected agent_mission_reward_corporation_tax entry for {expected_isk_tax_cents} cents"
        );
        if expected_bonus_tax_cents > 0 {
            assert!(
                new_entries.iter().any(|e| {
                    e.character_id == char_id
                        && e.entry_type == "agent_mission_time_bonus_reward_corporation_tax"
                        && e.amount.raw_cents() == -expected_bonus_tax_cents
                }),
                "expected agent_mission_time_bonus_reward_corporation_tax entry for {expected_bonus_tax_cents} cents"
            );
        }
    });

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
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_id, 72_101, epic_ag.agent_id, esys)
                    .current_room_id(1),
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

const TEST_LOC_DUNGEON: &str = "dungeon";
const TEST_LOC_BOOKMARK: &str = "bookmark";

async fn setup_undocked_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    start_pos: DVec3,
) -> (SessionState, crate::system_actor::SystemHandle) {
    ensure_char(ctx, char_id, None, sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: start_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
            SlimItemMeta::default(),
        )
        .await;
    let mut sess = char_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    (sess, sys_h)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_location_dungeon_initiates_warp_arrives_and_spawns_wave_gh1367() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;
    let agent_id = 3_012_050_i64; // Velmudainen Tykigi in Inoue
    let mission_id = 875_u32; // The Guristas Spies
    let char_id = 91_001_367_i64;
    let ship_id = 100_910_367_i64;
    let agent_station_id = ctx
        .sde
        .agents
        .get(&agent_id)
        .and_then(|a| a.station_id)
        .unwrap_or(60_003_910);
    let undock_anchor = ctx
        .sde
        .stations
        .get(&agent_station_id)
        .map(|st| st.position + DVec3::new(35_000.0, 0.0, 0.0))
        .unwrap_or(DVec3::new(35_000.0, 0.0, 0.0));

    ensure_char(&ctx, char_id, Some(agent_station_id), inoue_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = inoue_sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: agent_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, mission_id, agent_id, inoue_sys_id)
                .current_room_id(1),
        );
    });

    // Accept the offered mission via DoAction(3) while docked at the agent station.
    let mut docked_sess = char_session(char_id, inoue_sys_id, Some(agent_station_id));
    docked_sess.ship_id = Some(ship_id);
    let accept_rep = handle(
        &ctx,
        &mut docked_sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(agent_id),
            PyRep::Integer(i64::from(mission_id)),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        accept_rep.dict_get("state"),
        Some(&PyRep::String("Accepted".into()))
    );

    // Docked pilot calling WarpToLocation('dungeon', ...) must fail with ShipDocked.
    let docked_err = handle(
        &ctx,
        &mut docked_sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(TEST_LOC_DUNGEON.into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");

    // Simulate downtime/stale NPC ball IDs so pocket entry must spawn fresh mission NPCs.
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, char_id, mission_id, |m| {
            m.spawned_npc_ball_ids = vec![999_999_888];
            m.npcs_remaining = 3;
        });
    });

    let (mut space_sess, sys_h) =
        setup_undocked_pilot(&ctx, char_id, ship_id, inoue_sys_id, undock_anchor).await;
    let first_station_pos = ctx
        .sde
        .solar_systems
        .get(&inoue_sys_id)
        .and_then(|s| s.stations.first().map(|st| st.position))
        .unwrap_or(DVec3::ZERO);
    let site_origin = ctx.db.with_snapshot(|snap| {
        let iid = snap
            .missions
            .get_by_character_template(char_id, mission_id)
            .unwrap()
            .instance_id;
        mission_site_origin_for_instance(&ctx, inoue_sys_id, iid)
    });
    assert!(
        site_origin.distance(first_station_pos) > 150_000.0,
        "mission site must sit well outside station undock grids"
    );

    // Exact V24 client payload for Encounter Coordinate -> Warp to Location.
    let formation = PyRep::Dict(vec![
        (PyRep::String("formationType".into()), PyRep::Integer(0)),
        (
            PyRep::String("formationSpacing".into()),
            PyRep::Integer(1_000),
        ),
        (
            PyRep::String("formationSize".into()),
            PyRep::Integer(10_000),
        ),
    ]);
    let warp_rep = handle(
        &ctx,
        &mut space_sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::None,
            formation,
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(warp_rep, PyRep::Bool(true));

    // Verify Destiny ball immediately entered MovementMode::Warp toward site_origin.
    let (_, warping_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dest = match warping_ball.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected ship in MovementMode::Warp, got {other:?}"),
    };
    assert!(
        dest.distance(site_origin) < 1_000.0,
        "warp destination {dest:?} should match dungeon origin {site_origin:?}"
    );

    // Verify stale NPC ball was reconciled and pocket NPC wave spawned in ballpark near site_origin.
    let spawned_ids = ctx.db.with_snapshot(|snap| {
        mission_fixture::get(snap, char_id, mission_id)
            .map(|m| (m.npcs_remaining, m.spawned_npc_ball_ids))
            .unwrap()
    });
    assert!(spawned_ids.0 > 0);
    assert!(!spawned_ids.1.is_empty());
    assert!(!spawned_ids.1.contains(&999_999_888));
    for &npc_id in &spawned_ids.1 {
        let npc_pos = sys_h
            .get_ball_position_sync(npc_id)
            .expect("mission NPC ball must be spawned in system ballpark");
        assert!(npc_pos.distance(site_origin) < 50_000.0);
    }

    // Advance simulation until warp completes and confirm ship lands near the spawned NPCs.
    for _ in 0..80 {
        let _ = sys_h.step_manual(1.0).await.unwrap();
        let (_, b) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if !matches!(b.mode, MovementMode::Warp { .. }) {
            break;
        }
    }
    let (_, landed_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        !matches!(landed_ball.mode, MovementMode::Warp { .. }),
        "ship should complete warp within 80s"
    );
    assert!(landed_ball.position.distance(site_origin) < 10_000.0);
    let first_npc_pos = sys_h.get_ball_position_sync(spawned_ids.1[0]).unwrap();
    assert!(
        landed_ball.position.distance(first_npc_pos) < 50_000.0,
        "landed ship must have mission NPCs reachable on grid"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_location_range_homebase_objective_and_agent_in_space() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;
    let agent_id = 3_012_050_i64;
    let home_st_id = ctx
        .sde
        .agents
        .get(&agent_id)
        .and_then(|a| a.station_id)
        .unwrap_or(60_003_910);
    let dest_st_id = ctx
        .sde
        .solar_systems
        .get(&inoue_sys_id)
        .and_then(|s| s.stations.iter().find(|st| st.station_id != home_st_id))
        .map(|st| st.station_id)
        .expect("Inoue must have a second station");
    let home_pos = ctx.sde.stations.get(&home_st_id).unwrap().position;
    let dest_pos = ctx.sde.stations.get(&dest_st_id).unwrap().position;

    let char_id = 91_001_368_i64;
    let ship_id = 100_910_368_i64;
    let mission_id = 875_u32;
    // Start far (> 1 AU / >= 1,000 km) from both stations and dungeon so every leg > 150 km.
    let start_pos = home_pos + DVec3::new(10_000_000.0, -5_000_000.0, 0.0);
    let (mut sess, sys_h) =
        setup_undocked_pilot(&ctx, char_id, ship_id, inoue_sys_id, start_pos).await;

    let mission_iid = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mission_id, agent_id, inoue_sys_id)
                .current_room_id(1)
                .npcs_remaining(2)
                .offered_at_unix(1)
                .offer_expiry_unix(u64::MAX / 2)
                .accepted_at_unix(Some(1))
                .accept_expiry_unix(Some(u64::MAX / 2))
                .origin_station_id(Some(home_st_id))
                .destination_station_id(Some(dest_st_id)),
        );
        snap.missions
            .get_by_character_template(char_id, mission_id)
            .unwrap()
            .instance_id
    });
    let site_origin = mission_site_origin_for_instance(&ctx, inoue_sys_id, mission_iid);

    // 1. Non-zero warpRange (25 km) lands 25 km short of dungeon anchor while NPCs spawn at anchor.
    let r_dungeon = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(25_000.0),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r_dungeon, PyRep::Bool(true));
    let (_, b1) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let d1 = match b1.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected Warp, got {other:?}"),
    };
    assert!((d1.distance(site_origin) - 25_000.0).abs() < 500.0);

    // Reset ship to start_pos (Stop) for next legs.
    let _ = sys_h.set_ball_position(ship_id, start_pos).await;
    let _ = sys_h
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;

    // 2. "agenthomebase" warps to agent's home station docking perimeter (outside station radius).
    let r_home = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("agenthomebase".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r_home, PyRep::Bool(true));
    let (_, b2) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let d2 = match b2.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected Warp, got {other:?}"),
    };
    let home_st = ctx.sde.stations.get(&home_st_id).unwrap();
    let st_radius = ctx
        .sde
        .types
        .get(&home_st.station_type_id)
        .map(|t| t.radius)
        .unwrap_or(5_000.0);
    assert!(d2.distance(home_pos) >= st_radius - 1.0);

    // 3. "objective.destination" via GotoLocation warps to mission destination station.
    let _ = sys_h
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;
    let r_obj = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[PyRep::String("objective.destination".into()), PyRep::Integer(0)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r_obj, PyRep::Bool(true));
    let (_, b3) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let d3 = match b3.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected Warp, got {other:?}"),
    };
    assert!(d3.distance(dest_pos) < 100_000.0);

    // 4. WarpToAgentInSpace targets an in-space agent ball directly.
    let _ = sys_h
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;
    let space_agent_id = 3_099_901_i64;
    let agent_space_pos = start_pos + DVec3::new(0.0, 2_000_000.0, 0.0);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: space_agent_id,
                type_id: 24,
                position: agent_space_pos,
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
    let r_ag = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToAgentInSpace",
        &[PyRep::Integer(space_agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(r_ag, PyRep::Bool(true));
    let (_, b4) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match b4.mode {
        MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(agent_space_pos) < 1.0);
        }
        other => panic!("expected Warp to in-space agent, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_location_negative_guards_do_not_mutate_state() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;
    let jita_sys_id = 30_000_142_u32;
    let char_id = 91_001_369_i64;
    let ship_id = 100_910_369_i64;

    // 1. Unauthenticated session -> error without character
    let mut anon = SessionState::default();
    assert!(
        handle(
            &ctx,
            &mut anon,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .is_err()
    );

    // 2. No active ship -> NoActiveShip
    let mut no_ship = char_session(char_id, inoue_sys_id, None);
    no_ship.ship_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut no_ship,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveShip"
    );

    // 3. Undocked session without ball in system -> ShipNotInSpace
    let mut missing_ball = char_session(char_id, inoue_sys_id, None);
    missing_ball.ship_id = Some(ship_id + 99);
    assert_eq!(
        handle(
            &ctx,
            &mut missing_ball,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // Spawn stopped ship in Inoue.
    let mission_id = 875_u32;
    let site_origin = mission_site_origin_for_instance(&ctx, inoue_sys_id, 1);
    let close_pos = site_origin + DVec3::new(50_000.0, 0.0, 0.0); // < 150 km from site_origin
    let (mut sess, sys_h) =
        setup_undocked_pilot(&ctx, char_id, ship_id, inoue_sys_id, close_pos).await;

    // 4. Non-finite warpRange -> InvalidArgument
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String(TEST_LOC_DUNGEON.into()),
                PyRep::Integer(0),
                PyRep::Real(f64::NAN),
            ],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // 5. Missing active mission -> InvalidMissionLocation
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMissionLocation"
    );

    // 6. Cross-system mission in Jita while pilot is in Inoue -> CrossSystemWarp
    let jita_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(jita_sys_id) && a.station_id.is_some())
        .map(|a| a.agent_id)
        .unwrap_or(3_008_416);
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mission_id, jita_agent, jita_sys_id)
                .current_room_id(1)
                .npcs_remaining(2),
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrossSystemWarp"
    );

    // 7. Same-system mission < 150 km away -> WarpDistanceTooShort
    let close_origin = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, char_id, mission_id, |m| {
            m.solar_system_id = inoue_sys_id;
            m.agent_id = 3_012_050;
        });
        let iid = snap
            .missions
            .get_by_character_template(char_id, mission_id)
            .unwrap()
            .instance_id;
        mission_site_origin_for_instance(&ctx, inoue_sys_id, iid)
    });
    let _ = sys_h
        .set_ball_position(ship_id, close_origin + DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_DUNGEON.into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpDistanceTooShort"
    );

    // Rejected warps must leave ship stopped with no spawned NPC balls.
    let (_, final_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(final_ball.mode, MovementMode::Stop));
    let npc_ids = ctx.db.with_snapshot(|snap| {
        mission_fixture::get(snap, char_id, mission_id)
            .map(|m| m.spawned_npc_ball_ids)
            .unwrap()
    });
    assert!(npc_ids.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_location_bookmark_acl_expiry_and_zarzakh_enforcement() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;
    let zarzakh_sys_id = 30_100_000_u32;
    let char_a = 91_001_371_i64;
    let ship_a = 100_910_371_i64;
    let char_b = 91_001_372_i64;
    let char_z = 91_001_373_i64;
    let ship_z = 100_910_373_i64;
    let corp_a = 1_000_009_i64;
    let corp_b = 9_800_777_i64;

    let origin = mission_site_origin_for_instance(&ctx, inoue_sys_id, 875);
    let start_pos = origin + DVec3::new(300_000.0, 0.0, 0.0);
    let (mut sess_a, sys_h) =
        setup_undocked_pilot(&ctx, char_a, ship_a, inoue_sys_id, start_pos).await;
    sess_a.corporation_id = Some(corp_a);
    let (mut sess_z, _) =
        setup_undocked_pilot(&ctx, char_z, ship_z, zarzakh_sys_id, DVec3::new(500_000.0, 0.0, 0.0))
            .await;

    let bm_private_b = 700_101_i64;
    let bm_shared_no_folder_b = 700_102_i64;
    let bm_corp_b = 700_103_i64;
    let bm_acl_view = 700_104_i64;
    let bm_acl_use = 700_105_i64;
    let bm_expired_a = 700_106_i64;
    let bm_zarzakh = 700_107_i64;
    let unauth_coords = start_pos + DVec3::new(0.0, 400_000.0, 0.0);
    let acl_use_coords = start_pos + DVec3::new(0.0, 0.0, 450_000.0);

    let corp_folder_b = 800_101_i64;
    let acl_view_folder = 800_102_i64;
    let acl_use_folder = 800_103_i64;
    let grp_a = 810_101_i64;
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(grp_a, vec![char_a]);
        snap.bookmark_folders.insert(
            corp_folder_b,
            eve_db::BookmarkFolderRecord {
                folder_id: corp_folder_b,
                owner_id: char_b,
                folder_name: "Corp B".into(),
                is_shared: true,
                corporation_id: Some(corp_b),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            acl_view_folder,
            eve_db::BookmarkFolderRecord {
                folder_id: acl_view_folder,
                owner_id: char_b,
                folder_name: "ACL View Only".into(),
                is_shared: true,
                view_group_id: Some(grp_a),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            acl_use_folder,
            eve_db::BookmarkFolderRecord {
                folder_id: acl_use_folder,
                owner_id: char_b,
                folder_name: "ACL Use".into(),
                is_shared: true,
                use_group_id: Some(grp_a),
                ..Default::default()
            },
        );

        let mk_bm = |id: i64, owner: i64, sys: u32, pos: DVec3, folder: Option<i64>, shared: bool| {
            eve_db::BookmarkRecord {
                bookmark_id: id,
                owner_id: owner,
                label: format!("bm-{id}"),
                solar_system_id: sys,
                x: pos.x,
                y: pos.y,
                z: pos.z,
                item_id: None,
                memo: String::new(),
                folder_id: folder,
                is_shared: shared,
            }
        };
        snap.bookmarks.insert(bm_private_b, mk_bm(bm_private_b, char_b, inoue_sys_id, unauth_coords, None, false));
        snap.bookmarks.insert(bm_shared_no_folder_b, mk_bm(bm_shared_no_folder_b, char_b, inoue_sys_id, unauth_coords, None, true));
        snap.bookmarks.insert(bm_corp_b, mk_bm(bm_corp_b, char_b, inoue_sys_id, unauth_coords, Some(corp_folder_b), true));
        snap.bookmarks.insert(bm_acl_view, mk_bm(bm_acl_view, char_b, inoue_sys_id, unauth_coords, Some(acl_view_folder), true));
        snap.bookmarks.insert(bm_acl_use, mk_bm(bm_acl_use, char_b, inoue_sys_id, acl_use_coords, Some(acl_use_folder), true));
        snap.bookmarks.insert(bm_expired_a, mk_bm(bm_expired_a, char_a, inoue_sys_id, unauth_coords, None, false));
        snap.bookmark_expiries.insert(bm_expired_a, now_unix.saturating_sub(10));
        snap.bookmarks.insert(bm_zarzakh, mk_bm(bm_zarzakh, char_z, zarzakh_sys_id, DVec3::new(0.0, 300_000.0, 0.0), None, false));

        // Mission whose bookmark_id points to an unauthorized shared bookmark must ignore its coords.
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_a, 877, 3_012_050, inoue_sys_id)
                .current_room_id(1)
                .npcs_remaining(1)
                .bookmark_id(Some(bm_shared_no_folder_b)),
        );
    });

    for (bmid, expected_code) in [
        (bm_private_b, "BookmarkNotFound"),
        (bm_shared_no_folder_b, "BookmarkNotFound"),
        (bm_corp_b, "BookmarkNotFound"),
        (bm_acl_view, "BookmarkNotFound"),
        (bm_expired_a, "BookmarkExpired"),
    ] {
        let err = handle(
            &ctx,
            &mut sess_a,
            "agentMgr",
            "WarpToLocation",
            &[PyRep::String(TEST_LOC_BOOKMARK.into()), PyRep::Integer(bmid)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code, "bookmark {bmid} must reject with {expected_code}");
    }

    let zarzakh_err = handle(
        &ctx,
        &mut sess_z,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_BOOKMARK.into()), PyRep::Integer(bm_zarzakh)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(zarzakh_err.code, "DunBlacklistCannotWarp");

    // Rejected warps left ship_a stopped.
    let (_, idle_ball) = sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(matches!(idle_ball.mode, MovementMode::Stop));

    // Mission dungeon warp must ignore unauthorized shared bm_shared_no_folder_b coordinates and warp to per-instance mission origin.
    let iid_877 = ctx.db.with_snapshot(|snap| {
        snap.missions
            .get_by_character_template(char_a, 877)
            .unwrap()
            .instance_id
    });
    let expected_mission_origin = mission_site_origin_for_instance(&ctx, inoue_sys_id, iid_877);
    let m_res = handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_DUNGEON.into()), PyRep::Integer(877)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(m_res, PyRep::Bool(true));
    let (_, m_ball) = sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    match m_ball.mode {
        MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(expected_mission_origin) < 1.0);
            assert!(destination.distance(unauth_coords) > 10_000.0);
        }
        other => panic!("expected warp for mission 877, got {other:?}"),
    }

    // Reset ship_a to Stop and verify ACL Use folder grants bookmark warp.
    let _ = sys_h.set_movement_mode(ship_a, MovementMode::Stop).await;
    let ok_res = handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_BOOKMARK.into()), PyRep::Integer(bm_acl_use)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_res, PyRep::Bool(true));
    let (_, ok_ball) = sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    match ok_ball.mode {
        MovementMode::Warp { destination, .. } => {
            assert!(destination.distance(acl_use_coords) < 1.0);
        }
        other => panic!("expected warp for ACL Use bookmark, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goto_location_v24_referring_agent_and_on_grid_vs_off_grid() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;
    let char_id = 91_001_374_i64;
    let ship_id = 100_910_374_i64;
    let mission_id = 876_u32;
    let agent_id = 3_012_050_i64;
    let other_agent_id = 3_008_416_i64;

    let (mut sess, sys_h) =
        setup_undocked_pilot(&ctx, char_id, ship_id, inoue_sys_id, DVec3::ZERO).await;
    let iid_876 = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mission_id, agent_id, inoue_sys_id)
                .current_room_id(1)
                .npcs_remaining(2),
        );
        snap.missions
            .get_by_character_template(char_id, mission_id)
            .unwrap()
            .instance_id
    });
    let site_origin = mission_site_origin_for_instance(&ctx, inoue_sys_id, iid_876);
    let off_grid_start = site_origin + DVec3::new(350_000.0, 0.0, 0.0);
    assert!(sys_h.set_ball_position(ship_id, off_grid_start).await);

    // P2 #1: explicit non-matching mission_id or bound agent must NOT fall back to an unrelated mission.
    let bad_mid = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_DUNGEON.into()), PyRep::Integer(999_999)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_mid.code, "InvalidMissionLocation");

    let other_bound = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: other_agent_id,
        oid_str: format!("agentMgr:{other_agent_id}"),
        ref_id: 1,
    };
    let bad_bound = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&other_bound),
        "agentMgr",
        "WarpToLocation",
        &[PyRep::String(TEST_LOC_DUNGEON.into()), PyRep::Integer(0)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_bound.code, "InvalidMissionLocation");

    // 1. Off-grid V24 GotoLocation("dungeon", mission_id, referringAgentID) must NOT clamp args[2] as 100 km warpRange.
    let off_grid_res = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[
            PyRep::String(TEST_LOC_DUNGEON.into()),
            PyRep::Integer(i64::from(mission_id)),
            PyRep::Integer(agent_id),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(off_grid_res, PyRep::Bool(true));
    let (_, warping_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match warping_ball.mode {
        MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(site_origin) < 1.0,
                "GotoLocation off-grid must land at 0 m (referringAgentID not parsed as warpRange)"
            );
        }
        other => panic!("expected Warp for off-grid GotoLocation, got {other:?}"),
    }
    for _ in 0..80 {
        let _ = sys_h.step_manual(1.0).await.unwrap();
        if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(ship_id).await {
            if !matches!(b.mode, MovementMode::Warp { .. }) {
                break;
            }
        }
    }
    let (_, arrived_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(arrived_ball.position.distance(site_origin) <= 5_000.0);

    // 2. On-grid (< 150 km) GotoLocation("dungeon", mission_id, referringAgentID) executes subwarp GotoPoint
    //    and cancels ship protection + safe logoff.
    let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;
    let on_grid_pos = site_origin + DVec3::new(40_000.0, 0.0, 0.0);
    let _ = sys_h.set_ball_position(ship_id, on_grid_pos).await;
    let _ = sys_h
        .modify_combat_state(ship_id, |cs| {
            cs.undock_invulnerable_remaining_ticks = 30;
            cs.warp_exit_invulnerable_remaining_ticks = 20;
        })
        .await;
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            char_id,
            eve_db::SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: inoue_sys_id,
                started_unix: ctx.now_unix(),
                complete_at_unix: ctx.now_unix() + 30,
                completed: false,
            },
        );
    });

    let on_grid_res = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[
            PyRep::String(TEST_LOC_DUNGEON.into()),
            PyRep::Integer(i64::from(mission_id)),
            PyRep::Integer(agent_id),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(on_grid_res, PyRep::Bool(true));

    let (_, goto_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match goto_ball.mode {
        MovementMode::GotoDirection { direction } => {
            let expected_dir = (site_origin - on_grid_pos).normalize();
            assert!(direction.distance(expected_dir) < 1e-4);
            assert!(goto_ball.speed_fraction > 0.99);
        }
        other => panic!("expected subwarp GotoDirection for on-grid GotoLocation, got {other:?}"),
    }
    let cs_after = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after.undock_invulnerable_remaining_ticks, 0);
    assert_eq!(cs_after.warp_exit_invulnerable_remaining_ticks, 0);
    let has_safe_logoff = ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id));
    assert!(!has_safe_logoff);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_location_breaks_invuln_covert_cloak_safe_logoff_and_p2_reconcile() {
    let ctx = test_ctx();
    let inoue_sys_id = 30_002_738_u32;

    // Part A: normal ship clears undock_invuln, safe_logoff, and reconciles stale NPC IDs.
    let char_a = 91_001_375_i64;
    let ship_a = 100_910_375_i64;
    let mid_a = 878_u32;
    let (mut sess_a, sys_h) =
        setup_undocked_pilot(&ctx, char_a, ship_a, inoue_sys_id, DVec3::ZERO).await;
    let iid_a = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_a, mid_a, 3_012_050, inoue_sys_id)
                .current_room_id(1)
                .npcs_remaining(2)
                .spawned_npc_ball_ids(vec![9_999_001]), // stale missing ball ID
        );
        snap.missions
            .get_by_character_template(char_a, mid_a)
            .unwrap()
            .instance_id
    });
    let origin_a = mission_site_origin_for_instance(&ctx, inoue_sys_id, iid_a);
    assert!(
        sys_h
            .set_ball_position(ship_a, origin_a + DVec3::new(350_000.0, 0.0, 0.0))
            .await
    );
    let _ = sys_h
        .modify_combat_state(ship_a, |cs| {
            cs.undock_invulnerable_remaining_ticks = 25;
        })
        .await;
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            char_a,
            eve_db::SafeLogoffEntry {
                char_id: char_a,
                ship_id: ship_a,
                solar_system_id: inoue_sys_id,
                started_unix: ctx.now_unix(),
                complete_at_unix: ctx.now_unix() + 30,
                completed: false,
            },
        );
    });

    let res_a = handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(TEST_LOC_DUNGEON.into()),
            PyRep::Integer(i64::from(mid_a)),
            PyRep::Real(0.0),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_a, PyRep::Bool(true));
    assert_eq!(
        sys_h
            .get_combat_state(ship_a)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        0
    );
    assert!(!ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_a)));
    let npc_ids_a = ctx.db.with_snapshot(|snap| {
        mission_fixture::get(snap, char_a, mid_a)
            .map(|m| m.spawned_npc_ball_ids)
            .unwrap()
    });
    assert!(!npc_ids_a.is_empty());
    assert!(!npc_ids_a.contains(&9_999_001));

    // Part B: covert-cloaked ship retains cloak while still clearing undock_invuln and safe_logoff.
    let char_b = 91_001_376_i64;
    let ship_b = 100_910_376_i64;
    let cloak_mod_id = 100_910_399_i64;
    let mid_b = 879_u32;
    let (mut sess_b, _) =
        setup_undocked_pilot(&ctx, char_b, ship_b, inoue_sys_id, DVec3::ZERO).await;
    let iid_b = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_b, mid_b, 3_012_050, inoue_sys_id)
                .current_room_id(1)
                .npcs_remaining(1),
        );
        snap.missions
            .get_by_character_template(char_b, mid_b)
            .unwrap()
            .instance_id
    });
    let origin_b = mission_site_origin_for_instance(&ctx, inoue_sys_id, iid_b);
    assert!(
        sys_h
            .set_ball_position(ship_b, origin_b + DVec3::new(350_000.0, 0.0, 0.0))
            .await
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11_578, // Covert Ops Cloaking Device II
                owner_id: char_b,
                location_id: ship_b,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    let _ = sys_h
        .modify_combat_state(ship_b, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = true;
            cs.undock_invulnerable_remaining_ticks = 40;
        })
        .await;
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            char_b,
            eve_db::SafeLogoffEntry {
                char_id: char_b,
                ship_id: ship_b,
                solar_system_id: inoue_sys_id,
                started_unix: ctx.now_unix(),
                complete_at_unix: ctx.now_unix() + 30,
                completed: false,
            },
        );
    });

    let res_b = handle(
        &ctx,
        &mut sess_b,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String(TEST_LOC_DUNGEON.into()),
            PyRep::Integer(i64::from(mid_b)),
            PyRep::Real(0.0),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res_b, PyRep::Bool(true));

    let (_, ball_b) = sys_h.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert!(matches!(ball_b.mode, MovementMode::Warp { .. }));
    let cs_b = sys_h.get_combat_state(ship_b).await.unwrap().unwrap();
    assert!(cs_b.is_cloaked && cs_b.is_covert_cloak, "covert cloak must persist across warp");
    assert_eq!(cs_b.undock_invulnerable_remaining_ticks, 0);
    assert!(!ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_b)));
}
