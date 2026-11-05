use super::*;
use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    MissionTimingAndCargoMeta, save_mission_meta,
};
use eve_machonet::SessionState;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
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

    let (isk, lp, bonus) =
        compute_character_mission_rewards(&ctx, char_id, 3_018_921, 100, 30_000_142);
    assert_eq!(lp, 1_500);
    assert_eq!(isk, 1_200_000);
    assert_eq!(bonus, 600_000);

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
        snap.active_missions.insert(
            (char_id, 100),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 100,
                agent_id: 3_018_921,
                state: "Accepted".into(),
                solar_system_id: 30_000_142,
                current_room_id: 0,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![900_001, 900_002],
                bookmark_id: None,
            },
        );
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
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: 100,
                agent_id: 3_018_921,
                granted_courier_item_id: Some(55_001),
                granted_courier_qty: 10,
                ..Default::default()
            },
        );
    });

    on_mission_npc_destroyed_core(&ctx.db, 30_000_142, 900_001, Some(char_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, 100)].npcs_remaining, 1);
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
