//! Additional edge-case and branch coverage tests for `services::pve_ess_reserve_banks`.

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::{AgentInfo, InvType, SdeRegistry};
use glam::DVec3;
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
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn seed_ship_type(sde: &mut Arc<SdeRegistry>, tid: u32, group_id: u32) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().unwrap();
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = 6;
    rec.name = format!("EssShip_{tid}");
    rec.radius = 150.0;
    rec.mass = 10_000_000.0;
    reg.types.insert(tid, rec);
}

#[test]
fn ess_eligibility_rnd_access_rp_and_storyline_edges() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let agent_id = 3_018_401_i64;

    // 1. is_ess_gate_eligible_hull
    seed_ship_type(&mut ctx.sde, 81_001, 26); // Cruiser
    seed_ship_type(&mut ctx.sde, 81_002, 358); // HAC
    seed_ship_type(&mut ctx.sde, 81_003, 832); // Logistics Cruiser
    seed_ship_type(&mut ctx.sde, 81_004, 419); // Combat Battlecruiser
    seed_ship_type(&mut ctx.sde, 81_005, 27); // Battleship
    seed_ship_type(&mut ctx.sde, 81_006, 25); // Frigate
    seed_ship_type(&mut ctx.sde, 81_007, 900); // Marauder
    seed_ship_type(&mut ctx.sde, 81_008, 898); // Black Ops
    seed_ship_type(&mut ctx.sde, 81_009, 28); // Hauler
    for ok_tid in 81_001..=81_005 {
        assert!(is_ess_gate_eligible_hull(&ctx.sde, ok_tid), "expected eligible {ok_tid}");
    }
    for no_tid in 81_006..=81_009 {
        assert!(!is_ess_gate_eligible_hull(&ctx.sde, no_tid), "expected restricted {no_tid}");
    }

    // 2. is_storyline_qualifying_agent_type & is_interbus_or_militia_corp
    assert!(is_storyline_qualifying_agent_type(2)); // BasicAgent
    assert!(!is_storyline_qualifying_agent_type(3)); // Tutorial
    assert!(is_storyline_qualifying_agent_type(4)); // Research
    assert!(!is_storyline_qualifying_agent_type(5)); // Data Center
    assert!(!is_storyline_qualifying_agent_type(6)); // Storyline
    assert!(!is_storyline_qualifying_agent_type(12)); // Career
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.npc_corporations.insert(
            1_000_125,
            eve_sde::NpcCorporationInfo {
                corporation_id: 1_000_125,
                name: "InterBus".into(),
                ticker_name: "IB".into(),
                faction_id: None,
                solar_system_id: None,
                station_id: None,
                tax_rate: 0.0,
            },
        );
    }
    assert!(is_interbus_or_militia_corp(&ctx.sde, 1_000_125));
    assert!(!is_interbus_or_militia_corp(&ctx.sde, 1_000_009));

    // 3. resolve_datacore_for_research_skill
    assert!(resolve_datacore_for_research_skill(&ctx.sde, SKILL_SCIENCE).is_none());
    assert!(resolve_datacore_for_research_skill(&ctx.sde, SKILL_RESEARCH_PROJECT_MANAGEMENT).is_none());
    assert!(resolve_datacore_for_research_skill(&ctx.sde, 587).is_none());

    let field_skill_id = 11_441_u32;
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let template = reg.types.values().next().cloned().unwrap();
        let mut sk = template.clone();
        sk.type_id = field_skill_id;
        sk.group_id = 270;
        sk.category_id = 16;
        sk.published = true;
        reg.types.insert(field_skill_id, sk);

        let mut dc = template;
        dc.type_id = 20_410;
        dc.group_id = 333;
        dc.category_id = 17;
        dc.published = true;
        dc.default_attributes.insert(182, field_skill_id as f64);
        reg.types.insert(20_410, dc);
    }
    assert_eq!(
        resolve_datacore_for_research_skill(&ctx.sde, field_skill_id),
        Some(20_410)
    );

    // 4. record_agent_mission_completed idempotency & has_completed_mission_with_agent
    ctx.db.with_mut_snapshot(|snap| {
        assert!(!has_completed_mission_with_agent(snap, char_id, agent_id));
        record_agent_mission_completed(snap, char_id, agent_id);
        record_agent_mission_completed(snap, char_id, agent_id);
        assert!(has_completed_mission_with_agent(snap, char_id, agent_id));
    });

    // 5. validate_rnd_research_access error hierarchy
    let agent = AgentInfo {
        agent_id,
        name: "R&D Test".into(),
        corporation_id: 1_000_009,
        level: 4,
        division_id: 22,
        agent_type_id: 4,
        is_locator: false,
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        research_skills: vec![field_skill_id],
    };

    ctx.db.with_mut_snapshot(|snap| {
        // Not docked -> AgentNotCoLocated
        assert_eq!(
            validate_rnd_research_access(&ctx, None, sys_id, snap, char_id, &agent, Some(field_skill_id))
                .unwrap_err()
                .code,
            "AgentNotCoLocated"
        );
        // Wrong skill offered -> InvalidResearchSkill
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, Some(SKILL_SCIENCE))
                .unwrap_err()
                .code,
            "InvalidResearchSkill"
        );
        // Missing Science V -> ErrorNoRelevantResearchSkills
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, Some(field_skill_id))
                .unwrap_err()
                .code,
            "ErrorNoRelevantResearchSkills"
        );

        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(SKILL_SCIENCE, TrainedSkillRecord { type_id: SKILL_SCIENCE, level: 5, skill_points: 256_000.0 });
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, Some(field_skill_id))
                .unwrap_err()
                .code,
            "ErrorNoRelevantResearchSkills"
        );

        snap.skills.entry(char_id).or_default().insert(SKILL_CPU_MANAGEMENT, TrainedSkillRecord { type_id: SKILL_CPU_MANAGEMENT, level: 5, skill_points: 256_000.0 });
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, Some(field_skill_id))
                .unwrap_err()
                .code,
            "ErrorNoRelevantResearchSkills"
        );

        snap.skills.entry(char_id).or_default().insert(field_skill_id, TrainedSkillRecord { type_id: field_skill_id, level: 4, skill_points: 45_255.0 });
        assert_eq!(
            validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, Some(field_skill_id))
                .unwrap_err()
                .code,
            "InsufficientAgentStanding"
        );

        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.standings.insert(agent_id, 6.0);
        ch.standings.insert(1_000_009, 6.0);
        snap.characters.insert(char_id, ch);
        let (sk, lvl) = validate_rnd_research_access(&ctx, Some(st_id), sys_id, snap, char_id, &agent, None).unwrap();
        assert_eq!(sk, field_skill_id);
        assert_eq!(lvl, 4);

        let rp = compute_rnd_rp_per_day(&ctx, snap, char_id, agent_id, 4, 4);
        assert!(rp > 0.0);
    });

    // 6. record_mission_completion_and_resolve_storyline: 15 normal missions return None, 16th checks storyline
    let story_src = AgentInfo {
        agent_id: 3_018_402,
        name: "Mission Agent".into(),
        corporation_id: 1_000_009,
        level: 1,
        division_id: 24,
        agent_type_id: 2,
        is_locator: false,
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        research_skills: Vec::new(),
    };
    ctx.db.with_mut_snapshot(|snap| {
        for _ in 0..15 {
            assert!(record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &story_src).is_none());
        }
        let _ = record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &story_src);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ess_gate_restrictions_autopayout_reserve_links_and_bounty_split_edges() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let high_sys = 30_000_142_u32;
    let null_sys = 30_004_001_u32;
    let now = ctx.now_unix();

    seed_ship_type(&mut ctx.sde, 622, 26); // Stabber Cruiser (eligible)
    seed_ship_type(&mut ctx.sde, 587, 25); // Rifter Frigate (restricted)
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let mut null_info = reg.solar_systems.get(&high_sys).unwrap().clone();
        null_info.solar_system_id = null_sys;
        null_info.security_status = -0.5;
        reg.solar_systems.insert(null_sys, null_info);
    }

    // 1. ensure_ess_intrusion_site_balls skips non-nullsec, spawns in nullsec, and is idempotent
    ensure_ess_intrusion_site_balls(&ctx, high_sys).await;
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;

    let gate_id = ESS_GATE_ID_BASE + i64::from(null_sys);
    let beacon_id = ESS_BEACON_ID_BASE + i64::from(null_sys);
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let gate_ball = sys_h.get_ball_snapshot(gate_id).await.unwrap().expect("ess gate spawned").1;
    let beacon_ball = sys_h.get_ball_snapshot(beacon_id).await.unwrap().expect("ess beacon spawned").1;

    // 2. try_activate_ess_acceleration_gate: ShipNotInSpace -> OutsideGateActivationRange -> ShipRestrictedByEssGate
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Frigate
                owner_id: char_id,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let far_pos = gate_ball.position + DVec3::new(10_000.0, 0.0, 0.0);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: far_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );

    sys_h.try_set_ball_position(ship_id, gate_ball.position + DVec3::new(500.0, 0.0, 0.0));
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipRestrictedByEssGate"
    );

    // 3. check_ess_intrusion_bubble_restriction inside ESS bubble (cloaking blocked within 500km)
    sys_h.try_set_ball_position(ship_id, beacon_ball.position + DVec3::new(500.0, 0.0, 0.0));
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cloak")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );
    assert_eq!(
        check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "cyno")
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCynoBlocked"
    );
    assert_eq!(
        check_ess_warp_destination_allowed(&ctx, null_sys, beacon_ball.position, beacon_ball.position)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );

    // 4. tick_ess_main_bank_autopayout conserves cents across contributors
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.system_states.get(&null_sys).cloned().unwrap_or(eve_db::SystemDynamicState {
            solar_system_id: null_sys,
            dbs_multiplier: 1.35,
            ess_main_bank: IskCents(0),
            ess_reserve_bank: IskCents(0),
            ess_linked_character_id: None,
            ess_link_started_tick: None,
            ess_cycle_started_unix: 0,
            ess_main_bank_contributors: Default::default(),
            sov_alliance_id: None,
            military_index: 1,
            industrial_index: 1,
            strategic_index: 1,
            fw_contested_pct: 0.0,
            insurgency_corruption_stage: 0,
            insurgency_suppression_stage: 0,
            wormhole_remaining_mass_kg: 0.0,
            wormhole_destination_system_id: None,
            signature_position: None,
            ..Default::default()
        });
        st.ess_main_bank = IskCents(100_000);
        st.ess_reserve_bank = IskCents(500_000);
        st.ess_cycle_started_unix = now.saturating_sub(15_000);
        st.ess_main_bank_contributors.insert(char_id, 2);
        st.ess_main_bank_contributors.insert(90_000_002, 1);
        snap.system_states.insert(null_sys, st);
    });
    let paid_cnt = tick_ess_main_bank_autopayout(&ctx, now);
    assert_eq!(paid_cnt, 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.system_states.get(&null_sys).unwrap().ess_main_bank, IskCents(0));
    });

    // 5. ess_reserve_pulse_weight & compute_ess_reserve_pulse_payout_cents & tick_ess_reserve_bank_links
    assert_eq!(ess_reserve_pulse_weight(1, 1), 300);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(0, 15, 5), 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(10_000, 15, 0), 0);
    assert_eq!(compute_ess_reserve_pulse_payout_cents(10_000, 1, 1), 10_000);

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys);
        st.reserve_linked_character_id = Some(char_id);
        st.link_started_unix = Some(now - 61);
        st.pulses_remaining = 1;
        st.pulses_total = 1;
        st.unlocked_until_unix = now + 60;
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys, &st);

        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.solar_system_id = null_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        assert!(ensure_ess_reserve_bank_not_already_unlocked(snap, null_sys, now, 0).is_err());
    });

    // Position ship >2500m from ESS beacon -> tick_ess_reserve_bank_links severs link with reason 1
    sys_h.try_set_ball_position(ship_id, beacon_ball.position + DVec3::new(4_000.0, 0.0, 0.0));
    let _ = tick_ess_reserve_bank_links(&ctx, now).await;
    ctx.db.with_snapshot(|snap| {
        let st = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys);
        assert!(st.reserve_linked_character_id.is_none());
    });

    // 6. credit_ballpark_npc_bounty_with_ess_split: highsec direct vs nullsec player corp tax & ESS split
    let player_corp = 98_700_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = player_corp; __corp.name = "ESS Bounty Corp".into(); __corp.tax_rate = 0.10; __corp.ensure_wallet_divisions(); __corp },
        );
        let before_bal = snap.characters.get(&char_id).unwrap().balance.0;
        credit_ballpark_npc_bounty_with_ess_split(&ctx.sde, snap, high_sys, char_id, 50_000.0, 50_000.0, 1.0, 23_000, now);
        let after_hs = snap.characters.get(&char_id).unwrap().balance.0;
        assert_eq!(after_hs - before_bal, 5_000_000);

        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.corporation_id = player_corp;
        snap.characters.insert(char_id, ch);
        let ess_main_before = snap.system_states.get(&null_sys).map(|e| e.ess_main_bank.0).unwrap_or(0);
        credit_ballpark_npc_bounty_with_ess_split(&ctx.sde, snap, null_sys, char_id, 100_000.0, 100_000.0, 1.0, 23_000, now);
        assert!(snap.corporations.get(&player_corp).unwrap().master_wallet_balance().0 > 0);
        assert!(snap.system_states.get(&null_sys).unwrap().ess_main_bank.0 > ess_main_before);
    });
}
