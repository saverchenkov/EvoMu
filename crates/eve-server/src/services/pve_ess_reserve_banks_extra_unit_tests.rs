//! Extra unit and mutation-killing tests for `services::pve_ess_reserve_banks`
//! covering R&D access validation branches (co-location, field skill, Science/Engineering V,
//! concurrent agent limit, agent & corp standing thresholds), RP/day formula with Connections/Diplomacy,
//! storyline mission selection & militia/InterBus exclusions, ESS gate & bubble restrictions,
//! Main Bank multi-contributor remainder conservation, Reserve Bank link severance, and bounty splits.

use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    save_rnd_research_entries, RndResearchEntry,
};
use crate::services::pve_ess_reserve_banks::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    ActiveMissionRecord, CorporationRecord, IskCents, TrainedSkillRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{AgentInfo, SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn rnd_access_validation_branches_standing_thresholds_and_rp_formula() {
    let mut sde = (*shared_sde()).clone();
    let sample_t = sde.types.values().next().cloned().unwrap();
    let field_skill = 99_270_001u32;
    let unoffered_skill = 99_270_002u32;
    for (tid, grp, req) in [
        (field_skill, 270, None),
        (unoffered_skill, 270, None),
        (99_333_010, 333, Some(field_skill)),
        (99_333_011, 333, Some(unoffered_skill)),
    ] {
        let mut t = sample_t.clone();
        t.type_id = tid;
        t.group_id = grp;
        t.published = true;
        t.default_attributes.clear();
        if let Some(r) = req {
            t.default_attributes.insert(182, r as f64);
        }
        sde.types.insert(tid, t);
    }

    let char_id = 90_060_001i64;
    let station_id = 60_003_760i64;
    let sys_id: SolarSystemId = 30_000_142;
    let corp_id = 1_000_035i64;

    let mut agent = AgentInfo {
        agent_id: 3_018_500,
        name: "R&D Level 4".into(),
        corporation_id: corp_id,
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        level: 4,
        division_id: 24,
        agent_type_id: 4,
        is_locator: false,
        research_skills: vec![field_skill],
    };

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("rnd_extra", "pw", char_id, "RndExtra");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db.clone());

    db.with_mut_snapshot(|snap| {
        // 1. Wrong solar system or wrong station -> AgentNotCoLocated
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), 30_000_144, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "AgentNotCoLocated"
        );
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id + 1), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "AgentNotCoLocated"
        );

        // 2. InvalidResearchSkill: non-datacore skill, or skill not offered by agent
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, Some(SKILL_SCIENCE))
                .unwrap_err()
                .code,
            "InvalidResearchSkill"
        );
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, Some(unoffered_skill))
                .unwrap_err()
                .code,
            "InvalidResearchSkill"
        );

        // 3. Science V present, but no Engineering V -> ErrorNoRelevantResearchSkills
        snap.skills.entry(char_id).or_default().insert(
            SKILL_SCIENCE,
            TrainedSkillRecord {
                type_id: SKILL_SCIENCE,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "ErrorNoRelevantResearchSkills"
        );

        // 4. Add CPU Management V (3426), but field_skill is still 0 -> ErrorNoRelevantResearchSkills
        snap.skills.entry(char_id).or_default().insert(
            SKILL_CPU_MANAGEMENT,
            TrainedSkillRecord {
                type_id: SKILL_CPU_MANAGEMENT,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "ErrorNoRelevantResearchSkills"
        );

        // 5. Add field_skill = 3, but character already has 1 active R&D agent (with RPM = 0 -> max 1) -> MaxResearchAgentsReached
        snap.skills.entry(char_id).or_default().insert(
            field_skill,
            TrainedSkillRecord {
                type_id: field_skill,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: 3_018_999,
                research_type_id: field_skill,
                points_per_day: 50.0,
                initial_points: 0.0,
                started_unix: 1_000,
            }],
        );
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "MaxResearchAgentsReached"
        );

        // Clear active entries; level 4 requires >= 5.0 effective standing and >= 3.0 corp standing
        save_rnd_research_entries(snap, char_id, &[]);
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "InsufficientAgentStanding"
        );

        // Personal agent standing = 6.0 (>= 5.0), but corp standing = 2.0 (< 3.0) -> InsufficientAgentStanding!
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(agent.agent_id, 6.0);
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(corp_id, 2.0);
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None)
                .unwrap_err()
                .code,
            "InsufficientAgentStanding"
        );

        // Raise corp standing to 3.5 -> Level 4 R&D access succeeds!
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(corp_id, 3.5);
        let ok = validate_rnd_research_access(&ctx, Some(station_id), sys_id, snap, char_id, &agent, None).unwrap();
        assert_eq!(ok, (field_skill, 3));

        // 6. Verify RP/day exact math:
        // With Negotiation = 0, raw_agent_st = 6.0 (no completed mission -> Connections ignored):
        // mult = 1.0 + (20 + 0 + 6.0)/100 = 1.26; base = (3 + 4)^2 = 49 -> 1.26 * 49 = 61.74
        let rp = compute_rnd_rp_per_day(&ctx, snap, char_id, agent.agent_id, 4, 3);
        assert!((rp - 61.74).abs() < 1e-6);

        // has_completed_mission_with_agent via active_missions state == "Completed"
        snap.active_missions.insert(
            (char_id, 701),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 701,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Completed".into(),
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        assert!(has_completed_mission_with_agent(snap, char_id, agent.agent_id));
        agent.level = 1;
    });
}

#[test]
fn storyline_selection_militia_exclusion_and_ess_pulse_edge_cases() {
    let sde = shared_sde();

    // 1. Militia corp detection and storyline mission selection for levels 1..=5
    if let Some(militia_corp) = sde.factions.values().find_map(|f| f.militia_corporation_id) {
        assert!(is_interbus_or_militia_corp(&sde, militia_corp));
    }
    let active = HashMap::new();
    let m_lvl1 = select_storyline_mission_for_level(&sde, &active, 90_000_001, 1);
    assert!(m_lvl1.is_some());

    // 2. ess_reserve_pulse_weight when pulses_total == 1 (peak == 1 -> returns 300)
    assert_eq!(ess_reserve_pulse_weight(1, 1), 300);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(500, 1, 1), 500);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(-10, 5, 5), 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(500, 5, 0), 0);
    assert_eq!(max_affordable_datacores(0.0), 0);
    assert_eq!(max_affordable_datacores(99.9), 0);
    assert_eq!(max_affordable_datacores(100.0), 1);

    // 3. ensure_ess_reserve_bank_not_already_unlocked when reserve_linked_character_id is Some
    let mut snap = WorldStateSnapshot::default();
    let sys_id = 30_004_759;
    let pulse_st = crate::services::space_destiny::EssReservePulseState {
        pulses_total: 15,
        pulses_remaining: 10,
        last_pulse_sec: None,
        reserve_linked_character_id: Some(90_000_001),
        reserve_link_started_tick: Some(1),
        link_started_unix: Some(1_000),
        unlocked_until_unix: 2_000,
    };
    crate::services::space_destiny::set_ess_reserve_pulses(&mut snap, sys_id, &pulse_st);
    assert_eq!(
        ensure_ess_reserve_bank_not_already_unlocked(&snap, sys_id, 1_500, 0)
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ess_gate_restrictions_module_checks_autopayout_tiebreak_and_reserve_link_severance() {
    let sde = shared_sde();
    let null_sys: SolarSystemId = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap();
    let high_sys: SolarSystemId = 30_000_142;

    let c1 = 90_060_101i64;
    let c2 = 90_060_102i64;
    let corp_id = 98_060_001i64;
    let ship_id = 1_060_000_001i64;
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let now_unix = 1_780_000_000u64;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ess_c1", "pw", c1, "EssC1");
    db.provision_gm_account("ess_c2", "pw", c2, "EssC2");
    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "NullCorp".into();
                __corp.ticker = "NUL".into();
                __corp.ceo_id = c1;
                __corp.tax_rate = 0.10;
                __corp },
        );
        for cid in [c1, c2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.solar_system_id = null_sys;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents(0);
            }
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));

    // 1. Highsec system returns None for try_activate_ess_acceleration_gate and Ok(()) for bubble checks
    assert!(try_activate_ess_acceleration_gate(&ctx, c1, high_sys, ship_id, gate_id).await.is_none());
    assert!(check_ess_warp_destination_allowed(&ctx, high_sys, DVec3::ZERO, DVec3::ZERO).await.is_ok());
    assert!(check_ess_intrusion_bubble_restriction(&ctx, high_sys, ship_id, "warp").await.is_ok());

    // 2. Nullsec ESS gate checks: ShipNotInSpace, ShipInWarp, OutsideGateActivationRange, ShipRestrictedByEssGate
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, c1, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let gate_pos = DVec3::new(50_000_200_000.0, 50_000_000_000.0, 0.0);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587, // Frigate (ineligible for ESS gate!)
            position: gate_pos + DVec3::new(10_000.0, 0.0, 0.0), // 10km (> 2500m)
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::new_warp(DVec3::ZERO, gate_pos, 3.0),
        })
        .await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, c1, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, c1, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );

    let _ = sys_handle.set_ball_position(ship_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, c1, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipRestrictedByEssGate"
    );

    // 3. Move ship inside 75km ESS bubble and test mjd, cyno, filament, and module activation checks
    let ess_pos = DVec3::new(50_000_000_000.0, 50_000_000_000.0, 0.0);
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "mjd").await.unwrap_err().code,
        "EssIntrusionBubbleMjdBlocked"
    );
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cyno").await.unwrap_err().code,
        "EssIntrusionBubbleCynoBlocked"
    );
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "filament").await.unwrap_err().code,
        "EssIntrusionBubbleRestricted"
    );

    let mut sample_mod = sde.types.values().next().cloned().unwrap();
    sample_mod.group_id = 330; // Cloak
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &sample_mod, "cloaking")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );
    sample_mod.group_id = 658; // Cyno
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &sample_mod, "cyno")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCynoBlocked"
    );
    sample_mod.group_id = 1189; // MJD
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &sample_mod, "mjd")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMjdBlocked"
    );

    // 4. Main Bank autopayout with 2 equal contributors and indivisible pool (101 cents -> 51 to c1, 50 to c2)
    ctx.db.with_mut_snapshot(|snap| {
        // Also test highsec direct bounty vs nullsec player corp tax split
        credit_ballpark_npc_bounty_with_ess_split(&sde, snap, high_sys, c1, 100_000.0, 100_000.0, 1.0, 23_753, now_unix);
        assert_eq!(snap.characters[&c1].balance, IskCents::from_f64_isk(100_000.0));
        snap.characters.get_mut(&c1).unwrap().balance = IskCents(0);

        credit_ballpark_npc_bounty_with_ess_split(&sde, snap, null_sys, c1, 1_000_000.0, 1_000_000.0, 1.0, 23_753, now_unix);
        assert!(snap.corporations[&corp_id].master_wallet_balance().0 > 0, "player corp should receive bounty tax");
        snap.characters.get_mut(&c1).unwrap().balance = IskCents(0);

        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = IskCents(101);
        st.ess_main_bank_contributors.clear();
        st.ess_cycle_started_unix = now_unix;
        record_ess_main_bank_contribution(st, c1, 50, now_unix);
        record_ess_main_bank_contribution(st, c2, 50, now_unix);
        // While linked character is hacking main bank, autopayout skips the system!
        st.ess_linked_character_id = Some(c1);
    });
    assert_eq!(tick_ess_main_bank_autopayout(&ctx, now_unix + ESS_MAIN_BANK_AUTOPAYOUT_SECS + 1), 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().ess_linked_character_id = None;
    });
    assert_eq!(tick_ess_main_bank_autopayout(&ctx, now_unix + ESS_MAIN_BANK_AUTOPAYOUT_SECS + 1), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&c1].balance, IskCents(51));
        assert_eq!(snap.characters[&c2].balance, IskCents(50));
    });

    // 5. Reserve Bank link severance when ship moves > 2,500m away from ESS beacon
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        let pulse_st = crate::services::space_destiny::EssReservePulseState {
            pulses_total: 15,
            pulses_remaining: 5,
            last_pulse_sec: None,
            reserve_linked_character_id: Some(c1),
            reserve_link_started_tick: Some(1),
            link_started_unix: Some(now_unix),
            unlocked_until_unix: now_unix + 3_600,
        };
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys, &pulse_st);
    });
    assert_eq!(tick_ess_reserve_bank_links(&ctx, now_unix + 60).await, 0);
    ctx.db.with_snapshot(|snap| {
        let pulse_st = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys);
        assert!(pulse_st.reserve_linked_character_id.is_none(), "out-of-range link must be severed");
    });
}
