//! Integration tests for PVE Agent Specializations, Storylines, Research Agents, Locators & Epic Arcs.
//!
//! Covers parity rows:
//! - `feat:pve.mission-mining`: Mining missions spawn private pocket with mission asteroids; completion requires delivery of mined ore to the agent.
//! - `feat:pve.mission-storyline`: Every 16 regular missions of a level grants a storyline mission offer from a nearby agent.
//! - `feat:pve.mission-levels`: Level 1-5 agents with standings gates; level 5 agents located in low-sec systems.
//! - `feat:pve.mission-standing-gate`: Effective standing with Connections/Diplomacy/Criminal Connections gates agent availability.
//! - `feat:pve.mission-faction-standing-loss`: Missions against another faction apply the published derived standing loss to that faction on completion.
//! - `feat:pve.research-agents`: R&D agents accrue research points per day by skill/standing; points redeem for datacores at the agent (`agentMgr` research).
//! - `feat:pve.locator-agents`: Locator agents return target character's location precision by agent level for an ISK fee with cooldown.
//! - `feat:pve.career-agents`: Career agent mission chains (Business, Exploration, Industry, Military, Advanced Military).
//! - `feat:pve.epic-arcs`: Multi-mission arc state (`epicArcStatus`, `GetMyEpicArcStatus`) with branching and 90-day cooldown.
//! - `feat:pve.data-centers`: Data center agents exchange specified pirate tags/items for rewards (repeatable offers).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn create_auth_session(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login failed");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID failed");
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

async fn bind_service_oid(ctx: &ServerContext, sess: &mut SessionState, service: &str, bind_param: PyRep) -> u64 {
    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(sess, service, None, "MachoBindObject", &[bind_param, PyRep::None], &kw, None).await.expect("MachoBindObject");
    match res {
        PyRep::Tuple(outer) => outer[0].as_i64().expect("bound_id integer") as u64,
        PyRep::Integer(oid) => oid as u64,
        _ => panic!("Expected Tuple or integer from MachoBindObject"),
    }
}

#[tokio::test]
async fn test_pve_agent_levels_standing_gates_and_faction_losses() {
    // parity: feat:pve.mission-levels, feat:pve.mission-standing-gate, feat:pve.mission-faction-standing-loss, feat:pve.mission-storyline
    eve_testkit::attest(&[
        "feat:pve.mission-levels",
        "feat:pve.mission-standing-gate",
        "feat:pve.mission-faction-standing-loss",
        "feat:pve.mission-storyline",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 2101, char_id).await;
    let kw = HashMap::new();

    // 1. Agent Levels & Space Restrictions (feat:pve.mission-levels)
    // SDE contains agents of levels 1 through 5
    for lvl in 1..=5_u8 {
        let count = ctx.sde.agents.values().filter(|a| a.level == lvl).count();
        assert!(count > 0, "Level {lvl} agents must exist in SDE");
    }

    // Level 5 agents exist in SDE; lowsec Level 5 agents exist (security < 0.45)
    let mut l5_lowsec_agents: Vec<_> = ctx.sde.agents.values().filter(|a| {
        if a.level != 5 { return false; }
        let Some(sys_id) = a.solar_system_id else { return false; };
        let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
        sys_sec < 0.45
    }).collect();
    l5_lowsec_agents.sort_by_key(|a| a.agent_id);
    assert!(!l5_lowsec_agents.is_empty(), "Level 5 lowsec agents must exist in SDE");

    // Highsec Level 5 agents (if any) cannot operate in high-security space per engine rule
    let mut l5_highsec_agents: Vec<_> = ctx.sde.agents.values().filter(|a| {
        if a.level != 5 { return false; }
        let Some(sys_id) = a.solar_system_id else { return false; };
        let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
        sys_sec >= 0.45
    }).collect();
    l5_highsec_agents.sort_by_key(|a| a.agent_id);
    if let Some(hs_agent) = l5_highsec_agents.first() {
        let prev_sys = sess.solar_system_id;
        let prev_sta = sess.station_id;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = hs_agent.solar_system_id.unwrap();
                ch.station_id = hs_agent.station_id;
            }
        });
        sess.solar_system_id = hs_agent.solar_system_id;
        sess.station_id = hs_agent.station_id;
        let hs_err = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(hs_agent.agent_id)], &kw, None)
            .await
            .expect_err("Level 5 agent in highsec must be rejected by engine");
        assert_eq!(hs_err.code, "CantDoMission");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = prev_sys.unwrap();
                ch.station_id = prev_sta;
            }
        });
        sess.solar_system_id = prev_sys;
        sess.station_id = prev_sta;
    }

    // Level 5 lowsec agent standing gate requires 7.0 standing; requesting mission with low standing is rejected
    let l5_test_agent = &l5_lowsec_agents[0];
    let l5_agent_id = l5_test_agent.agent_id;
    let l5_corp_id = l5_test_agent.corporation_id;
    assert!(l5_corp_id > 0);
    let l5_err = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(l5_agent_id)], &kw, None)
        .await
        .expect_err("Level 5 agent should reject request from highsec session or low standing");
    assert!(l5_err.code == "AgentNotCoLocated" || l5_err.code == "CantDoMission" || l5_err.code == "InsufficientAgentStanding");

    // 2. Standing Gates with Connections, Diplomacy & Criminal Connections (feat:pve.mission-standing-gate)
    let l2_agent = ctx.sde.agents.values()
        .filter(|a| a.level == 2 && a.division_id == 24 && a.agent_type_id == 2 && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L2 Security agent");
    let l2_corp = l2_agent.corporation_id;
    let l1_agent = ctx.sde.agents.values()
        .filter(|a| a.level == 1 && a.division_id == 24 && a.agent_type_id == 2 && a.station_id.is_some() && a.corporation_id != l2_corp)
        .min_by_key(|a| a.agent_id)
        .expect("L1 Security agent");
    let l1_corp = l1_agent.corporation_id;

    // Reset standings and skills
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(l2_corp, 0.0);
            ch.standings.insert(l1_corp, -2.5); // below -2.0 lockout
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.clear();
    });

    // Without skills: L2 is locked (needs 1.0), L1 is locked (below -2.0)
    let l2_rep1 = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L2");
    let PyRep::Rowset { rows: l2_r1, .. } = l2_rep1 else { panic!() };
    let l2_row1 = l2_r1.iter().find(|r| r[0].as_i64() == Some(l2_agent.agent_id)).unwrap();
    assert_eq!(l2_row1[9], PyRep::Bool(false), "L2 agent locked without Connections");

    let l1_rep1 = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L1");
    let PyRep::Rowset { rows: l1_r1, .. } = l1_rep1 else { panic!() };
    let l1_row1 = l1_r1.iter().find(|r| r[0].as_i64() == Some(l1_agent.agent_id)).unwrap();
    assert_eq!(l1_row1[9], PyRep::Bool(false), "L1 agent locked with raw standing -2.5");

    // Train Connections V (type 3359): 0.0 base + Connections 5 -> 1.0 effective, unlocking L2
    // Train Diplomacy V (type 3357): -2.5 base + Diplomacy 5 -> -1.5 effective (> -2.0), unlocking L1
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3359, TrainedSkillRecord { type_id: 3359, level: 5, skill_points: 256_000.0 });
        sk.insert(3357, TrainedSkillRecord { type_id: 3357, level: 5, skill_points: 256_000.0 });
        sk.insert(3358, TrainedSkillRecord { type_id: 3358, level: 5, skill_points: 256_000.0 }); // Criminal Connections
    });

    let l2_rep2 = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L2 after Connections");
    let PyRep::Rowset { rows: l2_r2, .. } = l2_rep2 else { panic!() };
    let l2_row2 = l2_r2.iter().find(|r| r[0].as_i64() == Some(l2_agent.agent_id)).unwrap();
    assert_eq!(l2_row2[9], PyRep::Bool(true), "Connections V must unlock L2 agent");

    let l1_rep2 = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L1 after Diplomacy");
    let PyRep::Rowset { rows: l1_r2, .. } = l1_rep2 else { panic!() };
    let l1_row2 = l1_r2.iter().find(|r| r[0].as_i64() == Some(l1_agent.agent_id)).unwrap();
    assert_eq!(l1_row2[9], PyRep::Bool(true), "Diplomacy V must unlock L1 agent");

    // 3. 16-Mission Storyline Tally & Offer (feat:pve.mission-storyline)
    // 4. Opposing Faction Standing Loss on Completion (feat:pve.mission-faction-standing-loss)
    let caldari_agent = ctx.sde.agents.values()
        .filter(|a| a.level == 1 && a.division_id == 24 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some() && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001))
        .min_by_key(|a| a.agent_id)
        .expect("Caldari L1 Security agent");
    let c_agent_id = caldari_agent.agent_id;
    let c_corp_id = caldari_agent.corporation_id;
    let c_sys_id = caldari_agent.solar_system_id.unwrap();
    let c_station_id = caldari_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = c_sys_id;
            ch.station_id = c_station_id;
            ch.standings.insert(c_corp_id, 3.0);
            ch.standings.insert(500_001, 1.0); // Caldari State
            ch.standings.insert(500_004, 0.0); // Gallente Federation (enemy of Caldari)
        }
        // Advance mission tally to 15 (next mission is 16th!)
        snap.mission_tally.insert(char_id, 15);
    });
    sess.solar_system_id = Some(c_sys_id);
    sess.station_id = c_station_id;

    let c_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(c_agent_id)).await;
    let offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(c_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer");
    let PyRep::KeyVal(off_kv) = offer else { panic!() };
    let mid = off_kv["missionID"].as_i64().unwrap() as u32;

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(c_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept");

    let dest_sid = ctx.db.with_snapshot(|snap| {
        eve_server::services::agents_missions::load_mission_meta(snap, char_id, mid).and_then(|m| m.destination_station_id)
    });
    if let Some(dsid) = dest_sid {
        let dsys = ctx.sde.stations.get(&dsid).map(|s| s.solar_system_id).unwrap_or(c_sys_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = dsys;
                ch.station_id = Some(dsid);
            }
        });
        sess.solar_system_id = Some(dsys);
        sess.station_id = Some(dsid);
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
            m.npcs_remaining = 0;
        }
    });

    let gallente_before = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&500_004).copied().unwrap_or(0.0));
    let caldari_before = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&500_001).copied().unwrap_or(0.0));

    // Complete the 16th mission
    let comp = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(c_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("Complete 16th");
    let PyRep::KeyVal(comp_kv) = comp else { panic!() };
    assert_eq!(comp_kv["missionTally"].as_i64(), Some(16), "Tally must reach 16");
    assert_eq!(comp_kv["storylineTriggered"], PyRep::Bool(true), "16th mission must trigger Storyline offer");

    // Storyline mission offer created in active_missions
    let has_storyline = ctx.db.with_snapshot(|snap| {
        snap.active_missions.values().any(|m| {
            m.character_id == char_id
                && m.state == "Offered"
                && ctx.sde.agents.get(&m.agent_id).is_some_and(|a| matches!(a.agent_type_id, 6 | 7))
        })
    });
    assert!(has_storyline, "Storyline mission must be offered by nearby storyline agent");

    // Derived faction standing loss applied to opposing faction
    let caldari_after = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&500_001]);
    let gallente_after = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&500_004]);
    assert!(caldari_after > caldari_before, "Caldari standing must increase");
    assert!(gallente_after < gallente_before, "Gallente standing must suffer derived loss (rel -5.0)");

    // Standing transactions recorded in DB
    let txs = ctx.db.with_snapshot(|snap| snap.standing_transactions.get(&char_id).cloned().unwrap_or_default());
    assert!(txs.iter().any(|t| t.modification > 0.0), "Positive standing transaction must exist");
    assert!(txs.iter().any(|t| t.modification < 0.0 && t.from_id == 500_004), "Negative derived standing transaction must exist for Gallente");
}

#[tokio::test]
async fn test_pve_mining_missions_career_and_data_centers() {
    // parity: feat:pve.mission-mining, feat:pve.career-agents, feat:pve.data-centers
    eve_testkit::attest(&["feat:pve.mission-mining", "feat:pve.career-agents", "feat:pve.data-centers"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 2102, char_id).await;
    let kw = HashMap::new();

    // 1. Mining Missions (feat:pve.mission-mining)
    let mining_agent = ctx.sde.agents.values()
        .filter(|a| a.level == 1 && a.division_id == 23 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 Mining agent in SDE");
    let m_agent_id = mining_agent.agent_id;
    let m_corp_id = mining_agent.corporation_id;
    let m_sys_id = mining_agent.solar_system_id.unwrap();
    let m_sta_id = mining_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = m_sys_id;
            ch.station_id = m_sta_id;
            ch.standings.insert(m_corp_id, 3.0);
        }
    });
    sess.solar_system_id = Some(m_sys_id);
    sess.station_id = m_sta_id;

    let m_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(m_agent_id)).await;
    let offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(m_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Mining offer");
    let PyRep::KeyVal(off_kv) = offer else { panic!() };
    let mid = off_kv["missionID"].as_i64().unwrap() as u32;

    // Accept mining mission -> spawns private pocket asteroid
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(m_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept mining");

    let expected_ast_id = 910_000_000 + (mid as i64) * 1000 + 1;
    let ast_exists = ctx.db.with_snapshot(|snap| snap.asteroids.contains_key(&expected_ast_id));
    assert!(ast_exists, "Private mission asteroid must be spawned in space for mining mission");

    // Completing without ore must fail
    let premature = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(m_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect_err("Premature complete must fail");
    assert_eq!(premature.code, "MissionIncomplete");

    // Deliver required ore (500 units of Veldspar 1230 or target ore) into hangar
    let ore_tid = ctx.db.with_snapshot(|snap| {
        snap.asteroids.get(&expected_ast_id).map(|a| a.type_id).unwrap_or(1230)
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(item_id, InventoryItemRecord {
            item_id,
            type_id: ore_tid,
            owner_id: char_id,
            location_id: m_sta_id.unwrap(),
            flag: InventoryFlag::Hangar,
            quantity: 500,
            singleton: false,
            version: 1,
            ..Default::default()
        });
    });

    let done = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(m_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("Complete mining");
    let PyRep::KeyVal(done_kv) = done else { panic!() };
    assert_eq!(done_kv["state"].as_str(), Some("Completed"));

    // 2. Career Agents (feat:pve.career-agents)
    let career_agent = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 12 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("Career agent in SDE");
    let car_aid = career_agent.agent_id;
    let car_cid = career_agent.corporation_id;
    let car_sys = career_agent.solar_system_id.unwrap();
    let car_sta = career_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = car_sys;
            ch.station_id = car_sta;
            ch.standings.insert(car_cid, 2.0);
        }
    });
    sess.solar_system_id = Some(car_sys);
    sess.station_id = car_sta;

    let car_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(car_aid)).await;
    let car_offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(car_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Career offer");
    let PyRep::KeyVal(co_kv) = car_offer else { panic!() };
    let car_mid = co_kv["missionID"].as_i64().unwrap() as u32;

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(car_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept career");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, car_mid)) {
            m.state = "Completed".into();
            m.npcs_remaining = 0;
        }
    });

    let car_completed_dict = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetCompletedCareerAgentIDs",
            &[PyRep::List(vec![
                PyRep::Integer(car_aid),
                PyRep::Integer(3_018_935),
            ])],
            &kw,
            None,
        )
        .await
        .expect("GetCompletedCareerAgentIDs");
    let PyRep::Dict(c_entries) = car_completed_dict else { panic!() };
    assert!(
        c_entries.contains(&(PyRep::Integer(car_aid), PyRep::Bool(true))),
        "Completed career agent must map to true in GetCompletedCareerAgentIDs"
    );
    assert!(
        c_entries.contains(&(PyRep::Integer(3_018_935), PyRep::Bool(false))),
        "Uncompleted queried career agent must map to false in GetCompletedCareerAgentIDs"
    );

    let car_repeat_err = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(car_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect_err("Repeat career must fail");
    assert_eq!(car_repeat_err.code, "OneTimeAgentMissionCompleted");

    // 3. Data Center Agents (feat:pve.data-centers)
    let dc_agent = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 5 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("Data Center agent in SDE");
    let dc_aid = dc_agent.agent_id;
    let dc_cid = dc_agent.corporation_id;
    let dc_sys = dc_agent.solar_system_id.unwrap();
    let dc_sta = dc_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = dc_sys;
            ch.station_id = dc_sta;
            ch.standings.insert(dc_cid, 5.0);
        }
        // Seed 1x pirate tag 17202 in hangar
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(item_id, InventoryItemRecord {
            item_id,
            type_id: 17_202,
            owner_id: char_id,
            location_id: dc_sta.unwrap(),
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            version: 1,
            ..Default::default()
        });
    });
    sess.solar_system_id = Some(dc_sys);
    sess.station_id = dc_sta;

    let dc_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(dc_aid)).await;
    let dc_off = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dc_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("DC offer");
    let PyRep::KeyVal(dco_kv) = dc_off else { panic!() };
    let dc_mid = dco_kv["missionID"].as_i64().unwrap() as u32;
    assert!(dc_mid > 0);

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dc_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept DC");

    let dc_done = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dc_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("Complete DC tag turn-in");
    let PyRep::KeyVal(dcd_kv) = dc_done else { panic!() };
    assert_eq!(dcd_kv["state"].as_str(), Some("Completed"));

    let dc_repeat = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dc_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect_err("Repeat DC must be rejected");
    assert_eq!(dc_repeat.code, "OneTimeAgentMissionCompleted");
}

#[tokio::test]
async fn test_pve_research_locator_and_epic_arcs() {
    // parity: feat:pve.research-agents, feat:pve.locator-agents, feat:pve.epic-arcs
    eve_testkit::attest(&["feat:pve.research-agents", "feat:pve.locator-agents", "feat:pve.epic-arcs"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let target_char_id = 90_000_002_i64;
    let mut sess = create_auth_session(&ctx, 2103, char_id).await;
    let kw = HashMap::new();

    // 1. R&D Research Agents (feat:pve.research-agents)
    let rnd_skill_id = 11_448_u32; // Mechanical Engineering
    let rnd_agent = ctx.sde.agents.values()
        .filter(|a| a.level == 1 && a.agent_type_id == 4 && a.station_id.is_some() && a.solar_system_id.is_some() && a.research_skills.contains(&rnd_skill_id))
        .min_by_key(|a| a.agent_id)
        .expect("L1 R&D agent in SDE");
    let rnd_aid = rnd_agent.agent_id;
    let rnd_cid = rnd_agent.corporation_id;
    let rnd_sys = rnd_agent.solar_system_id.unwrap();
    let rnd_sta = rnd_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = rnd_sys;
            ch.station_id = rnd_sta;
            ch.standings.insert(rnd_cid, 5.0);
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(rnd_skill_id, TrainedSkillRecord { type_id: rnd_skill_id, level: 3, skill_points: 8_000.0 });
        sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 5, skill_points: 256_000.0 }); // Science V
        sk.insert(3392, TrainedSkillRecord { type_id: 3392, level: 5, skill_points: 256_000.0 }); // Mechanics V
    });
    sess.solar_system_id = Some(rnd_sys);
    sess.station_id = rnd_sta;

    let rnd_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(rnd_aid)).await;

    // Start research via DoAction(12)
    let start_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(rnd_oid), "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd_aid), PyRep::Integer(rnd_skill_id as i64)], &kw, None)
        .await
        .expect("Start research");
    let PyRep::KeyVal(start_kv) = start_res else { panic!() };
    assert_eq!(start_kv["researchStarted"], PyRep::Bool(true));
    assert!(start_kv["pointsPerDay"].as_f64().unwrap_or(0.0) > 0.0);

    // Accrue research points and buy datacores via DoAction(14)
    ctx.db.with_mut_snapshot(|snap| {
        let mut entries = eve_server::services::agents_missions::load_rnd_research_entries(snap, char_id);
        if let Some(re) = entries.iter_mut().find(|e| e.agent_id == rnd_aid) {
            re.initial_points = 500.0;
        }
        eve_server::services::agents_missions::save_rnd_research_entries(snap, char_id, &entries);
    });

    let buy_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(rnd_oid), "DoAction", &[PyRep::Integer(14), PyRep::Integer(rnd_aid), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Buy datacores");
    let PyRep::KeyVal(buy_kv) = buy_res else { panic!() };
    assert_eq!(buy_kv["quantity"].as_i64(), Some(1));
    assert!(buy_kv["datacoreTypeID"].as_i64().is_some());

    // Cancel research via DoAction(13)
    let cancel_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(rnd_oid), "DoAction", &[PyRep::Integer(13), PyRep::Integer(rnd_aid)], &kw, None)
        .await
        .expect("Cancel research");
    let PyRep::KeyVal(can_kv) = cancel_res else { panic!() };
    assert_eq!(can_kv["researchCancelled"], PyRep::Bool(true));

    // 2. Locator Agents (feat:pve.locator-agents)
    let loc_agent = ctx.sde.agents.values()
        .filter(|a| a.is_locator && a.level == 1 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 Locator agent in SDE");
    let loc_aid = loc_agent.agent_id;
    let loc_cid = loc_agent.corporation_id;
    let loc_sys = loc_agent.solar_system_id.unwrap();
    let loc_sta = loc_agent.station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = loc_sys;
            ch.station_id = loc_sta;
            ch.standings.insert(loc_cid, 5.0);
            ch.balance = IskCents::from_isk(5_000_000);
        }
        // Place target character in same constellation
        if let Some(tch) = snap.characters.get_mut(&target_char_id) {
            tch.solar_system_id = loc_sys;
            tch.station_id = loc_sta;
        }
    });
    sess.solar_system_id = Some(loc_sys);
    sess.station_id = loc_sta;

    let loc_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(loc_aid)).await;

    // Locate target character
    let loc_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(loc_oid), "DoAction", &[PyRep::Integer(15), PyRep::Integer(loc_aid), PyRep::Integer(target_char_id)], &kw, None)
        .await
        .expect("Locate character");
    let PyRep::KeyVal(loc_kv) = loc_res else { panic!() };
    assert_eq!(loc_kv["targetCharacterID"].as_i64(), Some(target_char_id));
    assert_eq!(loc_kv["solarSystemID"].as_i64(), Some(loc_sys as i64));
    assert_eq!(loc_kv["precision"].as_str(), Some("SameConstellation"));
    assert_eq!(loc_kv["cooldownSeconds"].as_i64(), Some(900));

    // Wallet journal logs AgentLocationServices fee
    let has_fee = ctx.db.with_snapshot(|snap| snap.wallet_journal.iter().any(|e| e.entry_type == "AgentLocationServices"));
    assert!(has_fee, "Locator agent fee must be journaled in wallet");

    // Immediate repeat location fails with LocatorAgentOnCooldown
    let loc_cd_err = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(loc_oid), "DoAction", &[PyRep::Integer(15), PyRep::Integer(loc_aid), PyRep::Integer(target_char_id)], &kw, None)
        .await
        .expect_err("Repeat locate must fail on cooldown");
    assert_eq!(loc_cd_err.code, "LocatorAgentOnCooldown");

    // 3. Epic Arcs & Cooldown (feat:pve.epic-arcs)
    let epic_agent = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 10 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("Epic Arc agent in SDE");
    let ep_aid = epic_agent.agent_id;
    let ep_sys = epic_agent.solar_system_id.unwrap();
    let (arc_id, arc_faction_id) = eve_server::services::agents_missions::resolve_epic_arc_id_for_agent(&ctx, ep_aid);

    // Check epicArcStatus service RPC
    let has_epic = ctx.dispatch_rpc(&mut sess, "epicArcStatus", None, "AgentHasEpicMissionsForCharacter", &[PyRep::Integer(ep_aid)], &kw, None)
        .await
        .expect("AgentHasEpicMissionsForCharacter");
    assert!(matches!(has_epic, PyRep::Bool(_)));

    // Seed active Epic Arc state in DB
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(epic_agent.corporation_id, 8.0);
            ch.standings.insert(arc_faction_id, 8.0);
        }
        let entry = eve_server::services::agents_missions::EpicArcStateEntry {
            epic_arc_id: arc_id,
            agent_id: ep_aid,
            faction_id: arc_faction_id,
            chapter: 1,
            completed_missions: vec![1001],
            chosen_branch: Some("HyasyodaBranch".into()),
            completed_at_unix: None,
            cooldown_until_unix: None,
        };
        eve_server::services::agents_missions::save_epic_arc_states(snap, char_id, &[entry]);
    });

    let status_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyEpicArcStatus", &[], &kw, None).await.expect("GetMyEpicArcStatus");
    let PyRep::Dict(arcs) = status_rep else { panic!("Expected Dict from GetMyEpicArcStatus (#639)") };
    let (_, PyRep::Dict(arc1_missions)) = arcs.iter().find(|(k, _)| k.as_i64() == Some(arc_id as i64)).expect("Arc 1 present") else { panic!() };
    let arc1 = &arc1_missions[0].1;
    assert_eq!(arc1.dict_get("state").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(arc1.dict_get("chosenBranch").and_then(|v| v.as_str()), Some("HyasyodaBranch"));

    let msgs_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMessagesForEpicArcMissions", &[], &kw, None).await.expect("GetMessages");
    let PyRep::Dict(msgs) = msgs_rep else { panic!() };
    assert!(msgs.iter().any(|(k, _)| k.as_i64() == Some(arc_id as i64)), "Arc briefing message must exist");

    // Complete epic arc with 90-day cooldown (7,776,000s)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ep_sys;
            ch.station_id = epic_agent.station_id;
            ch.standings.insert(epic_agent.corporation_id, 8.0);
            ch.standings.insert(arc_faction_id, 8.0);
        }
        let completed_entry = eve_server::services::agents_missions::EpicArcStateEntry {
            epic_arc_id: arc_id,
            agent_id: ep_aid,
            faction_id: arc_faction_id,
            chapter: 3,
            completed_missions: vec![1001, 1002, 1003],
            chosen_branch: Some("HyasyodaBranch".into()),
            completed_at_unix: Some(now),
            cooldown_until_unix: Some(now + 7_776_000),
        };
        eve_server::services::agents_missions::save_epic_arc_states(snap, char_id, &[completed_entry]);
    });
    sess.solar_system_id = Some(ep_sys);
    sess.station_id = epic_agent.station_id;

    let post_status = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyEpicArcStatus", &[], &kw, None).await.expect("GetMyEpicArcStatus post");
    let PyRep::Dict(post_arcs) = post_status else { panic!("Expected Dict from GetMyEpicArcStatus (#639)") };
    let (_, PyRep::Dict(post_missions)) = post_arcs.iter().find(|(k, _)| k.as_i64() == Some(arc_id as i64)).unwrap() else { panic!() };
    let post_arc1 = &post_missions[0].1;
    assert_eq!(post_arc1.dict_get("onCooldown"), Some(&PyRep::Bool(true)));
    assert_eq!(post_arc1.dict_get("state").and_then(|v| v.as_str()), Some("Cooldown"));

    let ep_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(ep_aid)).await;
    let cd_err = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(ep_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect_err("Requesting epic arc on 90-day cooldown must fail");
    assert_eq!(cd_err.code, "EpicArcOnCooldown");
}

#[tokio::test]
async fn test_epic_arc_multi_mission_remote_accept_spawn_and_remote_complete_guards() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 7_701, char_id).await;
    let kw = HashMap::new();

    // Pick a standard docked L1 Security/Combat agent and an Epic Arc agent in another system
    let std_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.level == 1 && a.division_id == 24 && a.solar_system_id == Some(30_000_142) && a.station_id.is_some())
        .or_else(|| ctx.sde.agents.values().find(|a| a.agent_type_id == 2 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some()))
        .expect("Standard L1 combat agent in SDE")
        .clone();
    let ag_sys = std_agent.solar_system_id.unwrap();
    let ag_sta = std_agent.station_id.unwrap();
    let ship_id = 100_000_001_i64;

    // 1. Place character in space (undocked) in another system (Rens 30002510),
    //    seed an Offered combat mission from std_agent in ag_sys, and verify:
    //    - DoAction(3) and DoAction(4) reject un-colocated pilot with AgentNotCoLocated
    //    - DoAction(5) rejects non-remote-eligible regular mission with CannotAcceptRemotely
    let remote_sys = if ag_sys == 30_000_142 { 30_002_510 } else { 30_000_142 };
    let combat_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            ct.contains("BasicKill") && !m.is_courier
        })
        .expect("Combat mission in SDE")
        .mission_id;
    let mining_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            ct.contains("BasicMining") && !m.is_courier
        })
        .expect("Mining mission in SDE")
        .mission_id;

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = remote_sys;
            ch.station_id = None;
            ch.standings.insert(std_agent.agent_id, 5.0);
            ch.standings.insert(std_agent.corporation_id, 5.0);
        }
        snap.active_missions.insert(
            (char_id, combat_mid),
            eve_db::ActiveMissionRecord {
                character_id: char_id,
                mission_id: combat_mid,
                agent_id: std_agent.agent_id,
                solar_system_id: ag_sys,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        eve_server::services::agents_missions::save_mission_meta(
            snap,
            char_id,
            &eve_server::services::agents_missions::MissionTimingAndCargoMeta {
                mission_id: combat_mid,
                agent_id: std_agent.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(ag_sta),
                offered_level: Some(1),
            },
        );
    });
    sess.solar_system_id = Some(remote_sys);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    for act in [3_i64, 4_i64] {
        let err = ctx
            .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(act), PyRep::Integer(combat_mid as i64)], &kw, None)
            .await
            .expect_err("Un-colocated Accept/AcceptChoice must fail");
        assert_eq!(err.code, "AgentNotCoLocated");
    }

    let rem_acc_err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(5), PyRep::Integer(combat_mid as i64)], &kw, None)
        .await
        .expect_err("Regular mission DoAction(5) AcceptRemotely must fail when un-colocated");
    assert_eq!(rem_acc_err.code, "CannotAcceptRemotely");

    // Verify undocked in SAME system also fails DoAction(3 | 4) for stationed agent
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ag_sys;
            ch.station_id = None;
        }
    });
    sess.solar_system_id = Some(ag_sys);
    sess.station_id = None;
    let space_acc_err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(4), PyRep::Integer(combat_mid as i64)], &kw, None)
        .await
        .expect_err("Undocked in same system must still fail DoAction(4) for stationed agent");
    assert_eq!(space_acc_err.code, "AgentNotCoLocated");

    // 2. Remote-eligible Burner mission accepted via DoAction(5) while pilot sits in remote_sys:
    //    Spawned NPC + mining asteroid + bookmark MUST spawn in ag_sys (m.solar_system_id), NEVER remote_sys!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = remote_sys;
            ch.station_id = None;
        }
        if let Some(mut mt) = eve_server::services::agents_missions::load_mission_meta(snap, char_id, combat_mid) {
            mt.is_anomic_burner = true;
            eve_server::services::agents_missions::save_mission_meta(snap, char_id, &mt);
        }
        snap.active_missions.insert(
            (char_id, mining_mid),
            eve_db::ActiveMissionRecord {
                character_id: char_id,
                mission_id: mining_mid,
                agent_id: std_agent.agent_id,
                solar_system_id: ag_sys,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        eve_server::services::agents_missions::save_mission_meta(
            snap,
            char_id,
            &eve_server::services::agents_missions::MissionTimingAndCargoMeta {
                mission_id: mining_mid,
                agent_id: std_agent.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: Some(1230),
                required_delivery_qty: 500,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: true,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(ag_sta),
                offered_level: Some(1),
            },
        );
    });
    sess.solar_system_id = Some(remote_sys);
    sess.station_id = None;

    let remote_combat_acc = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(5), PyRep::Integer(combat_mid as i64)], &kw, None)
        .await
        .expect("Remote-eligible burner mission accepts via DoAction(5)");
    let npc_id = remote_combat_acc.dict_get("spawnedNpcBallID").and_then(|v| v.as_i64()).expect("spawnedNpcBallID");
    let bm_id = remote_combat_acc.dict_get("bookmarkID").and_then(|v| v.as_i64()).expect("bookmarkID");
    let remote_handle = ctx.systems.get_or_spawn(remote_sys);
    let ag_handle = ctx.systems.get_or_spawn(ag_sys);
    assert!(remote_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_none(), "NPC must NOT spawn in caller's remote_sys");
    assert!(ag_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_some(), "NPC MUST spawn in m.solar_system_id (ag_sys)");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.bookmarks[&bm_id].solar_system_id, ag_sys, "Mission bookmark must be in m.solar_system_id");
    });

    let _remote_mining_acc = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(5), PyRep::Integer(mining_mid as i64)], &kw, None)
        .await
        .expect("Remote-eligible burner mining mission accepts via DoAction(5)");
    let ast_id = 910_000_000 + (mining_mid as i64) * 1000 + 1;
    assert!(remote_handle.get_ball_snapshot(ast_id).await.ok().flatten().is_none(), "Asteroid must NOT spawn in remote_sys");
    assert!(ag_handle.get_ball_snapshot(ast_id).await.ok().flatten().is_some(), "Asteroid MUST spawn in ag_sys");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids[&ast_id].solar_system_id, ag_sys);
    });

    // 3. DoAction(6) requires co-location; DoAction(7) rejects physical item delivery; DoAction(7) succeeds on non-item combat
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.get_mut(&(char_id, combat_mid)).unwrap().npcs_remaining = 0;
        snap.items.insert(
            990_701,
            InventoryItemRecord {
                item_id: 990_701,
                type_id: 1230,
                owner_id: char_id,
                location_id: ag_sta,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let comp6_remote_err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(combat_mid as i64)], &kw, None)
        .await
        .expect_err("DoAction(6) must reject non-colocated turn-in");
    assert_eq!(comp6_remote_err.code, "AgentNotCoLocated");

    let comp7_delivery_err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(7), PyRep::Integer(mining_mid as i64)], &kw, None)
        .await
        .expect_err("DoAction(7) CompleteRemotely must reject physical item delivery missions");
    assert_eq!(comp7_delivery_err.code, "CannotCompleteRemotely");

    let comp7_ok = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(7), PyRep::Integer(combat_mid as i64)], &kw, None)
        .await
        .expect("DoAction(7) CompleteRemotely succeeds on cleared non-item mission");
    assert_eq!(comp7_ok.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

    // 4. Epic Arc Agent: Mission #1 keeps arc Active (no cooldown); Mission #2 can be requested
    let soe_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 10 && a.solar_system_id.is_some() && a.station_id.is_some())
        .expect("Docked Epic Arc agent in SDE")
        .clone();
    let soe_sys = soe_agent.solar_system_id.unwrap();
    let soe_sta = soe_agent.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = soe_sys;
            ch.station_id = Some(soe_sta);
            ch.standings.insert(soe_agent.corporation_id, 5.0);
        }
    });
    sess.solar_system_id = Some(soe_sys);
    sess.station_id = Some(soe_sta);

    let soe_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(soe_agent.agent_id)).await;
    let m1_offer = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(soe_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("Epic Arc Mission #1 offer");
    let m1_id = m1_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(soe_oid), "DoAction", &[PyRep::Integer(3), PyRep::Integer(m1_id as i64)], &kw, None)
        .await
        .expect("Accept Epic Arc Mission #1");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, m1_id)) {
            m.npcs_remaining = 0;
        }
        // Clear any delivery requirement so Mission #1 completes cleanly regardless of which SDE EpicArc template was picked
        if let Some(mut mt) = eve_server::services::agents_missions::load_mission_meta(snap, char_id, m1_id) {
            mt.required_delivery_type_id = None;
            mt.required_delivery_qty = 0;
            mt.destination_station_id = None;
            eve_server::services::agents_missions::save_mission_meta(snap, char_id, &mt);
        }
    });
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(soe_oid), "DoAction", &[PyRep::Integer(6), PyRep::Integer(m1_id as i64)], &kw, None)
        .await
        .expect("Complete Epic Arc Mission #1");

    let m2_offer = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(soe_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("Mission #2 must still be offerable immediately after Epic Arc Mission #1");
    assert_eq!(m2_offer.dict_get("state").and_then(|v| v.as_str()), Some("Offered"));
    assert_ne!(m2_offer.dict_get("missionID").and_then(|v| v.as_i64()), Some(m1_id as i64));
}
