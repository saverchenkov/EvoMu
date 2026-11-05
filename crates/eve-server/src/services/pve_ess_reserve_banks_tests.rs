//! Unit and integration tests for `pve_ess_reserve_banks`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{SystemDynamicState, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn ess_reserve_pulse_curve_conservation_research_and_storyline_helpers() {
    // Curve spool-up and 100% conservation across all N pulses
    let n = 15_u32;
    let mut remaining = 1_500_000_000_i64;
    let w1 = ess_reserve_pulse_weight(1, n);
    let w_peak = ess_reserve_pulse_weight(12, n);
    let w_end = ess_reserve_pulse_weight(n, n);
    assert!(w_peak > w1);
    assert!(w_peak > w_end);

    for rem in (1..=n).rev() {
        let paid = compute_ess_reserve_pulse_payout_cents(remaining, n, rem);
        assert!(paid > 0 && paid <= remaining);
        remaining -= paid;
    }
    assert_eq!(remaining, 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(0, n, n), 0);

    let sde = shared_sde();
    assert_eq!(max_affordable_datacores(550.0), 5);
    assert_eq!(max_affordable_datacores(-10.0), 0);
    let mut dc_sde = (*sde).clone();
    if let Some(sample_skill) = dc_sde.types.values().next().cloned() {
        let mut mech_skill = sample_skill.clone();
        mech_skill.type_id = SKILL_MECHANICS;
        mech_skill.group_id = 270;
        mech_skill.published = true;
        dc_sde.types.insert(SKILL_MECHANICS, mech_skill);

        let mut custom_dc = sample_skill;
        custom_dc.type_id = 99_333_001;
        custom_dc.group_id = 333;
        custom_dc.published = true;
        custom_dc.default_attributes.insert(182, SKILL_MECHANICS as f64);
        dc_sde.types.insert(custom_dc.type_id, custom_dc);
    }
    assert_eq!(resolve_datacore_for_research_skill(&dc_sde, SKILL_MECHANICS), Some(99_333_001));
    assert!(resolve_datacore_for_research_skill(&dc_sde, SKILL_SCIENCE).is_none());
    assert!(is_storyline_qualifying_agent_type(2));
    assert!(!is_storyline_qualifying_agent_type(99));

    // Gate eligibility check across SDE ships
    let cruiser_tid = sde
        .types
        .values()
        .find(|t| ESS_ALLOWED_SHIP_GROUP_IDS.contains(&t.group_id))
        .map(|t| t.type_id)
        .expect("SDE has ESS-eligible hull");
    assert!(is_ess_gate_eligible_hull(&sde, cruiser_tid));

    let mut snap = WorldStateSnapshot::default();
    record_agent_mission_completed(&mut snap, 90_050_301, 3_018_418);
    assert!(has_completed_mission_with_agent(&snap, 90_050_301, 3_018_418));
    assert!(!has_completed_mission_with_agent(&snap, 90_050_301, 9_999_999));

    assert!(ensure_ess_reserve_bank_not_already_unlocked(&snap, 30_004_759, 1_000, 0).is_ok());
    assert!(ensure_ess_reserve_bank_not_already_unlocked(&snap, 30_004_759, 1_000, 2_000).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ess_bubble_restriction_main_bank_autopayout_and_bounty_split() {
    let sde = shared_sde();
    // Pick a K-space null-sec system (security <= 0.0, id < 31_000_000)
    let null_sys: SolarSystemId = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .expect("SDE has K-space null-sec system");

    let char_id: i64 = 90_050_302;
    let ship_id: i64 = 1_050_300_201;
    let beacon_id = ESS_BEACON_ID_BASE + null_sys as i64;
    let now_unix = 1_775_750_000_u64;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ess_pilot", "pw", char_id, "EssPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.active_ship_id = ship_id;
            ch.balance = eve_db::IskCents::from_f64_isk(10_000.0);
        }
        snap.system_states
            .insert(null_sys, SystemDynamicState::default());

        // Credit a null-sec NPC bounty with ESS main + reserve split
        credit_ballpark_npc_bounty_with_ess_split(
            &sde,
            snap,
            null_sys,
            char_id,
            1_000_000.0,
            1_000_000.0,
            1.0,
            23_753,
            now_unix,
        );

        let st = snap.system_states.get_mut(&null_sys).unwrap();
        record_ess_main_bank_contribution(st, char_id, 50_000_000, now_unix);
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let sys_handle = ctx.systems.get_or_spawn(null_sys);

    // Spawn ESS beacon + player ship inside 75 km bubble
    let _ = sys_handle
        .spawn_ball(Ball {
            id: beacon_id,
            type_id: ESS_BEACON_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 1_500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 1.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "warp")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "mwd")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cloak")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );

    // Advance past 2h45m autopayout timer and trigger payout + reserve bank tick
    let paid = tick_ess_main_bank_autopayout(&ctx, now_unix + ESS_MAIN_BANK_AUTOPAYOUT_SECS + 10);
    assert!(paid >= 1);

    // Ensure ESS intrusion balls + test warp destination check, gate activation & reserve bank pulses
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let warp_blocked = check_ess_warp_destination_allowed(
        &ctx,
        null_sys,
        DVec3::new(500_000.0, 0.0, 0.0),
        DVec3::new(5_000.0, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(warp_blocked.code, "EssIntrusionBubbleWarpBlocked");
    assert!(
        check_ess_warp_destination_allowed(
            &ctx,
            null_sys,
            DVec3::new(500_000.0, 0.0, 0.0),
            DVec3::new(400_000.0, 0.0, 0.0),
        )
        .await
        .is_ok()
    );

    if let Some(mwd_mod) = ctx.sde.types.values().find(|t| t.group_id == 46) {
        let mwd_err =
            check_ess_module_activation_allowed(&ctx, null_sys, ship_id, mwd_mod, "moduleBonusMicrowarpdrive")
                .await
                .unwrap_err();
        assert_eq!(mwd_err.code, "EssIntrusionBubbleMwdBlocked");
    }

    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let cruiser_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 26 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(626);
    let cruiser_ship_id: i64 = 1_050_300_202;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: cruiser_ship_id,
            type_id: cruiser_tid,
            position: DVec3::new(200_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .set_combat_state(
            cruiser_ship_id,
            eve_destiny::CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    let gate_ok =
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_ship_id, gate_id)
            .await
            .expect("handled")
            .expect("cruiser activates ESS acceleration gate");
    assert_eq!(gate_ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));

    // Move ship within <= 2,500 m of ESS beacon and run active reserve bank pulse & completion ticks
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(st) = snap.system_states.get_mut(&null_sys) {
            st.ess_reserve_bank = eve_db::IskCents(1_500_000_000);
        }
        let pulse_st = crate::services::space_destiny::EssReservePulseState {
            pulses_total: 15,
            pulses_remaining: 2,
            last_pulse_sec: None,
            reserve_linked_character_id: Some(char_id),
            reserve_link_started_tick: Some(1),
            link_started_unix: Some(now_unix),
            unlocked_until_unix: now_unix + 3_600,
        };
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys, &pulse_st);
    });
    let pulsed = tick_ess_reserve_bank_links(&ctx, now_unix + 120).await;
    assert_eq!(pulsed, 2);
}

#[test]
fn rnd_research_validation_and_storyline_sixteenth_mission_offer() {
    let base_sde = shared_sde();
    let mut sde = (*base_sde).clone();
    let char_id: i64 = 90_050_309;
    let station_id: i64 = 60_003_760;
    let sys_id: SolarSystemId = 30_000_142;

    if let Some(sample_skill) = sde.types.values().next().cloned() {
        let mut mech_skill = sample_skill.clone();
        mech_skill.type_id = SKILL_MECHANICS;
        mech_skill.group_id = 270;
        mech_skill.published = true;
        sde.types.insert(SKILL_MECHANICS, mech_skill);

        let mut custom_dc = sample_skill;
        custom_dc.type_id = 99_333_001;
        custom_dc.group_id = 333;
        custom_dc.published = true;
        custom_dc.default_attributes.insert(182, SKILL_MECHANICS as f64);
        sde.types.insert(custom_dc.type_id, custom_dc);
    }

    let rnd_agent = eve_sde::AgentInfo {
        agent_id: 3_018_418,
        name: "Test R&D Agent".into(),
        corporation_id: 1_000_035,
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        level: 1,
        division_id: 24,
        agent_type_id: 4,
        is_locator: false,
        research_skills: vec![SKILL_MECHANICS],
    };
    let storyline_agent = eve_sde::AgentInfo {
        agent_id: 3_019_999,
        name: "Test Storyline Agent".into(),
        corporation_id: 1_000_035,
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        level: 1,
        division_id: 24,
        agent_type_id: 6,
        is_locator: false,
        research_skills: Vec::new(),
    };
    sde.agents.insert(rnd_agent.agent_id, rnd_agent.clone());
    sde.agents
        .insert(storyline_agent.agent_id, storyline_agent.clone());

    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("rnd_pilot", "pw", char_id, "RndPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());

    // Undocked -> AgentNotCoLocated; docked without Science V -> ErrorNoRelevantResearchSkills; docked with skills -> Ok
    db.with_mut_snapshot(|snap| {
        let undocked = validate_rnd_research_access(
            &ctx,
            None,
            sys_id,
            snap,
            char_id,
            &rnd_agent,
            None,
        );
        assert_eq!(undocked.unwrap_err().code, "AgentNotCoLocated");

        let no_sci = validate_rnd_research_access(
            &ctx,
            Some(station_id),
            sys_id,
            snap,
            char_id,
            &rnd_agent,
            Some(SKILL_MECHANICS),
        );
        assert_eq!(no_sci.unwrap_err().code, "ErrorNoRelevantResearchSkills");

        let char_skills = snap.skills.entry(char_id).or_default();
        for tid in [SKILL_SCIENCE, SKILL_MECHANICS, SKILL_NEGOTIATION] {
            char_skills.insert(
                tid,
                eve_db::TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        let ok = validate_rnd_research_access(
            &ctx,
            Some(station_id),
            sys_id,
            snap,
            char_id,
            &rnd_agent,
            None,
        )
        .expect("R&D validation succeeds with Science V and Mechanics V");
        assert_eq!(ok, (SKILL_MECHANICS, 5));

        let rp_before = compute_rnd_rp_per_day(&ctx, snap, char_id, rnd_agent.agent_id, 1, 5);
        assert!(rp_before > 30.0);

        let reg_agent = eve_sde::AgentInfo {
            agent_id: 3_018_419,
            name: "Regular Agent".into(),
            corporation_id: 1_000_035,
            station_id: Some(station_id),
            solar_system_id: Some(sys_id),
            level: 1,
            division_id: 24,
            agent_type_id: 2,
            is_locator: false,
            research_skills: Vec::new(),
        };
        let mut offered = None;
        for i in 1..=16 {
            let res =
                record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &reg_agent);
            if i < 16 {
                assert!(res.is_none());
            } else {
                offered = res;
            }
        }
        assert!(offered.is_some());
        assert!(!is_interbus_or_militia_corp(&sde, 1_000_035));
    });
}
