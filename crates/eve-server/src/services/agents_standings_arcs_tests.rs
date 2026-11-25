use super::*;
use crate::router::ServerContext;
use crate::services::mission_fixture::{self, MissionFixture};
use eve_machonet::SessionState;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::{AgentInfo, NpcCorporationInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

#[test]
fn effective_standings_derived_standings_and_notifications_work() {
    let mut ctx = make_test_context();
    ctx.db
        .acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    let mut sde = (*ctx.sde).clone();
    sde.npc_corporations.insert(
        1_000_009,
        NpcCorporationInfo {
            corporation_id: 1_000_009,
            name: "Caldari Provisions".into(),
            ticker_name: "CP".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.10,
            ceo_id: None,
        },
    );
    sde.agents.insert(
        3_018_921,
        AgentInfo {
            agent_id: 3_018_921,
            name: "Agent One".into(),
            corporation_id: 1_000_009,
            division_id: 24,
            level: 2,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 2,
            is_locator: true,
            research_skills: vec![],
        },
    );
    ctx.sde = Arc::new(sde);

    assert!(is_pirate_entity(&ctx, 500_010));
    assert!(!is_pirate_entity(&ctx, 500_001));
    assert!(is_trig_or_edencom_entity(&ctx, 500_026));
    assert!(!is_trig_or_edencom_entity(&ctx, 500_001));
    assert!(!static_npc_faction_standings().is_empty());
    assert!(!get_sde_npc_npc_standings(&ctx.sde).is_empty());

    let char_id = 90_000_001;
    let changes = ctx.db.with_mut_snapshot(|snap| {
        for (sid, lvl) in [
            (SKILL_SOCIAL, 4),
            (SKILL_DIPLOMACY, 5),
            (SKILL_CONNECTIONS, 5),
            (SKILL_CRIMINAL_CONNECTIONS, 5),
        ] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }

        let (eff_trig, c1, d1, cr1) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_026, 2.0, true);
        assert_eq!((eff_trig, c1, d1, cr1), (2.0, 0.0, 0.0, 0.0));

        let (eff_neg, _, dip_b, _) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, -5.0, true);
        assert!((dip_b - 3.0).abs() < 1e-9 && (eff_neg - (-2.0)).abs() < 1e-9);

        let (eff_pir, _, _, crim_b) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_010, 0.0, true);
        assert!((crim_b - 2.0).abs() < 1e-9 && (eff_pir - 2.0).abs() < 1e-9);

        let (eff_emp, conn_b, _, _) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, 5.0, true);
        assert!((conn_b - 1.0).abs() < 1e-9 && (eff_emp - 6.0).abs() < 1e-9);

        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(3_018_921, 1.0);
        ch.standings.insert(1_000_009, 3.0);
        ch.standings.insert(500_001, -5.5);
        let eff_ag = compute_effective_agent_standing(
            &ctx,
            snap,
            char_id,
            3_018_921,
            1_000_009,
            Some(500_001),
        );
        assert!(eff_ag < -2.0);

        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(500_001, 0.0);
        let (new_corp, changes) = apply_mission_completion_standings(
            &ctx,
            snap,
            char_id,
            3_018_921,
            100,
            1_000_009,
            10.0,
            true,
        );
        assert!(new_corp > 3.0);
        assert!(changes.iter().any(|(id, _, _)| *id == 500_001));
        assert!(changes.iter().any(|(id, _, d)| *id == 500_004 && *d < 0.0));
        changes
    });
    notify_standing_changes(&ctx, char_id, &changes);

    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnStandingSet"));
    assert!(notes.iter().any(|n| n.event == "OnStandingsModified"));
}

#[test]
fn special_dialogue_actions_cover_locator_agent_and_epic_arc_branching() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.npc_corporations.insert(
        1_000_009,
        NpcCorporationInfo {
            corporation_id: 1_000_009,
            name: "Caldari Provisions".into(),
            ticker_name: "CP".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.10,
            ceo_id: None,
        },
    );
    sde.agents.insert(
        3_018_921,
        AgentInfo {
            agent_id: 3_018_921,
            name: "Locator Agent".into(),
            corporation_id: 1_000_009,
            division_id: 24,
            level: 1,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 2,
            is_locator: true,
            research_skills: vec![],
        },
    );
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    let target_id = 90_000_002;
    let mut session = char_session(char_id, 98_000_001);
    session.station_id = Some(60_003_760);
    session.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(100_000.0);
        }
        let mut tgt = snap.characters[&char_id].clone();
        tgt.character_id = target_id;
        tgt.user_id = 502;
        tgt.name = "Target Pilot".into();
        tgt.solar_system_id = 30_000_142;
        tgt.station_id = Some(60_003_760);
        snap.characters.insert(target_id, tgt);
    });

    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut session,
            char_id,
            30_000_142,
            13,
            3_018_921,
            &[],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ResearchNotActive"
    );

    let rej = handle_special_dialogue_action(
        &ctx,
        &mut session,
        char_id,
        30_000_142,
        17,
        3_018_921,
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        rej.dict_get("locateRejected").unwrap().as_bool(),
        Some(true)
    );

    let loc = handle_special_dialogue_action(
        &ctx,
        &mut session,
        char_id,
        30_000_142,
        15,
        3_018_921,
        &[PyRep::Integer(15), PyRep::Integer(target_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        loc.dict_get("solarSystemID").unwrap().as_i64(),
        Some(30_000_142)
    );
    assert_eq!(loc.dict_get("iskFee").unwrap().as_i64(), Some(2_000));

    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut session,
            char_id,
            30_000_142,
            15,
            3_018_921,
            &[PyRep::Integer(15), PyRep::Integer(target_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocatorAgentOnCooldown"
    );

    let branch = handle_special_dialogue_action(
        &ctx,
        &mut session,
        char_id,
        30_000_142,
        18,
        3_018_921,
        &[
            PyRep::Integer(18),
            PyRep::Integer(3_018_921),
            PyRep::String("Caldari".into()),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        branch.dict_get("chosenBranch").unwrap().as_str(),
        Some("Caldari")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_extended_rpcs_cover_agent_mgr_epic_arc_tracker_standings_keeper_and_npe() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.agents.insert(
        3_018_921,
        AgentInfo {
            agent_id: 3_018_921,
            name: "Epic Agent".into(),
            corporation_id: 1_000_009,
            division_id: 24,
            level: 1,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 10,
            is_locator: false,
            research_skills: vec![],
        },
    );
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    let mut session = char_session(char_id, 98_000_001);
    session.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(1_000_009, 2.5);
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 2,
                agent_id: 3_018_921,
                faction_id: 500_001,
                chapter: 1,
                completed_missions: vec![10],
                chosen_branch: Some("Caldari".into()),
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 500, 3_018_921, 30_000_142)
                .npcs_remaining(1)
                .is_anomic_burner(true),
        );
        snap.items.insert(
            100_000_999,
            InventoryItemRecord {
                item_id: 100_000_999,
                type_id: 640, // Scorpion (group 27 Battleship)
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "agentMgr",
            "corporationID",
            &[PyRep::Integer(3_018_921)],
            &HashMap::new()
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Integer(1_000_009)
    );
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "agentMgr",
            "GetDisabledMissions",
            &[],
            &HashMap::new()
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "agentMgr",
            "GetReplayTimestamp",
            &[],
            &HashMap::new()
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let epic_status = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&epic_status, PyRep::Dict(ref d) if d.len() == 1));
    if let PyRep::Dict(arcs) = &epic_status {
        let PyRep::Dict(missions) = &arcs[0].1 else {
            panic!("GetMyEpicArcStatus must map epicArcID -> dict of missionID -> KeyVal (#639)");
        };
        assert!(!missions.is_empty());
        assert!(missions[0].1.dict_get("acceptedDate").is_some());
        assert!(missions[0].1.dict_get("completedDate").is_some());
        assert!(missions[0].1.dict_get("quitDate").is_some());
        assert!(missions[0].1.dict_get("nameID").is_some());
    }
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "epicArcStatus",
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(3_018_921)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "missionTrackerMgr",
            "IsInActiveDungeonID",
            &[],
            &HashMap::new()
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert!(matches!(
        handle_extended(&ctx, &mut session, "standingMgr", "GetStandingCompositions", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(ref v) if !v.is_empty()
    ));

    session.station_id = None;
    session.structure_id = None;
    session.ship_id = Some(100_000_999);
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(500)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipRestrictedByGate"
    );

    session.ship_id = None;
    assert_eq!(
        handle_extended(
            &ctx,
            &mut session,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(500)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "RoomNpcsRemaining"
    );

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, char_id, 500, |m| m.npcs_remaining = 0);
    });
    let gate_ok = handle_extended(
        &ctx,
        &mut session,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(500)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(gate_ok.dict_get("currentRoomID").unwrap().as_i64(), Some(1));

    let npe_state = handle_extended(
        &ctx,
        &mut session,
        "air_npe",
        "get_air_npe_state",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        npe_state.dict_get("enabled").unwrap().as_bool(),
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_disabled_missions_and_replay_timestamp_read_persisted_state() {
    let ctx = make_test_context();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut session = char_session(90_000_001, 98_000_001);
    let kw = HashMap::new();
    let sample_mission_id = *ctx.sde.missions.keys().next().expect("SDE mission");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops
            .agent_global_disabled_missions
            .insert(sample_mission_id);
        snap.fleet_and_social_ops
            .agent_replay_ts
            .insert((90_000_001, 3_018_921), ctx.now_unix() + 600);
    });

    let disabled = handle_extended(&ctx, &mut session, "agentMgr", "GetDisabledMissions", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(disabled, PyRep::List(vec![PyRep::Integer(i64::from(sample_mission_id))]));

    let bound = crate::router::BoundObjectEntry {
        bound_id: 42,
        session_id: session.session_id,
        user_id: 1,
        character_id: 90_000_001,
        service: "agentMgr".into(),
        object_id: 3_018_921,
        oid_str: "N=1:42".into(),
        ref_id: 42,
    };
    let expected_ft = ((ctx.now_unix() + 600) as i64) * 10_000_000 + 116_444_736_000_000_000;
    let replay = handle_extended_with_bound(
        &ctx,
        &mut session,
        Some(&bound),
        "agentMgr",
        "GetReplayTimestamp",
        &[PyRep::Integer(90_000_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(replay, PyRep::Integer(expected_ft));
}

#[test]
fn synthesized_npc_faction_standings_sign_count_and_bounds_kill_unary_mutants() {
    assert_eq!(SYNTHESIZED_NPC_FACTION_STANDINGS.len(), 294);
    let mut neg_count = 0usize;
    let mut pos_count = 0usize;
    let mut sum_tenths = 0i64;
    for &(from_faction, to_faction, standing) in SYNTHESIZED_NPC_FACTION_STANDINGS {
        assert_ne!(from_faction, to_faction);
        assert!(standing.is_finite() && (-10.0..=10.0).contains(&standing));
        assert_ne!(standing, 0.0);
        if standing < 0.0 {
            neg_count += 1;
        } else {
            pos_count += 1;
        }
        sum_tenths += (standing * 10.0).round() as i64;
    }
    assert_eq!(neg_count, 189);
    assert_eq!(pos_count, 105);
    assert_eq!(sum_tenths, -2334);
    assert!(SYNTHESIZED_NPC_FACTION_STANDINGS.contains(&(500_026, 500_027, -10.0)));
    assert!(SYNTHESIZED_NPC_FACTION_STANDINGS.contains(&(500_027, 500_026, -10.0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_arc_messages_cover_all_v24_arcs_and_chapters_for_unstarted_character() {
    let ctx = make_test_context();
    let mut session = char_session(90_000_001, 98_000_001);

    let rep = handle_extended(
        &ctx,
        &mut session,
        "agentMgr",
        "GetMessagesForEpicArcMissions",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();

    let PyRep::Dict(top) = rep else {
        panic!("GetMessagesForEpicArcMissions must return Dict");
    };
    let (_, PyRep::Dict(chapter_titles)) = top
        .iter()
        .find(|(k, _)| k.as_str() == Some("messages.epicMission.journalText.chapterTitle"))
        .expect("chapterTitle key must be present")
    else {
        panic!("chapterTitle must map missionID -> messageID");
    };
    let (_, PyRep::Dict(in_progress)) = top
        .iter()
        .find(|(k, _)| k.as_str() == Some("messages.epicMission.journalText.inProgressMessage"))
        .expect("inProgressMessage key must be present")
    else {
        panic!("inProgressMessage must map missionID -> messageID");
    };
    let (_, PyRep::Dict(completed)) = top
        .iter()
        .find(|(k, _)| k.as_str() == Some("messages.epicMission.journalText.completedMessage"))
        .expect("completedMessage key must be present")
    else {
        panic!("completedMessage must map missionID -> messageID");
    };

    let assert_unique_positive_map = |entries: &[(PyRep, PyRep)], label: &str| {
        let mut seen = std::collections::HashSet::new();
        for (k, v) in entries {
            let mid = k.as_i64().expect("missionID key must be integer");
            let msg_id = v.as_i64().expect("messageID value must be integer");
            assert!(mid > 0 && msg_id > 0, "{label} invalid entry ({mid}, {msg_id})");
            assert!(seen.insert(mid), "duplicate missionID {mid} in {label}");
        }
    };
    assert_eq!(chapter_titles.len(), EPIC_ARC_CHAPTER_TITLES.len());
    assert_eq!(in_progress.len(), EPIC_ARC_MISSION_SPECS.len());
    assert_eq!(completed.len(), EPIC_ARC_MISSION_SPECS.len());
    assert_unique_positive_map(chapter_titles, "chapterTitle");
    assert_unique_positive_map(in_progress, "inProgressMessage");
    assert_unique_positive_map(completed, "completedMessage");

    let lookup_msg = |entries: &[(PyRep, PyRep)], mid: i64| {
        entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(mid))
            .and_then(|(_, v)| v.as_i64())
    };

    // Exact V24 chapter-start mission -> chapterTitle messageIDs across all 9 active Epic Arcs
    for (expected_mid, expected_chapter_msg) in [
        (14117_i64, 140784_i64),
        (14849, 141900),
        (14623, 141388),
        (14746, 142238),
        (14851, 141611),
        (15018, 142536),
        (15143, 143100),
        (15029, 142820),
        (17130, 1017644),
        (17188, 1040276),
    ] {
        assert_eq!(
            lookup_msg(chapter_titles, expected_mid),
            Some(expected_chapter_msg),
            "mission {expected_mid} chapterTitle mismatch"
        );
    }

    // Exact inProgressMessage / completedMessage IDs across main Epic Arcs and NPE certification arcs
    for (mid, expected_prog, expected_comp) in [
        (14117_i64, 140983_i64, 140853_i64),
        (14849, 141596, 141903),
        (14623, 141368, 141401),
        (14746, 141507, 141508),
        (14851, 142325, 141620),
        (15015, 142521, 142522),
        (15030, 142840, 142841),
        (15565, 144600, 144590),
        (15665, 144950, 144953),
        (17130, 1016272, 1016271),
        (17188, 1040278, 1040277),
    ] {
        assert_eq!(
            lookup_msg(in_progress, mid),
            Some(expected_prog),
            "mission {mid} inProgressMessage mismatch"
        );
        assert_eq!(
            lookup_msg(completed, mid),
            Some(expected_comp),
            "mission {mid} completedMessage mismatch"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_my_epic_arc_status_handles_unstarted_offered_accepted_mid_arc_and_completed() {
    let ctx = make_test_context();
    let char_id = 90_000_001_i64;
    let mut session = char_session(char_id, 98_000_001);

    // 1. Unstarted state returns an empty dict cleanly.
    let unstarted = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unstarted, PyRep::Dict(Vec::new()));

    // 2. Offered initial Epic Arc mission (before EpicArcStateEntry exists).
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 14117, 3_019_001, 30_000_142),
        );
    });
    let offered = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(offered_arcs) = offered else { panic!() };
    let (_, PyRep::Dict(offered_missions)) = offered_arcs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(29))
        .expect("Arc 29 must appear when mission 14117 is Offered")
    else {
        panic!();
    };
    let m14117 = &offered_missions
        .iter()
        .find(|(k, _)| k.as_i64() == Some(14117))
        .expect("Mission 14117 present")
        .1;
    assert_eq!(m14117.dict_get("acceptedDate"), Some(&PyRep::None));
    assert_eq!(m14117.dict_get("completedDate"), Some(&PyRep::None));
    assert_eq!(m14117.dict_get("nameID").and_then(|v| v.as_i64()), Some(57289));
    assert_eq!(m14117.dict_get("state").and_then(|v| v.as_str()), Some("Active"));

    // 3. Accepted initial Epic Arc mission populates acceptedDate.
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, char_id, 14117, |m| {
            m.state = "Accepted".into();
            m.accepted_at_unix = Some(1_775_400_100);
        });
    });
    let accepted = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(accepted_arcs) = accepted else { panic!() };
    let (_, PyRep::Dict(accepted_missions)) = accepted_arcs.iter().find(|(k, _)| k.as_i64() == Some(29)).unwrap() else {
        panic!();
    };
    let m14117_acc = &accepted_missions[0].1;
    assert!(matches!(m14117_acc.dict_get("acceptedDate"), Some(PyRep::Integer(_))));
    assert_eq!(m14117_acc.dict_get("completedDate"), Some(&PyRep::None));
    assert_eq!(m14117_acc.dict_get("nameID").and_then(|v| v.as_i64()), Some(57289));

    // 4. Mid-arc: mission 14117 completed and mission 14118 offered via a different chapter agent.
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::remove(snap, char_id, 14117);
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 14118, 3_019_002, 30_000_142),
        );
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 29,
                agent_id: 3_019_001,
                faction_id: 500_021,
                chapter: 1,
                completed_missions: vec![14117],
                chosen_branch: None,
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
    });
    let mid_arc = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(mid_arcs) = mid_arc else { panic!() };
    let (_, PyRep::Dict(mid_missions)) = mid_arcs.iter().find(|(k, _)| k.as_i64() == Some(29)).unwrap() else {
        panic!();
    };
    assert_eq!(mid_missions.len(), 2);
    let mid_done = &mid_missions.iter().find(|(k, _)| k.as_i64() == Some(14117)).unwrap().1;
    let mid_next = &mid_missions.iter().find(|(k, _)| k.as_i64() == Some(14118)).unwrap().1;
    assert!(matches!(mid_done.dict_get("completedDate"), Some(PyRep::Integer(_))));
    assert_eq!(mid_next.dict_get("acceptedDate"), Some(&PyRep::None));
    assert_eq!(mid_next.dict_get("completedDate"), Some(&PyRep::None));
    assert_eq!(mid_next.dict_get("nameID").and_then(|v| v.as_i64()), Some(57269));

    // 5. Completed / on cooldown state.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::remove(snap, char_id, 14118);
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 29,
                agent_id: 3_019_001,
                faction_id: 500_021,
                chapter: 7,
                completed_missions: vec![14117, 14118],
                chosen_branch: None,
                completed_at_unix: Some(now),
                cooldown_until_unix: Some(now + 7_776_000),
            }],
        );
    });
    let completed = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(comp_arcs) = completed else { panic!() };
    let (_, PyRep::Dict(comp_missions)) = comp_arcs.iter().find(|(k, _)| k.as_i64() == Some(29)).unwrap() else {
        panic!();
    };
    assert_eq!(comp_missions.len(), 2);
    assert_eq!(comp_missions[0].1.dict_get("state").and_then(|v| v.as_str()), Some("Cooldown"));
    assert_eq!(comp_missions[0].1.dict_get("onCooldown"), Some(&PyRep::Bool(true)));
    assert!(matches!(comp_missions[0].1.dict_get("completedDate"), Some(PyRep::Integer(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_my_epic_arc_status_isolates_regular_missions_coalesces_legacy_ids_and_restarts() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.npc_corporations.insert(
        1_000_009,
        NpcCorporationInfo {
            corporation_id: 1_000_009,
            name: "Caldari Provisions".into(),
            ticker_name: "CP".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.10,
            ceo_id: None,
        },
    );
    sde.npc_corporations.insert(
        1_000_130,
        NpcCorporationInfo {
            corporation_id: 1_000_130,
            name: "Sisters of EVE".into(),
            ticker_name: "SOE".into(),
            faction_id: Some(500_021),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.10,
            ceo_id: None,
        },
    );
    sde.agents.insert(
        3_018_921,
        AgentInfo {
            agent_id: 3_018_921,
            name: "Regular Caldari Agent".into(),
            corporation_id: 1_000_009,
            division_id: 24,
            level: 2,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 2,
            is_locator: false,
            research_skills: vec![],
        },
    );
    sde.agents.insert(
        3_019_500,
        AgentInfo {
            agent_id: 3_019_500,
            name: "Caldari Epic Agent".into(),
            corporation_id: 1_000_009,
            division_id: 24,
            level: 4,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 10,
            is_locator: false,
            research_skills: vec![],
        },
    );
    sde.agents.insert(
        3_019_001,
        AgentInfo {
            agent_id: 3_019_001,
            name: "Sister Alitura".into(),
            corporation_id: 1_000_130,
            division_id: 24,
            level: 1,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            agent_type_id: 10,
            is_locator: false,
            research_skills: vec![],
        },
    );
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001_i64;
    let mut session = char_session(char_id, 98_000_001);

    // 1. Regular mission from a Caldari non-epic agent must NOT leak into Caldari epic arc state.
    ctx.db.with_mut_snapshot(|snap| {
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 2,
                agent_id: 3_019_500,
                faction_id: 500_001,
                chapter: 1,
                completed_missions: Vec::new(),
                chosen_branch: Some("CaldariBranch".into()),
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 88_001, 3_018_921, 30_000_142),
        );
    });
    let isolated = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(isolated_arcs) = isolated else { panic!() };
    assert_eq!(isolated_arcs.len(), 1);
    let (_, PyRep::Dict(arc2_missions)) = isolated_arcs.iter().find(|(k, _)| k.as_i64() == Some(2)).unwrap() else {
        panic!();
    };
    assert_eq!(arc2_missions.len(), 1);
    assert_eq!(arc2_missions[0].0.as_i64(), Some(0));
    assert_eq!(arc2_missions[0].1.dict_get("nameID").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(arc2_missions[0].1.dict_get("chosenBranch").and_then(|v| v.as_str()), Some("CaldariBranch"));
    assert_eq!(arc2_missions[0].1.dict_get("completedDate"), Some(&PyRep::None));

    // 2. Legacy faction-mapped `epic_arc_id: 1` with V24 mission 14117 coalesces into Arc 29 alongside active 14118.
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::remove(snap, char_id, 88_001);
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 1,
                agent_id: 3_019_001,
                faction_id: 500_021,
                chapter: 1,
                completed_missions: vec![14117],
                chosen_branch: None,
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 14118, 3_019_002, 30_000_142),
        );
    });
    let coalesced = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(coal_arcs) = coalesced else { panic!() };
    assert_eq!(coal_arcs.len(), 1, "legacy id 1 must coalesce into V24 arc 29");
    let (_, PyRep::Dict(coal_missions)) = coal_arcs.iter().find(|(k, _)| k.as_i64() == Some(29)).unwrap() else {
        panic!();
    };
    assert_eq!(coal_missions.len(), 2);
    assert!(matches!(
        coal_missions.iter().find(|(k, _)| k.as_i64() == Some(14117)).unwrap().1.dict_get("completedDate"),
        Some(PyRep::Integer(_))
    ));
    assert_eq!(
        coal_missions.iter().find(|(k, _)| k.as_i64() == Some(14118)).unwrap().1.dict_get("completedDate"),
        Some(&PyRep::None)
    );

    // 3. Restarting an arc after cooldown expires resets stale completion timestamps in favor of active mission.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::remove(snap, char_id, 14118);
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 29,
                agent_id: 3_019_001,
                faction_id: 500_021,
                chapter: 7,
                completed_missions: vec![14117, 14118],
                chosen_branch: None,
                completed_at_unix: Some(now.saturating_sub(8_000_000)),
                cooldown_until_unix: Some(now.saturating_sub(100)),
            }],
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 14117, 3_019_001, 30_000_142),
        );
    });
    let restarted = handle_extended(&ctx, &mut session, "agentMgr", "GetMyEpicArcStatus", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(rest_arcs) = restarted else { panic!() };
    let (_, PyRep::Dict(rest_missions)) = rest_arcs.iter().find(|(k, _)| k.as_i64() == Some(29)).unwrap() else {
        panic!();
    };
    assert_eq!(rest_missions.len(), 1, "stale completed missions from prior run must not shadow restarted run");
    assert_eq!(rest_missions[0].0.as_i64(), Some(14117));
    assert_eq!(rest_missions[0].1.dict_get("state").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(rest_missions[0].1.dict_get("onCooldown"), Some(&PyRep::Bool(false)));
    assert_eq!(rest_missions[0].1.dict_get("cooldownUntilUnix"), Some(&PyRep::None));
    assert_eq!(rest_missions[0].1.dict_get("completedDate"), Some(&PyRep::None));
    assert!(matches!(rest_missions[0].1.dict_get("acceptedDate"), Some(PyRep::Integer(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epic_arc_messages_return_never_cached_result_over_machonet_26000() {
    let ctx = make_test_context();
    let mut session = char_session(90_000_001, 98_000_001);
    let kw = HashMap::new();

    // Direct/non-V24 dispatch preserves raw Dict for internal callers and IT parity tests.
    let raw = ctx
        .dispatch_rpc(
            &mut session,
            "agentMgr",
            None,
            "GetMessagesForEpicArcMissions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("direct GetMessagesForEpicArcMissions");
    assert!(matches!(raw, PyRep::Dict(_)));

    // Live V24 client (machoNet:26000) receives CachedMethodCallResult with versionCheck=("never", None, None)
    // so ServiceCallGPCS/objectCaching serves repeated epicArcController lookups locally instead of 825+ RPCs.
    let cached = ctx
        .dispatch_rpc(
            &mut session,
            "agentMgr",
            None,
            "GetMessagesForEpicArcMissions",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("machoNet:26000 GetMessagesForEpicArcMissions");
    let PyRep::Object { type_name, args } = cached else {
        panic!("expected CachedMethodCallResult object wrapper for GetMessagesForEpicArcMissions");
    };
    assert_eq!(type_name, eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS);
    let PyRep::Tuple(items) = *args else {
        panic!("expected 3-tuple args in CachedMethodCallResult");
    };
    assert_eq!(items.len(), 3);
    let PyRep::Dict(entries) = &items[0] else {
        panic!("expected details Dict in CachedMethodCallResult[0]");
    };
    let vc = entries
        .iter()
        .find(|(k, _)| k.as_str() == Some("versionCheck"))
        .map(|(_, v)| v);
    assert_eq!(
        vc,
        Some(&PyRep::Tuple(vec![
            PyRep::String("never".into()),
            PyRep::None,
            PyRep::None,
        ]))
    );
    let PyRep::SubStream(inner) = &items[1] else {
        panic!("expected SubStream payload in CachedMethodCallResult[1]");
    };
    assert_eq!(inner.as_ref(), &raw);
}

#[test]
fn npc_kill_faction_standing_loss_tick_dedup_derived_and_notifications() {
    // parity: feat:standings.standing-loss, feat:standings.npc-kill-standing-loss, plan:PVE-3
    let mut ctx = make_test_context();
    let char_id = 90_000_001_i64;
    let now_base = ctx.now_unix();
    ctx.db
        .acquire_online_lock(1, 511, Some(char_id), true, now_base)
        .unwrap();

    // Seed SDE entity groups & types matching live EVE Online SDE dogma attribute 562 (`entityFactionLoss`).
    let mut sde = (*ctx.sde).clone();
    for (gid, gname) in [
        (99, "Sentry Gun"),
        (306, "Spawn Container"),
        (615, "Deadspace Guristas Frigate"),
        (674, "Mission Caldari State Battleship"),
        (925, "FW Infrastructure Hub"),
        (1167, "FW Caldari State Frigate"),
        (1174, "Asteroid Rogue Drone Officer"),
        (1310, "Drifter Battleship"),
        (4028, "Triglavian Entities"),
    ] {
        sde.groups.insert(
            gid,
            eve_sde::GroupInfo {
                group_id: gid,
                category_id: 11,
                name: gname.into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
    }
    let template_type = sde
        .types
        .values()
        .next()
        .cloned()
        .expect("canonical seed must contain at least one InvType");
    let make_npc_type = |type_id: u32,
                         group_id: u32,
                         category_id: u32,
                         name: &str,
                         faction_id: Option<u32>,
                         loss_562: Option<f64>| {
        let mut attrs = HashMap::new();
        if let Some(v) = loss_562 {
            attrs.insert(ATTR_ENTITY_FACTION_LOSS, v);
        }
        let mut t = template_type.clone();
        t.type_id = type_id;
        t.group_id = group_id;
        t.category_id = category_id;
        t.name = name.into();
        t.faction_id = faction_id;
        t.race_id = None;
        t.default_attributes = attrs;
        t.published = true;
        t
    };
    sde.types.insert(
        12_391,
        make_npc_type(12_391, 674, 11, "Caldari Navy Raven", None, Some(0.03)),
    );
    sde.types.insert(
        29_610,
        make_npc_type(29_610, 1167, 11, "State Protectorate Frigate", Some(500_001), Some(0.0)),
    );
    sde.types.insert(
        3_743,
        make_npc_type(3_743, 99, 11, "Minmatar Sentry Gun", None, None),
    );
    sde.types.insert(
        16_997,
        make_npc_type(16_997, 615, 11, "Pithi Invader", None, Some(0.00175)),
    );
    sde.types.insert(
        52_320,
        make_npc_type(52_320, 4028, 11, "Zorya's Leshak", Some(500_026), Some(0.30)),
    );
    sde.types.insert(
        34_495,
        make_npc_type(34_495, 1310, 11, "Drifter Battleship", Some(500_024), Some(0.002535)),
    );
    sde.types.insert(
        17_000,
        make_npc_type(17_000, 306, 11, "Caldari Spawn Container", Some(500_001), None),
    );
    sde.types.insert(
        29_302,
        make_npc_type(29_302, 925, 11, "Caldari Infrastructure Hub", None, None),
    );
    sde.types.insert(
        32_959,
        make_npc_type(32_959, 1174, 11, "Unit D-34343", None, None),
    );
    sde.types.insert(
        587,
        make_npc_type(587, 25, 6, "Rifter", None, None),
    );
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        snap.standing_transactions.remove(&char_id);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let mut notify_rx = ctx.notify_tx.subscribe();

    // 1. Destroying one Caldari Navy Raven (12391, entityFactionLoss=0.03 -> -0.3% = -0.03 at 0.0)
    //    lowers Caldari State (500_001) and allies (Amarr 500_003, Mordu's 500_018), while enemies
    //    (Gallente 500_004, Minmatar 500_002) remain unchanged per NPC_standings.md:154.
    let changes1 = apply_npc_kill_faction_standing_loss(
        &ctx,
        30_000_142,
        char_id,
        12_391,
        None,
        None,
        now_base,
    );
    assert!(!changes1.is_empty());
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_001] - (-0.03)).abs() < 1e-9, "Caldari State standing: {}", st[&500_001]);
        assert!((st[&500_003] - (-0.015)).abs() < 1e-9, "Ally Amarr derived loss: {}", st[&500_003]);
        assert!(st[&500_018] < 0.0, "Ally Mordu's Legion must lose standing");
        assert_eq!(st.get(&500_004).copied().unwrap_or(0.0), 0.0, "Enemy Gallente must not gain");
        assert_eq!(st.get(&500_002).copied().unwrap_or(0.0), 0.0, "Enemy Minmatar must not gain");
        let txs = &snap.standing_transactions[&char_id];
        assert!(txs.iter().any(|t| t.from_id == 500_001 && t.modification < 0.0 && t.event_type_id == 83));
        assert!(txs.iter().any(|t| t.from_id == 500_003 && t.modification < 0.0 && t.event_type_id == 83));
    });

    // Verify client notifications arrive on both the live broadcast stream (`notify_tx`) and pending queue.
    let mut bcast_events = Vec::new();
    while let Ok((cid, notif)) = notify_rx.try_recv() {
        if cid == char_id {
            bcast_events.push(notif.event);
        }
    }
    assert!(bcast_events.contains(&"OnStandingSet".to_string()), "bcast missing OnStandingSet: {bcast_events:?}");
    assert!(bcast_events.contains(&"OnStandingsModified".to_string()), "bcast missing OnStandingsModified: {bcast_events:?}");
    let queued = ctx.drain_notifications_for_char(char_id);
    assert!(queued.iter().any(|n| n.event == "OnStandingSet"));
    assert!(queued.iter().any(|n| n.event == "OnStandingsModified"));

    // 2. Two more kills in the SAME solar system within 10 minutes (< 600s) change nothing and emit nothing.
    let c2 = apply_npc_kill_faction_standing_loss(&ctx, 30_000_142, char_id, 12_391, None, None, now_base + 120);
    let c3 = apply_npc_kill_faction_standing_loss(&ctx, 30_000_142, char_id, 12_391, None, None, now_base + 599);
    assert!(c2.is_empty() && c3.is_empty());
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    assert!(notify_rx.try_recv().is_err());
    let after_three_same_sys = ctx.db.with_snapshot(|s| s.characters[&char_id].standings[&500_001]);
    assert!((after_three_same_sys - (-0.03)).abs() < 1e-9);

    // 3. Kill at exactly +600s in the SAME system triggers the next 10-minute tick.
    let c4 = apply_npc_kill_faction_standing_loss(&ctx, 30_000_142, char_id, 12_391, None, None, now_base + 600);
    assert!(!c4.is_empty());
    let _ = ctx.drain_notifications_for_char(char_id);
    while notify_rx.try_recv().is_ok() {}

    // 4. Three kills in THREE DIFFERENT systems inside 10 minutes apply THREE times.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    let t_multi = now_base + 2_000;
    for (idx, sys_id) in [30_000_142_u32, 30_000_144, 30_000_145].into_iter().enumerate() {
        let chg = apply_npc_kill_faction_standing_loss(
            &ctx,
            sys_id,
            char_id,
            12_391,
            None,
            None,
            t_multi + (idx as u64) * 30,
        );
        assert!(!chg.is_empty(), "kill in distinct system {sys_id} must apply");
    }
    let expected_three = -10.0 * (1.0 - (1.0 - 0.003_f64).powi(3));
    let actual_three = ctx.db.with_snapshot(|s| s.characters[&char_id].standings[&500_001]);
    assert!(
        (actual_three - expected_three).abs() < 1e-9,
        "3 systems inside 10m must apply 3x: got {actual_three}, expected {expected_three}"
    );
    let multi_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(multi_notes.iter().filter(|n| n.event == "OnStandingSet").count(), 3);
    assert_eq!(multi_notes.iter().filter(|n| n.event == "OnStandingsModified").count(), 3);

    // 5. Non-counting entities: FW complex guard (attr 562 == 0.0), FW Infrastructure Hub (group 925),
    //    Abyssal system, Spawn Container, player ship (category 6), and non-character NPC killer ID
    //    must all be no-ops; Asteroid Rogue Drone Officer (group 1174) MUST NOT be excluded by 1166..=1186.
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, char_id, 29_610, Some(500_001), None, t_multi).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, char_id, 29_302, Some(500_001), None, t_multi).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 32_000_001, char_id, 12_391, Some(500_001), None, t_multi).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, char_id, 17_000, Some(500_001), None, t_multi).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, char_id, 587, Some(500_001), None, t_multi).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, 555_555_555, 12_391, Some(500_001), None, t_multi).is_empty());
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    let drone_officer_chg = apply_npc_kill_faction_standing_loss(&ctx, 30_000_146, char_id, 32_959, None, None, t_multi);
    assert_eq!(drone_officer_chg.len(), 1);
    assert_eq!(drone_officer_chg[0].0, 500_025);
    assert!(drone_officer_chg[0].2 < 0.0, "group 1174 Rogue Drone Officer must lower 500_025 standing");
    let _ = ctx.drain_notifications_for_char(char_id);

    // 6. Sentry gun without attr 562 (Minmatar Sentry Gun 3743 -> DERIVED fallback 0.01) lowers
    //    Minmatar (500_002) and friends (Gallente 500_004, Thukker 500_015), not enemy Amarr (500_003).
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    let sentry_chg = apply_npc_kill_faction_standing_loss(&ctx, 30_000_147, char_id, 3_743, None, None, t_multi);
    assert!(!sentry_chg.is_empty());
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_002] - (-0.01)).abs() < 1e-9);
        assert!(st[&500_004] < 0.0, "Minmatar friend Gallente must lose standing");
        assert!(st[&500_015] < 0.0, "Minmatar friend Thukker must lose standing");
        assert_eq!(st.get(&500_003).copied().unwrap_or(0.0), 0.0, "Minmatar enemy Amarr must not gain");
    });

    // 7. Triglavian (500_026) kill lowers Triglavian and raises opposing EDENCOM (500_027) equally;
    //    Drifter (500_024) kill lowers Drifter standing; Pirate (Guristas 500_010) lowers Guristas + Blood Raiders.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, 30_000_148, char_id, 52_320, None, None, t_multi).is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, 30_000_148, char_id, 34_495, None, None, t_multi).is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, 30_000_148, char_id, 16_997, None, None, t_multi).is_empty());
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_026] - (-0.30)).abs() < 1e-9);
        assert!((st[&500_027] - 0.30).abs() < 1e-9, "Bipolar EDENCOM gain on Triglavian kill");
        assert!((st[&500_024] - (-0.002535)).abs() < 1e-9, "Drifter standing loss");
        assert!((st[&500_010] - (-0.00175)).abs() < 1e-9, "Guristas standing loss");
        assert!(st[&500_012] < 0.0, "Guristas ally Blood Raiders derived loss");
    });
}

fn mk_standing_test_group(group_id: u32, category_id: u32, name: &str) -> eve_sde::GroupInfo {
    eve_sde::GroupInfo {
        group_id,
        category_id,
        name: name.to_string(),
        anchorable: false,
        anchored: false,
        fittable_non_singleton: false,
        use_base_price: false,
        published: true,
    }
}

fn mk_standing_test_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    faction_id: Option<u32>,
    entity_faction_loss: Option<f64>,
) -> eve_sde::InvType {
    let mut default_attributes = HashMap::new();
    if let Some(loss) = entity_faction_loss {
        default_attributes.insert(562_u16, loss);
    }
    eve_sde::InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000.0,
        volume: 1_000.0,
        capacity: 0.0,
        radius: 50.0,
        base_price: 0.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

fn spawn_standing_test_char(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    initial_caldari_standing: Option<f64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = eve_db::CharacterRecord {
            character_id: char_id,
            name: format!("Standings Tester {char_id}"),
            corporation_id: 1_000_044,
            solar_system_id,
            ..Default::default()
        };
        if let Some(val) = initial_caldari_standing {
            ch.standings.insert(500_001, val);
        }
        snap.characters.insert(char_id, ch);
    });
}

fn open_real_sde_for_npc_standings_test() -> Option<Arc<eve_sde::SdeRegistry>> {
    if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
        let p = std::path::PathBuf::from(env_path);
        if p.exists() {
            return Some(Arc::new(
                eve_sde::SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"),
            ));
        }
    }
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..5 {
        let candidate = dir.join("data/evomu_sde.sqlite");
        if candidate.exists() {
            return Some(Arc::new(
                eve_sde::SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"),
            ));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

// parity: feat:standings.standing-loss, feat:standings.npc-kill-standing-loss, plan:PVE-3
#[test]
fn npc_kill_faction_standings_highest_loss_wins_tick_and_legacy_note_compat() {
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    sde.groups
        .insert(671, mk_standing_test_group(671, 11, "Mission Caldari State Frigate"));
    sde.groups
        .insert(674, mk_standing_test_group(674, 11, "Mission Caldari State Battleship"));
    sde.types.insert(
        24_997,
        mk_standing_test_type(24_997, 671, 11, "State Bo-Hi", Some(500_001), Some(0.001)),
    );
    sde.types.insert(
        12_391,
        mk_standing_test_type(12_391, 674, 11, "Caldari Navy Raven", Some(500_001), Some(0.03)),
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142_u32;
    let t0 = 1_700_000_000_u64;

    // 1. Starting at 0.0: frigate (-0.001) then battleship (-0.03) upgrades cleanly to -0.03,
    //    then another frigate inside the same 600s tick adds zero extra loss.
    let char_zero = 98_100_001_i64;
    spawn_standing_test_char(&ctx, char_zero, sys_id, None);
    let chg_frig =
        apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_zero, 24_997, None, None, t0);
    assert!(!chg_frig.is_empty());
    ctx.db.with_snapshot(|snap| {
        let st = snap.characters[&char_zero].standings[&500_001];
        assert!((st - (-0.001)).abs() < 1e-12, "frigate initial standing loss got {st}");
    });

    let chg_bs =
        apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_zero, 12_391, None, None, t0 + 120);
    assert!(!chg_bs.is_empty());
    ctx.db.with_snapshot(|snap| {
        let st = snap.characters[&char_zero].standings[&500_001];
        assert!(
            (st - (-0.03)).abs() < 1e-12,
            "highest-loss-wins upgrade must equal direct -0.03, got {st}"
        );
    });

    let chg_lower =
        apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_zero, 24_997, None, None, t0 + 240);
    assert!(
        chg_lower.is_empty(),
        "lower frigate loss after battleship in same 600s tick must apply zero extra loss"
    );

    // 2. Starting at +5.0: frigate then battleship must match direct battleship under apply_standing_decrease,
    //    and tick_start_unix remains anchored at t0 (resets at t0 + 600, not t0 + 120 + 600).
    let char_upgraded = 98_100_002_i64;
    let char_direct = 98_100_003_i64;
    spawn_standing_test_char(&ctx, char_upgraded, sys_id, Some(5.0));
    spawn_standing_test_char(&ctx, char_direct, sys_id, Some(5.0));

    assert!(!apply_npc_kill_faction_standing_loss(
        &ctx,
        sys_id,
        char_upgraded,
        24_997,
        None,
        None,
        t0,
    )
    .is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(
        &ctx,
        sys_id,
        char_upgraded,
        12_391,
        None,
        None,
        t0 + 60,
    )
    .is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(
        &ctx,
        sys_id,
        char_direct,
        12_391,
        None,
        None,
        t0,
    )
    .is_empty());

    ctx.db.with_snapshot(|snap| {
        let upgraded = snap.characters[&char_upgraded].standings[&500_001];
        let direct = snap.characters[&char_direct].standings[&500_001];
        assert!(upgraded < 5.0);
        assert!(
            (upgraded - direct).abs() < 1e-12,
            "non-zero base incremental upgrade ({upgraded}) must equal direct battleship ({direct})"
        );
    });

    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_upgraded, 24_997, None, None, t0 + 599)
            .is_empty(),
        "tick window must remain anchored at t0"
    );
    assert!(
        !apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_upgraded, 24_997, None, None, t0 + 600)
            .is_empty(),
        "kill at t0 + 600 must start a new tick window"
    );

    // 3. Legacy 3-tuple OwnerNoteRecord format `(faction_id, solar_system_id, tick_start)` stays backward-compatible.
    let char_legacy = 98_100_004_i64;
    spawn_standing_test_char(&ctx, char_legacy, sys_id, None);
    ctx.db.with_mut_snapshot(|snap| {
        let nid = -(5_400_000_000_000_i64.saturating_add(char_legacy.abs()));
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: char_legacy,
                item_id: char_legacy,
                label: "npc_kill_standing_tick".to_string(),
                note: serde_json::to_string(&vec![(500_001_i64, sys_id, t0)]).unwrap(),
                timestamp_unix: t0,
            },
        );
    });
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_legacy, 12_391, None, None, t0 + 100)
            .is_empty(),
        "legacy 3-tuple record inside active 600s window must prevent extra standing loss"
    );
    assert!(
        !apply_npc_kill_faction_standing_loss(&ctx, sys_id, char_legacy, 24_997, None, None, t0 + 600)
            .is_empty(),
        "legacy 3-tuple record expires cleanly after 600s"
    );
}

// parity: feat:standings.standing-loss, feat:standings.npc-kill-standing-loss, plan:PVE-3
#[test]
fn npc_kill_faction_standings_group_exclusion_before_attr562_and_wh_sleepers() {
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    sde.groups
        .insert(925, mk_standing_test_group(925, 11, "FW Infrastructure Hub"));
    sde.groups.insert(
        1_167,
        mk_standing_test_group(1_167, 11, "FW Caldari State Frigate"),
    );
    sde.groups.insert(
        983,
        mk_standing_test_group(983, 11, "Deadspace Sleeper Sleepless Patroller"),
    );
    sde.groups
        .insert(4_036, mk_standing_test_group(4_036, 11, "Sleeper Entities"));
    // Deliberately assign category_id 11 + positive attr 562 to excluded groups so
    // resolve_npc_kill_standing_loss_spec passes the category_id == 11 gate and proves
    // is_excluded_non_standing_npc_group runs BEFORE attr 562.
    sde.types.insert(
        29_302,
        mk_standing_test_type(
            29_302,
            925,
            11,
            "Caldari Infrastructure Hub",
            Some(500_001),
            Some(0.05),
        ),
    );
    sde.types.insert(
        32_914,
        mk_standing_test_type(
            32_914,
            1_167,
            11,
            "State Frigate",
            Some(500_001),
            Some(0.005),
        ),
    );
    sde.types.insert(
        30_188,
        mk_standing_test_type(
            30_188,
            983,
            11,
            "Sleepless Patroller",
            Some(500_024),
            Some(0.0017761),
        ),
    );
    sde.types.insert(
        56_328,
        mk_standing_test_type(
            56_328,
            4_036,
            11,
            "Hypnosian Sentinel",
            Some(500_024),
            Some(0.142162994),
        ),
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let char_id = 98_100_010_i64;
    let k_sys = 30_000_142_u32;
    let wh_sys = 31_000_001_u32;
    let t0 = 1_700_001_000_u64;
    spawn_standing_test_char(&ctx, char_id, k_sys, None);

    assert!(is_excluded_non_standing_npc_group(925));
    assert!(is_excluded_non_standing_npc_group(1_167));
    assert_eq!(
        resolve_npc_kill_standing_loss_spec(&ctx.sde, 29_302, Some(500_001), None, k_sys),
        None,
        "Category 11 excluded FW i-Hub group 925 with positive attr 562 must resolve to None"
    );
    assert_eq!(
        resolve_npc_kill_standing_loss_spec(&ctx.sde, 32_914, Some(500_001), None, k_sys),
        None,
        "Category 11 excluded FW guard group 1167 with positive attr 562 must resolve to None"
    );
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 29_302, Some(500_001), None, t0)
            .is_empty(),
        "excluded FW i-Hub group 925 with positive attr 562 must yield zero standing loss"
    );
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 32_914, Some(500_001), None, t0)
            .is_empty(),
        "excluded FW plex guard group 1167 with positive attr 562 must yield zero standing loss"
    );
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 30_188, Some(500_024), None, t0)
            .is_empty(),
        "excluded Sleeper group 983 with positive attr 562 must yield zero standing loss in k-space"
    );
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, wh_sys, char_id, 30_188, Some(500_024), None, t0)
            .is_empty(),
        "excluded Sleeper group 983 must yield zero standing loss in w-space"
    );
    assert!(
        apply_npc_kill_faction_standing_loss(&ctx, wh_sys, char_id, 56_328, Some(500_024), None, t0)
            .is_empty(),
        "Sleeper Entities group 4036 in w-space (31_000_000..=31_999_999) must yield zero standing loss"
    );
    // F-4: unknown npc_type_id not present in SDE must return empty even when explicit_faction_id is passed.
    assert!(
        apply_npc_kill_faction_standing_loss(
            &ctx,
            k_sys,
            char_id,
            999_999_777,
            Some(500_001),
            None,
            t0
        )
        .is_empty(),
        "unknown NPC type ID absent from SdeRegistry must yield zero standing loss"
    );
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.standings.get(&500_001).copied().unwrap_or(0.0), 0.0);
        assert_eq!(ch.standings.get(&500_024).copied().unwrap_or(0.0), 0.0);
        assert_eq!(ch.standings.len(), 0);
    });

    // Positive control: `56_328` (Sleeper Entities group 4036) in K-space (outside WH range) IS eligible.
    let k_drifter =
        apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 56_328, None, None, t0);
    assert_eq!(k_drifter.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let st = snap.characters[&char_id].standings.get(&500_024).copied().unwrap_or(0.0);
        assert!((st - (-0.142162994)).abs() < 1e-9, "K-space 56328 standing got {st}");
    });
}

// parity: feat:standings.standing-loss, feat:standings.npc-kill-standing-loss, plan:PVE-3
#[test]
fn npc_kill_faction_standings_verified_against_real_sqlite_sde() {
    let sde = open_real_sde_for_npc_standings_test()
        .expect("data/evomu_sde.sqlite must be available for real-SDE verification");

    let frigate = sde.types.get(&24_997).expect("real SDE type 24997 State Bo-Hi");
    assert_eq!(frigate.group_id, 671);
    assert!((frigate.default_attributes.get(&562).copied().unwrap_or(0.0) - 0.001).abs() < 1e-9);

    let bs = sde.types.get(&12_391).expect("real SDE type 12391 Caldari Navy Raven");
    assert_eq!(bs.group_id, 674);
    assert!((bs.default_attributes.get(&562).copied().unwrap_or(0.0) - 0.03).abs() < 1e-9);

    let sleeper = sde.types.get(&30_188).expect("real SDE type 30188 Sleepless Patroller");
    assert_eq!(sleeper.group_id, 983);
    assert!(sleeper.default_attributes.get(&562).copied().unwrap_or(0.0) > 0.0);

    let trig = sde.types.get(&52_320).expect("real SDE type 52320 Zorya's Leshak");
    assert_eq!(trig.faction_id, Some(500_026));
    let expected_trig_loss = trig.default_attributes.get(&562).copied().unwrap_or(0.0);
    assert!(expected_trig_loss > 0.30);

    let ihub = sde
        .types
        .get(&29_302)
        .expect("real SDE type 29302 Caldari Infrastructure Hub");
    assert_eq!(ihub.group_id, 925);
    assert_eq!(ihub.category_id, 11);

    let fw_guard = sde
        .types
        .get(&32_914)
        .expect("real SDE type 32914 State Frigate");
    assert_eq!(fw_guard.group_id, 1_167);
    assert_eq!(fw_guard.category_id, 11);

    let ctx = ServerContext::with_sde_and_repository(
        sde,
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let char_id = 98_100_020_i64;
    let k_sys = 30_000_142_u32;
    let wh_sys = 31_000_001_u32;
    let t0 = 1_700_002_000_u64;
    spawn_standing_test_char(&ctx, char_id, k_sys, None);

    // Excluded real SDE types produce zero loss even with real SDE attr 562 / FW metadata.
    assert!(apply_npc_kill_faction_standing_loss(&ctx, wh_sys, char_id, 30_188, None, None, t0).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 30_188, None, None, t0).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, wh_sys, char_id, 56_328, None, None, t0).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 29_302, None, None, t0).is_empty());
    assert!(apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 32_914, None, None, t0).is_empty());

    // Real SDE highest-loss-wins tick upgrade: State Bo-Hi (24997) -> Caldari Navy Raven (12391).
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 24_997, None, None, t0).is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 12_391, None, None, t0 + 30).is_empty());
    assert!(!apply_npc_kill_faction_standing_loss(&ctx, k_sys, char_id, 52_320, None, None, t0 + 40).is_empty());

    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(
            (st[&500_001] - (-0.03)).abs() < 1e-9,
            "real SDE Caldari upgrade must settle at -0.03, got {}",
            st[&500_001]
        );
        assert!(
            (st[&500_026] - (-expected_trig_loss)).abs() < 1e-9,
            "real SDE Triglavian standing loss must match attr 562 ({expected_trig_loss}), got {}",
            st[&500_026]
        );
        assert!(
            (st[&500_027] - expected_trig_loss).abs() < 1e-9,
            "real SDE bipolar EDENCOM gain must match attr 562 ({expected_trig_loss}), got {}",
            st[&500_027]
        );
    });
}

#[cfg(test)]
#[path = "agents_standings_arcs_extra_unit_tests.rs"]
mod agents_standings_arcs_extra_unit_tests;
