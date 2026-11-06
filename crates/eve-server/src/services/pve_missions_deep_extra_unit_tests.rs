use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    compute_effective_agent_standing, compute_effective_standing_breakdown,
};
use crate::services::pve_ess_reserve_banks::{
    is_interbus_or_militia_corp, record_mission_completion_and_resolve_storyline,
};
use eve_db::{
    ActiveMissionRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, ServerClock, TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, MissionTemplate, NpcCorporationInfo, SdeRegistry, StationInfo};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn mk_char(character_id: i64, name: &str, clone_grade: u32, station_id: Option<i64>, ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id,
        user_id: character_id as u64,
        name: name.to_string(),
        corporation_id: 1_000_035,
        alliance_id: None,
        war_faction_id: None,
        station_id,
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: ship_id,
        balance: IskCents::from_isk(1_000_000),
        plex_balance: 0,
        loyalty_points: Default::default(),
        standings: Default::default(),
        security_status: 5.0,
        clone_grade,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 1,
        neural: NeuralAttributes {
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            charisma: 19,
            bonus_remaps: 0,
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
        },
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn build_mission_test_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    let station_a = 60_003_760i64;
    let station_b = 60_003_761i64;
    let corp_empire = 1_000_035i64;
    let corp_militia = 1_000_180i64;
    let corp_interbus = 1_000_054i64;

    for (sid, name, pos) in [
        (station_a, "Jita IV - Caldari Navy Assembly Plant", DVec3::ZERO),
        (station_b, "Jita IV - Moon 4 - Caldari Business Tribunal", DVec3::new(100_000.0, 0.0, 0.0)),
    ] {
        sde.stations.entry(sid).or_insert(StationInfo {
            station_id: sid,
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            station_type_id: 1531,
            name: name.into(),
            position: pos,
            owner_corporation_id: corp_empire,
            owner_faction_id: Some(500_001),
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        });
    }

    for (cid, name, ticker, fid) in [
        (corp_empire, "Caldari Navy", "CN", Some(500_001)),
        (corp_militia, "State Protectorate", "SPROT", Some(500_001)),
        (corp_interbus, "InterBus", "IB", None),
    ] {
        sde.npc_corporations.insert(
            cid,
            NpcCorporationInfo {
                corporation_id: cid,
                name: name.into(),
                ticker_name: ticker.into(),
                faction_id: fid,
                solar_system_id: Some(30_000_142),
                station_id: Some(station_a),
                tax_rate: 0.1,
            },
        );
    }

    let add_agent =
        |sde: &mut SdeRegistry, agent_id: i64, corp: i64, lvl: u8, div: u32, tid: u32| {
            sde.agents.insert(
                agent_id,
                AgentInfo {
                    agent_id,
                    name: format!("Agent {agent_id}"),
                    corporation_id: corp,
                    station_id: Some(station_a),
                    solar_system_id: Some(30_000_142),
                    level: lvl,
                    division_id: div,
                    agent_type_id: tid,
                    is_locator: false,
                    research_skills: Vec::new(),
                },
            );
        };

    add_agent(&mut sde, 3_010_001, corp_empire, 1, 24, 2);
    add_agent(&mut sde, 3_010_002, corp_empire, 1, 22, 2);
    add_agent(&mut sde, 3_010_003, corp_empire, 1, 23, 2);
    add_agent(&mut sde, 3_010_004, corp_empire, 4, 24, 2);
    add_agent(&mut sde, 3_010_005, corp_empire, 5, 24, 2);
    add_agent(&mut sde, 3_010_006, corp_empire, 1, 24, 6);
    add_agent(&mut sde, 3_010_007, corp_empire, 1, 24, 4);
    add_agent(&mut sde, 3_010_008, corp_militia, 1, 24, 9);

    for mid in 10_001..=10_020u32 {
        sde.missions.insert(
            mid,
            MissionTemplate {
                mission_id: mid,
                title: format!("Security Encounter {mid}"),
                is_courier: false,
                lp_alpha: 1_000,
                lp_omega: 1_500,
                has_standing_rewards: true,
                content_template: Some("BasicKillMission".into()),
                dungeon_id: None,
            },
        );
    }
    sde.missions.insert(
        10_050,
        MissionTemplate {
            mission_id: 10_050,
            title: "Urgent Courier Run".into(),
            is_courier: true,
            lp_alpha: 500,
            lp_omega: 750,
            has_standing_rewards: true,
            content_template: Some("BasicCourierMission".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        10_099,
        MissionTemplate {
            mission_id: 10_099,
            title: "Storyline Briefing".into(),
            is_courier: false,
            lp_alpha: 3_000,
            lp_omega: 4_500,
            has_standing_rewards: true,
            content_template: Some("StorylineMission".into()),
            dungeon_id: None,
        },
    );
    Arc::new(sde)
}

#[test]
fn test_effective_standing_breakdown_and_agent_min_max_rules() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(10_000));
    let ctx = ServerContext::with_sde_and_repository(build_mission_test_sde(), db);
    ctx.set_clock(clock);
    let char_id = 91_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = mk_char(char_id, "StandingsTester", 2, None, 1_000_001);
        for sk in [3359u32, 3357, 3361] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        ch.standings.insert(1_000_035, 5.0);
        ch.standings.insert(500_026, -4.0);
        ch.standings.insert(3_010_001, -5.0);
        ch.standings.insert(500_001, 6.0);
        snap.characters.insert(char_id, ch);
    });

    ctx.db.with_snapshot(|snap| {
        let (eff_empire, conn_b, dip_b, crim_b) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 1_000_035, 5.0, true);
        assert!((eff_empire - 6.0).abs() < 1e-6);
        assert!((conn_b - 1.0).abs() < 1e-6);
        assert_eq!((dip_b, crim_b), (0.0, 0.0));

        let (eff_trig, t_c, t_d, t_cr) =
            compute_effective_standing_breakdown(&ctx, snap, char_id, 500_026, -4.0, true);
        assert!((eff_trig - (-4.0)).abs() < 1e-6);
        assert_eq!((t_c, t_d, t_cr), (0.0, 0.0, 0.0));

        let eff_ag = compute_effective_agent_standing(
            &ctx,
            snap,
            char_id,
            3_010_001,
            1_000_035,
            Some(500_001),
        );
        assert!(eff_ag <= -2.0 + 1e-6);
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(3_010_001, 0.0);
    });
    ctx.db.with_snapshot(|snap| {
        let eff_ag = compute_effective_agent_standing(
            &ctx,
            snap,
            char_id,
            3_010_001,
            1_000_035,
            Some(500_001),
        );
        assert!((eff_ag - 6.8).abs() < 1e-6);
    });
}

async fn call_agent_view(
    ctx: &ServerContext,
    sess: &mut SessionState,
    agent_id: i64,
) -> Option<Result<PyRep, eve_proto::BridgeRpcError>> {
    crate::services::agents_missions::handle(
        ctx,
        sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
        &HashMap::new(),
    )
    .await
}

#[tokio::test]
async fn test_agent_do_action_eligibility_gates_and_standing_thresholds() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(20_000));
    let ctx = ServerContext::with_sde_and_repository(build_mission_test_sde(), db);
    ctx.set_clock(clock);
    let char_id = 91_000_002i64;
    let ship_id = 2_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, "GateTester", 1, Some(60_003_760), ship_id));
    });

    let mut sess = SessionState::new(2);
    sess.user_id = Some(2);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(ship_id);

    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_006).await.unwrap().unwrap_err().code, "StorylineAgentRequiresReferral");
    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_008).await.unwrap().unwrap_err().code, "NotEnlistedInMilitia");
    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_004).await.unwrap().unwrap_err().code, "CantDoMissionAlphaClone");

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().clone_grade = 2);
    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_005).await.unwrap().unwrap_err().code, "CantDoMission");
    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_007).await.unwrap().unwrap_err().code, "ResearchNotActive");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_035, 4.99);
    });
    assert_eq!(call_agent_view(&ctx, &mut sess, 3_010_004).await.unwrap().unwrap_err().code, "InsufficientAgentStanding");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_035, 5.0);
    });
    let ok_offer = call_agent_view(&ctx, &mut sess, 3_010_004).await.unwrap().expect("L4 offer ok");
    assert_eq!(ok_offer.dict_get("agentID").and_then(|v| v.as_i64()), Some(3_010_004));
}

#[tokio::test]
async fn test_agent_decline_four_hour_penalty_window_and_expiry() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(100_000));
    let ctx = ServerContext::with_sde_and_repository(build_mission_test_sde(), db);
    ctx.set_clock(clock.clone());
    let char_id = 91_000_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, "DeclineTester", 2, Some(60_003_760), 2_000_003));
    });
    let mut sess = SessionState::new(3);
    sess.user_id = Some(3);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(2_000_003);

    for (advance_secs, expect_penalty) in [(0u64, false), (14_399, true), (14_400, false)] {
        if advance_secs > 0 {
            clock.advance_secs(advance_secs);
        }
        let offer = crate::services::agents_missions::handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(3_010_001)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        let mid = offer.dict_get("missionID").unwrap().as_i64().unwrap();
        let dec = crate::services::agents_missions::handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(9), PyRep::Integer(mid), PyRep::Integer(3_010_001)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            dec.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()),
            Some(expect_penalty)
        );
    }
}

#[tokio::test]
async fn test_storyline_16_mission_trigger_and_courier_expiry_lifecycle() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(200_000));
    let ctx = ServerContext::with_sde_and_repository(build_mission_test_sde(), db);
    ctx.set_clock(clock.clone());
    assert!(is_interbus_or_militia_corp(&ctx.sde, 1_000_054));
    assert!(is_interbus_or_militia_corp(&ctx.sde, 1_000_180));
    assert!(!is_interbus_or_militia_corp(&ctx.sde, 1_000_035));
    let char_id = 91_000_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, "CourierStoryTester", 2, Some(60_003_760), 2_000_004));
        snap.active_missions.insert(
            (char_id, 10_050),
            ActiveMissionRecord {
                mission_id: 10_050,
                character_id: char_id,
                agent_id: 3_010_002,
                solar_system_id: 30_000_142,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.items.insert(
            2_000_004,
            InventoryItemRecord {
                item_id: 2_000_004,
                type_id: 588,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        crate::services::agents_missions::agents_standings_arcs::save_mission_meta(
            snap,
            char_id,
            &crate::services::agents_missions::agents_standings_arcs::MissionTimingAndCargoMeta {
                mission_id: 10_050,
                agent_id: 3_010_002,
                offered_at_unix: 200_000,
                offer_expiry_unix: 200_000 + 604_800,
                offered_level: Some(1),
                required_delivery_type_id: Some(34),
                required_delivery_qty: 100,
                origin_station_id: Some(60_003_760),
                destination_station_id: Some(60_003_761),
                ..Default::default()
            },
        );
    });

    let empire_agent = ctx.sde.agents[&3_010_001].clone();
    let mut interbus_agent = empire_agent.clone();
    interbus_agent.agent_id = 3_010_099;
    interbus_agent.corporation_id = 1_000_054;
    ctx.db.with_mut_snapshot(|snap| {
        for _ in 0..15u32 {
            let triggered =
                record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &empire_agent);
            assert!(triggered.is_none());
        }
        let sl_ib =
            record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &interbus_agent);
        assert!(sl_ib.is_none());
        let sl_16 =
            record_mission_completion_and_resolve_storyline(&ctx, snap, char_id, &empire_agent);
        let Some((sl_agent_id, 1)) = sl_16 else {
            panic!("expected storyline agent on 16th mission, got {sl_16:?}");
        };
        assert!(ctx.sde.agents.contains_key(&sl_agent_id));
    });

    let mut sess = SessionState::new(4);
    sess.user_id = Some(4);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(2_000_004);

    let acc = crate::services::agents_missions::handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(10_050), PyRep::Integer(3_010_002)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("accept courier");
    assert_eq!(acc.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));

    clock.advance_secs(604_801);
    let exp_err = crate::services::agents_missions::handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(7), PyRep::Integer(10_050), PyRep::Integer(3_010_002)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(exp_err.code, "MissionExpired");
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert!((ch.standings[&3_010_002] - (-0.10)).abs() < 1e-6);
        assert!((ch.standings[&1_000_035] - (-0.10)).abs() < 1e-6);
        assert!((ch.standings[&500_001] - (-0.05)).abs() < 1e-6);
    });
}
