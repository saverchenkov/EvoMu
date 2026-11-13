//! Deep branch-coverage and mutation-killing unit tests for `services::agents_missions::agents_missions_helpers`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::agents_missions::agents_missions_helpers::*;
use crate::services::agents_missions::agents_standings_arcs::{
    load_mission_meta, save_mission_meta, MissionTimingAndCargoMeta,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    AgentInfo, DungeonTemplate, MissionTemplate, NpcSpawnGroupInfo, SdeRegistry, SolarSystem,
    StationInfo,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx(base_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn rebuild_ctx_with_sde(ctx: &ServerContext, sde: SdeRegistry) -> ServerContext {
    let next = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::clone(&ctx.db));
    next.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        ctx.now_unix_secs(),
    )));
    next
}

fn tmpl(id: u32, title: &str, ct: Option<&str>, is_courier: bool) -> MissionTemplate {
    MissionTemplate {
        mission_id: id,
        title: title.into(),
        content_template: ct.map(Into::into),
        is_courier,
        lp_omega: 1_000,
        lp_alpha: 600,
        has_standing_rewards: true,
        dungeon_id: None,
    }
}

fn agent(id: i64, corp_id: i64, agent_type_id: u32, division_id: u32, level: u8) -> AgentInfo {
    AgentInfo {
        agent_id: id,
        name: format!("Agent {id}"),
        corporation_id: corp_id,
        division_id,
        level,
        station_id: Some(60_003_760),
        solar_system_id: Some(30_000_142),
        agent_type_id,
        is_locator: false,
        research_skills: vec![],
    }
}

#[test]
fn parse_mission_id_and_agent_pool_classification_exhaustive_branches() {
    assert_eq!(parse_mission_id(&[PyRep::Integer(0)]).unwrap(), 0);
    assert_eq!(
        parse_mission_id(&[PyRep::Integer(u32::MAX as i64)]).unwrap(),
        u32::MAX
    );
    assert!(parse_mission_id(&[PyRep::String("42".into())]).is_err());
    assert!(parse_mission_id(&[PyRep::Integer(i64::from(u32::MAX) + 1)]).is_err());

    // Storyline template detection via content_template and title keywords
    assert!(is_storyline_template(&tmpl(1, "Routine", Some("StorylineKill"), false)));
    assert!(is_storyline_template(&tmpl(2, "A Storyline Offer", None, false)));
    assert!(is_storyline_template(&tmpl(3, "Important Delivery", None, true)));
    assert!(is_storyline_template(&tmpl(4, "Materials For War Preparation", None, false)));
    assert!(!is_storyline_template(&tmpl(5, "Recon", Some("BasicKill"), false)));

    // All 8 FW militia corporation IDs
    for corp_id in [
        1_000_121, 1_000_147, 1_000_085, 1_000_009, 1_000_179, 1_000_180, 1_000_181, 1_000_182,
    ] {
        assert!(is_fw_agent(&agent(1, corp_id, 2, 24, 1)));
    }
    assert!(!is_fw_agent(&agent(1, 1_000_035, 2, 24, 1)));

    // Warzone systems for secondary militia corps
    assert_eq!(resolve_fw_warzone_system(1_000_181), 30_002_062);
    assert_eq!(resolve_fw_warzone_system(1_000_179), 30_002_097);
    assert_eq!(resolve_fw_warzone_system(1_000_182), 30_002_098);
    assert_eq!(resolve_fw_warzone_system(1_000_180), 30_002_061);
    assert_eq!(resolve_fw_warzone_system(9_999_999), 30_002_062);

    // Clamping on rnd_tritanium_required_qty and mission_bonus_duration
    assert_eq!(rnd_tritanium_required_qty(0), 300);
    assert_eq!(rnd_tritanium_required_qty(9), 24_300);
    assert_eq!(mission_bonus_duration(0), 21_600);
    assert_eq!(mission_bonus_duration(9), 21_600);

    // Exhaustive mission_matches_agent_pool matrix across agent_type_id & division_id
    let t_story = tmpl(1, "Important Task", Some("BasicKill"), false);
    let t_epic = tmpl(2, "Epic Chapter", Some("EpicArc"), false);
    let t_res = tmpl(3, "Field Data", Some("Research"), false);
    let t_her = tmpl(4, "Emblem", Some("Heraldry"), false);
    let t_kill = tmpl(5, "Clear Outpost", Some("BasicKill"), false);
    let t_cour = tmpl(6, "Haul", Some("BasicCourier"), true);
    let t_mine = tmpl(7, "Asteroid Extraction", Some("BasicMining"), false);
    let t_mine_title = tmpl(8, "Ice Mining Operation", Some("Custom"), false);
    let t_trade = tmpl(9, "Supply Run", Some("BasicTrade"), false);

    assert!(!mission_matches_agent_pool(&t_story, &agent(1, 1_000_009, 2, 24, 1)));
    assert!(mission_matches_agent_pool(&t_epic, &agent(1, 1_000_009, 10, 24, 1)));
    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 10, 24, 1)));
    assert!(mission_matches_agent_pool(&t_res, &agent(1, 1_000_009, 4, 18, 1)));
    assert!(mission_matches_agent_pool(&t_her, &agent(1, 1_000_009, 13, 24, 1)));
    assert!(mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 13, 24, 1)));
    assert!(mission_matches_agent_pool(&t_mine, &agent(1, 1_000_009, 12, 24, 1)));
    assert!(mission_matches_agent_pool(&t_trade, &agent(1, 1_000_009, 5, 24, 1)));
    assert!(mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 8, 24, 1)));
    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 11, 24, 1)));

    // Division branches for regular agent_type_id == 2
    assert!(mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 2, 18, 1)));
    assert!(mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 2, 22, 1)));
    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 2, 22, 1)));
    assert!(mission_matches_agent_pool(&t_mine_title, &agent(1, 1_000_009, 2, 23, 1)));
    assert!(mission_matches_agent_pool(&t_her, &agent(1, 1_000_009, 2, 37, 1)));
    assert!(mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 2, 99, 1)));
}

#[test]
fn courier_trade_and_mining_keyword_matrix_and_station_fallbacks() {
    let mut ctx = make_ctx(1_775_000_000);

    // Exhaustive trade keyword table
    for (id, title, expected_tid) in [
        (1, "Tritanium Shipment", 34),
        (2, "Holoreel Delivery", 3647),
        (3, "Microorganism Sample", 43),
        (4, "Synthetic Oil Reserves", 3691),
        (5, "Wheat Harvest", 3673),
        (6, "Oxygen Tanks", 3683),
        (7, "Cattle Transport", 3725),
        (8, "Consumer Goods", 3673),
        (9, "Mining Drill Parts", 21632),
        (10, "Electronic Components", 3687),
        (11, "Fine Wine Cases", 42),
        (12, "Pyroxeres Batch", 1224),
        (13, "Scordite Batch", 1228),
        (14, "Unlisted Commodity", 34), // 14 % 8 == 6 -> (34, 1000)
        (15, "Unlisted Commodity", 21632), // 15 % 8 == 7 -> (21632, 10)
    ] {
        let res = resolve_trade_mission_details(&ctx, &tmpl(id, title, None, false));
        assert_eq!(res.trade_type_id, expected_tid, "failed trade title {title}");
        assert!(res.trade_quantity > 0);
    }

    // Exhaustive mining keyword table
    for (id, title, expected_tid, expected_qty) in [
        (1, "Banidine Deposit", 28617, 500),
        (2, "Augumene Belt", 28618, 500),
        (3, "Mercium Rush", 28619, 500),
        (4, "Lyavite Pocket", 28620, 250),
        (5, "Pithix Cluster", 28621, 250),
        (6, "Arisite Vein", 28622, 100),
        (7, "Geodite Core", 28624, 50),
        (8, "Gneiss Formation", 1229, 100),
        (9, "Omber Fields", 1227, 500),
        (10, "Kernite Rich", 20, 250),
        (11, "Pyroxeres Belt", 1224, 500),
        (12, "Scordite Fragments", 1228, 500),
        (13, "Plagioclase Claim", 18, 500),
        (14, "Veldspar Survey", 1230, 500),
        (15, "Generic Asteroid", 20, 250), // 15 % 4 == 3 -> (20, 250)
    ] {
        let res = resolve_mining_mission_details(&ctx, &tmpl(id, title, None, false));
        assert_eq!(res.ore_type_id, expected_tid, "failed mining title {title}");
        assert_eq!(res.required_units, expected_qty);
        assert!(res.objective_text.contains(&expected_qty.to_string()));
    }

    // Exhaustive courier keyword table & station fallback (region fallback, global fallback, error paths)
    let mut sde = (*ctx.sde).clone();
    sde.stations.clear();
    sde.solar_systems.clear();
    let st_origin = StationInfo {
        station_id: 60_100_001,
        solar_system_id: 30_100_001,
        constellation_id: 20_100_001,
        region_id: 10_100_001,
        station_type_id: 1529,
        name: "Origin Station".into(),
        position: DVec3::ZERO,
        owner_corporation_id: 1_000_009,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    };
    let mut st_region_peer = st_origin.clone();
    st_region_peer.station_id = 60_100_002;
    st_region_peer.solar_system_id = 30_100_002;
    st_region_peer.constellation_id = 20_100_999; // Different constellation, same region!

    sde.solar_systems.insert(
        30_100_001,
        SolarSystem {
            solar_system_id: 30_100_001,
            region_id: 10_100_001,
            constellation_id: 20_100_001,
            name: "Sys1".into(),
            security_status: 0.8,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![st_origin.clone()],
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_100_002,
        SolarSystem {
            solar_system_id: 30_100_002,
            region_id: 10_100_001,
            constellation_id: 20_100_999,
            name: "Sys2".into(),
            security_status: 0.5,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![st_region_peer.clone()],
            wormhole_class_id: None,
        },
    );
    sde.stations.insert(60_100_001, st_origin);
    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());

    let mut ag = agent(5001, 1_000_009, 2, 22, 1);
    ag.station_id = None;
    ag.solar_system_id = None;
    assert_eq!(
        resolve_courier_mission_details(&ctx, &tmpl(1, "Courier", None, true), &ag)
            .unwrap_err()
            .code,
        "InvalidMissionLocation"
    );

    ag.station_id = Some(60_100_001);
    ag.solar_system_id = Some(30_100_001);
    // Only 1 station in entire SDE -> NoDestinationStationAvailable
    assert_eq!(
        resolve_courier_mission_details(&ctx, &tmpl(1, "Courier", None, true), &ag)
            .unwrap_err()
            .code,
        "NoDestinationStationAvailable"
    );

    // Add region peer station -> resolves via region fallback
    sde.stations.insert(60_100_002, st_region_peer);
    ctx = rebuild_ctx_with_sde(&ctx, sde);

    for (id, title, expected_tid) in [
        (1, "Marine Escort", 3810),
        (2, "Militant Transfer", 25373),
        (3, "Small Arms Crate", 9844),
        (4, "Antibiotic Supply", 43),
        (5, "Party Spirits", 9850),
        (6, "Tobacco Leaves", 9852),
        (7, "Quafe Ultra", 3699),
        (8, "Food Ration", 3673),
        (9, "Breakdown Tool", 21632),
        (10, "Secret Intelligence Report", 3814),
        (16, "Unclassified Box", 3814), // 16 % 8 == 0 -> 3814
    ] {
        let det = resolve_courier_mission_details(&ctx, &tmpl(id, title, None, true), &ag).unwrap();
        assert_eq!(det.cargo_type_id, expected_tid, "failed courier title {title}");
        assert_eq!(det.origin_station_id, 60_100_001);
        assert_eq!(det.destination_station_id, 60_100_002);
    }
}

#[test]
fn rewards_divisions_alpha_vs_omega_target_resolution_and_security_encounters() {
    let mut ctx = make_ctx(1_775_000_000);
    let mut sde = (*ctx.sde).clone();

    // Seed agents in divisions 22 (Distribution -> 3894), 23 (Mining -> 3893), 24 (Security -> 3895), and 18 (R&D -> None)
    for (aid, div, a_type, lvl) in [
        (3_100_001, 22, 2, 1),
        (3_100_002, 23, 2, 2),
        (3_100_003, 24, 2, 4),
        (3_100_004, 18, 4, 3),
        (3_100_005, 24, 6, 1), // storyline agent
    ] {
        sde.agents
            .insert(aid, agent(aid, 1_000_009, a_type, div, lvl));
    }
    sde.missions.insert(200, tmpl(200, "Angel Cartel Assault", Some("BasicKill"), false));
    sde.dungeons.clear();
    sde.dungeons.insert(
        901,
        DungeonTemplate {
            dungeon_id: 901,
            name: "Angel Hideout".into(),
            archetype_id: Some(20),
            faction_id: Some(0),
            difficulty: Some(4),
        },
    );
    sde.npc_spawn_groups.insert(
        801,
        NpcSpawnGroupInfo {
            spawn_group_id: 801,
            group_id: 801,
            group_name: "Angel Cartel Battleship".into(),
            faction_id: 500_011,
            ship_class: 3,
            npc_type_ids: vec![587],
        },
    );
    ctx = rebuild_ctx_with_sde(&ctx, sde);

    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1; // Alpha clone uses lp_alpha (600) or offered_level * 100 floor
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3894,
            TrainedSkillRecord {
                type_id: 3894,
                level: 3,
                skill_points: 20_000.0,
            },
        );
        sk.insert(
            3893,
            TrainedSkillRecord {
                type_id: 3893,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 200,
                agent_id: 3_100_001,
                offered_level: Some(5), // floor = 500, lp_alpha = 600 -> 600
                ..Default::default()
            },
        );
    });

    let CharacterMissionRewards {
        isk_reward: isk_dist,
        lp_reward: lp_dist,
        bonus_reward: bonus_dist,
    } = compute_character_mission_rewards(&ctx, char_id, 3_100_001, 200, 30_000_142);
    let lp_mine = compute_character_mission_rewards(&ctx, char_id, 3_100_002, 200, 30_000_142).lp_reward;
    let lp_rnd = compute_character_mission_rewards(&ctx, char_id, 3_100_004, 200, 30_000_142).lp_reward;
    assert_eq!(isk_dist, 600_000);
    assert_eq!(bonus_dist, 300_000);
    assert_eq!(lp_rnd, 0);
    assert!(lp_mine > lp_dist && lp_dist > lp_rnd);

    // Storyline, Epic Arc, and Career Agent missions yield 0 LP while retaining ISK rewards (`Loyalty_Points.md:53`)
    let mut sde_ext = (*ctx.sde).clone();
    sde_ext.agents.insert(3_100_005, agent(3_100_005, 1_000_009, 6, 24, 1)); // Storyline
    sde_ext.agents.insert(3_100_006, agent(3_100_006, 1_000_009, 10, 24, 1)); // Epic Arc
    sde_ext.agents.insert(3_100_007, agent(3_100_007, 1_000_009, 12, 24, 1)); // Career Agent
    sde_ext.missions.insert(
        202,
        tmpl(202, "Storyline Important Task", Some("StorylineKill"), false),
    );
    sde_ext.missions.insert(
        203,
        tmpl(203, "Epic Arc Ch1", Some("EpicArcKill"), false),
    );
    ctx = rebuild_ctx_with_sde(&ctx, sde_ext);
    let CharacterMissionRewards {
        isk_reward: isk_story,
        lp_reward: lp_story,
        ..
    } = compute_character_mission_rewards(&ctx, char_id, 3_100_005, 202, 30_000_142);
    let CharacterMissionRewards {
        isk_reward: isk_epic,
        lp_reward: lp_epic,
        ..
    } = compute_character_mission_rewards(&ctx, char_id, 3_100_006, 203, 30_000_142);
    let CharacterMissionRewards {
        isk_reward: isk_career,
        lp_reward: lp_career,
        ..
    } = compute_character_mission_rewards(&ctx, char_id, 3_100_007, 200, 30_000_142);
    assert!(isk_story > 0 && isk_epic > 0 && isk_career > 0);
    assert_eq!(lp_story, 0);
    assert_eq!(lp_epic, 0);
    assert_eq!(lp_career, 0);

    // Security encounter faction keyword resolution (Guristas, Angel, Blood, Sansha, Serpentis)
    for (title, expected_faction) in [
        ("Guristas Pirates", 500_010),
        ("Angel Cartel Raid", 500_011),
        ("Blood Raiders Covenant", 500_012),
        ("Sansha Incursion", 500_019),
        ("Serpentis Narcotics", 500_020),
    ] {
        let enc = resolve_security_mission_encounter(
            &ctx,
            &tmpl(201, title, Some("BasicKill"), false),
            ctx.sde.agents.get(&3_100_003).unwrap(),
            30_000_142,
        );
        assert_eq!(enc.faction_id, expected_faction);
        assert_eq!(enc.wave_npc_count, 5);
    }

    // resolve_target_agent_id via arg[0], arg[1], bound_objects, and system fallback
    let mut sess = SessionState::new(777);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();

    assert_eq!(
        resolve_target_agent_id(&ctx, &sess, char_id, 30_000_142, &[PyRep::Integer(3_100_002)], &kw),
        (3_100_002, true)
    );
    assert_eq!(
        resolve_target_agent_id(
            &ctx,
            &sess,
            char_id,
            30_000_142,
            &[PyRep::Integer(1), PyRep::Integer(3_100_003)],
            &kw
        ),
        (3_100_003, true)
    );
    ctx.bound_objects.write().unwrap().insert(
        99,
        BoundObjectEntry {
            bound_id: 99,
            session_id: 777,
            user_id: 1,
            character_id: char_id,
            service: "agentMgr".into(),
            object_id: 3_100_004,
            oid_str: "N=1:99".into(),
            ref_id: 1,
        },
    );
    assert_eq!(
        resolve_target_agent_id(&ctx, &sess, char_id, 30_000_142, &[], &kw),
        (3_100_004, true)
    );
    ctx.bound_objects.write().unwrap().clear();
    let (sys_ag, explicit) = resolve_target_agent_id(&ctx, &sess, char_id, 30_000_142, &[], &kw);
    assert!(!explicit && sys_ag > 0);
    let (story_ag, _) =
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_000_142, Some(60_003_760), true);
    assert_eq!(story_ag, 3_100_005);
}

#[tokio::test]
async fn resolve_target_mission_id_and_decline_cooldown_vs_rnd_standing_rules() {
    let mut ctx = make_ctx(1_775_000_000);
    let mut sde = (*ctx.sde).clone();
    sde.agents
        .insert(3_200_001, agent(3_200_001, 1_000_009, 2, 24, 1));
    sde.agents
        .insert(3_200_002, agent(3_200_002, 1_000_009, 4, 18, 1)); // R&D agent
    sde.missions
        .insert(301, tmpl(301, "M1", Some("BasicKill"), false));
    ctx = rebuild_ctx_with_sde(&ctx, sde);

    let char_id = 90_000_001_i64;
    let kw = HashMap::new();
    let mut sess = SessionState::new(888);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);

    // No active missions -> MissionNotActive
    assert_eq!(
        resolve_target_mission_id(&ctx, char_id, 3_200_001, Some("Offered"), &[], &kw)
            .unwrap_err()
            .code,
        "MissionNotActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        for (mid, aid, st) in [
            (301_u32, 3_200_001_i64, "Offered"),
            (302_u32, 3_200_001_i64, "Offered"),
            (303_u32, 3_200_002_i64, "Offered"),
            (304_u32, 3_200_001_i64, "Completed"),
        ] {
            snap.active_missions.insert(
                (char_id, mid),
                ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: mid,
                    agent_id: aid,
                    state: st.into(),
                    solar_system_id: 30_000_142,
                    current_room_id: 0,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: vec![],
                    bookmark_id: Some(88_001),
                },
            );
        }
        // Partial courier cargo stack (qty 25 > 10 granted) to test partial reclaim
        snap.items.insert(
            66_001,
            InventoryItemRecord {
                item_id: 66_001,
                type_id: 3810,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 301,
                agent_id: 3_200_001,
                granted_courier_item_id: Some(66_001),
                granted_courier_qty: 10,
                ..Default::default()
            },
        );
    });

    // Arg[0] <= 19 is an action code, NOT a mission_id; arg[1] or active_missions lookup resolves
    assert_eq!(
        resolve_target_mission_id(
            &ctx,
            char_id,
            3_200_001,
            Some("Offered"),
            &[PyRep::Integer(19)],
            &kw
        )
        .unwrap(),
        301
    );
    assert_eq!(
        resolve_target_mission_id(
            &ctx,
            char_id,
            3_200_001,
            Some("Offered"),
            &[PyRep::Integer(302)],
            &kw
        )
        .unwrap(),
        302
    );

    // Terminal state mission cannot be abandoned
    assert_eq!(
        abandon_or_decline_mission(&ctx, &sess, char_id, 304)
            .await
            .unwrap_err()
            .code,
        "MissionNotActive"
    );

    // R&D agent mission decline (303) never applies standing penalty or sets decline cooldown
    let (st_rnd, pen_rnd, acc_rnd) = abandon_or_decline_mission(&ctx, &sess, char_id, 303)
        .await
        .unwrap();
    assert_eq!(st_rnd, "Declined");
    assert!(!pen_rnd && !acc_rnd);

    // First regular mission decline (301) -> no standing penalty, partial courier stack decremented 25 -> 15
    let (st1, pen1, acc1) = abandon_or_decline_mission(&ctx, &sess, char_id, 301)
        .await
        .unwrap();
    assert_eq!(st1, "Declined");
    assert!(!pen1 && !acc1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&66_001].quantity, 15);
        assert!(load_mission_meta(snap, char_id, 301)
            .unwrap()
            .granted_courier_item_id
            .is_none());
    });

    // Second regular mission decline within 4 hours (302) -> standing penalty (-0.05) applied!
    let (st2, pen2, acc2) = abandon_or_decline_mission(&ctx, &sess, char_id, 302)
        .await
        .unwrap();
    assert_eq!(st2, "Declined");
    assert!(pen2 && !acc2);
    ctx.db.with_snapshot(|snap| {
        assert!((snap.characters[&char_id].standings[&1_000_009] - (-0.05)).abs() < 1e-9);
    });
}

#[test]
fn wave4_agent_pool_divisions_npc_selector_and_mission_target_fallbacks() {
    let ctx = make_ctx(1_775_100_000);
    let mut sde = (*ctx.sde).clone();

    // Complete division matrix & None content_template check
    let t_res = tmpl(1, "Field Theory", Some("Research"), false);
    let t_kill = tmpl(2, "Ambush", Some("BasicKill"), false);
    let t_cour = tmpl(3, "Delivery", Some("BasicCourier"), false);
    let t_mine = tmpl(4, "Rocks", Some("BasicMining"), false);
    let t_none = tmpl(5, "Blank", None, false);

    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 4, 18, 1)));
    assert!(mission_matches_agent_pool(&t_res, &agent(1, 1_000_009, 2, 18, 1)));
    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 2, 18, 1)));
    assert!(mission_matches_agent_pool(&t_mine, &agent(1, 1_000_009, 2, 23, 1)));
    assert!(!mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 2, 23, 1)));
    assert!(!mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 2, 24, 1)));
    assert!(mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 2, 37, 1)));
    assert!(!mission_matches_agent_pool(&t_cour, &agent(1, 1_000_009, 2, 37, 1)));
    assert!(mission_matches_agent_pool(&t_kill, &agent(1, 1_000_009, 2, 99, 1)));
    assert!(!mission_matches_agent_pool(&t_mine, &agent(1, 1_000_009, 2, 99, 1)));
    assert!(!mission_matches_agent_pool(&t_mine, &agent(1, 1_000_009, 5, 24, 1)));
    assert!(!mission_matches_agent_pool(&t_none, &agent(1, 1_000_009, 12, 24, 1)));

    // resolve_division_connections_skill all fallback division IDs
    assert_eq!(resolve_division_connections_skill(&ctx, 27), Some(3894));
    for sec_div in [20_u32, 21, 24, 25, 26, 28] {
        assert_eq!(
            resolve_division_connections_skill(&ctx, sec_div),
            Some(3895),
            "division {sec_div} must resolve Security Connections"
        );
    }
    assert_eq!(resolve_division_connections_skill(&ctx, 999), None);

    // resolve_mission_npc_type boundary & fallback tests
    let base_inv = sde.types.values().next().cloned().unwrap();
    let make_rat = |tid: u32, bounty: bool, radius: f64, shield: f64, armor: f64, hull: f64| {
        let mut t = base_inv.clone();
        t.type_id = tid;
        t.category_id = 11;
        t.radius = radius;
        t.default_attributes.clear();
        if bounty {
            t.default_attributes.insert(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 5_000.0);
        }
        t.default_attributes.insert(eve_sde::attr_ids::SHIELD_CAPACITY, shield);
        t.default_attributes.insert(eve_sde::attr_ids::ARMOR_HP, armor);
        t.default_attributes.insert(eve_sde::attr_ids::HP_STRUCT, hull);
        t
    };

    sde.types.clear();
    sde.npc_spawn_groups.clear();
    // Invalid boundaries: no bounty, radius 24.9 (<25), EHP 49.9 (<50), EHP 450.1 (>450)
    sde.types.insert(91_001, make_rat(91_001, false, 30.0, 100.0, 100.0, 50.0));
    sde.types.insert(91_002, make_rat(91_002, true, 24.9, 100.0, 100.0, 50.0));
    sde.types.insert(91_003, make_rat(91_003, true, 25.0, 20.0, 20.0, 9.9));
    sde.types.insert(91_004, make_rat(91_004, true, 25.0, 200.0, 200.0, 50.1));
    // Valid exact boundaries: EHP 50.0 (tid 91_010) & EHP 450.0 (tid 91_020)
    sde.types.insert(91_010, make_rat(91_010, true, 25.0, 20.0, 20.0, 10.0));
    sde.types.insert(91_020, make_rat(91_020, true, 35.0, 200.0, 150.0, 100.0));
    sde.npc_spawn_groups.insert(
        701,
        NpcSpawnGroupInfo {
            spawn_group_id: 701,
            group_id: 701,
            group_name: "Test Frigs".into(),
            faction_id: 500_010,
            ship_class: 1,
            npc_type_ids: vec![91_001, 91_002, 91_003, 91_004, 91_020],
        },
    );
    let ctx_group = rebuild_ctx_with_sde(&ctx, sde.clone());
    assert_eq!(resolve_mission_npc_type(&ctx_group), 91_020);

    // Without matching ship_class == 1 group, falls back to min type_id in sde.types matching category_id == 11
    sde.npc_spawn_groups.clear();
    let ctx_fallback = rebuild_ctx_with_sde(&ctx, sde.clone());
    assert_eq!(resolve_mission_npc_type(&ctx_fallback), 91_010);

    // Without any valid rat -> 0
    sde.types.remove(&91_010);
    sde.types.remove(&91_020);
    let ctx_empty = rebuild_ctx_with_sde(&ctx, sde);
    assert_eq!(resolve_mission_npc_type(&ctx_empty), 0);
}

#[test]
fn wave4_agent_location_tiers_and_target_resolution_priority() {
    let mut ctx = make_ctx(1_775_100_000);
    let mut sde = (*ctx.sde).clone();
    sde.agents.clear();
    sde.solar_systems.clear();
    sde.stations.clear();

    let st_a = StationInfo {
        station_id: 60_200_001,
        solar_system_id: 30_200_001,
        constellation_id: 20_200_001,
        region_id: 10_200_001,
        station_type_id: 1529,
        name: "Station A".into(),
        position: DVec3::ZERO,
        owner_corporation_id: 1_000_010,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: false,
    };
    let mut st_b = st_a.clone();
    st_b.station_id = 60_200_002;
    st_b.owner_corporation_id = 1_000_020;

    sde.solar_systems.insert(
        30_200_001,
        SolarSystem {
            solar_system_id: 30_200_001,
            region_id: 10_200_001,
            constellation_id: 20_200_001,
            name: "AlphaSys".into(),
            security_status: 0.9,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![st_a.clone(), st_b.clone()],
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_200_002,
        SolarSystem {
            solar_system_id: 30_200_002,
            region_id: 10_200_001,
            constellation_id: 20_200_001,
            name: "BetaSys".into(),
            security_status: 0.7,
            radius: 1e12,
            galactic_pos_ly: [1.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_200_003,
        SolarSystem {
            solar_system_id: 30_200_003,
            region_id: 10_200_001,
            constellation_id: 20_200_999,
            name: "GammaSys".into(),
            security_status: 0.5,
            radius: 1e12,
            galactic_pos_ly: [2.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sde.stations.insert(60_200_001, st_a);
    sde.stations.insert(60_200_002, st_b);

    // Unsupported/storyline agent at lower level/id must be skipped for regular selection
    let mut ag_unsup = agent(4_000_001, 1_000_010, 11, 24, 1);
    ag_unsup.solar_system_id = Some(30_200_001);
    ag_unsup.station_id = Some(60_200_001);
    let mut ag_reg_st2 = agent(4_000_010, 1_000_020, 2, 24, 2);
    ag_reg_st2.solar_system_id = Some(30_200_001);
    ag_reg_st2.station_id = Some(60_200_002);
    let mut ag_reg_other = agent(4_000_020, 1_000_030, 2, 24, 1);
    ag_reg_other.solar_system_id = Some(30_200_001);
    ag_reg_other.station_id = None;

    // Storyline agents in constellation (BetaSys) & region (GammaSys)
    let mut ag_story_const = agent(4_000_100, 1_000_099, 6, 24, 1);
    ag_story_const.solar_system_id = Some(30_200_002);
    let mut ag_story_reg = agent(4_000_200, 1_000_099, 7, 24, 1);
    ag_story_reg.solar_system_id = Some(30_200_003);

    sde.agents.insert(ag_unsup.agent_id, ag_unsup);
    sde.agents.insert(ag_reg_st2.agent_id, ag_reg_st2);
    sde.agents.insert(ag_reg_other.agent_id, ag_reg_other);
    sde.agents.insert(ag_story_const.agent_id, ag_story_const.clone());
    sde.agents.insert(ag_story_reg.agent_id, ag_story_reg);

    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());
    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.home_station_id = 60_200_002; // preferred_station owner = 1_000_020
        }
    });

    // Regular resolution: with station_id == None, preferred_station falls back to home_station_id (60_200_002, corp 1_000_020 -> agent 4_000_010).
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, None, false),
        (4_000_010, 1_000_020)
    );
    // When station 60_200_001 only has unsupported type 11, primary_corp is 1_000_010 so Tier 2 misses & Tier 3 picks lowest eligible regular agent (4_000_020).
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_001), false),
        (4_000_020, 1_000_030)
    );
    // Exact station match picks 4_000_010 despite higher level than 4_000_020.
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_002), false),
        (4_000_010, 1_000_020)
    );
    // Remove station 60_200_002 agent -> home_station fallback also drops to Tier 3 (4_000_020).
    sde.agents.remove(&4_000_010);
    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, None, false),
        (4_000_020, 1_000_030)
    );

    // Storyline fallback chain: none in AlphaSys -> picks BetaSys (same constellation)
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_001), true),
        (4_000_100, 1_000_099)
    );
    // Remove BetaSys storyline -> falls back to GammaSys (same region)
    sde.agents.remove(&4_000_100);
    // Test primary_corp storyline match priority before constellation/region
    let mut ag_story_corp = ag_story_const;
    ag_story_corp.agent_id = 4_000_050;
    ag_story_corp.corporation_id = 1_000_010;
    ag_story_corp.solar_system_id = Some(30_200_999);
    sde.agents.insert(ag_story_corp.agent_id, ag_story_corp);
    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_001), true),
        (4_000_050, 1_000_010)
    );
    sde.agents.remove(&4_000_050);
    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_001), true),
        (4_000_200, 1_000_099)
    );
    // Remove all agents -> (0, primary_corp)
    sde.agents.remove(&4_000_200);
    sde.agents.remove(&4_000_020);
    ctx = rebuild_ctx_with_sde(&ctx, sde);
    assert_eq!(
        resolve_agent_and_corp_for_system(&ctx, char_id, 30_200_001, Some(60_200_001), true),
        (0, 1_000_010)
    );

    // resolve_target_agent_id_with_bound prioritizes agentMgr bound_entry over args & skips wrong service/id
    let mut sess = SessionState::new(901);
    sess.character_id = Some(char_id);
    let kw = HashMap::new();
    let valid_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 901,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: 4_111_111,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let wrong_service_bound = BoundObjectEntry {
        service: "wallet".into(),
        ..valid_bound.clone()
    };
    assert_eq!(
        resolve_target_agent_id_with_bound(
            &ctx,
            &sess,
            Some(&valid_bound),
            char_id,
            30_200_001,
            &[PyRep::Integer(4_000_001)],
            &kw,
        ),
        (4_111_111, true)
    );
    assert_eq!(
        resolve_target_agent_id_with_bound(
            &ctx,
            &sess,
            Some(&wrong_service_bound),
            char_id,
            30_200_001,
            &[],
            &kw,
        ),
        (0, false)
    );
}

#[test]
fn wave4_rewards_edge_branches_security_encounters_and_target_mission_id() {
    let mut ctx = make_ctx(1_775_100_000);
    let mut sde = (*ctx.sde).clone();
    sde.dungeons.clear();
    sde.npc_spawn_groups.clear();
    sde.solar_systems.insert(
        30_300_001,
        SolarSystem {
            solar_system_id: 30_300_001,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "ExtremeSec".into(),
            security_status: 2.0, // 1.6288 - 2.0 < 0 -> sec_factor clamps to 0.01
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    let mut ag = agent(4_200_001, 1_000_009, 2, 24, 2);
    ag.solar_system_id = Some(30_300_001);
    sde.agents.insert(4_200_001, ag.clone());

    let mut story_zero_lp = tmpl(501, "Storyline Zero LP", Some("StorylineKill"), false);
    story_zero_lp.lp_omega = 0;
    story_zero_lp.lp_alpha = 0;
    let mut normal_zero_lp = tmpl(502, "Normal Zero LP", Some("BasicKill"), false);
    normal_zero_lp.lp_omega = 0;
    normal_zero_lp.lp_alpha = 0;
    let mut alpha_half_lp = tmpl(503, "Alpha Half", Some("BasicKill"), false);
    alpha_half_lp.lp_omega = 1_200;
    alpha_half_lp.lp_alpha = 0;

    sde.missions.insert(501, story_zero_lp);
    sde.missions.insert(502, normal_zero_lp);
    sde.missions.insert(503, alpha_half_lp);
    ctx = rebuild_ctx_with_sde(&ctx, sde.clone());

    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1; // Alpha
        }
        snap.skills.remove(&char_id);
        snap.active_missions.insert(
            (char_id, 701),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 701,
                agent_id: 4_200_002,
                state: "Accepted".into(),
                solar_system_id: 30_300_001,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, 702),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 702,
                agent_id: 4_200_001,
                state: "Completed".into(),
                solar_system_id: 30_300_001,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
    });

    // sec_factor clamp (0.01) + Storyline 1.5x floor (200 * 3/2 = 300 -> 300_000 ISK, Storyline LP = 0)
    let CharacterMissionRewards {
        isk_reward: isk_st,
        lp_reward: lp_st,
        bonus_reward: bonus_st,
    } = compute_character_mission_rewards(&ctx, char_id, 4_200_001, 501, 30_300_001);
    assert_eq!(isk_st, 300_000);
    assert_eq!(bonus_st, 150_000);
    assert_eq!(lp_st, 0);

    // Normal zero LP -> min_level_lp = 200
    let isk_nm =
        compute_character_mission_rewards(&ctx, char_id, 4_200_001, 502, 30_300_001).isk_reward;
    assert_eq!(isk_nm, 200_000);

    // Alpha with lp_alpha == 0 halves lp_omega (1200 / 2 = 600 -> 600_000 ISK)
    let isk_half =
        compute_character_mission_rewards(&ctx, char_id, 4_200_001, 503, 30_300_001).isk_reward;
    assert_eq!(isk_half, 600_000);

    // Empty dungeons + empty spawn groups fallback in resolve_security_mission_encounter
    let enc_empty = resolve_security_mission_encounter(
        &ctx,
        &tmpl(601, "No Matching Dungeon", Some("BasicKill"), false),
        &ag,
        30_300_001,
    );
    assert_eq!(enc_empty.dungeon_id, 0);
    assert_eq!(enc_empty.dungeon_name, "Encounter Pocket");
    assert_eq!(enc_empty.spawn_group_id, 0);
    assert_eq!(enc_empty.wave_npc_count, 3);

    // Difficulty tier wave_npc_count: level 5 -> diff 5 -> 6 NPCs, diff 3 -> 4 NPCs
    let ag_l5 = agent(4_200_099, 1_000_009, 2, 24, 5);
    assert_eq!(
        resolve_security_mission_encounter(&ctx, &tmpl(602, "L5", None, false), &ag_l5, 30_300_001)
            .wave_npc_count,
        6
    );
    let ag_l3 = agent(4_200_098, 1_000_009, 2, 24, 3);
    assert_eq!(
        resolve_security_mission_encounter(&ctx, &tmpl(603, "L3", None, false), &ag_l3, 30_300_001)
            .wave_npc_count,
        4
    );

    // resolve_target_mission_id: arg[1] precedence, active_missions (non-SDE) via arg[0] > 19, fallback ignoring agent_id, fallback ignoring state
    let kw = HashMap::new();
    assert_eq!(
        resolve_target_mission_id(
            &ctx,
            char_id,
            4_200_001,
            None,
            &[PyRep::Integer(501), PyRep::Integer(701)],
            &kw
        )
        .unwrap(),
        701
    );
    assert_eq!(
        resolve_target_mission_id(&ctx, char_id, 4_200_001, None, &[PyRep::Integer(701)], &kw)
            .unwrap(),
        701
    );
    // Fallback to state match on different agent only when unbound (agent_id <= 0);
    // bound agent_id > 0 must NOT leak another agent's mission (#791, #979).
    assert_eq!(
        resolve_target_mission_id(&ctx, char_id, 0, Some("Accepted"), &[], &kw).unwrap(),
        701
    );
    assert_eq!(
        resolve_target_mission_id(&ctx, char_id, 4_200_999, None, &[], &kw)
            .unwrap_err()
            .code,
        "MissionNotActive"
    );
    // Fallback to agent match with different state (702 Completed on 4_200_001 when querying "Offered")
    assert_eq!(
        resolve_target_mission_id(&ctx, char_id, 4_200_001, Some("Offered"), &[], &kw).unwrap(),
        702
    );
}

#[tokio::test]
async fn wave4_courier_multi_stack_reclaim_decline_boundary_clamp_and_npc_filters() {
    let mut ctx = make_ctx(1_775_200_000);
    let mut sde = (*ctx.sde).clone();
    sde.agents.clear();
    sde.stations.clear();
    sde.missions.clear();

    // Only 2 stations total in different regions -> resolve_courier_mission_details exercises global fallback
    let st_1 = StationInfo {
        station_id: 60_400_001,
        solar_system_id: 30_000_142,
        constellation_id: 20_400_001,
        region_id: 10_400_001,
        station_type_id: 1529,
        name: "S1".into(),
        position: DVec3::ZERO,
        owner_corporation_id: 1_000_009,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: false,
    };
    let mut st_2 = st_1.clone();
    st_2.station_id = 60_400_002;
    st_2.solar_system_id = 30_400_002;
    st_2.constellation_id = 20_400_002;
    st_2.region_id = 10_400_002;
    sde.stations.insert(60_400_001, st_1);
    sde.stations.insert(60_400_002, st_2);
    let mut ag_no_station = agent(4_300_001, 1_000_009, 2, 22, 1);
    ag_no_station.station_id = None;
    ag_no_station.solar_system_id = Some(30_000_142);
    sde.agents.insert(4_300_001, ag_no_station.clone());
    sde.missions.insert(801, tmpl(801, "Boundary 1", None, false));
    sde.missions.insert(802, tmpl(802, "Boundary 2", None, false));
    ctx = rebuild_ctx_with_sde(&ctx, sde);

    let c_det =
        resolve_courier_mission_details(&ctx, &tmpl(1, "NoKeyword", None, true), &ag_no_station)
            .unwrap();
    assert_eq!(c_det.origin_station_id, 60_400_001);
    assert_eq!(c_det.destination_station_id, 60_400_002);

    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        // Multi-stack exact courier cargo: item 77_001 (6 units, via cid) + item 77_002 (4 units, via tag) + other_char item 77_003 (must stay!)
        for (iid, owner, qty, tagged) in [
            (77_001_i64, char_id, 6_i32, false),
            (77_002_i64, char_id, 4_i32, true),
            (77_003_i64, other_char, 10_i32, true),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 3810,
                    owner_id: owner,
                    location_id: 60_400_001,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            if tagged {
                snap.item_custom_names
                    .insert(iid, format!("courier_cargo:{char_id}:801"));
            }
        }
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 801,
                agent_id: 4_300_001,
                granted_courier_item_id: Some(77_001),
                granted_courier_qty: 10, // deletes both 77_001 (6) and 77_002 (4) regardless of HashMap order
                ..Default::default()
            },
        );
        reclaim_courier_cargo(snap, char_id, 801);
        assert!(!snap.items.contains_key(&77_001));
        assert!(!snap.items.contains_key(&77_002));
        assert_eq!(snap.items[&77_003].quantity, 10);

        // Single-stack partial reclaim on mission 805: 20 units - 10 granted -> 10 remaining, version += 1, custom name cleared
        snap.items.insert(
            77_004,
            InventoryItemRecord {
                item_id: 77_004,
                type_id: 3810,
                owner_id: char_id,
                location_id: 60_400_001,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(77_004, format!("courier_cargo:{char_id}:805"));
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 805,
                agent_id: 4_300_001,
                granted_courier_item_id: Some(77_004),
                granted_courier_qty: 10,
                ..Default::default()
            },
        );
        reclaim_courier_cargo(snap, char_id, 805);
        assert_eq!(snap.items[&77_004].quantity, 10);
        assert_eq!(snap.items[&77_004].version, 2);
        assert!(!snap.item_custom_names.contains_key(&77_004));

        // Decline boundary at exact 14_400s (no penalty) vs 14_399s (penalty clamped at -10.0)
        snap.active_missions.insert(
            (char_id, 801),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 801,
                agent_id: 4_300_001,
                state: "Offered".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, 802),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 802,
                agent_id: 4_300_001,
                state: "Offered".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        snap.last_mission_decline_times
            .insert(char_id, 1_775_200_000 - 14_400);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_009, -9.98);
        }
    });

    let sess = char_session_helper(char_id);
    // Exact 14_400s elapsed -> NO penalty!
    let (_, pen_eq, _) = abandon_or_decline_mission(&ctx, &sess, char_id, 801)
        .await
        .unwrap();
    assert!(!pen_eq);
    ctx.db.with_snapshot(|snap| {
        assert!((snap.characters[&char_id].standings[&1_000_009] - (-9.98)).abs() < 1e-9);
    });

    // Immediate 2nd decline (0s elapsed < 14_400s) -> penalty clamped at -10.0 (delta == -0.02)
    let (_, pen_in, _) = abandon_or_decline_mission(&ctx, &sess, char_id, 802)
        .await
        .unwrap();
    assert!(pen_in);
    ctx.db.with_snapshot(|snap| {
        assert!((snap.characters[&char_id].standings[&1_000_009] - (-10.0)).abs() < 1e-9);
        let last_tx = snap.standing_transactions[&char_id].last().unwrap();
        assert!((last_tx.modification - (-0.02)).abs() < 1e-9);
    });

    // on_mission_npc_destroyed_core: ignores Offered/Completed & wrong sys_id, decrements even with mismatched killer when ball_id matches!
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 803),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 803,
                agent_id: 4_300_001,
                state: "Accepted".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![999_001],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, 804),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 804,
                agent_id: 4_300_001,
                state: "Offered".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![999_002],
                bookmark_id: None,
            },
        );
    });
    // Wrong system does not decrement
    on_mission_npc_destroyed_core(&ctx.db, 30_000_144, 999_001, Some(char_id));
    on_mission_npc_destroyed_core(&ctx.db, 30_000_142, 999_002, Some(char_id)); // Offered ignored
    // Fleetmate kill (killer != char_id) still decrements when ball_id matches
    on_mission_npc_destroyed_core(&ctx.db, 30_000_142, 999_001, Some(other_char));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, 803)].npcs_remaining, 0);
        assert!(snap.active_missions[&(char_id, 803)].spawned_npc_ball_ids.is_empty());
        assert_eq!(snap.active_missions[&(char_id, 804)].npcs_remaining, 1);
    });

    // `compute_character_mission_rewards`: verify every `no_lp_mission` disjunct in isolation
    let mut sde_lp = (*ctx.sde).clone();
    for (aid, atype, div) in [
        (5_100_002_i64, 2_u32, 24_u32), // regular -> LP > 0
        (5_100_003, 6, 24),             // storyline agent_type_id == 6 -> LP == 0
        (5_100_004, 4, 18),             // R&D agent_type_id == 4 -> LP == 0
        (5_100_009, 9, 24),             // agent_type_id == 9 (outside 10..=12, not storyline) -> LP > 0
        (5_100_010, 10, 24),            // agent_type_id == 10 -> LP == 0
        (5_100_011, 11, 24),            // agent_type_id == 11 -> LP == 0
        (5_100_012, 12, 24),            // agent_type_id == 12 -> LP == 0
    ] {
        sde_lp.agents.insert(aid, agent(aid, 1_000_009, atype, div, 2));
    }
    sde_lp
        .missions
        .insert(910_001, tmpl(910_001, "RegularOp", Some("BasicKill"), false));
    sde_lp
        .missions
        .insert(910_002, tmpl(910_002, "ImportantStory", None, true));
    sde_lp
        .missions
        .insert(910_003, tmpl(910_003, "ArcStage", Some("Caldari_EpicArc_01"), false));
    let mut ctx_lp = make_ctx(1_775_200_000);
    ctx_lp.sde = Arc::new(sde_lp);

    assert!(compute_character_mission_rewards(&ctx_lp, char_id, 5_100_002, 910_001, 30_000_142).lp_reward > 0);
    assert_eq!(compute_character_mission_rewards(&ctx_lp, char_id, 5_100_003, 910_001, 30_000_142).lp_reward, 0);
    assert_eq!(compute_character_mission_rewards(&ctx_lp, char_id, 5_100_004, 910_001, 30_000_142).lp_reward, 0);
    assert!(compute_character_mission_rewards(&ctx_lp, char_id, 5_100_009, 910_001, 30_000_142).lp_reward > 0);
    for aid_zero in [5_100_010_i64, 5_100_011, 5_100_012] {
        let CharacterMissionRewards {
            isk_reward: isk_z,
            lp_reward: lp_z,
            bonus_reward: bon_z,
        } = compute_character_mission_rewards(&ctx_lp, char_id, aid_zero, 910_001, 30_000_142);
        assert!(isk_z > 0 && bon_z > 0);
        assert_eq!(lp_z, 0, "agent {aid_zero} must award 0 LP");
    }
    // Template-only storyline (`important = true`) and `EpicArc` `content_template` with `agent_info = None`
    assert_eq!(compute_character_mission_rewards(&ctx_lp, char_id, 9_999_999, 910_002, 30_000_142).lp_reward, 0);
    assert_eq!(compute_character_mission_rewards(&ctx_lp, char_id, 9_999_999, 910_003, 30_000_142).lp_reward, 0);
    assert!(compute_character_mission_rewards(&ctx_lp, char_id, 9_999_999, 910_001, 30_000_142).lp_reward > 0);
}

fn char_session_helper(char_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_400_001),
        ..Default::default()
    }
}

#[test]
fn compute_character_mission_rewards_exact_lp_and_isk_formula_non_unit_factors() {
    let mut ctx = make_ctx(1_775_300_000);
    let char_id = ctx
        .db
        .with_snapshot(|s| *s.characters.keys().min().expect("demo char"));

    let test_sys = 30_900_001_u32;
    let mut sde = (*ctx.sde).clone();
    let mut sys_rec = sde.solar_systems[&30_000_142].clone();
    sys_rec.solar_system_id = test_sys;
    sys_rec.name = "ExactFormulaSec".into();
    sys_rec.security_status = 0.4;
    sde.solar_systems.insert(test_sys, sys_rec);
    let mut formula_ag = agent(5_200_001, 1_000_035, 2, 24, 1);
    formula_ag.solar_system_id = Some(test_sys);
    sde.agents.insert(5_200_001, formula_ag);
    sde.missions
        .insert(920_001, tmpl(920_001, "ExactFormulaOp", Some("BasicKill"), false));
    ctx = rebuild_ctx_with_sde(&ctx, sde);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega so lp_omega = 1_000 applies
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3895, // Security Connections lvl 2 -> conn_factor = 1.2
            TrainedSkillRecord {
                type_id: 3895,
                level: 2,
                skill_points: 10_000.0,
            },
        );
        sk.insert(
            3356, // Negotiation lvl 3 -> neg_factor = 1.15
            TrainedSkillRecord {
                type_id: 3356,
                level: 3,
                skill_points: 20_000.0,
            },
        );
    });

    // sec_factor = 1.6288 - 0.4 = 1.2288; conn_factor = 1.2; base_lp = 1_000
    // lp_reward = (1000 * 1.2288 * 1.2).round() = 1475
    // isk_reward = (1_000_000 * 1.15).round() = 1_150_000; bonus = 575_000
    let rewards = compute_character_mission_rewards(&ctx, char_id, 5_200_001, 920_001, test_sys);
    assert_eq!(rewards.lp_reward, 1_475);
    assert_eq!(rewards.isk_reward, 1_150_000);
    assert_eq!(rewards.bonus_reward, 575_000);
}
