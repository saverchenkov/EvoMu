//! Integration tests for open `feat:` rows (Batch 6 Part 2).
//!
//! Covers `feat:pve.insurgency-ice-heist`, `feat:pve.mission-bonus-timer`,
//! `feat:pve.mission-offer-expiry-no-penalty`,
//! `feat:pve.mission-level-clone-and-space-restrictions`,
//! `feat:pve.research-agent-daily-mission`,
//! `feat:ships.board-ship-skill-and-clone-check`,
//! `feat:sovereignty.mercenary-den-anchoring`,
//! `feat:sovereignty.mercenary-den-development-anarchy`,
//! `feat:sovereignty.mercenary-den-anarchy-evolution`, and
//! `feat:sovereignty.mercenary-den-reinforcement-loot`.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::quasar_sov_equinox_hub::{id_req, rpc as q_sov_rpc, structure};
use crate::sovereignty_equinox_gameplay_closure::{
    find_nullsec_planets, make_ball, register_online_pilot, rpc_call, ALLIANCE, CORP, PILOT,
    PILOT_SHIP,
};
use eve_db::{
    corp_roles, ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_CONFLICT, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::{save_rnd_research_entries, RndResearchEntry};
use eve_server::services::insurgency_services::{InsurgencyCampaign, FACTION_GURISTAS};
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    apply_mercenary_den_weapon_damage, MERC_DEN_BASE_HP, MERC_DEN_REINFORCEMENT_SECS,
    TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
};
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

const PILOT_A: i64 = 90_100_001;
const STATION_ID: i64 = 60_003_760;
const SYS_ID: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    crate::common::pin_clock(&ctx);
    ctx
}

fn char_session(
    char_id: i64,
    station_id: Option<i64>,
    solar_system_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_044);
    sess.station_id = station_id;
    sess.solar_system_id = solar_system_id;
    sess.ship_id = ship_id;
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn reload_db(ctx: &ServerContext) -> Arc<WorldRepository> {
    let db = WorldRepository::new_empty();
    let snap = ctx.db.with_snapshot(|s| s.clone());
    db.with_mut_snapshot(|s| *s = snap);
    Arc::new(db)
}

fn ensure_char(ctx: &ServerContext, char_id: i64, name: &str, war_fac: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| template);
        ch.character_id = char_id;
        ch.name = name.to_string();
        ch.corporation_id = 1_000_044;
        ch.solar_system_id = SYS_ID;
        ch.station_id = Some(STATION_ID);
        ch.active_ship_id = char_id + 10_000;
        ch.balance = IskCents::from_isk(10_000_000);
        ch.war_faction_id = war_fac;
    });
}

fn colocate_with_agent(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, agent_id: i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(SYS_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
}

#[tokio::test]
async fn test_pve_insurgency_ice_heist_instances_and_persistence() {
    // parity: feat:pve.insurgency-ice-heist
    eve_testkit::attest(&["feat:pve.insurgency-ice-heist"]);
    let ctx = new_ctx();
    let hub_sys: u32 = 30_002_537;
    let camp_id: i64 = 504;
    ensure_char(&ctx, PILOT_A, "HeistScout", Some(500_001));
    let mut sess = char_session(PILOT_A, None, Some(hub_sys), Some(80_020));

    let empty_heists = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[],
    )
    .await
    .expect("GetPirateInsurgencyIceHeistInstances");
    let PyRep::List(list0) = empty_heists else { panic!("expected List") };
    assert!(list0.is_empty());

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut camp = InsurgencyCampaign {
            campaign_id: camp_id,
            pirate_faction_id: FACTION_GURISTAS,
            target_faction_id: 500_001,
            fob_solar_system_id: hub_sys,
            systems: vec![hub_sys],
            created_at_unix: now.saturating_sub(3600),
            forecasting_end_unix: now.saturating_sub(1800),
            active_end_unix: now + 86_400,
            winddown_end_unix: now + 172_800,
            corruption_points: HashMap::new(),
            suppression_points: HashMap::new(),
        };
        camp.corruption_points.insert(hub_sys, 60_000);
        snap.insurgency_runtime.campaigns.insert(camp_id, camp);
    });
    let heist_id = eve_server::services::insurgency_services::spawn_ice_heist(&ctx, hub_sys);

    let heists = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[PyRep::Integer(i64::from(hub_sys))],
    )
    .await
    .expect("GetPirateInsurgencyIceHeistInstances at corruption 3");
    let PyRep::List(list3) = heists else { panic!("expected List") };
    assert!(
        list3
            .iter()
            .any(|item| item.dict_get("instanceID").and_then(|v| v.as_i64()) == Some(heist_id)),
        "Ice Refinery Heist must spawn at Corruption Stage 3"
    );
}

#[tokio::test]
async fn test_pve_mission_bonus_timer_payout_and_expiry() {
    // parity: feat:pve.mission-bonus-timer
    eve_testkit::attest(&["feat:pve.mission-bonus-timer"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "BonusRunner", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 2 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .expect("Level 2 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(agent.corporation_id, 5.0);
    });

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
        .await
        .expect("Offer mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let accept_rep = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await
        .expect("Accept mission");
    let bonus_expiry = accept_rep.dict_get("bonusExpiryUnix").and_then(|v| v.as_i64()).unwrap() as u64;
    assert!(bonus_expiry > ctx.now_unix());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid)) {
            m.npcs_remaining = 0;
        }
    });
    let comp = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect("Complete within bonus window");
    let bonus_paid = comp.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap();
    assert!(bonus_paid > 0);

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Completed");
    });

    let agent_l1 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.agent_id != agent.agent_id && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent_l1.agent_id);
    let offer_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_l1.agent_id)])
        .await
        .expect("Offer mission B");
    let mid_b = offer_b.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid_b as i64)])
        .await
        .expect("Accept mission B");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid_b)) {
            m.npcs_remaining = 0;
        }
    });
    ctx.advance_secs(10_800);
    let comp_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid_b as i64)])
        .await
        .expect("Complete after bonus window");
    assert_eq!(comp_b.dict_get("bonusReward").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_pve_mission_offer_expiry_no_penalty_and_cosmos_lockout() {
    // parity: feat:pve.mission-offer-expiry-no-penalty
    eve_testkit::attest(&["feat:pve.mission-offer-expiry-no-penalty"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "OfferExpiryPilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id != 18 && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
        .await
        .expect("Offer mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let standing_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0)
    });

    ctx.advance_secs(605_000);
    let _ = rpc(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Expired");
        let standing_after = snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0);
        assert_eq!(standing_before, standing_after);
    });

    let cosmos_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5 && a.solar_system_id.is_some())
        .expect("Data Center agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, cosmos_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(cosmos_agent.corporation_id, 5.0);
        snap.active_missions.insert(
            (PILOT_A, 99_999),
            ActiveMissionRecord {
                mission_id: 99_999,
                character_id: PILOT_A,
                agent_id: cosmos_agent.agent_id,
                solar_system_id: SYS_ID,
                state: "Expired".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });
    let reoffer_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(cosmos_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(reoffer_err.code, "CosmosMissionExpired");
}

#[tokio::test]
async fn test_pve_mission_level_clone_and_space_restrictions() {
    // parity: feat:pve.mission-level-clone-and-space-restrictions
    eve_testkit::attest(&["feat:pve.mission-level-clone-and-space-restrictions"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "CloneSpacePilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let l4_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 4 && a.solar_system_id.is_some() && a.station_id.is_some() && a.agent_type_id == 2 && a.division_id != 18)
        .expect("Level 4 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, l4_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_A).unwrap();
        ch.clone_grade = 1;
        ch.standings.insert(l4_agent.corporation_id, 8.0);
        ch.standings.insert(l4_agent.agent_id, 8.0);
    });

    let alpha_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(alpha_err.code, "CantDoMissionAlphaClone");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().clone_grade = 2;
    });
    let omega_ok = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
        .await
        .expect("Omega clone L4 mission");
    assert!(omega_ok.dict_get("missionID").and_then(|v| v.as_i64()).is_some());

    let l5_highsec = ctx.sde.agents.get(&3008513).expect("L5 highsec agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, l5_highsec.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(l5_highsec.corporation_id, 9.0);
    });
    let space_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l5_highsec.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(space_err.code, "CantDoMission");
}

#[tokio::test]
async fn test_pve_research_agent_daily_mission_and_persistence() {
    // parity: feat:pve.research-agent-daily-mission
    eve_testkit::attest(&["feat:pve.research-agent-daily-mission"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "RndPilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let rnd_agent = ctx.sde.agents.get(&3009318).expect("R&D agent 3009318");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, rnd_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_A).unwrap();
        ch.clone_grade = 2;
        ch.standings.insert(rnd_agent.corporation_id, 6.0);
    });

    let no_res_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(no_res_err.code, "ResearchNotActive");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        save_rnd_research_entries(
            snap,
            PILOT_A,
            &[RndResearchEntry {
                agent_id: rnd_agent.agent_id,
                research_type_id: 11444,
                points_per_day: 120.0,
                initial_points: 50.0,
                started_unix: now,
            }],
        );
    });

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
        .await
        .expect("Offer daily R&D mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(mid > 0);

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Offered");
    });
}

#[tokio::test]
async fn test_ships_board_ship_skill_and_clone_check_and_persistence() {
    // parity: feat:ships.board-ship-skill-and-clone-check
    eve_testkit::attest(&["feat:ships.board-ship-skill-and-clone-check"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "ShipBoarder", None);
    let target_ship_id = 910_001i64;
    let frigate_type_id = 587u32; // Rifter (requires Minmatar Frigate 3330)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&PILOT_A);
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: frigate_type_id,
                owner_id: PILOT_A,
                location_id: STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));
    let skill_err = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
        .await
        .unwrap_err();
    assert_eq!(skill_err.code, "ShipHasSkillPrerequisites");

    let saved_skills = ctx.db.with_snapshot(|s| s.skills.get(&90_000_001).cloned().unwrap_or_default());
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.insert(PILOT_A, saved_skills);
    });
    let board_rep = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
        .await
        .expect("Board ship with required skill");
    assert_eq!(board_rep.dict_get("shipID").and_then(|v| v.as_i64()), Some(target_ship_id));
    assert_eq!(sess.ship_id, Some(target_ship_id));

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.characters[&PILOT_A].active_ship_id, target_ship_id);
    });
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_anchoring_validation_and_persistence() {
    // parity: feat:sovereignty.mercenary-den-anchoring
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-anchoring"]);
    let ctx = new_ctx();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        72_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        DVec3::new(50_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_201,
            InventoryItemRecord {
                item_id: 990_201,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenTooFarFromSkyhook"
    );

    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet)])
            .await
            .unwrap_err()
            .code,
        "PlanetNotTemperate"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
    });
    let den_rep = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await
        .expect("Anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_id));
    });
}

async fn setup_anchored_merc_den(ctx: &ServerContext, sess_id: u64, with_hub: bool) -> (SessionState, i64, i64) {
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(ctx);
    let hub_id = 1_050_000_001i64;
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);
    let mut s_owner = register_online_pilot(
        ctx, sess_id, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        if with_hub {
            snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        }
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_200 + sess_id as i64,
            InventoryItemRecord {
                item_id: 990_200 + sess_id as i64,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;
    let den_rep = rpc_call(
        ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    )
    .await
    .expect("Anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    (s_owner, hub_id, den_id)
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_development_anarchy_siphons_workforce() {
    // parity: feat:sovereignty.mercenary-den-development-anarchy
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-development-anarchy"]);
    let ctx = new_ctx();
    let (mut s_owner, hub_id, den_id) = setup_anchored_merc_den(&ctx, 72_002, true).await;

    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    ctx.advance_secs(25 * 3_600);
    assert_eq!(
        q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
            .await
            .status_code,
        STATUS_OK
    );
    let siphoned_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(siphoned_wf + 500, base_wf);
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_anarchy_evolution_and_persistence() {
    // parity: feat:sovereignty.mercenary-den-anarchy-evolution
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-anarchy-evolution"]);
    let ctx = new_ctx();
    let (mut s_owner, _hub_id, den_id) = setup_anchored_merc_den(&ctx, 72_003, false).await;

    ctx.advance_secs(12 * 3_600);
    let rsp = q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(rsp.status_code, STATUS_OK);

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        let den = &snap.sov_runtime.mercenary_dens[&den_id];
        assert!(den.infomorphs > 0 || den.anarchy_level > 0);
    });
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_reinforcement_loot_and_wreck() {
    // parity: feat:sovereignty.mercenary-den-reinforcement-loot
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-reinforcement-loot"]);
    let ctx = new_ctx();
    let (mut s_owner, _hub_id, den_id) = setup_anchored_merc_den(&ctx, 72_004, false).await;

    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..8i64 {
            snap.items.insert(
                994_000 + idx,
                InventoryItemRecord {
                    item_id: 994_000 + idx,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: PILOT,
                    location_id: den_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        }
    });

    let reinf_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .expect("Deplete shield into reinforcement");
    assert_eq!(reinf_rep.dict_get("state").and_then(|v| v.as_str()), Some("HullReinforced"));
    assert_eq!(
        q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
            .await
            .status_code,
        STATUS_CONFLICT
    );

    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 120);
    let dest_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .expect("Deplete hull in vulnerability window");
    assert_eq!(dest_rep.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
    let dropped_stacks = dest_rep.dict_get("droppedStacks").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!((1..=7).contains(&dropped_stacks));
}
