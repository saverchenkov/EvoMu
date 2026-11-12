//! Extra unit and mutation-killing tests for `services::agents_missions::agents_standings_arcs`
//! covering R&D research persistence (`load_rnd_research_entries` / `save_rnd_research_entries`),
//! `resolve_epic_arc_id_for_agent` faction mappings, `is_pirate_entity` and `is_trig_or_edencom_entity`
//! corporation/agent resolution, negative derived faction standings, Epic Arc / Career Agent derived
//! standing suppression, R&D Start/Cancel research dialogue actions (12/13), Locator Agent levels 1..5
//! range/standing/wormhole/funds checks, and `agentMgr` / `missionTrackerMgr` / `standingMgr` / `air_npe` RPCs.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::agents_missions::agents_standings_arcs::*;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, NpeProgressRecord,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, NpcCorporationInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

fn insert_agent(
    sde: &mut SdeRegistry,
    agent_id: i64,
    corp_id: i64,
    division_id: u32,
    level: u8,
    agent_type_id: u32,
    is_locator: bool,
    research_skills: Vec<u32>,
) {
    sde.agents.insert(
        agent_id,
        AgentInfo {
            agent_id,
            name: format!("Agent_{agent_id}"),
            corporation_id: corp_id,
            division_id,
            level,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id,
            is_locator,
            research_skills,
        },
    );
}

#[test]
fn standings_epic_arc_ids_pirate_trig_derived_losses_and_career_suppression() {
    let mut ctx = make_ctx();
    let sde = Arc::make_mut(&mut ctx.sde);

    for (corp_id, fac_id, agent_id, agent_type) in [
        (1_900_001, 500_001, 3_900_001, 10),
        (1_900_002, 500_002, 3_900_002, 10),
        (1_900_003, 500_003, 3_900_003, 10),
        (1_900_004, 500_004, 3_900_004, 10),
        (1_900_010, 500_010, 3_900_010, 10),
        (1_900_011, 500_011, 3_900_011, 10),
        (1_900_026, 500_026, 3_900_026, 2),
        (1_900_012, 500_001, 3_900_012, 12),
    ] {
        sde.npc_corporations.insert(
            corp_id,
            NpcCorporationInfo {
                corporation_id: corp_id,
                name: format!("Corp_{corp_id}"),
                ticker_name: "TST".into(),
                faction_id: Some(fac_id),
                solar_system_id: Some(30_000_142),
                station_id: Some(60_003_760),
                tax_rate: 0.1,
                ceo_id: None,
            },
        );
        insert_agent(sde, agent_id, corp_id, 24, 1, agent_type, false, vec![]);
    }

    let mut career_m = sde.missions.values().next().unwrap().clone();
    career_m.mission_id = 999_010;
    career_m.title = "Balancing the Books (10 of 10)".into();
    sde.missions.insert(999_010, career_m);

    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_001), (2, 500_001));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_002), (3, 500_002));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_003), (4, 500_003));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_004), (5, 500_004));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_010), (6, 500_010));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_011), (7, 500_011));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_026), (1, 500_026));

    assert!(is_pirate_entity(&ctx, 1_900_010) && is_pirate_entity(&ctx, 3_900_010));
    assert!(!is_pirate_entity(&ctx, 3_900_001));
    assert!(is_trig_or_edencom_entity(&ctx, 1_900_026) && is_trig_or_edencom_entity(&ctx, 3_900_026));
    assert!(!is_trig_or_edencom_entity(&ctx, 3_900_001));

    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3359,
            TrainedSkillRecord {
                type_id: 3359,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, 0.0, false),
            (0.0, 0.0, 0.0, 0.0)
        );
        let (eff_zero_entry, conn_b, _, _) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, 0.0, true);
        assert!((eff_zero_entry - 2.0).abs() < 1e-9 && (conn_b - 2.0).abs() < 1e-9);

        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let mut neg_changes = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_001, -10.0, true, &mut neg_changes);
        assert!(neg_changes.iter().any(|&(fid, _, d)| fid == 500_001 && d < 0.0));
        assert!(neg_changes.iter().any(|&(fid, _, d)| fid == 500_003 && d < 0.0));
        assert!(!neg_changes.iter().any(|&(fid, _, _)| fid == 500_004));

        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let (_, epic_changes) = apply_mission_completion_standings(
            &ctx, snap, char_id, 3_900_001, 100, 1_900_001, 10.0, true,
        );
        assert!(epic_changes.iter().any(|&(fid, _, _)| fid == 500_001));
        assert!(!epic_changes.iter().any(|&(fid, _, _)| fid == 500_003 || fid == 500_004));

        let (_, career_changes) = apply_mission_completion_standings(
            &ctx, snap, char_id, 3_900_012, 999_010, 1_900_012, 10.0, true,
        );
        assert!(!career_changes.iter().any(|&(fid, _, _)| fid == 500_003 || fid == 500_004));
    });
}

#[test]
fn rnd_research_and_locator_agent_levels_and_error_guards() {
    let mut ctx = make_ctx();
    let sde = Arc::make_mut(&mut ctx.sde);
    let (char_id, target_id) = (90_000_001_i64, 90_000_002_i64);
    let (rnd_aid, loc_l2_aid, loc_l4_aid) = (3_910_001_i64, 3_910_002_i64, 3_910_004_i64);

    sde.npc_corporations.insert(
        1_000_009,
        NpcCorporationInfo {
            corporation_id: 1_000_009,
            name: "Caldari Provisions".into(),
            ticker_name: "CP".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.1,
            ceo_id: None,
        },
    );
    insert_agent(sde, rnd_aid, 1_000_009, 18, 1, 4, false, vec![11442]);
    insert_agent(sde, loc_l2_aid, 1_000_009, 24, 2, 2, true, vec![]);
    insert_agent(sde, loc_l4_aid, 1_000_009, 24, 4, 2, true, vec![]);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: rnd_aid,
                research_type_id: 11442,
                points_per_day: 25.0,
                initial_points: 10.0,
                started_unix: 1_700_000_000,
            }],
        );
        assert_eq!(load_rnd_research_entries(snap, char_id).len(), 1);
        assert_eq!(snap.char_research_agents[&char_id].len(), 1);
    });

    for (aid, args, expected) in [
        (999_999, vec![], "UnknownAgent"),
        (loc_l2_aid, vec![], "NotResearchAgent"),
        (rnd_aid, vec![PyRep::Integer(12), PyRep::Integer(rnd_aid), PyRep::Integer(-5)], "InvalidResearchSkill"),
    ] {
        let err = handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 12, aid, &args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, expected);
    }

    let cancel_ok = handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 13, rnd_aid, &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(cancel_ok.dict_get("researchCancelled").and_then(PyRep::as_bool), Some(true));

    for (sys_id, aid, args, expected) in [
        (30_000_142, rnd_aid, vec![], "AgentHasNoLocatorService"),
        (30_000_144, loc_l2_aid, vec![], "AgentNotCoLocated"),
        (30_000_142, loc_l2_aid, vec![PyRep::Integer(15), PyRep::Integer(target_id)], "InsufficientAgentStanding"),
    ] {
        let err = handle_special_dialogue_action(&ctx, &mut sess, char_id, sys_id, 15, aid, &args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, expected);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(1_000_009, 6.0);
        ch.balance = IskCents::from_isk(1_000);
        let mut tgt = ch.clone();
        tgt.character_id = target_id;
        tgt.solar_system_id = 31_000_005;
        snap.characters.insert(target_id, tgt);
    });

    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 15, loc_l2_aid, &[PyRep::Integer(15), PyRep::Integer(target_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "LocatorTargetInUnknownSpace"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&target_id).unwrap().solar_system_id = 30_000_142;
    });
    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 15, loc_l2_aid, &[PyRep::Integer(15), PyRep::Integer(target_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(500_000);
    });
    let l4_res = handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 16, loc_l4_aid, &[PyRep::Integer(16), PyRep::Integer(target_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(l4_res.dict_get("precision").and_then(PyRep::as_str), Some("AllKnownSpace"));
    assert_eq!(l4_res.dict_get("iskFee").and_then(PyRep::as_i64), Some(75_000));
}

#[tokio::test]
async fn extended_rpcs_plausible_missions_career_standings_mission_tracker_and_npe() {
    let mut ctx = make_ctx();
    let sde = Arc::make_mut(&mut ctx.sde);
    let (char_id, career_aid, herald_aid) = (90_000_001_i64, 3_920_001_i64, 3_920_002_i64);

    insert_agent(sde, career_aid, 1_000_009, 24, 1, 12, false, vec![]);
    insert_agent(sde, herald_aid, 1_000_009, 37, 1, 13, false, vec![]);

    let mut sess = SessionState {
        session_id: 901,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        for (mid, state) in [(701, "Completed"), (702, "Accepted")] {
            snap.active_missions.insert(
                (char_id, mid),
                ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: mid,
                    agent_id: career_aid,
                    state: state.into(),
                    solar_system_id: 30_000_142,
                    current_room_id: 0,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: vec![],
                    bookmark_id: None,
                },
            );
        }
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 702,
                agent_id: career_aid,
                required_delivery_type_id: Some(34),
                required_delivery_qty: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            999_501,
            InventoryItemRecord {
                item_id: 999_501,
                type_id: 34,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 120,
                ..Default::default()
            },
        );
        snap.npe_progress.insert(
            char_id,
            NpeProgressRecord {
                character_id: char_id,
                node_graph_id: 716,
                current_node_id: 5,
                completed: false,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            },
        );
    });

    assert_eq!(
        handle_extended(&ctx, &mut sess, "agentMgr", "GetCompletedCareerAgentIDs", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(career_aid), PyRep::Bool(true))])
    );
    assert_eq!(
        handle_extended(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetCompletedCareerAgentIDs",
            &[PyRep::List(vec![
                PyRep::Integer(career_aid),
                PyRep::Integer(3_018_935),
            ])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Dict(vec![
            (PyRep::Integer(career_aid), PyRep::Bool(true)),
            (PyRep::Integer(3_018_935), PyRep::Bool(false)),
        ])
    );
    assert_eq!(
        handle_extended(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetCompletedCareerAgentIDs",
            &[PyRep::List(vec![PyRep::Integer(-1)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle_extended(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AgentNotFound"
    );
    assert!(handle_extended(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(herald_aid)], &kw).await.unwrap().is_ok());
    assert!(handle_extended(&ctx, &mut sess, "agentMgr", "GetMessagesForEpicArcMissions", &[], &kw).await.unwrap().is_ok());

    ctx.bound_objects.write().unwrap().insert(
        1,
        BoundObjectEntry {
            bound_id: 1,
            session_id: sess.session_id,
            user_id: 1,
            character_id: char_id,
            service: "agentMgr".into(),
            object_id: career_aid,
            oid_str: format!("N=1:{career_aid}"),
            ref_id: 1,
        },
    );
    for attr in ["agentID", "factionID", "solarsystemID", "stationID"] {
        assert!(handle_extended(&ctx, &mut sess, "agentMgr", attr, &[], &kw).await.unwrap().is_ok());
    }

    let PyRep::List(items) = handle_extended(&ctx, &mut sess, "missionTrackerMgr", "UpdateAllMissions", &[PyRep::Integer(702)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected list")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].dict_get("objectivesCompleted").and_then(PyRep::as_bool), Some(true));

    assert!(handle_extended(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[], &kw).await.unwrap().is_ok());
    assert!(handle_extended(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[], &kw).await.unwrap().is_ok());
    assert!(handle_extended(&ctx, &mut sess, "air_npe", "skip_air_npe", &[], &kw).await.unwrap().is_ok());
}
