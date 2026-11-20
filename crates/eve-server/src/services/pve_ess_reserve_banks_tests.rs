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

fn synthetic_inv_type(
    type_id: eve_sde::TypeId,
    group_id: eve_sde::GroupId,
    name: &str,
) -> eve_sde::InvType {
    eve_sde::InvType {
        type_id,
        group_id,
        category_id: 0,
        name: name.into(),
        mass: 0.0,
        volume: 0.0,
        capacity: 0.0,
        radius: 0.0,
        base_price: 0.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
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
        default_attributes: Default::default(),
    }
}

fn synthetic_solar_system(
    solar_system_id: SolarSystemId,
    security_status: f64,
) -> eve_sde::SolarSystem {
    eve_sde::SolarSystem {
        solar_system_id,
        constellation_id: 20_000_001,
        region_id: 10_000_001,
        name: format!("Synthetic-{solar_system_id}"),
        security_status,
        radius: 1_000_000_000.0,
        galactic_pos_ly: [0.0; 3],
        planet_ids: Vec::new(),
        stations: Vec::new(),
        wormhole_class_id: None,
    }
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
    let mech_skill = synthetic_inv_type(SKILL_MECHANICS, 270, "Synthetic Mechanics");
    dc_sde.types.insert(SKILL_MECHANICS, mech_skill);

    let mut custom_dc = synthetic_inv_type(99_333_001, 333, "Synthetic Datacore");
    custom_dc.default_attributes.insert(182, SKILL_MECHANICS as f64);
    dc_sde.types.insert(custom_dc.type_id, custom_dc);
    assert_eq!(resolve_datacore_for_research_skill(&dc_sde, SKILL_MECHANICS), Some(99_333_001));
    assert!(resolve_datacore_for_research_skill(&dc_sde, SKILL_SCIENCE).is_none());
    assert!(is_storyline_qualifying_agent_type(2));
    assert!(!is_storyline_qualifying_agent_type(99));

    // Gate eligibility check across SDE ships
    let cruiser_tid = sde
        .types
        .values()
        .filter(|t| ESS_ALLOWED_SHIP_GROUP_IDS.contains(&t.group_id))
        .min_by_key(|t| t.type_id)
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
        .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .min_by_key(|s| s.solar_system_id)
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

    if let Some(mwd_mod) = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 46)
        .min_by_key(|t| t.type_id)
    {
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
        .filter(|t| t.group_id == 26 && t.published)
        .min_by_key(|t| t.type_id)
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

    let mech_skill = synthetic_inv_type(SKILL_MECHANICS, 270, "Synthetic Mechanics");
    sde.types.insert(SKILL_MECHANICS, mech_skill);

    let mut custom_dc = synthetic_inv_type(99_333_001, 333, "Synthetic Datacore");
    custom_dc.default_attributes.insert(182, SKILL_MECHANICS as f64);
    sde.types.insert(custom_dc.type_id, custom_dc);

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

    let sk = synthetic_inv_type(99_340, 270, "Synthetic Research Skill");
    sde.types.insert(99_340, sk);

    let mut base = synthetic_inv_type(99_333, 333, "Synthetic Datacore");
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
        let mut hull = synthetic_inv_type(tid, gid, "Synthetic Allowed Hull");
        hull.category_id = 6;
        sde.types.insert(tid, hull);
        assert!(
            is_ess_gate_eligible_hull(&sde, tid),
            "group {gid} must be allowed through ESS gate"
        );
    }
    for (idx, gid) in [0_u32, 25, 29, 30, 31, 485, 513, 547, 883].into_iter().enumerate() {
        let tid = 98_500 + idx as u32;
        let mut hull = synthetic_inv_type(tid, gid, "Synthetic Disallowed Hull");
        hull.category_id = 6;
        sde.types.insert(tid, hull);
        assert!(
            !is_ess_gate_eligible_hull(&sde, tid),
            "disallowed ship group {gid} must be rejected at ESS gate"
        );
    }
    assert!(!is_ess_gate_eligible_hull(&sde, 99_999_999));

    sde.solar_systems
        .insert(30_999_001, synthetic_solar_system(30_999_001, 0.0));
    assert!(is_kspace_nullsec(&sde, 30_999_001));

    sde.solar_systems
        .insert(30_999_002, synthetic_solar_system(30_999_002, 0.01));
    assert!(!is_kspace_nullsec(&sde, 30_999_002));

    sde.solar_systems
        .insert(31_000_001, synthetic_solar_system(31_000_001, -1.0));
    assert!(!is_kspace_nullsec(&sde, 31_000_001));
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
        .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .min_by_key(|s| s.solar_system_id)
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

    // Module check: group_id == 46 with attr 554, attr 986, effect 6730, or "Microwarpdrive" name
    // is blocked inside the 75 km bubble, while a clean group_id == 46 Afterburner is allowed.
    let _ = sys_handle.set_ball_position(ship_id, ess_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let mut prop_mod = synthetic_inv_type(99_046_001, 46, "Synthetic Afterburner");
    prop_mod.default_attributes.insert(554, 500.0);
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &prop_mod, "")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    prop_mod.default_attributes.clear();
    assert!(check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &prop_mod, "").await.is_ok());

    prop_mod.default_attributes.insert(986, 25.0);
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &prop_mod, "")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    prop_mod.default_attributes.clear();
    prop_mod.effect_ids.push(6730);
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &prop_mod, "")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    prop_mod.effect_ids.clear();
    prop_mod.name = "Synthetic 50MN Microwarpdrive".into();
    assert_eq!(
        check_ess_module_activation_allowed(&ctx, null_sys, ship_id, &prop_mod, "")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );

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

#[tokio::test]
async fn ess_gate_dogma_activate_decloak_clears_actor_cycle_slim_meta_velocity_and_allows_ess_link_gh_1408()
{
    let mut sde = (*shared_sde()).clone();
    let null_sys: SolarSystemId = 30_099_408;
    sde.solar_systems
        .insert(null_sys, synthetic_solar_system(null_sys, -0.5));
    let cloak_type_id: eve_sde::TypeId = 11577; // Improved Cloaking Device II (non-covert)
    assert!(
        sde.types.contains_key(&cloak_type_id),
        "SDE must contain Improved Cloaking Device II (11577)"
    );
    let cruiser_type_id: eve_sde::TypeId = 626; // Vexor (group 26, ESS-eligible)
    let frig_type_id: eve_sde::TypeId = 587; // Rifter (group 25, ESS-ineligible)
    let sde = Arc::new(sde);

    let db = Arc::new(WorldRepository::new_in_memory().unwrap());
    let now_unix = 1_700_050_000_i64;
    let char_id = 91_408_501_i64;
    let cruiser_id = 91_408_502_i64;
    let cloak_id = 91_408_503_i64;
    let frig_char_id = 91_408_511_i64;
    let frig_id = 91_408_512_i64;
    let frig_cloak_id = 91_408_513_i64;

    db.provision_gm_account("ess_dogma_pilot", "pw", char_id, "EssDogmaPilot");
    db.provision_gm_account("ess_dogma_frig", "pw", frig_char_id, "EssDogmaFrig");

    let req_skills: Vec<u32> = [182_u16, 183, 184]
        .into_iter()
        .filter_map(|aid| {
            sde.types
                .get(&cloak_type_id)
                .and_then(|t| t.default_attributes.get(&aid))
                .map(|&v| v as u32)
        })
        .chain([SKILL_CPU_MANAGEMENT, SKILL_POWER_GRID_MANAGEMENT, 11579])
        .collect();

    db.with_mut_snapshot(|snap| {
        for &(cid, sid, ship_tid, mid) in &[
            (char_id, cruiser_id, cruiser_type_id, cloak_id),
            (frig_char_id, frig_id, frig_type_id, frig_cloak_id),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = null_sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            let skills = snap.skills.entry(cid).or_default();
            for &skill_tid in &req_skills {
                skills.insert(
                    skill_tid,
                    eve_db::TrainedSkillRecord {
                        type_id: skill_tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: i64::from(null_sys),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: cloak_type_id,
                    owner_id: cid,
                    location_id: sid,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents::from_isk(15_000_000),
                ess_reserve_bank: IskCents::from_isk(45_000_000),
                ..SystemDynamicState::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix as u64)));
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
        rt.safety_levels
            .insert(frig_char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let ess_ball_id = ESS_BEACON_ID_BASE + i64::from(null_sys);
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let sys = ctx.systems.get_or_spawn(null_sys);
    let ess_pos = sys.get_ball_snapshot(ess_ball_id).await.unwrap().unwrap().1.position;
    let gate_pos = sys.get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;
    let cloak_activate_pos = ess_pos + glam::DVec3::new(600_000.0, 0.0, 0.0);

    let baseline_max_vel = ctx
        .sde
        .types
        .get(&cruiser_type_id)
        .and_then(|t| t.default_attributes.get(&37).copied())
        .unwrap_or(195.0);
    for &(cid, sid, ship_tid, group_id, radius, pos) in &[
        (
            char_id,
            cruiser_id,
            cruiser_type_id,
            26_u32,
            150.0_f64,
            cloak_activate_pos,
        ),
        (
            frig_char_id,
            frig_id,
            frig_type_id,
            25_u32,
            40.0_f64,
            cloak_activate_pos + glam::DVec3::new(0.0, 5_000.0, 0.0),
        ),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: ship_tid,
                    position: pos,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: baseline_max_vel,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    base_max_velocity: Some(baseline_max_vel),
                    shield_hp: 1000.0,
                    max_shield_hp: 1000.0,
                    armor_hp: 1000.0,
                    max_armor_hp: 1000.0,
                    structure_hp: 1000.0,
                    capacitor: 1000.0,
                    max_capacitor: 1000.0,
                    ..Default::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id,
                    category_id: 6,
                    owner_id: cid,
                    char_id: Some(cid),
                    corp_id: 1_000_009,
                    ..Default::default()
                },
            )
            .await;
    }

    let make_sess = |cid: i64, sid: i64| {
        let uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
        ctx.db
            .acquire_online_lock(uid, cid as u64, Some(cid), true, now_unix as u64)
            .unwrap();
        let mut s = eve_machonet::SessionState::new(cid as u64);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(1_000_009);
        s.solar_system_id = Some(null_sys);
        s.ship_id = Some(sid);
        s
    };
    let mut cruiser_sess = make_sess(char_id, cruiser_id);
    let mut frig_sess = make_sess(frig_char_id, frig_id);
    let kw = HashMap::new();

    // Activate real non-covert cloak via dogmaIM.Activate on both cruiser and frigate outside 500 km ESS zone.
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut cruiser_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("cruiser dogmaIM.Activate cloak should succeed");
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut frig_sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(frig_cloak_id),
            PyRep::String("cloaking".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("frigate dogmaIM.Activate cloak should succeed");

    // Preconditions: active module cycle, CombatState cloak + velocity multiplier, nerfed ball speed, slim meta cloaked.
    assert!(sys.has_module_cycle(cruiser_id, cloak_id).await);
    let pre_cs = sys.get_combat_state(cruiser_id).await.unwrap().unwrap();
    assert!(pre_cs.is_cloaked);
    assert!(pre_cs.cloak_velocity_multiplier.is_some());
    let (_, pre_ball) = sys.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!(
        pre_ball.max_velocity < baseline_max_vel - 1.0,
        "cloaked cruiser max_velocity {} must be below baseline {}",
        pre_ball.max_velocity,
        baseline_max_vel
    );

    // 1. Out-of-range ESS gate activation fails WITHOUT clearing actor cycle, DB state, or CombatState cloak.
    let _ = sys
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );
    assert!(sys.has_module_cycle(cruiser_id, cloak_id).await);
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&cloak_id)));
    assert!(sys.get_combat_state(cruiser_id).await.unwrap().unwrap().is_cloaked);

    // 2. Restricted hull within range fails WITHOUT clearing actor cycle or decloaking.
    let _ = sys
        .set_ball_position(frig_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, frig_char_id, null_sys, frig_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipRestrictedByEssGate"
    );
    assert!(sys.has_module_cycle(frig_id, frig_cloak_id).await);
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&frig_cloak_id)));
    assert!(sys.get_combat_state(frig_id).await.unwrap().unwrap().is_cloaked);

    // 3. Move cloaked cruiser within <= 2500m of ESS gate, clear buffered events/notifs, and activate gate.
    let _ = sys
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    sys.ballpark.write().unwrap().pending_events.clear();

    try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .unwrap()
        .expect("cloaked eligible cruiser within range should activate ESS gate and auto-decloak");

    // (a) Actor ActiveModuleCycle + DB active state completely removed
    assert!(
        !sys.has_module_cycle(cruiser_id, cloak_id).await,
        "cloak ActiveModuleCycle must be removed on ESS gate decloak"
    );
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&cloak_id)));

    // (b) CombatState decloaked & velocity multiplier cleared
    let post_cs = sys.get_combat_state(cruiser_id).await.unwrap().unwrap();
    assert!(!post_cs.is_cloaked);
    assert!(!post_cs.is_covert_cloak);
    assert!(post_cs.cloak_velocity_multiplier.is_none());
    assert!(post_cs.decloak_targeting_delay_remaining_s > 0.0);

    // (c) Ball max_velocity restored to uncloaked baseline immediately on ESS gate decloak
    let (_, post_ball) = sys.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!(
        (post_ball.max_velocity - baseline_max_vel).abs() < 1e-6,
        "expected restored max_velocity {}, got {}",
        baseline_max_vel,
        post_ball.max_velocity
    );

    // (d) ball_slim_meta.is_cloaked cleared + CloakBall(0) + OnSpecialFX + OnGodmaShipEffect(active=0) emitted
    assert!(
        !sys.ballpark
            .read()
            .unwrap()
            .ball_slim_meta
            .get(&cruiser_id)
            .is_some_and(|m| m.is_cloaked),
        "ball_slim_meta.is_cloaked must be false after ESS gate decloak"
    );
    {
        let bp = sys.ballpark.read().unwrap();
        assert!(
            bp.pending_events.iter().any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == cruiser_id
            )),
            "expected DestinyEvent::CloakBall(cloaked=0), got {:?}",
            bp.pending_events
        );
        assert!(
            bp.pending_events.iter().any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
                    if *ball_id == cruiser_id && guid == "effects.Cloak"
            )),
            "expected DestinyEvent::OnSpecialFX(effects.Cloak, false), got {:?}",
            bp.pending_events
        );
    }
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnCloakModuleForceDeactivated"),
        "expected OnCloakModuleForceDeactivated: {notifs:?}"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnGodmaShipEffect"
                && n.args.first().and_then(|v| v.as_i64()) == Some(cloak_id)
                && n.args.get(4).and_then(|v| v.as_i64()) == Some(0)
        }),
        "expected OnGodmaShipEffect(active=0) for cloak {cloak_id}: {notifs:?}"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnSpecialFX"
                && n.args.first().and_then(|v| v.as_i64()) == Some(cruiser_id)
                && matches!(n.args.get(5), Some(PyRep::String(g)) if g == "effects.Cloak")
                && n.args.get(8).and_then(|v| v.as_i64()) == Some(0)
        }),
        "expected Macho OnSpecialFX(effects.Cloak, active=0): {notifs:?}"
    );

    // Subsequent set_combat_state + tick must NOT resurrect cloak_velocity_multiplier or re-nerf max_velocity
    let _ = sys.set_combat_state(cruiser_id, post_cs.clone()).await;
    let _ = sys.try_step_full_tick(1.0);
    let after_tick_cs = sys.get_combat_state(cruiser_id).await.unwrap().unwrap();
    let (_, after_tick_ball) = sys.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!(
        after_tick_cs.cloak_velocity_multiplier.is_none(),
        "reconcile_tackle_and_bubbles must not resurrect cloak_velocity_multiplier"
    );
    assert!(
        (after_tick_ball.max_velocity - baseline_max_vel).abs() < 1e-6,
        "expected baseline max_velocity {} after tick, got {}",
        baseline_max_vel,
        after_tick_ball.max_velocity
    );

    // (e) ESS main bank link succeeds (NOT rejected with CantLinkWhileCloaked) once positioned at beacon
    {
        let mut bp = sys.ballpark.write().unwrap();
        let b = bp.balls.get_mut(&cruiser_id).unwrap();
        b.position = ess_pos + glam::DVec3::new(1_000.0, 0.0, 0.0);
        b.mode = MovementMode::Stop;
        b.velocity = glam::DVec3::ZERO;
    }
    let link_res = crate::services::encounter_surveillance_system::handle_ess_link(
        &ctx,
        &cruiser_sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await;
    assert!(
        link_res.is_ok(),
        "AttemptLinkToMainBank should succeed after ESS gate decloak, got: {:?}",
        link_res
    );

    // (f) Reactivating cloak outside 500 km must NOT hit ModuleAlreadyActive (orphaned cycle is gone)
    let _ = sys
        .set_ball_position(cruiser_id, cloak_activate_pos)
        .await;
    let reactivate_res = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut cruiser_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())],
        &kw,
    )
    .await;
    if let Some(Err(err)) = reactivate_res {
        assert_ne!(
            err.code, "ModuleAlreadyActive",
            "cloak cycle must not stay locked as ModuleAlreadyActive"
        );
    }

    // (g) Residual cloak_velocity_multiplier without is_cloaked still clears cleanly at ESS gate
    let _ = sys.remove_module_cycle(cruiser_id, cloak_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_id);
        if let Some(st) = snap.system_states.get_mut(&null_sys) {
            st.ess_linked_character_id = None;
            st.ess_link_started_tick = None;
        }
    });
    let _ = sys
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let _ = sys
        .set_combat_state(
            cruiser_id,
            eve_destiny::CombatState {
                base_max_velocity: Some(baseline_max_vel),
                cloak_velocity_multiplier: Some(0.25),
                ..Default::default()
            },
        )
        .await;
    let _ = sys.set_ball_max_velocity(cruiser_id, baseline_max_vel * 0.25).await;
    try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .unwrap()
        .expect("residual cloak_velocity_multiplier should still clear cleanly");
    let residual_cs = sys.get_combat_state(cruiser_id).await.unwrap().unwrap();
    let (_, residual_ball) = sys.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!(residual_cs.cloak_velocity_multiplier.is_none());
    assert!((residual_ball.max_velocity - baseline_max_vel).abs() < 1e-6);

    // (h) Stale `ball_slim_meta.is_cloaked = true` (which blocks AttemptLinkToMainBank with
    // CantLinkWhileCloaked) is cleared by `break_gate_cloak` + ESS link succeeds afterwards.
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.ball_slim_meta.get_mut(&cruiser_id).unwrap().is_cloaked = true;
        let b = bp.balls.get_mut(&cruiser_id).unwrap();
        b.position = ess_pos + glam::DVec3::new(1_000.0, 0.0, 0.0);
        b.mode = MovementMode::Stop;
        b.velocity = glam::DVec3::ZERO;
        bp.pending_events.clear();
    }
    assert_eq!(
        crate::services::encounter_surveillance_system::handle_ess_link(
            &ctx,
            &cruiser_sess,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CantLinkWhileCloaked"
    );
    let _ = sys
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .unwrap()
        .expect("ESS gate must clear stale ball_slim_meta.is_cloaked");
    assert!(
        !sys.ballpark
            .read()
            .unwrap()
            .ball_slim_meta
            .get(&cruiser_id)
            .is_some_and(|m| m.is_cloaked)
    );
    assert!(
        sys.ballpark
            .read()
            .unwrap()
            .pending_events
            .iter()
            .any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == cruiser_id
            )),
        "expected DestinyEvent::CloakBall(cloaked=0) when clearing stale slim_meta.is_cloaked"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        let b = bp.balls.get_mut(&cruiser_id).unwrap();
        b.position = ess_pos + glam::DVec3::new(1_000.0, 0.0, 0.0);
        b.mode = MovementMode::Stop;
        b.velocity = glam::DVec3::ZERO;
    }
    crate::services::encounter_surveillance_system::handle_ess_link(
        &ctx,
        &cruiser_sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .expect("AttemptLinkToMainBank must succeed once stale ball_slim_meta.is_cloaked is cleared");
}

#[tokio::test]
async fn ess_gate_auto_decloaks_fitted_cloak_module_only_after_range_and_hull_checks_gh_1408() {
    let sde = shared_sde();
    let null_sys: SolarSystemId = sde
        .solar_systems
        .values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("SDE has K-space null-sec system");
    let cruiser_tid = sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 26 && t.published)
        .map(|(tid, _)| *tid)
        .expect("cruiser type");
    let frigate_tid = sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 25 && t.published)
        .map(|(tid, _)| *tid)
        .expect("frigate type");
    let cloak_tid = sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 330 && t.published)
        .map(|(tid, _)| *tid)
        .expect("cloak module type");

    let char_id: i64 = 90_050_408;
    let cruiser_id: i64 = 1_050_408_001;
    let frigate_id: i64 = 1_050_408_002;
    let cloak_mod_id: i64 = 1_050_408_101;
    let now_unix = 1_775_760_000_u64;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ess_cloak_pilot", "pw", char_id, "EssCloakPilot");
    let user_id = db.with_snapshot(|s| s.accounts["ess_cloak_pilot"].user_id);
    db.acquire_online_lock(user_id, 501, Some(char_id), true, now_unix)
        .expect("online lock");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.active_ship_id = cruiser_id;
        }
        snap.system_states
            .insert(null_sys, SystemDynamicState::default());
        snap.items.insert(
            cruiser_id,
            eve_db::InventoryItemRecord {
                item_id: cruiser_id,
                type_id: cruiser_tid,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            frigate_id,
            eve_db::InventoryItemRecord {
                item_id: frigate_id,
                type_id: frigate_tid,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cloak_mod_id,
            eve_db::InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: cruiser_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(cloak_mod_id, true);
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cloak_mod_id);
    });
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let gate_pos = sys_handle
        .get_ball_snapshot(gate_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;

    // Spawn cloaked cruiser outside activation range (>2500m) with fitted cloak penalty.
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: cruiser_id,
            type_id: cruiser_tid,
            position: gate_pos + glam::DVec3::new(3_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 20.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            is_cloaked: true,
            is_covert_cloak: false,
            cloak_velocity_multiplier: Some(0.1),
            cloak_stabilization_remaining_s: 600.0,
            ..Default::default()
        },
    );
    crate::services::mobile_deployables_ext::record_ship_cloak_activated(&ctx, cruiser_id);

    // 1. Out of range (>2500m) fails OutsideGateActivationRange WITHOUT decloaking or clearing module_active_states.
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );
    assert!(sys_handle
        .get_combat_state(cruiser_id)
        .await
        .unwrap()
        .unwrap()
        .is_cloaked);
    assert!(ctx
        .db
        .with_snapshot(|s| s.module_active_states.contains(&cloak_mod_id)));

    // 2. Ineligible hull inside range fails ShipRestrictedByEssGate WITHOUT decloaking.
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: frigate_id,
            type_id: frigate_tid,
            position: gate_pos + glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            is_cloaked: true,
            ..Default::default()
        },
    );
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, frigate_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipRestrictedByEssGate"
    );
    assert!(sys_handle
        .get_combat_state(frigate_id)
        .await
        .unwrap()
        .unwrap()
        .is_cloaked);

    // 3. Eligible cruiser inside <= 2500m activates, auto-decloaks both combat state and fitted cloak module, and emits notification.
    let _ = sys_handle
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(500.0, 0.0, 0.0))
        .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    let ok = try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .expect("handled")
        .expect("eligible cloaked cruiser inside 2500m activates ESS gate");
    assert_eq!(ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    let cs_after = sys_handle.get_combat_state(cruiser_id).await.unwrap().unwrap();
    assert!(!cs_after.is_cloaked);
    assert!(!cs_after.is_covert_cloak);
    assert_eq!(cs_after.cloak_stabilization_remaining_s, 0.0);
    assert_eq!(cs_after.cloak_velocity_multiplier, None);
    assert!(!ctx
        .db
        .with_snapshot(|s| s.module_active_states.contains(&cloak_mod_id)));
    assert!(ctx.db.with_snapshot(|s| {
        !crate::services::mobile_deployables::read_deployable_state_snap(s)
            .cloak_activated_unix
            .contains_key(&cruiser_id)
    }));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs
            .iter()
            .any(|n| n.event == "OnCloakModuleForceDeactivated"),
        "expected OnCloakModuleForceDeactivated notification, got: {notifs:?}"
    );
}

#[tokio::test]
async fn ess_gate_auto_decloak_records_exact_attr_669_reactivation_delay_without_rem_cycle_inflation_gh_1423()
{
    let mut sde = (*shared_sde()).clone();
    let null_sys: SolarSystemId = 30_099_423;
    sde.solar_systems
        .insert(null_sys, synthetic_solar_system(null_sys, -0.5));
    let cloak_type_id: eve_sde::TypeId = 11577; // Improved Cloaking Device II (attr 669 = 30_000 ms)
    let base_cloak = sde
        .types
        .get(&cloak_type_id)
        .expect("SDE must contain Improved Cloaking Device II (11577)")
        .clone();
    assert_eq!(
        base_cloak.default_attributes.get(&669).copied(),
        Some(30_000.0),
        "Improved Cloaking Device II (11577) must define attr 669 = 30000 ms"
    );
    let fallback_cloak_type_id: eve_sde::TypeId = 99_330_423;
    let mut fallback_cloak = base_cloak;
    fallback_cloak.type_id = fallback_cloak_type_id;
    fallback_cloak.name = "Synthetic Cloak Without Attr 669".into();
    fallback_cloak.default_attributes.remove(&669);
    sde.types.insert(fallback_cloak_type_id, fallback_cloak);

    let cruiser_type_id: eve_sde::TypeId = 626; // Vexor (group 26, ESS-eligible)
    let sde = Arc::new(sde);

    let db = Arc::new(WorldRepository::new_in_memory().unwrap());
    let now_unix = 1_700_060_000_u64;
    let char_id = 91_423_101_i64;
    let cruiser_id = 91_423_102_i64;
    let cloak_id = 91_423_103_i64;
    let fb_char_id = 91_423_201_i64;
    let fb_cruiser_id = 91_423_202_i64;
    let fb_cloak_id = 91_423_203_i64;

    db.provision_gm_account("ess_1423_pilot", "pw", char_id, "Ess1423Pilot");
    db.provision_gm_account("ess_1423_fb", "pw", fb_char_id, "Ess1423Fallback");

    let req_skills: Vec<u32> = [182_u16, 183, 184]
        .into_iter()
        .filter_map(|aid| {
            sde.types
                .get(&cloak_type_id)
                .and_then(|t| t.default_attributes.get(&aid))
                .map(|&v| v as u32)
        })
        .chain([SKILL_CPU_MANAGEMENT, SKILL_POWER_GRID_MANAGEMENT, 11579])
        .collect();

    db.with_mut_snapshot(|snap| {
        for &(cid, sid, mid, mtid) in &[
            (char_id, cruiser_id, cloak_id, cloak_type_id),
            (fb_char_id, fb_cruiser_id, fb_cloak_id, fallback_cloak_type_id),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = null_sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            let skills = snap.skills.entry(cid).or_default();
            for &skill_tid in &req_skills {
                skills.insert(
                    skill_tid,
                    eve_db::TrainedSkillRecord {
                        type_id: skill_tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: cruiser_type_id,
                    owner_id: cid,
                    location_id: i64::from(null_sys),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: mtid,
                    owner_id: cid,
                    location_id: sid,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.system_states
            .insert(null_sys, SystemDynamicState::default());
    });

    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
        rt.safety_levels
            .insert(fb_char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let ess_ball_id = ESS_BEACON_ID_BASE + i64::from(null_sys);
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let sys = ctx.systems.get_or_spawn(null_sys);
    let ess_pos = sys.get_ball_snapshot(ess_ball_id).await.unwrap().unwrap().1.position;
    let gate_pos = sys.get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;
    let cloak_activate_pos = ess_pos + glam::DVec3::new(600_000.0, 0.0, 0.0);

    for &(cid, sid, pos) in &[
        (char_id, cruiser_id, cloak_activate_pos),
        (
            fb_char_id,
            fb_cruiser_id,
            cloak_activate_pos + glam::DVec3::new(0.0, 5_000.0, 0.0),
        ),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: cruiser_type_id,
                    position: pos,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 150.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    base_max_velocity: Some(200.0),
                    shield_hp: 1000.0,
                    max_shield_hp: 1000.0,
                    armor_hp: 1000.0,
                    max_armor_hp: 1000.0,
                    structure_hp: 1000.0,
                    capacitor: 1000.0,
                    max_capacitor: 1000.0,
                    ..Default::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id: 26,
                    category_id: 6,
                    owner_id: cid,
                    char_id: Some(cid),
                    corp_id: 1_000_009,
                    ..Default::default()
                },
            )
            .await;
    }

    let make_sess = |cid: i64, sid: i64| {
        let uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
        ctx.db
            .acquire_online_lock(uid, cid as u64, Some(cid), true, now_unix)
            .unwrap();
        let mut s = eve_machonet::SessionState::new(cid as u64);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(1_000_009);
        s.solar_system_id = Some(null_sys);
        s.ship_id = Some(sid);
        s
    };
    let mut cruiser_sess = make_sess(char_id, cruiser_id);
    let mut fb_sess = make_sess(fb_char_id, fb_cruiser_id);
    let kw = HashMap::new();

    // Activate real Improved Cloaking Device II (11577) via dogmaIM.Activate (creates ActiveModuleCycle
    // with duration_s = 1.0 and reactivation_delay_s = 30.0 so remove_module_cycle returns ~31.0).
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut cruiser_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("cruiser dogmaIM.Activate cloak 11577 should succeed");
    assert!(sys.has_module_cycle(cruiser_id, cloak_id).await);
    assert!(!ctx.is_module_in_reactivation_delay(cruiser_id, cloak_id));

    // Move within ESS gate activation range and trigger forced auto-decloak.
    let _ = sys
        .set_ball_position(cruiser_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .unwrap()
        .expect("cloaked cruiser activates ESS gate");
    assert!(!sys.has_module_cycle(cruiser_id, cloak_id).await);

    // Exact 30.0s reactivation delay from SDE attr 669 (no +rem_cycle inflation to 31.0s).
    let recorded_delay_s = ctx
        .module_reactivation_cooldowns
        .read()
        .unwrap()
        .get(&(cruiser_id, cloak_id, cloak_type_id))
        .map(|entry| entry.delay_s)
        .expect("cloak reactivation cooldown must be recorded");
    assert!(
        (recorded_delay_s - 30.0).abs() < 1e-6,
        "expected exact 30.0s reactivation delay from attr 669 (no +rem_cycle inflation), got {recorded_delay_s}"
    );
    let until_unix = ctx
        .module_reactivation_cooldown_until_unix(cruiser_id, cloak_id)
        .expect("active reactivation cooldown expiry");
    assert!(
        (until_unix - (now_unix as f64 + 30.0)).abs() < 1e-6,
        "expected cooldown expiry at now+30.0 ({}), got {until_unix}",
        now_unix as f64 + 30.0
    );

    // Real cloak module deactivation emits OnCloakModuleForceDeactivated and NOT OnJumpCloakCancelled.
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs
            .iter()
            .any(|n| n.event == "OnCloakModuleForceDeactivated"),
        "expected OnCloakModuleForceDeactivated on active cloak module decloak: {notifs:?}"
    );
    assert!(
        !notifs.iter().any(|n| n.event == "OnJumpCloakCancelled"),
        "non-gate cloak module decloak must not emit OnJumpCloakCancelled: {notifs:?}"
    );

    // Verify exact 30.0s boundary via virtual clock: still blocked at +29s, cleared at +30s.
    ctx.clock().advance_secs(29);
    assert!(
        ctx.is_module_in_reactivation_delay(cruiser_id, cloak_id),
        "cloak must remain on reactivation delay at +29s"
    );
    ctx.clock().advance_secs(1);
    assert!(
        !ctx.is_module_in_reactivation_delay(cruiser_id, cloak_id),
        "cloak reactivation delay must expire at exact +30s (no +rem_cycle inflation)"
    );

    // Fallback check: when SDE attr 669 is absent, fall back to `removed_delay` from remove_module_cycle.
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut fb_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(fb_cloak_id), PyRep::String("cloaking".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("fallback cloak dogmaIM.Activate should succeed");
    assert!(sys.has_module_cycle(fb_cruiser_id, fb_cloak_id).await);
    let _ = sys
        .set_ball_position(fb_cruiser_id, gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    try_activate_ess_acceleration_gate(&ctx, fb_char_id, null_sys, fb_cruiser_id, gate_id)
        .await
        .unwrap()
        .expect("fallback cloak cruiser activates ESS gate");
    let fb_delay_s = ctx
        .module_reactivation_cooldowns
        .read()
        .unwrap()
        .get(&(fb_cruiser_id, fb_cloak_id, fallback_cloak_type_id))
        .map(|entry| entry.delay_s)
        .expect("fallback removed_delay must be recorded when attr 669 is absent");
    assert!(
        fb_delay_s > 0.0 && fb_delay_s <= 1.0 + 1e-6,
        "expected fallback removed_delay in (0.0, 1.0], got {fb_delay_s}"
    );
}

#[tokio::test]
async fn ess_gate_jump_gate_cloak_and_stale_slim_meta_do_not_emit_cloak_module_force_deactivated_gh_1423()
{
    let mut sde = (*shared_sde()).clone();
    let null_sys: SolarSystemId = 30_099_424;
    sde.solar_systems
        .insert(null_sys, synthetic_solar_system(null_sys, -0.5));
    let cruiser_type_id: eve_sde::TypeId = 626; // Vexor (group 26, ESS-eligible)
    let sde = Arc::new(sde);

    let db = Arc::new(WorldRepository::new_in_memory().unwrap());
    let now_unix = 1_700_070_000_u64;
    let char_id = 91_424_101_i64;
    let cruiser_id = 91_424_102_i64;

    db.provision_gm_account("ess_1423_gate_cloak", "pw", char_id, "Ess1423GateCloak");
    let user_id = db.with_snapshot(|s| s.accounts["ess_1423_gate_cloak"].user_id);
    db.acquire_online_lock(user_id, char_id as u64, Some(char_id), true, now_unix)
        .expect("online lock");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = cruiser_id;
        }
        snap.items.insert(
            cruiser_id,
            eve_db::InventoryItemRecord {
                item_id: cruiser_id,
                type_id: cruiser_type_id,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.system_states
            .insert(null_sys, SystemDynamicState::default());
    });

    let ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let sys = ctx.systems.get_or_spawn(null_sys);
    let gate_pos = sys.get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;

    // 1. Jump-gate-cloak-only ESS gate activation (was_gate_cloak == true, active_cloaks.is_empty()).
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: cruiser_id,
                type_id: cruiser_type_id,
                position: gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 60,
                base_max_velocity: Some(200.0),
                ..Default::default()
            },
            eve_destiny::SlimItemMeta {
                group_id: 26,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;

    let _ = ctx.drain_notifications_for_char(char_id);
    sys.ballpark.write().unwrap().pending_events.clear();

    let gate_ok = try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .expect("handled")
        .expect("jump-gate-cloaked cruiser activates ESS gate");
    assert_eq!(gate_ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));

    let post_cs = sys.get_combat_state(cruiser_id).await.unwrap().unwrap();
    assert!(!post_cs.is_cloaked);
    assert_eq!(post_cs.gate_cloak_remaining_ticks, 0);
    assert!(
        !sys.ballpark
            .read()
            .unwrap()
            .ball_slim_meta
            .get(&cruiser_id)
            .is_some_and(|m| m.is_cloaked),
        "ball_slim_meta.is_cloaked must be cleared when breaking jump-gate cloak"
    );
    assert!(
        sys.ballpark
            .read()
            .unwrap()
            .pending_events
            .iter()
            .any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == cruiser_id
            )),
        "expected DestinyEvent::CloakBall(cloaked=0) on jump-gate cloak cancel"
    );

    let gate_cloak_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        gate_cloak_notifs.iter().any(|n| {
            n.event == "OnJumpCloakCancelled"
                && n.args.first().and_then(|v| v.as_i64()) == Some(cruiser_id)
        }),
        "expected OnJumpCloakCancelled({cruiser_id}), got: {gate_cloak_notifs:?}"
    );
    assert!(
        !gate_cloak_notifs
            .iter()
            .any(|n| n.event == "OnCloakModuleForceDeactivated"),
        "jump-gate-cloak-only ESS gate activation must NOT emit OnCloakModuleForceDeactivated: {gate_cloak_notifs:?}"
    );

    // 2. Stale `ball_slim_meta.is_cloaked = true` clearance (no active cloak module, no CS cloak)
    // clears slim meta + emits CloakBall(0) without emitting OnCloakModuleForceDeactivated.
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.ball_slim_meta.get_mut(&cruiser_id).unwrap().is_cloaked = true;
        let b = bp.balls.get_mut(&cruiser_id).unwrap();
        b.position = gate_pos + glam::DVec3::new(1_000.0, 0.0, 0.0);
        b.mode = MovementMode::Stop;
        b.velocity = glam::DVec3::ZERO;
        bp.pending_events.clear();
    }
    let _ = ctx.drain_notifications_for_char(char_id);

    try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, cruiser_id, gate_id)
        .await
        .expect("handled")
        .expect("stale slim_meta.is_cloaked clears cleanly on ESS gate activation");
    assert!(
        !sys.ballpark
            .read()
            .unwrap()
            .ball_slim_meta
            .get(&cruiser_id)
            .is_some_and(|m| m.is_cloaked)
    );
    assert!(
        sys.ballpark
            .read()
            .unwrap()
            .pending_events
            .iter()
            .any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == cruiser_id
            )),
        "expected DestinyEvent::CloakBall(cloaked=0) on stale slim_meta.is_cloaked clearance"
    );
    let stale_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !stale_notifs
            .iter()
            .any(|n| n.event == "OnCloakModuleForceDeactivated"),
        "stale ball_slim_meta.is_cloaked clearance must NOT emit OnCloakModuleForceDeactivated: {stale_notifs:?}"
    );
    assert!(
        !stale_notifs
            .iter()
            .any(|n| n.event == "OnJumpCloakCancelled"),
        "stale ball_slim_meta.is_cloaked clearance must NOT emit OnJumpCloakCancelled: {stale_notifs:?}"
    );
}
