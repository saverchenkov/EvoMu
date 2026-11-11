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

#[test]
fn wave4_storyline_qualifying_agent_types_and_datacore_bounds() {
    for qualifying in [2_u32, 4, 10, 11] {
        assert!(
            is_storyline_qualifying_agent_type(qualifying),
            "agent type {qualifying} must qualify for storyline tally"
        );
    }
    for non_qualifying in [0_u32, 1, 3, 5, 6, 7, 8, 9, 12, 99] {
        assert!(
            !is_storyline_qualifying_agent_type(non_qualifying),
            "agent type {non_qualifying} must not qualify for storyline tally"
        );
    }

    assert_eq!(max_affordable_datacores(-50.0), 0);
    assert_eq!(max_affordable_datacores(0.0), 0);
    assert_eq!(max_affordable_datacores(99.999), 0);
    assert_eq!(max_affordable_datacores(100.0), 1);
    assert_eq!(max_affordable_datacores(299.999), 2);
    assert_eq!(max_affordable_datacores(300.0), 3);

    assert_eq!(ess_reserve_pulse_weight(1, 0), 300);
    assert_eq!(ess_reserve_pulse_weight(1, 1), 300);
    assert!(ess_reserve_pulse_weight(2, 5) > ess_reserve_pulse_weight(1, 5));
    assert!(ess_reserve_pulse_weight(5, 5) > 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(0, 1, 1), 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(-100, 1, 1), 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(5_000, 1, 1), 5_000);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(5_000, 1, 0), 0);
    assert!(ess_reserve_pulse_weight(5, 15) > ess_reserve_pulse_weight(1, 15));
    assert!(ess_reserve_pulse_weight(5, 15) > ess_reserve_pulse_weight(15, 15));
}

#[test]
fn wave4_agent_mission_completion_tracking_and_datacore_exclusions() {
    let db = eve_db::WorldRepository::new_empty();
    let char_id = 90_099_101_i64;
    db.provision_gm_account("ess_w4_trk", "pw", char_id, "EssTracker");
    let ag1 = 3_099_101_i64;
    let ag2 = 3_099_102_i64;
    let ag_pending = 3_099_103_i64;
    db.with_mut_snapshot(|snap| {
        assert!(!has_completed_mission_with_agent(snap, char_id, ag1));
        record_agent_mission_completed(snap, char_id, ag1);
        record_agent_mission_completed(snap, char_id, ag1);
        assert!(has_completed_mission_with_agent(snap, char_id, ag1));

        snap.active_missions.insert(
            (char_id, 8801),
            eve_db::ActiveMissionRecord {
                character_id: char_id,
                mission_id: 8801,
                agent_id: ag2,
                state: "Completed".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, 8802),
            eve_db::ActiveMissionRecord {
                character_id: char_id,
                mission_id: 8802,
                agent_id: ag_pending,
                state: "Accepted".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        assert!(has_completed_mission_with_agent(snap, char_id, ag2));
        assert!(!has_completed_mission_with_agent(snap, char_id, ag_pending));
    });

    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    assert!(resolve_datacore_for_research_skill(&sde, SKILL_SCIENCE).is_none());
    assert!(resolve_datacore_for_research_skill(&sde, SKILL_RESEARCH_PROJECT_MANAGEMENT).is_none());
    assert!(resolve_datacore_for_research_skill(&sde, 34).is_none());

    if let Some(mut base) = sde.types.values().next().cloned() {
        let mut sk = base.clone();
        sk.type_id = 99_340;
        sk.group_id = 270;
        sk.published = true;
        sde.types.insert(99_340, sk);

        base.type_id = 99_333;
        base.group_id = 333;
        base.published = false;
        base.default_attributes.insert(182, 99_340.0);
        sde.types.insert(99_333, base.clone());
        assert_eq!(resolve_datacore_for_research_skill(&sde, 99_340), None);

        let mut published_dc = base.clone();
        published_dc.type_id = 99_334;
        published_dc.published = true;
        sde.types.insert(99_334, published_dc);
        assert_eq!(resolve_datacore_for_research_skill(&sde, 99_340), Some(99_334));

        // ESS gate hull eligibility against every allowed and disallowed ship group
        for (idx, &gid) in ESS_ALLOWED_SHIP_GROUP_IDS.iter().enumerate() {
            let tid = 98_000 + idx as u32;
            let mut hull = base.clone();
            hull.type_id = tid;
            hull.group_id = gid;
            sde.types.insert(tid, hull);
            assert!(
                is_ess_gate_eligible_hull(&sde, tid),
                "group {gid} must be allowed through ESS gate"
            );
        }
        for (idx, gid) in [0_u32, 25, 29, 30, 31, 485, 513, 547, 883].into_iter().enumerate() {
            let tid = 98_500 + idx as u32;
            let mut hull = base.clone();
            hull.type_id = tid;
            hull.group_id = gid;
            sde.types.insert(tid, hull);
            assert!(
                !is_ess_gate_eligible_hull(&sde, tid),
                "disallowed ship group {gid} must be rejected at ESS gate"
            );
        }
        assert!(!is_ess_gate_eligible_hull(&sde, 99_999_999));
    }

    if let Some(mut sys) = sde.solar_systems.values().next().cloned() {
        sys.solar_system_id = 30_999_001;
        sys.security_status = 0.0;
        sde.solar_systems.insert(30_999_001, sys.clone());
        assert!(is_kspace_nullsec(&sde, 30_999_001));

        sys.solar_system_id = 30_999_002;
        sys.security_status = 0.01;
        sde.solar_systems.insert(30_999_002, sys.clone());
        assert!(!is_kspace_nullsec(&sde, 30_999_002));

        sys.solar_system_id = 31_000_001;
        sys.security_status = -1.0;
        sde.solar_systems.insert(31_000_001, sys);
        assert!(!is_kspace_nullsec(&sde, 31_000_001));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wave4_ess_boundary_thresholds_tax_exempt_and_autopayout_edges() {
    use crate::router::ServerContext;
    use crate::services::space_destiny::{set_ess_reserve_pulses, EssReservePulseState};
    use eve_db::clock::VirtualServerClock;
    use eve_db::{CorporationRecord, IskCents, WorldRepository, WorldStateSnapshot};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;
    use std::sync::Arc;

    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let null_sys = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap();
    let char_id = 90_099_201_i64;
    let ship_id = 1_099_000_201_i64;
    let player_corp_id = 98_099_201_i64;
    let cruiser_tid = 626_u32; // Thorax (group 26 Cruiser)
    let now_unix = 1_785_000_000_u64;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ess_w4_b", "pw", char_id, "EssBounds");
    db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = player_corp_id;
        corp.name = "EssW4Corp".into();
        corp.ticker = "EW4".into();
        corp.ceo_id = char_id;
        corp.tax_rate = 0.10;
        snap.corporations.insert(player_corp_id, corp);

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.active_ship_id = ship_id;
            ch.corporation_id = 1_000_035; // NPC starter corp initially
            ch.balance = IskCents(0);
            ch.security_status = 4.95;
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let ess_pos = DVec3::new(50_000_000_000.0, 50_000_000_000.0, 0.0);
    let gate_pos = DVec3::new(50_000_200_000.0, 50_000_000_000.0, 0.0);

    // 1. Reserve bank unlock exact boundary (unlocked_until_unix == now -> Ok vs now + 1 -> Err; rt_until > now -> Err)
    let mut tmp_snap = WorldStateSnapshot::default();
    let mut ps = EssReservePulseState {
        pulses_total: 15,
        pulses_remaining: 15,
        last_pulse_sec: None,
        reserve_linked_character_id: None,
        reserve_link_started_tick: None,
        link_started_unix: None,
        unlocked_until_unix: now_unix,
    };
    set_ess_reserve_pulses(&mut tmp_snap, null_sys, &ps);
    assert!(ensure_ess_reserve_bank_not_already_unlocked(&tmp_snap, null_sys, now_unix, 0).is_ok());
    ps.unlocked_until_unix = now_unix + 1;
    set_ess_reserve_pulses(&mut tmp_snap, null_sys, &ps);
    assert_eq!(
        ensure_ess_reserve_bank_not_already_unlocked(&tmp_snap, null_sys, now_unix, 0)
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
    ps.unlocked_until_unix = 0;
    set_ess_reserve_pulses(&mut tmp_snap, null_sys, &ps);
    assert!(ensure_ess_reserve_bank_not_already_unlocked(&tmp_snap, null_sys, now_unix, now_unix).is_ok());
    assert_eq!(
        ensure_ess_reserve_bank_not_already_unlocked(&tmp_snap, null_sys, now_unix, now_unix + 1)
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );

    // 2. Spawn cloaked cruiser right inside 75km / 500km / 2500m gate boundaries
    let mut cs = CombatState::default();
    cs.is_cloaked = true;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: cruiser_tid,
                position: ess_pos + DVec3::new(74_999.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 0.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;

    // Inside < 75_000m: mwd/warp/other blocked; at 75_000.1m, mwd/warp allowed
    assert_eq!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "warp").await.unwrap_err().code, "EssIntrusionBubbleWarpBlocked");
    assert_eq!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "mwd").await.unwrap_err().code, "EssIntrusionBubbleMwdBlocked");
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "custom_unknown")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(75_000.1, 0.0, 0.0)).await;
    assert!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "warp").await.is_ok());
    assert!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "mwd").await.is_ok());

    // Cloak boundary: inside < 500_000.0 blocked, > 500_000.0 allowed
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(499_999.0, 0.0, 0.0)).await;
    assert!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cloak").await.is_err());
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(500_000.1, 0.0, 0.0)).await;
    assert!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cloak").await.is_ok());

    // Module check: group_id == 46 with attr 554 (MWD sig radius bonus) blocked, vs group_id == 46 without attr 554 (Afterburner) allowed!
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let mut mwd_by_attr = sde.types.values().next().cloned().unwrap();
    mwd_by_attr.group_id = 46;
    mwd_by_attr.default_attributes.clear();
    mwd_by_attr.default_attributes.insert(554, 500.0);
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &mwd_by_attr, "")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    mwd_by_attr.default_attributes.clear();
    assert!(check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &mwd_by_attr, "").await.is_ok());

    // 3. ESS gate exact 2_500.0m boundary + force uncloak on eligible hull activation
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let _ = sys_handle.set_ball_position(ship_id, gate_pos + DVec3::new(2_500.1, 0.0, 0.0)).await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );
    let _ = sys_handle.set_ball_position(ship_id, gate_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    let res = try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, gate_id)
        .await
        .unwrap();
    assert!(res.is_ok(), "cruiser at exact 2500.0m should activate ESS gate");
    assert!(!sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);

    // 4. Nullsec NPC < 100k zero-tax exemption vs player corp division tax & sec_status 5.0 cap
    ctx.db.with_mut_snapshot(|snap| {
        credit_ballpark_npc_bounty_with_ess_split(
            &sde,
            snap,
            null_sys,
            char_id,
            50_000.0,
            50_000.0,
            1.0,
            23_753,
            now_unix,
        );
        assert!(snap.characters[&char_id].balance.0 > 0);
        assert!(!snap
            .wallet_journal
            .iter()
            .any(|j| j.entry_type == "bounty_prize_corporation_tax"));
        assert!(snap.characters[&char_id].security_status <= 5.0);

        // Switch to player corp with 10% tax rate -> credit_division(1, tax_cents) + tax journal
        snap.characters.get_mut(&char_id).unwrap().corporation_id = player_corp_id;
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
        assert!(snap.corporations[&player_corp_id].master_wallet_balance().0 > 0);
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.entry_type == "bounty_prize_corporation_tax"));

        // Autopayout exact time boundary (ESS_MAIN_BANK_AUTOPAYOUT_SECS - 1 skipped, == SECS paid)
        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = IskCents(1_000);
        st.ess_main_bank_contributors.clear();
        record_ess_main_bank_contribution(st, char_id, 1_000, now_unix);
        st.ess_cycle_started_unix = now_unix;
        st.ess_linked_character_id = None;
    });
    assert_eq!(
        tick_ess_main_bank_autopayout(&ctx, now_unix + ESS_MAIN_BANK_AUTOPAYOUT_SECS - 1),
        0
    );
    assert_eq!(
        tick_ess_main_bank_autopayout(&ctx, now_unix + ESS_MAIN_BANK_AUTOPAYOUT_SECS),
        1
    );
}
