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

    // Seed agents in divisions 22 (Distribution -> 3893), 23 (Mining -> 3894), 24 (Security -> 3895), and 18 (R&D -> None)
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
            3893,
            TrainedSkillRecord {
                type_id: 3893,
                level: 3,
                skill_points: 20_000.0,
            },
        );
        sk.insert(
            3894,
            TrainedSkillRecord {
                type_id: 3894,
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

    let (isk_dist, lp_dist, bonus_dist) =
        compute_character_mission_rewards(&ctx, char_id, 3_100_001, 200, 30_000_142);
    let (_, lp_mine, _) =
        compute_character_mission_rewards(&ctx, char_id, 3_100_002, 200, 30_000_142);
    let (_, lp_rnd, _) =
        compute_character_mission_rewards(&ctx, char_id, 3_100_004, 200, 30_000_142);
    assert_eq!(isk_dist, 600_000);
    assert_eq!(bonus_dist, 300_000);
    assert!(lp_mine > lp_dist && lp_dist > lp_rnd);

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
