//! Edge-case behavioral tests for `agents_standings_arcs`.

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{
    ActiveMissionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, NpcCorporationInfo, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_with_sde<F: FnOnce(&mut SdeRegistry)>(f: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    f(&mut sde);
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    ctx
}

fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

#[test]
fn standing_breakdown_edge_cases_pirate_trig_diplomacy_and_agent_min_rule() {
    let ctx = make_ctx_with_sde(|sde| {
        sde.npc_corporations.insert(
            1_000_174,
            NpcCorporationInfo {
                corporation_id: 1_000_174,
                name: "Guristas".into(),
                ticker_name: "GUR".into(),
                faction_id: Some(500_010), // Guristas Pirates
                solar_system_id: Some(30_000_142),
                station_id: Some(60_003_760),
                tax_rate: 0.10,
                ceo_id: None,
            },
        );
        sde.npc_corporations.insert(
            1_000_298,
            NpcCorporationInfo {
                corporation_id: 1_000_298,
                name: "Triglavian Collective Corp".into(),
                ticker_name: "TRIG".into(),
                faction_id: Some(500_026), // Triglavian
                solar_system_id: Some(30_000_142),
                station_id: Some(60_003_760),
                tax_rate: 0.0,
                ceo_id: None,
            },
        );
        sde.agents.insert(
            3_099_001,
            AgentInfo {
                agent_id: 3_099_001,
                name: "Trig Agent".into(),
                corporation_id: 1_000_298,
                division_id: 24,
                level: 1,
                station_id: Some(60_003_760),
                solar_system_id: Some(30_000_142),
                agent_type_id: 2,
                is_locator: false,
                research_skills: vec![],
            },
        );
    });

    let char_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3359, TrainedSkillRecord { type_id: 3359, level: 5, skill_points: 256_000.0 });
        skills.insert(3361, TrainedSkillRecord { type_id: 3361, level: 4, skill_points: 45_255.0 });
        skills.insert(3357, TrainedSkillRecord { type_id: 3357, level: 5, skill_points: 256_000.0 });
    });

    ctx.db.with_snapshot(|snap| {
        // returns (effective, conn_bonus, dip_bonus, crim_bonus)
        let no_entry = compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, 0.0, false);
        assert_eq!(no_entry, (0.0, 0.0, 0.0, 0.0));

        // has_entry = true with raw = 0.0 non-pirate -> Connections lv 5 (20% of 10 = 2.0)
        let with_entry = compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, 0.0, true);
        assert!((with_entry.0 - 2.0).abs() < 1e-6);
        assert!((with_entry.1 - 2.0).abs() < 1e-6);
        assert_eq!((with_entry.2, with_entry.3), (0.0, 0.0));

        // Pirate corp (faction 500_010) uses Criminal Connections lv 4 (16% of 10 = 1.6)
        let pirate = compute_effective_standing_breakdown(&ctx, snap, char_id, 1_000_174, 0.0, true);
        assert!((pirate.0 - 1.6).abs() < 1e-6);
        assert!((pirate.3 - 1.6).abs() < 1e-6);
        assert_eq!((pirate.1, pirate.2), (0.0, 0.0));

        // Negative raw standing (-5.0) uses Diplomacy lv 5 (20% of 15 = 3.0 -> eff = -2.0)
        let neg = compute_effective_standing_breakdown(&ctx, snap, char_id, 500_001, -5.0, true);
        assert!((neg.0 - (-2.0)).abs() < 1e-6);
        assert!((neg.2 - 3.0).abs() < 1e-6);
        assert_eq!((neg.1, neg.3), (0.0, 0.0));

        // Triglavian direct faction, corp, and agent all bypass skills and clamp to [-10, 10]
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_026, -15.0, true),
            (-10.0, 0.0, 0.0, 0.0)
        );
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, char_id, 1_000_298, 2.0, true),
            (2.0, 0.0, 0.0, 0.0)
        );
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, char_id, 3_099_001, 2.0, true),
            (2.0, 0.0, 0.0, 0.0)
        );

        // compute_effective_agent_standing: missing char, empty standings
        assert_eq!(
            compute_effective_agent_standing(&ctx, snap, 99_999_999, 3_099_001, 1_000_298, Some(500_026)),
            0.0
        );
        assert_eq!(
            compute_effective_agent_standing(&ctx, snap, char_id, 3_099_001, 1_000_298, Some(500_026)),
            0.0
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(3_099_001, 8.0);
        ch.standings.insert(1_000_298, -2.5);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(
            (compute_effective_agent_standing(&ctx, snap, char_id, 3_099_001, 1_000_298, Some(500_026))
                - (-2.5))
                .abs()
                < 1e-6
        );
    });
}

#[test]
fn derived_standings_negative_caps_and_epic_career_final_suppression() {
    let ctx = make_ctx_with_sde(|sde| {
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
        sde.agents.insert(
            3_099_010,
            AgentInfo {
                agent_id: 3_099_010,
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
        sde.agents.insert(
            3_099_011,
            AgentInfo {
                agent_id: 3_099_011,
                name: "Career Agent".into(),
                corporation_id: 1_000_009,
                division_id: 24,
                level: 1,
                station_id: Some(60_003_760),
                solar_system_id: Some(30_000_142),
                agent_type_id: 11,
                is_locator: false,
                research_skills: vec![],
            },
        );
    });
    let char_id = 90_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        // fac_pct == 0.0 -> no-op
        let mut noop = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_001, 0.0, true, &mut noop);
        assert!(noop.is_empty());

        // Negative faction standing with apply_derived=true reduces allies (500_003) and leaves enemies (500_002, 500_004) unchanged
        let mut neg_changes = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_001, -10.0, true, &mut neg_changes);
        assert!(neg_changes.iter().any(|(id, _, _)| *id == 500_001));
        assert!(neg_changes.iter().any(|(id, _, d)| *id == 500_003 && *d < 0.0));
        assert!(!neg_changes.iter().any(|(id, _, _)| *id == 500_004));

        // apply_derived=false updates only primary
        let mut single = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_001, 5.0, false, &mut single);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].0, 500_001);

        // Epic Arc agent suppresses derived faction standings
        let (_, epic_changes) = apply_mission_completion_standings(
            &ctx,
            snap,
            char_id,
            3_099_010,
            990_001,
            1_000_009,
            5.0,
            true,
        );
        assert_eq!(epic_changes.iter().filter(|(id, _, _)| *id == 500_002 || *id == 500_004).count(), 0);

        // Negative mission completion records event_type_id 83
        let (_, neg_tx_changes) = apply_mission_completion_standings(
            &ctx,
            snap,
            char_id,
            3_099_011,
            990_001,
            1_000_009,
            -2.0,
            false,
        );
        assert!(!neg_tx_changes.is_empty());
        assert!(snap
            .standing_transactions
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|tx| tx.event_type_id == 83)));
    });
}

#[test]
fn special_dialogue_actions_research_and_locator_error_edges() {
    let ctx = make_ctx_with_sde(|sde| {
        sde.npc_corporations.insert(
            1_000_009,
            NpcCorporationInfo {
                corporation_id: 1_000_009,
                name: "CP".into(),
                ticker_name: "CP".into(),
                faction_id: Some(500_001),
                solar_system_id: Some(30_000_142),
                station_id: Some(60_003_760),
                tax_rate: 0.1,
                ceo_id: None,
            },
        );
        sde.agents.insert(
            3_099_020,
            AgentInfo {
                agent_id: 3_099_020,
                name: "Plain Agent".into(),
                corporation_id: 1_000_009,
                division_id: 24,
                level: 1,
                station_id: Some(60_003_760),
                solar_system_id: Some(30_000_142),
                agent_type_id: 2,
                is_locator: false,
                research_skills: vec![],
            },
        );
        sde.agents.insert(
            3_099_021,
            AgentInfo {
                agent_id: 3_099_021,
                name: "R&D Agent".into(),
                corporation_id: 1_000_009,
                division_id: 24,
                level: 3,
                station_id: Some(60_003_760),
                solar_system_id: Some(30_000_142),
                agent_type_id: 4,
                is_locator: false,
                research_skills: vec![11455],
            },
        );
        sde.agents.insert(
            3_099_022,
            AgentInfo {
                agent_id: 3_099_022,
                name: "Locator L1".into(),
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
    });

    let char_id = 90_000_001;
    let target_id = 90_000_002;
    let mut sess = char_session(char_id, 98_000_001);

    // Action 12 on non-research agent -> NotResearchAgent
    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 12, 3_099_020, &[], &HashMap::new())
            .unwrap()
            .unwrap_err()
            .code,
        "NotResearchAgent"
    );

    // Action 12 with invalid skill type (args[2] < 0) -> InvalidResearchSkill
    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut sess,
            char_id,
            30_000_142,
            12,
            3_099_021,
            &[PyRep::Integer(12), PyRep::None, PyRep::Integer(-1)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidResearchSkill"
    );

    // Action 12 re-starting research banks accrued points from prior days (requires Science V + field skill)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(3_099_021, 5.0);
        ch.standings.insert(1_000_009, 5.0);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 5, skill_points: 256_000.0 });
        sk.insert(3392, TrainedSkillRecord { type_id: 3392, level: 5, skill_points: 256_000.0 });
        sk.insert(11455, TrainedSkillRecord { type_id: 11455, level: 5, skill_points: 256_000.0 });
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: 3_099_021,
                research_type_id: 11455,
                points_per_day: 25.0,
                started_unix: 1_775_400_000 - 2 * 86_400,
                initial_points: 10.0,
            }],
        );
    });
    let res_rnd = handle_special_dialogue_action(
        &ctx,
        &mut sess,
        char_id,
        30_000_142,
        12,
        3_099_021,
        &[PyRep::Integer(12), PyRep::Integer(11455)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!((res_rnd.dict_get("pointsPerDay").unwrap().as_f64().unwrap() - 80.0).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| {
        let entries = load_rnd_research_entries(snap, char_id);
        assert!((entries[0].initial_points - 60.0).abs() < 1e-6);
    });

    // Action 15 locator error checks
    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 15, 3_099_999, &[], &HashMap::new())
            .unwrap()
            .unwrap_err()
            .code,
        "UnknownAgent"
    );
    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, char_id, 30_000_142, 15, 3_099_020, &[], &HashMap::new())
            .unwrap()
            .unwrap_err()
            .code,
        "AgentHasNoLocatorService"
    );

    // AgentNotCoLocated when caller at wrong station
    sess.station_id = Some(60_000_001);
    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut sess,
            char_id,
            30_000_142,
            15,
            3_099_022,
            &[PyRep::Integer(15), PyRep::Integer(target_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AgentNotCoLocated"
    );
    sess.station_id = Some(60_003_760);

    // Target in wormhole (>= 31_000_000) -> LocatorTargetInUnknownSpace
    ctx.db.with_mut_snapshot(|snap| {
        let mut tgt = snap.characters[&char_id].clone();
        tgt.character_id = target_id;
        tgt.solar_system_id = 31_000_001;
        snap.characters.insert(target_id, tgt);
    });
    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut sess,
            char_id,
            30_000_142,
            15,
            3_099_022,
            &[PyRep::Integer(15), PyRep::Integer(target_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocatorTargetInUnknownSpace"
    );

    // Level 1 agent out of constellation -> LocatorTargetOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&target_id).unwrap().solar_system_id = 30_002_187;
    });
    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut sess,
            char_id,
            30_000_142,
            15,
            3_099_022,
            &[PyRep::Integer(15), PyRep::Integer(target_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocatorTargetOutOfRange"
    );

    // Same system but insufficient ISK -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&target_id).unwrap().solar_system_id = 30_000_142;
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(500);
    });
    assert_eq!(
        handle_special_dialogue_action(
            &ctx,
            &mut sess,
            char_id,
            30_000_142,
            15,
            3_099_022,
            &[PyRep::Integer(15), PyRep::Integer(target_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    // Action 19 updates an existing EpicArcStateEntry in place
    ctx.db.with_mut_snapshot(|snap| {
        save_epic_arc_states(
            snap,
            char_id,
            &[EpicArcStateEntry {
                epic_arc_id: 2,
                agent_id: 3_099_022,
                faction_id: 500_001,
                chapter: 1,
                completed_missions: vec![1],
                chosen_branch: None,
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
    });
    let updated_branch = handle_special_dialogue_action(
        &ctx,
        &mut sess,
        char_id,
        30_000_142,
        19,
        3_099_022,
        &[PyRep::Integer(19), PyRep::Integer(3_099_022), PyRep::String("Amarr".into())],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(updated_branch.dict_get("chosenBranch").and_then(|v| v.as_str()), Some("Amarr"));
    ctx.db.with_snapshot(|snap| {
        let states = load_epic_arc_states(snap, char_id);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].chapter, 2);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extended_rpcs_cover_agent_attr_errors_corp_standings_and_gate_checks() {
    let ctx = make_ctx_with_sde(|sde| {
        sde.npc_corporations.insert(
            1_000_009,
            NpcCorporationInfo {
                corporation_id: 1_000_009,
                name: "CP".into(),
                ticker_name: "CP".into(),
                faction_id: Some(500_001),
                solar_system_id: Some(30_000_142),
                station_id: Some(60_003_760),
                tax_rate: 0.1,
                ceo_id: None,
            },
        );
        sde.agents.insert(
            3_099_030,
            AgentInfo {
                agent_id: 3_099_030,
                name: "Epic Gate Agent".into(),
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
    });

    let char_id = 90_000_001;
    let char2_id = 90_000_002;
    let ship_id = 100_000_999;
    let unknown_agent_err_code = "UnknownAgent";
    let mut sess = char_session(char_id, 98_000_001);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_009, 4.0);
        let mut c2 = snap.characters[&char_id].clone();
        c2.character_id = char2_id;
        c2.standings.clear();
        c2.standings.insert(1_000_009, 2.0);
        snap.characters.insert(char2_id, c2);
        snap.corporations.insert(
            98_000_001,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_000_001; __corp.name = "T".into(); __corp.ticker = "T".into(); __corp.ceo_id = char_id; __corp.members = HashSet::from([char_id, char2_id]); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.active_missions.insert(
            (char_id, 700),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 700,
                agent_id: 3_099_030,
                state: "Offered".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 700,
                agent_id: 3_099_030,
                is_anomic_burner: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 640, // Scorpion battleship
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Missing agent attr read returns error
    let unk_attr = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "agentMgr",
        "corporationID",
        &[PyRep::Integer(3_099_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unk_attr.code, unknown_agent_err_code);

    // GetPlausibleMissionIDs unknown agent -> AgentNotFound
    let miss_err = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[PyRep::Integer(3_099_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(miss_err.code, "AgentNotFound");

    // epicArcStatus invalid agentID <= 0
    let bad_epic = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "epicArcStatus",
        "AgentHasEpicMissionsForCharacter",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_epic.code, "InvalidArguments");

    // standingMgr.GetCorpStandings
    let corp_stand = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "standingMgr",
        "GetCorpStandings",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(corp_stand, PyRep::Rowset { .. }));

    // standingMgr.GetStandingCompositions
    let comp = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "standingMgr",
        "GetStandingCompositions",
        &[PyRep::Integer(1_000_009)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(comp, PyRep::List(items) if !items.is_empty()));

    // keeper.ActivateAccelerationGate: inactive/missing mission -> None
    let keeper_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: "keeper".into(),
        object_id: 0,
        oid_str: "keeper".into(),
        ref_id: 1,
    };
    let no_mission = handle_extended_with_bound(
        &ctx,
        &mut sess,
        Some(&keeper_bound),
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(999_999)],
        &kw,
    )
    .await;
    assert!(no_mission.is_none());

    // Docked -> ShipDocked
    let docked_gate = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(700)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_gate.code, "ShipDocked");

    // Undocked Anomic Burner battleship -> ShipRestrictedByGate (checked before mission state)
    sess.station_id = None;
    let restricted = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(700)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(restricted.code, "ShipRestrictedByGate");

    // Switch ship to frigate (587): state != "Accepted" -> InvalidMissionState
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
    });
    let state_err = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(700)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(state_err.code, "InvalidMissionState");

    // State Accepted, npcs_remaining = 2 -> RoomNpcsRemaining
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.get_mut(&(char_id, 700)).unwrap().state = "Accepted".into();
    });
    let npc_err = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(700)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(npc_err.code, "RoomNpcsRemaining");

    // npcs_remaining = 0, frigate -> succeeds & advances room 0 -> 1
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.get_mut(&(char_id, 700)).unwrap().npcs_remaining = 0;
    });
    let gate_ok = handle_extended_with_bound(
        &ctx,
        &mut sess,
        None,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(700)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(gate_ok.dict_get("currentRoomID").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, 700)].current_room_id, 1);
    });
}

#[test]
fn epic_arc_faction_resolution_pirate_trig_detection_and_rnd_note_roundtrip() {
    let ctx = make_ctx_with_sde(|sde| {
        for (cid, fid) in [
            (1_000_201, 500_001),
            (1_000_202, 500_002),
            (1_000_203, 500_003),
            (1_000_204, 500_004),
            (1_000_210, 500_010),
            (1_000_211, 500_011),
            (1_000_226, 500_026),
            (1_000_227, 500_027),
            (1_000_299, 500_099),
        ] {
            sde.npc_corporations.insert(
                cid,
                NpcCorporationInfo {
                    corporation_id: cid,
                    name: format!("Corp {cid}"),
                    ticker_name: "C".into(),
                    faction_id: Some(fid),
                    solar_system_id: Some(30_000_142),
                    station_id: Some(60_003_760),
                    tax_rate: 0.1,
                    ceo_id: None,
                },
            );
            let aid = 3_900_000 + cid;
            sde.agents.insert(
                aid,
                AgentInfo {
                    agent_id: aid,
                    name: format!("Agent {aid}"),
                    corporation_id: cid,
                    division_id: 24,
                    level: 1,
                    station_id: Some(60_003_760),
                    solar_system_id: Some(30_000_142),
                    agent_type_id: 2,
                    is_locator: false,
                    research_skills: vec![],
                },
            );
        }
    });

    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_201), (2, 500_001));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_202), (3, 500_002));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_203), (4, 500_003));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_204), (5, 500_004));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_210), (6, 500_010));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_211), (7, 500_011));
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_000 + 1_000_299), (1, 500_099));

    for fid in [500_010, 500_011, 500_012, 500_019, 500_020] {
        assert!(is_pirate_entity(&ctx, fid));
    }
    assert!(is_pirate_entity(&ctx, 1_000_210));
    assert!(is_pirate_entity(&ctx, 3_900_000 + 1_000_211));
    assert!(!is_pirate_entity(&ctx, 500_001));
    assert!(!is_pirate_entity(&ctx, 1_000_201));
    assert!(!is_pirate_entity(&ctx, 3_900_000 + 1_000_201));
    assert!(!is_pirate_entity(&ctx, 999_999_999));

    assert!(is_trig_or_edencom_entity(&ctx, 500_026));
    assert!(is_trig_or_edencom_entity(&ctx, 500_027));
    assert!(is_trig_or_edencom_entity(&ctx, 1_000_226));
    assert!(is_trig_or_edencom_entity(&ctx, 3_900_000 + 1_000_227));
    assert!(!is_trig_or_edencom_entity(&ctx, 500_001));
    assert!(!is_trig_or_edencom_entity(&ctx, 1_000_201));
    assert!(!is_trig_or_edencom_entity(&ctx, 3_900_000 + 1_000_201));
    assert!(!is_trig_or_edencom_entity(&ctx, 999_999_999));

    let nid_m1 = mission_meta_note_id(90_000_001, 100);
    let nid_m2 = mission_meta_note_id(90_000_001, 101);
    let nid_rnd = rnd_research_note_id(90_000_001);
    let nid_arc = epic_arc_note_id(90_000_001);
    let nid_loc = locator_cooldown_note_id(90_000_001, 3_018_922);
    assert!(nid_m1 < -5_000_000_000_000 && nid_m1 != nid_m2);
    assert!(nid_rnd < -5_100_000_000_000 && nid_arc < -5_200_000_000_000 && nid_loc < -5_300_000_000_000);

    ctx.db.with_mut_snapshot(|snap| {
        let entries = vec![RndResearchEntry {
            agent_id: 3_018_922,
            research_type_id: 11_446,
            points_per_day: 52.5,
            started_unix: 1_700_000_000,
            initial_points: 10.0,
        }];
        save_rnd_research_entries(snap, 90_000_001, &entries);
        let loaded = load_rnd_research_entries(snap, 90_000_001);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].agent_id, 3_018_922);
        assert_eq!(loaded[0].research_type_id, 11_446);
        assert_eq!(snap.char_research_agents[&90_000_001][0].skill_type_id, 11_446);
    });
}
