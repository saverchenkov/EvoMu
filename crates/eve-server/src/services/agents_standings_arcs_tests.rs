use super::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    WorldRepository,
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
        snap.active_missions.insert(
            (char_id, 500),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 500,
                agent_id: 3_018_921,
                state: "Accepted".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 500,
                agent_id: 3_018_921,
                is_anomic_burner: true,
                ..Default::default()
            },
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
        snap.active_missions
            .get_mut(&(char_id, 500))
            .unwrap()
            .npcs_remaining = 0;
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

#[cfg(test)]
#[path = "agents_standings_arcs_extra_unit_tests.rs"]
mod agents_standings_arcs_extra_unit_tests;
