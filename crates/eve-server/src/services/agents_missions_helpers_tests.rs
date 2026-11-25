use super::*;
use crate::router::ServerContext;
use crate::services::mission_fixture::{self, MissionFixture};
use eve_machonet::SessionState;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::{AgentInfo, MissionTemplate, SdeRegistry, SolarSystem, StationInfo};
use glam::DVec3;
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

fn make_agent(agent_id: i64, agent_type_id: u32, division_id: u32, level: u8) -> AgentInfo {
    AgentInfo {
        agent_id,
        name: "Test Agent".into(),
        corporation_id: 1_000_009,
        division_id,
        level,
        station_id: Some(60_003_760),
        solar_system_id: Some(30_000_142),
        agent_type_id,
        is_locator: false,
        research_skills: vec![],
    }
}

fn make_tmpl(mission_id: u32, title: &str, ct: Option<&str>, is_courier: bool) -> MissionTemplate {
    MissionTemplate {
        mission_id,
        title: title.into(),
        content_template: ct.map(Into::into),
        is_courier,
        lp_omega: 1000,
        lp_alpha: 500,
        has_standing_rewards: true,
        dungeon_id: None,
    }
}

#[test]
fn pure_classification_and_scaling_helpers_cover_all_branches() {
    assert_eq!(parse_mission_id(&[PyRep::Integer(42)]).unwrap(), 42);
    assert!(parse_mission_id(&[]).is_err());
    assert!(parse_mission_id(&[PyRep::Integer(-1)]).is_err());

    assert!(is_storyline_template(&make_tmpl(
        1,
        "Normal",
        Some("StorylineCourier"),
        false
    )));
    assert!(is_storyline_template(&make_tmpl(
        1,
        "Materials For War Preparation",
        None,
        false
    )));
    assert!(!is_storyline_template(&make_tmpl(
        1,
        "Worlds Collide",
        Some("BasicKill"),
        false
    )));

    assert!(is_storyline_agent_type(6) && is_storyline_agent_type(7) && !is_storyline_agent_type(2));
    assert!(
        is_unsupported_direct_agent_type(11)
            && is_unsupported_direct_agent_type(12)
            && !is_unsupported_direct_agent_type(2)
    );
    assert!(is_rnd_agent(&make_agent(1, 4, 18, 1)));
    assert!(!is_rnd_agent(&make_agent(1, 2, 24, 1)));

    let mut fw_ag = make_agent(1, 2, 24, 1);
    fw_ag.corporation_id = 1_000_121;
    assert!(is_fw_agent(&fw_ag));

    assert_eq!(rnd_tritanium_required_qty(1), 300);
    assert_eq!(rnd_tritanium_required_qty(2), 900);
    assert_eq!(rnd_tritanium_required_qty(3), 2_700);
    assert_eq!(rnd_tritanium_required_qty(4), 8_100);
    assert_eq!(rnd_tritanium_required_qty(5), 24_300);

    assert_eq!(mission_bonus_duration(1), 1_800);
    assert_eq!(mission_bonus_duration(2), 3_600);
    assert_eq!(mission_bonus_duration(3), 7_200);
    assert_eq!(mission_bonus_duration(4), 14_400);
    assert_eq!(mission_bonus_duration(5), 21_600);

    assert_eq!(resolve_fw_warzone_system(1_000_121), 30_002_062);
    assert_eq!(resolve_fw_warzone_system(1_000_147), 30_002_097);
    assert_eq!(resolve_fw_warzone_system(1_000_085), 30_002_098);
    assert_eq!(resolve_fw_warzone_system(1_000_009), 30_002_061);

    let kill_tmpl = make_tmpl(10, "Pirate Invasion", Some("BasicKill"), false);
    let courier_tmpl = make_tmpl(11, "Delivery", Some("BasicCourier"), true);
    let epic_tmpl = make_tmpl(12, "Arc Chapter", Some("EpicArcKill"), false);
    assert!(mission_matches_agent_pool(
        &epic_tmpl,
        &make_agent(1, 10, 24, 1)
    ));
    assert!(mission_matches_agent_pool(
        &kill_tmpl,
        &make_agent(1, 2, 24, 1)
    ));
    assert!(mission_matches_agent_pool(
        &courier_tmpl,
        &make_agent(1, 2, 22, 1)
    ));
    assert!(!mission_matches_agent_pool(
        &kill_tmpl,
        &make_agent(1, 11, 24, 1)
    ));
}

#[test]
fn mission_rewards_and_courier_trade_mining_encounter_details_resolve() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    let st_info = StationInfo {
        station_id: 60_003_760,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        station_type_id: 1529,
        name: "Jita IV - Moon 4".into(),
        position: DVec3::ZERO,
        owner_corporation_id: 1_000_035,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    };
    let mut st_info2 = st_info.clone();
    st_info2.station_id = 60_003_761;
    st_info2.name = "Jita IV - Moon 5".into();

    sde.solar_systems.insert(
        30_000_142,
        SolarSystem {
            solar_system_id: 30_000_142,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "Jita".into(),
            security_status: 0.6288,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![st_info.clone(), st_info2.clone()],
            wormhole_class_id: None,
        },
    );
    sde.stations.insert(60_003_760, st_info);
    sde.stations.insert(60_003_761, st_info2);
    let ag = make_agent(3_018_921, 2, 24, 2);
    sde.agents.insert(3_018_921, ag.clone());
    let tmpl = make_tmpl(100, "Marine Guristas Delivery", Some("BasicCourier"), true);
    sde.missions.insert(100, tmpl.clone());
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        snap.skills.remove(&char_id);
        snap.skills.entry(char_id).or_default().insert(
            3356,
            TrainedSkillRecord {
                type_id: 3356,
                level: 4,
                skill_points: 50_000.0,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3895,
            TrainedSkillRecord {
                type_id: 3895,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let CharacterMissionRewards {
        isk_reward: isk,
        lp_reward: lp,
        bonus_reward: bonus,
    } = compute_character_mission_rewards(&ctx, char_id, 3_018_921, 100, 30_000_142);
    let def_100 = resolve_mission_reward_definition(&ctx, char_id, 3_018_921, 100);
    assert_eq!(def_100.source, RewardCatalogSource::DerivedMissionCurve);
    assert_eq!(lp, 1_500);
    // Negotiation IV (+20%) scales per-mission base ISK and bonus ISK independently of base_lp * 1_000 / 2.
    assert_eq!(isk, ((def_100.base_isk as f64) * 1.20).round() as i64);
    assert_eq!(
        bonus,
        ((def_100.time_bonus_isk as f64) * 1.20).round() as i64
    );
    assert_ne!(def_100.base_isk, def_100.base_lp * 1_000);
    assert_ne!(def_100.time_bonus_isk, def_100.base_isk / 2);

    let courier = resolve_courier_mission_details(&ctx, &tmpl, &ag).unwrap();
    assert_eq!(courier.cargo_type_id, 3810);
    assert_eq!(courier.origin_station_id, 60_003_760);
    assert!(courier.destination_station_id > 0);

    let trade =
        resolve_trade_mission_details(&ctx, &make_tmpl(101, "Tritanium Supply", None, false));
    assert_eq!(trade.trade_type_id, 34);
    assert!(trade.trade_quantity >= 1_000);

    let mining = resolve_mining_mission_details(&ctx, &make_tmpl(102, "Veldspar Rush", None, false));
    assert_eq!(mining.ore_type_id, 1230);
    assert_eq!(mining.required_units, 500);

    let enc = resolve_security_mission_encounter(&ctx, &tmpl, &ag, 30_000_142);
    assert_eq!(enc.faction_id, 500_001);
    assert_eq!(enc.wave_npc_count, 3);
}

#[tokio::test]
async fn abandon_or_decline_reclaims_courier_cargo_and_tracks_npc_destruction() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    let ag = make_agent(3_018_921, 2, 24, 1);
    sde.agents.insert(3_018_921, ag);
    sde.missions.insert(
        100,
        make_tmpl(100, "Test Mission", Some("BasicCourier"), true),
    );
    sde.missions.insert(
        101,
        make_tmpl(101, "Test Mission 2", Some("BasicKill"), false),
    );
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    let session = char_session(char_id, 98_000_001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            55_001,
            InventoryItemRecord {
                item_id: 55_001,
                type_id: 3810,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(55_001, format!("courier_cargo:{char_id}:100"));
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::accepted(char_id, 100, 3_018_921, 30_000_142)
                .granted_courier_item_id(Some(55_001))
                .granted_courier_qty(10)
                .destination_station_id(Some(60_003_761)),
        );
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::accepted(char_id, 101, 3_018_921, 30_000_142)
                .npcs_remaining(2)
                .spawned_npc_ball_ids(vec![900_001, 900_002]),
        );
    });

    on_mission_npc_destroyed_core(&ctx.db, 30_000_142, 900_001, Some(char_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(mission_fixture::get(snap, char_id, 101).unwrap().npcs_remaining, 1);
    });

    let (new_state, penalty, was_acc) = abandon_or_decline_mission(&ctx, &session, char_id, 100)
        .await
        .unwrap();
    assert_eq!(new_state, "Quit");
    assert!(penalty && was_acc);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&55_001));
        assert!((snap.characters[&char_id].standings[&1_000_009] - (-0.1)).abs() < 1e-9);
    });
}

#[tokio::test]
async fn wave4_courier_min_qty_clamp_missing_sde_mission_quit_and_bookmark_fallback_cleanup() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.agents.insert(3_018_921, make_agent(3_018_921, 2, 24, 1));
    sde.missions.remove(&999);
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    let session = char_session(char_id, 98_000_001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            55_999,
            InventoryItemRecord {
                item_id: 55_999,
                type_id: 3810,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        // Mission 999 is not in sde.missions -> quit skips standing penalty;
        // bookmark_id == None triggers memo == "Mission Pocket Destination" fallback cleanup;
        // granted_courier_qty == 0 (.max(1)) still reclaims single-unit courier item.
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 999, 3_018_921, 30_000_142)
                .granted_courier_item_id(Some(55_999))
                .granted_courier_qty(0),
        );
        if let Some((_, bm)) = snap.bookmarks.iter().next().map(|(id, b)| (*id, b.clone())) {
            let mut m_bm = bm.clone();
            m_bm.bookmark_id = 77_001;
            m_bm.owner_id = char_id;
            m_bm.solar_system_id = 30_000_142;
            m_bm.memo = "Mission Pocket Destination".into();
            snap.bookmarks.insert(77_001, m_bm);

            let mut keep_bm = bm;
            keep_bm.bookmark_id = 77_002;
            keep_bm.owner_id = char_id;
            keep_bm.solar_system_id = 30_000_142;
            keep_bm.memo = "Keep Safe Bookmark".into();
            snap.bookmarks.insert(77_002, keep_bm);
        }
    });

    let (st, penalty, was_acc) = abandon_or_decline_mission(&ctx, &session, char_id, 999)
        .await
        .unwrap();
    assert_eq!(st, "Quit");
    assert!(!penalty && was_acc);
    assert!(!bookmarks_remain(&ctx, char_id));
}

fn bookmarks_remain(ctx: &ServerContext, char_id: i64) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.memo == "Mission Pocket Destination")
    })
}

#[test]
fn per_mission_wiki_catalog_and_derived_curves_produce_distinct_isk_bonus_windows_and_lp() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    // Custom system with exact security_status = 0.6288 -> (1.6288 - 0.6288) = 1.0000
    sde.solar_systems.insert(
        30_910_001,
        SolarSystem {
            solar_system_id: 30_910_001,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "CatalogUnitSys".into(),
            security_status: 0.6288,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    let mut l1_sec = make_agent(3_900_001, 2, 24, 1);
    l1_sec.solar_system_id = Some(30_910_001);
    let mut l4_sec = make_agent(3_900_004, 2, 24, 4);
    l4_sec.solar_system_id = Some(30_910_001);
    sde.agents.insert(3_900_001, l1_sec);
    sde.agents.insert(3_900_004, l4_sec);
    ctx.sde = Arc::new(sde);

    let char_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
        }
        snap.skills.remove(&char_id);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3356, // Negotiation 4 -> 1.20x ISK & bonus
            TrainedSkillRecord {
                type_id: 3356,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        sk.insert(
            3895, // Security Connections 3 -> 1.30x LP
            TrainedSkillRecord {
                type_id: 3895,
                level: 3,
                skill_points: 50_000.0,
            },
        );
    });

    // Three real L1 SDE missions from Wiki reports must have distinct rewards and timers
    let r_denial = compute_character_mission_rewards(&ctx, char_id, 3_900_001, 13_777, 30_910_001);
    assert_eq!(r_denial.isk_reward, 80_400); // 67,000 * 1.20
    assert_eq!(r_denial.bonus_reward, 76_800); // 64,000 * 1.20
    assert_eq!(r_denial.lp_reward, 95); // round(73 * 1.0 * 1.30)
    assert_eq!(
        mission_bonus_window_secs(&ctx, char_id, 3_900_001, 13_777),
        3_600
    );

    let r_rail3 = compute_character_mission_rewards(&ctx, char_id, 3_900_001, 13_492, 30_910_001);
    assert_eq!(r_rail3.isk_reward, 16_800); // 14,000 * 1.20
    assert_eq!(r_rail3.bonus_reward, 10_800); // 9,000 * 1.20
    assert_eq!(r_rail3.lp_reward, 18); // round(14 * 1.0 * 1.30)

    let r_patriotic = compute_character_mission_rewards(&ctx, char_id, 3_900_001, 13_514, 30_910_001);
    assert_eq!(r_patriotic.isk_reward, 34_800); // 29,000 * 1.20
    assert_eq!(r_patriotic.bonus_reward, 44_400); // 37,000 * 1.20
    assert_eq!(r_patriotic.lp_reward, 46); // round(35 * 1.0 * 1.30)
    assert_eq!(
        mission_bonus_window_secs(&ctx, char_id, 3_900_001, 13_514),
        3_840 // 64m exact Wiki timer
    );

    // Distinct L4 security missions + L4 Anomic Burner must differ widely
    let r_spies = compute_character_mission_rewards(&ctx, char_id, 3_900_004, 900, 30_910_001);
    assert_eq!(r_spies.isk_reward, 429_600); // 358,000 * 1.20
    assert_eq!(r_spies.bonus_reward, 612_000); // 510,000 * 1.20
    assert_eq!(r_spies.lp_reward, 2_573); // round(1,979 * 1.0 * 1.30)
    assert_eq!(
        mission_bonus_window_secs(&ctx, char_id, 3_900_004, 900),
        14_400
    );

    let r_desp = compute_character_mission_rewards(&ctx, char_id, 3_900_004, 13_741, 30_910_001);
    assert_eq!(r_desp.isk_reward, 3_120_000); // 2,600,000 * 1.20
    assert_eq!(r_desp.bonus_reward, 3_360_000); // 2,800,000 * 1.20
    assert_eq!(r_desp.lp_reward, 17_163); // round(13,202 * 1.0 * 1.30)
    assert_eq!(
        mission_bonus_window_secs(&ctx, char_id, 3_900_004, 13_741),
        21_600
    );

    let r_burner = compute_character_mission_rewards(&ctx, char_id, 3_900_004, 16_154, 30_910_001);
    assert_eq!(r_burner.isk_reward, 7_440_000); // 6,200,000 * 1.20
    assert_eq!(r_burner.bonus_reward, 3_780_000); // 3,150,000 * 1.20
    assert_eq!(r_burner.lp_reward, 18_850); // round(14,500 * 1.0 * 1.30)
    assert_eq!(
        mission_bonus_window_secs(&ctx, char_id, 3_900_004, 16_154),
        10_800
    );

    // Derived non-Wiki SDE L1 Courier missions also vary independently and keep bonus window < 7_200s
    let l1_cour_ids: Vec<u32> = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.content_template
                .as_deref()
                .is_some_and(|ct| ct.contains("BasicCourier"))
                && !matches!(m.mission_id, 13_491 | 13_512)
        })
        .take(5)
        .map(|m| m.mission_id)
        .collect();
    assert!(l1_cour_ids.len() >= 2);
    for &mid in &l1_cour_ids {
        let win = mission_bonus_window_secs(&ctx, char_id, 3_900_001, mid);
        assert!((600..7_200).contains(&win));
    }
    let def_a = resolve_mission_reward_definition(&ctx, char_id, 3_900_001, l1_cour_ids[0]);
    let def_b = resolve_mission_reward_definition(&ctx, char_id, 3_900_001, l1_cour_ids[1]);
    assert!(
        (def_a.base_isk, def_a.time_bonus_isk, def_a.base_lp)
            != (def_b.base_isk, def_b.time_bonus_isk, def_b.base_lp)
    );
}

#[test]
fn cosmos_and_data_center_agents_grant_zero_lp_on_real_sde_missions() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let dc_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5)
        .expect("Real SDE Data Center agent (agent_type_id == 5)")
        .clone();
    let cosmos_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 8)
        .expect("Real SDE COSMOS agent (agent_type_id == 8)")
        .clone();
    let std_sec_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1)
        .expect("Real SDE Basic L1 Security agent")
        .clone();

    let mut non_lp_mids: Vec<u32> = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.lp_omega == 0
                && !m.title.trim().is_empty()
                && !m
                    .content_template
                    .as_deref()
                    .is_some_and(|ct| ct.contains("Research"))
        })
        .map(|m| m.mission_id)
        .collect();
    non_lp_mids.sort_unstable();
    non_lp_mids.truncate(2);
    non_lp_mids.push(13_777); // Wiki Security mission that pays LP for Basic agents

    for (label, agent) in [("DataCenter(5)", &dc_agent), ("COSMOS(8)", &cosmos_agent)] {
        let sys_id = agent.solar_system_id.unwrap_or(30_000_142);
        for &mid in &non_lp_mids {
            let def = resolve_mission_reward_definition(&ctx, char_id, agent.agent_id, mid);
            assert!(
                !def.grants_lp && def.base_lp == 0 && def.base_lp_alpha == 0,
                "{label} agent {} must not grant LP for mission {mid}: {def:?}",
                agent.agent_id
            );
            assert!(def.base_isk > 0, "{label} agent {} mission {mid} must pay ISK", agent.agent_id);
            let rewards = compute_character_mission_rewards(&ctx, char_id, agent.agent_id, mid, sys_id);
            assert_eq!(
                rewards.lp_reward, 0,
                "{label} agent {} mission {mid} computed LP must be 0",
                agent.agent_id
            );
            assert!(rewards.isk_reward > 0);
        }
    }

    // Control check: standard Basic Security agent on 13_777 still pays positive LP.
    let std_sys = std_sec_agent.solar_system_id.unwrap_or(30_000_142);
    let std_rewards = compute_character_mission_rewards(&ctx, char_id, std_sec_agent.agent_id, 13_777, std_sys);
    assert!(std_rewards.lp_reward > 0);
}

#[test]
fn rnd_research_missions_award_zero_isk_and_zero_lp() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let rnd_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| is_rnd_agent(a) && a.solar_system_id.is_some())
        .expect("SDE R&D agent")
        .clone();
    let rnd_sys = rnd_agent.solar_system_id.unwrap();
    let rnd_mids: Vec<u32> = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.content_template
                .as_deref()
                .is_some_and(|ct| ct.contains("Research"))
        })
        .map(|m| m.mission_id)
        .collect();
    assert!(!rnd_mids.is_empty(), "SDE must include Research missions");

    for mid in rnd_mids {
        let def = resolve_mission_reward_definition(&ctx, char_id, rnd_agent.agent_id, mid);
        assert_eq!(def.base_isk, 0, "R&D mission {mid} base_isk must be 0");
        assert_eq!(def.time_bonus_isk, 0, "R&D mission {mid} bonus ISK must be 0");
        assert!(!def.grants_lp && def.base_lp == 0 && def.base_lp_alpha == 0);
        let rewards = compute_character_mission_rewards(&ctx, char_id, rnd_agent.agent_id, mid, rnd_sys);
        assert_eq!(rewards.isk_reward, 0);
        assert_eq!(rewards.bonus_reward, 0);
        assert_eq!(rewards.lp_reward, 0);
        let win = mission_bonus_window_secs(&ctx, char_id, rnd_agent.agent_id, mid);
        assert!((1_800..=5_400).contains(&win));
    }
}

#[test]
fn level_gating_blocks_higher_dungeon_and_anomic_offers_and_payouts_for_l1_agents() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let find_sec = |lvl: u8| {
        ctx.sde
            .agents
            .values()
            .filter(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == lvl && a.solar_system_id.is_some())
            .min_by_key(|a| a.agent_id)
            .expect("SDE Basic Security agent")
            .clone()
    };
    let ag1 = find_sec(1);
    let ag2 = find_sec(2);
    let ag3 = find_sec(3);
    let ag4 = find_sec(4);
    let mut ag1_zero = ag1.clone();
    ag1_zero.solar_system_id = ag4.solar_system_id; // same true-sec for strict level check

    // Pool matching with SDE enforces dungeon difficulty == agent.level and L4+ for Anomic/Burner
    let t_l1 = &ctx.sde.missions[&13_777];
    let t_l2 = &ctx.sde.missions[&4_886];
    let t_l3 = &ctx.sde.missions[&1_140];
    let t_l4 = &ctx.sde.missions[&900];
    let t_l4_desp = &ctx.sde.missions[&13_741];
    let t_burner = &ctx.sde.missions[&16_154];

    assert!(mission_matches_agent_pool_with_sde(&ctx.sde, t_l1, &ag1));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_l2, &ag1));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_l3, &ag1));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_l4, &ag1));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_l4_desp, &ag1));
    assert!(!mission_matches_agent_pool(&t_burner, &ag1));
    assert!(!mission_matches_agent_pool(&t_burner, &ag3));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_burner, &ag1));
    assert!(!mission_matches_agent_pool_with_sde(&ctx.sde, t_burner, &ag3));

    assert!(mission_matches_agent_pool_with_sde(&ctx.sde, t_l2, &ag2));
    assert!(mission_matches_agent_pool_with_sde(&ctx.sde, t_l3, &ag3));
    assert!(mission_matches_agent_pool_with_sde(&ctx.sde, t_l4, &ag4));
    assert!(mission_matches_agent_pool_with_sde(&ctx.sde, t_burner, &ag4));

    // L1 agent evaluating L2-L4/Burner missions cannot claim higher-level Wiki/Burner overrides
    let ag1_sys = ag1.solar_system_id.unwrap();
    for &high_mid in &[4_886_u32, 1_140, 900, 13_741, 16_154] {
        let def = resolve_mission_reward_definition(&ctx, char_id, ag1.agent_id, high_mid);
        assert!(
            def.base_isk <= 90_000 && def.base_lp <= 120,
            "L1 agent must stay in L1 reward bounds for mismatched mission {high_mid}: {def:?}"
        );
        let rw = compute_character_mission_rewards(&ctx, char_id, ag1.agent_id, high_mid, ag1_sys);
        assert!(rw.isk_reward <= 90_000 && rw.bonus_reward <= 120_000);
    }
    // Canonical matching agents receive full Wiki/Burner definitions
    assert_eq!(resolve_mission_reward_definition(&ctx, char_id, ag2.agent_id, 4_886).base_isk, 82_000);
    assert_eq!(resolve_mission_reward_definition(&ctx, char_id, ag3.agent_id, 1_140).base_isk, 692_000);
    assert_eq!(resolve_mission_reward_definition(&ctx, char_id, ag4.agent_id, 900).base_isk, 358_000);
    assert_eq!(resolve_mission_reward_definition(&ctx, char_id, ag4.agent_id, 16_154).base_isk, 6_200_000);
}

#[test]
fn resolve_security_mission_encounter_filters_by_canonical_group_id_not_spawn_index() {
    use eve_sde::{DungeonTemplate, NpcSpawnGroupInfo};

    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.dungeons.clear();
    sde.npc_spawn_groups.clear();
    sde.dungeons.insert(
        9_100,
        DungeonTemplate {
            dungeon_id: 9_100,
            name: "Guristas Scout Outpost".into(),
            archetype_id: Some(20),
            faction_id: Some(500_010),
            difficulty: Some(1),
        },
    );

    // 1. Place non-combat prop/authority/Sleeper groups at high shifted spawn_group_id indices
    //    AND place the legitimate combat group (canonical group_id = 552) at old excluded index 11.
    for (shifted_sgid, canonical_gid, name, fac) in [
        (5_001_u32, 306_u32, "Spawn Container", 500_010_u32),
        (5_002, 494, "Deadspace Overseer's Structure", 500_010),
        (5_003, 922, "Capture Point", 500_010),
        (5_004, 4073, "Temporary Collidable Structure", 500_010),
        (5_005, 301, "Concord Drone", 500_010),
        (5_006, 1307, "Roaming Sleepers Cruiser", 500_021),
    ] {
        sde.npc_spawn_groups.insert(
            shifted_sgid,
            NpcSpawnGroupInfo {
                spawn_group_id: shifted_sgid,
                group_id: canonical_gid,
                group_name: name.into(),
                faction_id: fac,
                ship_class: 1,
                npc_type_ids: vec![16_997],
            },
        );
    }
    sde.npc_spawn_groups.insert(
        11,
        NpcSpawnGroupInfo {
            spawn_group_id: 11,
            group_id: 552, // Guristas Frigate (valid combat NPC despite spawn_group_id == 11)
            group_name: "Guristas Frigate".into(),
            faction_id: 500_010,
            ship_class: 1,
            npc_type_ids: vec![16_997],
        },
    );
    ctx.sde = Arc::new(sde.clone());

    let ag = make_agent(3_018_921, 2, 24, 1);
    let tmpl = make_tmpl(9_200, "Guristas Trouble", Some("BasicKill"), false);
    let enc = resolve_security_mission_encounter(&ctx, &tmpl, &ag, 30_000_142);
    assert_eq!(enc.spawn_group_id, 11);
    assert_eq!(enc.npc_type_ids, vec![16_997]);

    // 2. When only non-combat prop, authority, and non-explicit Sleeper/Drifter groups exist,
    //    even fallback selection must reject all of them instead of picking a prop or CONCORD.
    sde.npc_spawn_groups.remove(&11);
    for (shifted_sgid, canonical_gid, name) in [
        (5_007_u32, 319_u32, "Large Collidable Structure"),
        (5_008, 496, "Deadspace Overseer's Belongings"),
        (5_009, 4494, "Control Point"),
        (5_010, 4576, "Homefront Operations Structure"),
        (5_011, 4771, "Homefront Operations Noncombatant"),
        (5_012, 300, "CONCORD Police"),
        (5_013, 182, "Police Drone"),
        (5_014, 1768, "Seeker Scouts"),
        (5_015, 1804, "Hidden Zenith Amarr Capital"),
    ] {
        sde.npc_spawn_groups.insert(
            shifted_sgid,
            NpcSpawnGroupInfo {
                spawn_group_id: shifted_sgid,
                group_id: canonical_gid,
                group_name: name.into(),
                faction_id: 500_010,
                ship_class: 1,
                npc_type_ids: vec![99_999],
            },
        );
    }
    ctx.sde = Arc::new(sde.clone());
    let fallback_enc = resolve_security_mission_encounter(&ctx, &tmpl, &ag, 30_000_142);
    assert_eq!(
        fallback_enc.spawn_group_id, 0,
        "Non-combat props, authority NPCs, and non-explicit Sleepers must never match mission fallback"
    );

    // 3. Explicit Sleeper/Drifter encounter allows canonical Sleeper group_id regardless of shifted spawn_group_id
    let sleeper_tmpl = make_tmpl(9_201, "Sleeper Incursion", Some("BasicKill"), false);
    sde.dungeons.insert(
        9_101,
        DungeonTemplate {
            dungeon_id: 9_101,
            name: "Sleeper Incursion".into(),
            archetype_id: Some(20),
            faction_id: Some(500_024),
            difficulty: Some(1),
        },
    );
    sde.npc_spawn_groups.insert(
        226,
        NpcSpawnGroupInfo {
            spawn_group_id: 226,
            group_id: 1307,
            group_name: "Roaming Sleepers Cruiser".into(),
            faction_id: 500_024,
            ship_class: 1,
            npc_type_ids: vec![34_337],
        },
    );
    ctx.sde = Arc::new(sde);
    let sleeper_enc = resolve_security_mission_encounter(&ctx, &sleeper_tmpl, &ag, 30_000_142);
    assert_eq!(sleeper_enc.spawn_group_id, 226);
    assert_eq!(sleeper_enc.npc_type_ids, vec![34_337]);
}
