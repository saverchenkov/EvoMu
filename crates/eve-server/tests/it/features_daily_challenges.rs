//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 1: R&D access, RP formula, datacores).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
const SKILL_SCIENCE: u32 = 3402;
const SKILL_MECHANICS: u32 = 3392;
const SKILL_POWER_GRID_MANAGEMENT: u32 = 3413;
const SKILL_NEGOTIATION: u32 = 3356;
const SKILL_CONNECTIONS: u32 = 3359;
const SKILL_RESEARCH_PROJECT_MANAGEMENT: u32 = 12179;

fn open_ctx_at(path: &Path, now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    let ctx = ServerContext::with_repository(db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn set_skill(ctx: &ServerContext, char_id: i64, type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            type_id,
            TrainedSkillRecord {
                type_id,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

fn dock_at_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) {
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
        }
    });
}

fn find_sde_datacore_for_skill(ctx: &ServerContext, skill_id: u32) -> Option<u32> {
    ctx.sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 333
                && t.published
                && t.default_attributes
                    .get(&182)
                    .is_some_and(|&req| req as u32 == skill_id)
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
}

async fn do_start_rnd(
    ctx: &ServerContext,
    s: &mut SessionState,
    aid: i64,
    field_skill: u32,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        s,
        "agentMgr",
        None,
        "DoAction",
        &[
            PyRep::Integer(12),
            PyRep::Integer(aid),
            PyRep::Integer(i64::from(field_skill)),
        ],
        &kw,
        None,
    )
    .await
}

async fn do_buy_datacores(
    ctx: &ServerContext,
    s: &mut SessionState,
    aid: i64,
    qty: i64,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        s,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(14), PyRep::Integer(aid), PyRep::Integer(qty)],
        &kw,
        None,
    )
    .await
}

async fn query_journal_research(
    ctx: &ServerContext,
    s: &mut SessionState,
) -> Vec<HashMap<String, PyRep>> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(s, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(tup) = rep else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(research) = &tup[1] else {
        panic!("Expected research list");
    };
    research.iter().map(kv_map).collect()
}

#[tokio::test]
async fn test_b36_rnd_agent_access_rp_formula_and_datacore_purchase() {
    // parity: feat:pve.research-agent-access-requirements, feat:pve.research-agent-rp-formula, feat:pve.datacore-purchase
    eve_testkit::attest(&[
        "feat:pve.research-agent-access-requirements",
        "feat:pve.research-agent-rp-formula",
        "feat:pve.datacore-purchase",
    ]);
    let db_path = std::env::temp_dir().join(format!(
        "evomu_b36_rnd_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&db_path);
    let base_unix = 1_750_000_000_u64;
    let ctx = open_ctx_at(&db_path, base_unix);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    let mut rnd_agents: Vec<_> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 4
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && a.research_skills
                    .iter()
                    .any(|&sk| find_sde_datacore_for_skill(&ctx, sk).is_some())
        })
        .cloned()
        .collect();
    rnd_agents.sort_by_key(|a| a.agent_id);

    let l4_agent = rnd_agents.iter().find(|a| a.level == 4).unwrap().clone();
    let field_skill_a = l4_agent
        .research_skills
        .iter()
        .copied()
        .find(|&sk| find_sde_datacore_for_skill(&ctx, sk).is_some())
        .unwrap();
    let expected_dc_a = find_sde_datacore_for_skill(&ctx, field_skill_a).unwrap();

    let other_field_agent = rnd_agents
        .iter()
        .find(|a| {
            a.level == 1
                && a.agent_id != l4_agent.agent_id
                && a.research_skills.iter().any(|&sk| {
                    sk != field_skill_a && find_sde_datacore_for_skill(&ctx, sk).is_some()
                })
        })
        .unwrap()
        .clone();
    let field_skill_b = other_field_agent
        .research_skills
        .iter()
        .copied()
        .find(|&sk| sk != field_skill_a && find_sde_datacore_for_skill(&ctx, sk).is_some())
        .unwrap();
    let expected_dc_b = find_sde_datacore_for_skill(&ctx, field_skill_b).unwrap();
    assert_ne!(expected_dc_a, expected_dc_b);

    let unoffered_skill = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 270 && !l4_agent.research_skills.contains(&t.type_id))
        .find(|t| find_sde_datacore_for_skill(&ctx, t.type_id).is_some())
        .map(|t| t.type_id)
        .unwrap();

    let extra_l1_agents: Vec<_> = rnd_agents
        .iter()
        .filter(|a| {
            a.level == 1
                && a.agent_id != l4_agent.agent_id
                && a.agent_id != other_field_agent.agent_id
        })
        .take(5)
        .cloned()
        .collect();
    assert_eq!(extra_l1_agents.len(), 5);

    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| {
        *snap = demo;
        snap.skills.entry(char_id).or_default().clear();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.standings.clear();
            ch.balance = IskCents::from_isk(100_000);
        }
    });

    let mut sess = SessionState::new(3601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    let l4_sys = l4_agent.solar_system_id.unwrap();
    let l4_sta = l4_agent.station_id.unwrap();

    // 1. Undocked and wrong-station refusal on StartResearch (12)
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, None);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "AgentNotCoLocated"
    );
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, Some(l4_sta + 1));
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "AgentNotCoLocated"
    );
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, Some(l4_sta));

    // 2. Non-datacore and unoffered research skills refused
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, 11487).await.unwrap_err().code,
        "InvalidResearchSkill"
    );
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, unoffered_skill).await.unwrap_err().code,
        "InvalidResearchSkill"
    );

    // 3. Science V, Mechanics/PG/CPU V, and fieldSkill >= 1 gates
    set_skill(&ctx, char_id, SKILL_SCIENCE, 4);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "ErrorNoRelevantResearchSkills"
    );
    set_skill(&ctx, char_id, SKILL_SCIENCE, 5);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "ErrorNoRelevantResearchSkills"
    );
    set_skill(&ctx, char_id, SKILL_POWER_GRID_MANAGEMENT, 5);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "ErrorNoRelevantResearchSkills"
    );
    set_skill(&ctx, char_id, field_skill_a, 5);
    set_skill(&ctx, char_id, SKILL_MECHANICS, 5);
    set_skill(&ctx, char_id, SKILL_NEGOTIATION, 5);
    set_skill(&ctx, char_id, SKILL_CONNECTIONS, 5);
    set_skill(&ctx, char_id, SKILL_RESEARCH_PROJECT_MANAGEMENT, 0);

    // 4. L4 standing gates: Faction 10.0 + Corp 0.0 refused; Faction eff 4.8 (< 5.0) refused; Faction eff >= 5.0 + Corp eff >= 3.0 succeeds!
    let corp_id = l4_agent.corporation_id;
    let fac_id = ctx.sde.npc_corporations[&corp_id].faction_id.unwrap() as i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(fac_id, 10.0);
        ch.standings.insert(corp_id, 0.0);
    });
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "InsufficientAgentStanding"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.standings.insert(fac_id, 3.5);
        ch.standings.insert(corp_id, 3.0);
    });
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap_err().code,
        "InsufficientAgentStanding"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.standings.insert(fac_id, 6.0);
        ch.standings.insert(corp_id, 3.0);
        ch.standings.insert(l4_agent.agent_id, 5.0);
    });

    // 5. RP formula BEFORE 1st mission: Connections V does NOT raise personal agentStanding (5.0 -> 121.50 RP/day)
    let res_pre = do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap();
    let ppd_pre = kv_map(&res_pre)["pointsPerDay"].as_f64().unwrap();
    assert!((ppd_pre - 121.50).abs() < 1e-6, "Expected 121.50 pre-mission, got {ppd_pre}");

    // Complete 1 real R&D mission for l4_agent via live DoAction(2 -> 3 -> 6)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            810_001,
            InventoryItemRecord {
                item_id: 810_001,
                type_id: 34,
                owner_id: char_id,
                location_id: l4_sta,
                flag: InventoryFlag::Hangar,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let offer_rnd = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)], &kw, None)
        .await
        .unwrap();
    let rnd_mid = kv_map(&offer_rnd)["missionID"].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(rnd_mid)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(rnd_mid)], &kw, None)
        .await
        .unwrap();

    let res_after_m1 = query_journal_research(&ctx, &mut sess).await;
    let ppd_after_m1 = res_after_m1
        .iter()
        .find(|m| m.get("agentID").and_then(|v| v.as_i64()) == Some(l4_agent.agent_id))
        .unwrap()["ppd"]
        .as_f64()
        .unwrap();
    assert!(ppd_after_m1 > ppd_pre + 0.8, "Connections V must raise RP/day after 1st mission");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(l4_agent.agent_id, 10.0);
    });
    let res_max = do_start_rnd(&ctx, &mut sess, l4_agent.agent_id, field_skill_a).await.unwrap();
    let ppd_max = kv_map(&res_max)["pointsPerDay"].as_f64().unwrap();
    assert!((ppd_max - 125.55).abs() < 1e-6, "L4 passive max must be 125.55, got {ppd_max}");

    // 6. Concurrent R&D agent slots: 1 + RPM up to 6 (RPM 0 -> 2nd fails; RPM 5 -> 6 succeed, 7th fails!)
    set_skill(&ctx, char_id, field_skill_b, 5);
    dock_at_station(&ctx, &mut sess, char_id, other_field_agent.solar_system_id.unwrap(), other_field_agent.station_id);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, other_field_agent.agent_id, field_skill_b).await.unwrap_err().code,
        "MaxResearchAgentsReached"
    );
    set_skill(&ctx, char_id, SKILL_RESEARCH_PROJECT_MANAGEMENT, 5);
    do_start_rnd(&ctx, &mut sess, other_field_agent.agent_id, field_skill_b).await.unwrap();
    for extra in &extra_l1_agents[..4] {
        let sk = extra.research_skills.iter().copied().find(|&s| find_sde_datacore_for_skill(&ctx, s).is_some()).unwrap();
        set_skill(&ctx, char_id, sk, 3);
        dock_at_station(&ctx, &mut sess, char_id, extra.solar_system_id.unwrap(), extra.station_id);
        do_start_rnd(&ctx, &mut sess, extra.agent_id, sk).await.unwrap();
    }
    let seventh = &extra_l1_agents[4];
    let sk7 = seventh.research_skills.iter().copied().find(|&s| find_sde_datacore_for_skill(&ctx, s).is_some()).unwrap();
    set_skill(&ctx, char_id, sk7, 3);
    dock_at_station(&ctx, &mut sess, char_id, seventh.solar_system_id.unwrap(), seventh.station_id);
    assert_eq!(
        do_start_rnd(&ctx, &mut sess, seventh.agent_id, sk7).await.unwrap_err().code,
        "MaxResearchAgentsReached"
    );

    // 7. Reopen SQLite DB 2 days later -> verify persisted 372.60 RP (121.50 mission bonus + 2 * 125.55 passive) & Datacore purchase
    drop(ctx);
    let ctx = open_ctx_at(&db_path, base_unix + 2 * 86_400);
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, Some(l4_sta));
    let res_reloaded = query_journal_research(&ctx, &mut sess).await;
    let pts = res_reloaded
        .iter()
        .find(|m| m.get("agentID").and_then(|v| v.as_i64()) == Some(l4_agent.agent_id))
        .unwrap()["points"]
        .as_f64()
        .unwrap();
    assert!((pts - 372.60).abs() < 1e-4, "Expected persisted 372.60 RP, got {pts}");

    dock_at_station(&ctx, &mut sess, char_id, l4_sys, None);
    assert_eq!(do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 1).await.unwrap_err().code, "AgentNotCoLocated");
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, Some(l4_sta + 1));
    assert_eq!(do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 1).await.unwrap_err().code, "AgentNotCoLocated");
    dock_at_station(&ctx, &mut sess, char_id, l4_sys, Some(l4_sta));
    assert_eq!(do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 0).await.unwrap_err().code, "InvalidDatacoreQuantity");
    assert_eq!(do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 4).await.unwrap_err().code, "InsufficientResearchPoints");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000);
    });
    assert_eq!(do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 1).await.unwrap_err().code, "InsufficientFunds");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000);
    });
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let buy_a = kv_map(&do_buy_datacores(&ctx, &mut sess, l4_agent.agent_id, 3).await.unwrap());
    assert!((buy_a["remainingResearchPoints"].as_f64().unwrap() - 72.60).abs() < 1e-4);
    let dc_a_iid = buy_a["datacoreItemID"].as_i64().unwrap();
    let (isk_after, it_a, has_fee_journal) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].balance.0,
            s.items[&dc_a_iid].clone(),
            s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "DatacoreFee" && j.amount.0 == -3_000_000),
        )
    });
    assert_eq!(isk_before - isk_after, 3_000_000);
    assert!(has_fee_journal);
    assert_eq!(it_a.type_id, expected_dc_a);
    assert_eq!(it_a.quantity, 3);
    assert_eq!(it_a.location_id, l4_sta);
    assert_eq!(it_a.flag, InventoryFlag::Hangar);

    ctx.db.clock().advance_secs(5 * 86_400);
    dock_at_station(&ctx, &mut sess, char_id, other_field_agent.solar_system_id.unwrap(), other_field_agent.station_id);
    let buy_b = kv_map(&do_buy_datacores(&ctx, &mut sess, other_field_agent.agent_id, 1).await.unwrap());
    assert_eq!(buy_b["datacoreTypeID"].as_i64(), Some(i64::from(expected_dc_b)));
    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

// Folded submodule: part2
#[allow(unused_imports, dead_code)]
mod part2 {
    use super::*;

// Integration tests for Batch 36 open `feat:pve.*` parity rows (part 2: Storyline offer counter).


use std::collections::{HashMap, VecDeque};

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn dock_at_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) {
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
        }
    });
}

async fn query_journal_missions(
    ctx: &ServerContext,
    s: &mut SessionState,
) -> Vec<HashMap<String, PyRep>> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(s, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(tup) = rep else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(missions) = &tup[0] else {
        panic!("Expected missions list");
    };
    missions.iter().map(kv_map).collect()
}

async fn complete_live_courier(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    agent: &eve_sde::AgentInfo,
) {
    let kw = HashMap::new();
    dock_at_station(
        ctx,
        sess,
        char_id,
        agent.solar_system_id.unwrap(),
        agent.station_id,
    );
    let offer = kv_map(
        &ctx.dispatch_rpc(
            sess,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)],
            &kw,
            None,
        )
        .await
        .expect("Request courier mission"),
    );
    let mid = offer["missionID"].as_i64().unwrap();
    let dest_sta = offer["destinationStationID"].as_i64().unwrap();
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Accept courier mission");

    let tag = format!("courier_cargo:{char_id}:{mid}");
    ctx.db.with_mut_snapshot(|snap| {
        let cid = snap
            .item_custom_names
            .iter()
            .find(|(_, v)| *v == &tag)
            .map(|(&k, _)| k)
            .expect("granted courier cargo item");
        if let Some(it) = snap.items.get_mut(&cid) {
            it.location_id = ship_id;
            it.flag = InventoryFlag::Cargo;
        }
    });
    let dest_sys = ctx.sde.stations[&dest_sta].solar_system_id;
    dock_at_station(ctx, sess, char_id, dest_sys, Some(dest_sta));
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Complete courier mission");
}

async fn complete_live_fw_mission(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    fw_agent: &eve_sde::AgentInfo,
) {
    let kw = HashMap::new();
    let sys_id = fw_agent.solar_system_id.unwrap();
    dock_at_station(ctx, sess, char_id, sys_id, fw_agent.station_id);
    let offer = kv_map(
        &ctx.dispatch_rpc(
            sess,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(fw_agent.agent_id)],
            &kw,
            None,
        )
        .await
        .expect("Request FW mission"),
    );
    let mid = offer["missionID"].as_i64().unwrap();
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Accept FW mission");
    let (m_sys_id, npc_ids) = ctx.db.with_snapshot(|s| {
        let m = &s.active_missions[&(char_id, mid as u32)];
        (m.solar_system_id, m.spawned_npc_ball_ids.clone())
    });

    dock_at_station(ctx, sess, char_id, m_sys_id, None);
    let sys_h = ctx.systems.get_or_spawn(m_sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 639,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;
    for nid in npc_ids {
        let _ = sys_h.set_ball_position(nid, DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(nid)], &kw, None)
            .await
            .expect("AddTarget FW mission NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == nid && l.locked) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(
            sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(820_002), PyRep::Integer(nid)],
            &kw,
            None,
        )
        .await
        .expect("Fire weapon at FW mission NPC");
        for _ in 0..180 {
            if sys_h.get_ball_snapshot(nid).await.unwrap().is_none() {
                break;
            }
            let _ = sys_h.set_ball_position(nid, DVec3::new(1_000.0, 0.0, 0.0)).await;
            let _ = sys_h.step_manual(1.0).await;
        }
        assert!(
            sys_h.get_ball_snapshot(nid).await.unwrap().is_none(),
            "FW mission NPC {nid} must be destroyed"
        );
        let _ = sys_h.remove_module_cycle(ship_id, 820_002).await;
    }
    dock_at_station(ctx, sess, char_id, sys_id, fw_agent.station_id);
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Complete FW mission");
}

#[tokio::test]
async fn test_b36_storyline_offer_counter() {
    // parity: feat:pve.storyline-offer-counter
    eve_testkit::attest(&["feat:pve.storyline-offer-counter"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 820_001_i64;
    let weapon_id = 820_002_i64;
    let ammo_id = 820_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.active_ship_id = ship_id;
            ch.standings.insert(500_001, 6.0);
            ch.standings.insert(1_000_148, 5.0);
            ch.standings.insert(1_000_182, 5.0);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 639,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            weapon_id,
            InventoryItemRecord {
                item_id: weapon_id,
                type_id: 2929,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 17670,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.weapon_banks
            .entry(ship_id)
            .or_default()
            .insert(weapon_id, vec![weapon_id; 8]);
        snap.module_online_states.insert(weapon_id, true);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 20327, 21667] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 1_280_000.0,
                },
            );
        }
        for tid in [639_u32, 2929, 17670] {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [182_u16, 183, 184] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let mut sess = SessionState::new(3602);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);

    let count_storyline_offers = |missions: &[HashMap<String, PyRep>]| -> Vec<(i64, i64, i64)> {
        missions
            .iter()
            .filter(|m| {
                m.get("state").and_then(|v| v.as_str()) == Some("Offered")
                    && m.get("agentID")
                        .and_then(|v| v.as_i64())
                        .and_then(|aid| ctx.sde.agents.get(&aid))
                        .is_some_and(|a| matches!(a.agent_type_id, 6 | 7))
            })
            .map(|m| {
                (
                    m["agentID"].as_i64().unwrap(),
                    m["missionID"].as_i64().unwrap(),
                    m.get("level").and_then(|v| v.as_i64()).unwrap_or(0),
                )
            })
            .collect()
    };

    // Negative RPC check: completing an unaccepted mission is refused
    assert!(ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(999_999)], &kw, None)
        .await
        .is_err());

    // 1. Real SDE InterBus agent (3015955, corp 1000148): 16 live completions -> 0 storyline offers
    let interbus_agent = ctx.sde.agents[&3_015_955].clone();
    for _ in 0..16 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &interbus_agent).await;
    }
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    // 2. Real SDE FW militia agent (3019210, corp 1000182 Tribal Liberation Force): 16 live completions -> 0 storyline offers
    let fw_agent = ctx.sde.agents[&3_019_210].clone();
    dock_at_station(&ctx, &mut sess, char_id, fw_agent.solar_system_id.unwrap(), fw_agent.station_id);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(fw_agent.agent_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_002)], &kw, None)
        .await
        .expect("Enlist in Minmatar militia");
    for _ in 0..16 {
        complete_live_fw_mission(&ctx, &mut sess, char_id, ship_id, &fw_agent).await;
    }
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    // 2b. Tutorial (3), Data Center (5), COSMOS (8), and Career (12) agents:
    // 16 live completions across non-qualifying agent types -> 0 storyline offers and 0 tally!
    for (tid, expected) in [
        (2, true), (3, false), (4, true), (5, false), (6, false), (7, false),
        (8, false), (9, false), (10, true), (11, true), (12, false),
    ] {
        assert_eq!(eve_server::services::pve_ess_reserve_banks::is_storyline_qualifying_agent_type(tid), expected);
    }
    let tutorial_agent = ctx.sde.agents[&3_010_811].clone();
    assert_eq!(tutorial_agent.agent_type_id, 3);
    let career_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 12 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 Career agent")
        .clone();
    let cosmos_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 8 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 COSMOS agent")
        .clone();
    let dc_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 5 && a.level == 1 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 Data Center agent")
        .clone();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            for cid in [
                tutorial_agent.corporation_id,
                career_agent.corporation_id,
                cosmos_agent.corporation_id,
                dc_agent.corporation_id,
            ] {
                ch.standings.insert(cid, 6.0);
            }
        }
    });
    for (idx, non_qual_agent) in [
        &tutorial_agent, &tutorial_agent, &tutorial_agent, &tutorial_agent,
        &career_agent, &career_agent, &career_agent, &career_agent,
        &cosmos_agent, &cosmos_agent, &cosmos_agent, &cosmos_agent,
        &dc_agent, &dc_agent, &dc_agent, &dc_agent,
    ]
    .into_iter()
    .enumerate()
    {
        if non_qual_agent.agent_type_id == 3 {
            complete_live_courier(&ctx, &mut sess, char_id, ship_id, non_qual_agent).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.active_missions.retain(|_, m| m.agent_id != non_qual_agent.agent_id);
            });
        } else if non_qual_agent.agent_type_id == 5 {
            let dc_sys = non_qual_agent.solar_system_id.unwrap();
            let dc_sta = non_qual_agent.station_id.or_else(|| ctx.sde.solar_systems[&dc_sys].stations.first().map(|s| s.station_id)).unwrap_or(60_003_760);
            dock_at_station(&ctx, &mut sess, char_id, dc_sys, Some(dc_sta));
            let tag_iid = 825_000 + idx as i64;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(
                    tag_iid,
                    InventoryItemRecord {
                        item_id: tag_iid,
                        type_id: 17_202,
                        owner_id: char_id,
                        location_id: dc_sta,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: false,
                        ..Default::default()
                    },
                );
            });
            let off = kv_map(
                &ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(non_qual_agent.agent_id)], &kw, None)
                    .await
                    .expect("Offer Data Center mission"),
            );
            let mid = off["missionID"].as_i64().unwrap();
            ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)], &kw, None).await.unwrap();
            ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)], &kw, None).await.unwrap();
            ctx.db.with_mut_snapshot(|snap| {
                snap.active_missions.remove(&(char_id, mid as u32));
            });
        } else {
            complete_live_fw_mission(&ctx, &mut sess, char_id, ship_id, non_qual_agent).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.active_missions.retain(|_, m| m.agent_id != non_qual_agent.agent_id);
            });
        }
    }
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());
    assert_eq!(ctx.db.with_snapshot(|s| s.mission_tally.get(&char_id).copied().unwrap_or(0)), 0);

    // 3. Separate (faction, level) counters: 15 L3 + 1 L1 Caldari high-sec missions -> 0 offers; 16th L3 -> triggers L3 Storyline!
    let caldari_l3 = ctx.sde.agents[&3_008_423].clone();
    let caldari_l1 = ctx.sde.agents[&3_008_416].clone();
    for _ in 0..15 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    }
    complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l1).await;
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    let sl_offers_1 = count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await);
    assert_eq!(sl_offers_1.len(), 1);
    let (first_sl_aid, first_sl_mid, first_sl_lvl) = sl_offers_1[0];
    assert_eq!(first_sl_lvl, 3);

    let briefing_1 = kv_map(
        &ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMissionBriefingInfo", &[PyRep::Integer(first_sl_mid)], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(briefing_1["level"].as_i64(), Some(3));
    let sl_tmpl_diff = ctx
        .sde
        .missions
        .get(&(first_sl_mid as u32))
        .and_then(|t| t.dungeon_id)
        .and_then(|did| ctx.sde.dungeons.get(&did))
        .and_then(|d| d.difficulty);
    assert_eq!(sl_tmpl_diff, Some(3));

    let first_sl_info = &ctx.sde.agents[&first_sl_aid];
    assert!(ctx.sde.solar_systems[&first_sl_info.solar_system_id.unwrap()].security_status >= 0.45);

    let origin_sys = caldari_l3.solar_system_id.unwrap();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in ctx.sde.jumps.values() {
        adj.entry(j.from_solar_system_id)
            .or_default()
            .push(j.to_solar_system_id);
    }
    let mut dists = HashMap::from([(origin_sys, 0_u32)]);
    let mut q = VecDeque::from([origin_sys]);
    for _ in 0..ctx.sde.solar_systems.len().max(1) {
        let Some(u) = q.pop_front() else {
            break;
        };
        let nd = dists[&u] + 1;
        if let Some(nbs) = adj.get(&u) {
            for &v in nbs {
                if !dists.contains_key(&v) {
                    dists.insert(v, nd);
                    q.push_back(v);
                }
            }
        }
    }
    let min_jumps = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            matches!(a.agent_type_id, 6 | 7)
                && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001)
                && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).is_some_and(|s| s.security_status >= 0.45)
        })
        .filter_map(|a| dists.get(&a.solar_system_id?))
        .copied()
        .min()
        .unwrap();
    assert_eq!(dists[&first_sl_info.solar_system_id.unwrap()], min_jumps);

    // 4. With nearest storyline agent's offer still pending, 32nd L3 completion picks second-nearest high-sec Storyline agent!
    for _ in 0..16 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    }
    let sl_offers_2 = count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await);
    assert_eq!(sl_offers_2.len(), 2);
    let second_sl_aid = sl_offers_2
        .iter()
        .map(|&(aid, _, _)| aid)
        .find(|&aid| aid != first_sl_aid)
        .expect("second-nearest storyline agent");
    assert!(ctx.sde.solar_systems[&ctx.sde.agents[&second_sl_aid].solar_system_id.unwrap()].security_status >= 0.45);

    // 5. Completing a Storyline agent mission (6 | 7) does NOT increment the storyline counter!
    let tally_pre_sl = ctx.db.with_snapshot(|s| s.mission_tally[&char_id]);
    let sl_sys = first_sl_info.solar_system_id.unwrap();
    let sl_sta = first_sl_info.station_id.unwrap();
    dock_at_station(&ctx, &mut sess, char_id, sl_sys, Some(sl_sta));
    let sl_acc = kv_map(
        &ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(first_sl_mid)], &kw, None)
            .await
            .expect("Accept Storyline mission"),
    );
    let sl_npcs = ctx.db.with_snapshot(|s| s.active_missions[&(char_id, first_sl_mid as u32)].spawned_npc_ball_ids.clone());
    if let Some(dest_sta) = briefing_1.get("destinationStationID").and_then(|v| v.as_i64()) {
        let courier_tag = format!("courier_cargo:{char_id}:{first_sl_mid}");
        let cargo_iid = ctx.db.with_snapshot(|s| {
            s.item_custom_names.iter().find_map(|(&iid, tag)| (tag == &courier_tag).then_some(iid)).unwrap()
        });
        ctx.db.with_mut_snapshot(|snap| {
            let it = snap.items.get_mut(&cargo_iid).unwrap();
            it.location_id = dest_sta;
            it.flag = InventoryFlag::Hangar;
        });
        let dest_sys = ctx.sde.stations[&dest_sta].solar_system_id;
        dock_at_station(&ctx, &mut sess, char_id, dest_sys, Some(dest_sta));
    } else if !sl_npcs.is_empty() {
        dock_at_station(&ctx, &mut sess, char_id, sl_sys, None);
        let sys_h = ctx.systems.get_or_spawn(sl_sys);
        let _ = sys_h.spawn_ball_with_combat(Ball { id: ship_id, type_id: 639, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 100_000_000.0, radius: 250.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(639)).await;
        for nid in sl_npcs {
            let _ = sys_h.set_ball_position(nid, DVec3::new(1_000.0, 0.0, 0.0)).await;
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(nid)], &kw, None).await.unwrap();
            for _ in 0..30 {
                if sys_h.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == nid && l.locked) { break; }
                let _ = sys_h.step_manual(1.0).await;
            }
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(weapon_id), PyRep::Integer(nid)], &kw, None).await.unwrap();
            for _ in 0..180 {
                if sys_h.get_ball_snapshot(nid).await.unwrap().is_none() { break; }
                let _ = sys_h.set_ball_position(nid, DVec3::new(1_000.0, 0.0, 0.0)).await;
                let _ = sys_h.step_manual(1.0).await;
            }
            let _ = sys_h.remove_module_cycle(ship_id, weapon_id).await;
        }
        dock_at_station(&ctx, &mut sess, char_id, sl_sys, Some(sl_sta));
    } else if let Some(req_tid) = sl_acc.get("requiredTypeID").and_then(|v| v.as_i64()).or_else(|| briefing_1.get("requiredTypeID").and_then(|v| v.as_i64())) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(826_500, InventoryItemRecord { item_id: 826_500, type_id: req_tid as u32, owner_id: char_id, location_id: sl_sta, flag: InventoryFlag::Hangar, quantity: 100_000, singleton: false, ..Default::default() });
        });
    }
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(first_sl_mid)], &kw, None).await.expect("Complete Storyline mission");
    assert_eq!(ctx.db.with_snapshot(|s| s.mission_tally[&char_id]), tally_pre_sl);

    // 6. Research (4) + Epic Arc (10) + Basic (2) all increment counter & trigger L1 Caldari Storyline on 16th L1 completion!
    let rnd_l1 = ctx.sde.agents[&3_011_105].clone();
    let rnd_skill = rnd_l1.research_skills.iter().copied().find(|&sk| {
        ctx.sde.types.values().any(|t| t.group_id == 333 && t.published && t.default_attributes.get(&182).is_some_and(|&r| r as u32 == sk))
    }).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3402_u32, 3392, rnd_skill] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
        snap.items.insert(826_600, InventoryItemRecord { item_id: 826_600, type_id: 34, owner_id: char_id, location_id: rnd_l1.station_id.unwrap(), flag: InventoryFlag::Hangar, quantity: 50_000, singleton: false, ..Default::default() });
    });
    dock_at_station(&ctx, &mut sess, char_id, rnd_l1.solar_system_id.unwrap(), rnd_l1.station_id);
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd_l1.agent_id), PyRep::Integer(i64::from(rnd_skill))], &kw, None).await.unwrap();
    let rnd_mid = kv_map(&ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_l1.agent_id)], &kw, None).await.unwrap())["missionID"].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(rnd_mid)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(rnd_mid)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.mission_tally[&char_id]), tally_pre_sl + 1);

    let epic_l1 = ctx.sde.agents[&3_019_359].clone();
    assert_eq!(epic_l1.agent_type_id, 10);
    complete_live_fw_mission(&ctx, &mut sess, char_id, ship_id, &epic_l1).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.mission_tally[&char_id]), tally_pre_sl + 2);

    // 1 earlier Basic + 1 R&D + 1 Epic Arc = 3 L1 Caldari completions; 12 more Basic = 15 (no L1 offer); 16th -> L1 Storyline!
    for _ in 0..12 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l1).await;
    }
    assert!(!count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).iter().any(|&(_, _, lvl)| lvl == 1));
    complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l1).await;
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).iter().any(|&(_, _, lvl)| lvl == 1));
}

}

// Folded submodule: part3
#[allow(unused_imports, dead_code)]
mod part3 {
    use super::*;

// Integration tests for Batch 36 open `feat:pve.*` parity rows (part 3: Abyssal activation & Deadspace pockets).


use std::collections::HashMap;

use eve_db::{
    BookmarkRecord, FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const FIL_T1_ELEC: u32 = 47_765;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn step_until_stopped(ctx: &ServerContext, sys_id: u32, ship_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_h.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_b36_abyssal_activation_and_deadspace_pocket_rules() {
    // parity: feat:pve.abyssal-activation-restrictions
    eve_testkit::attest(&["feat:pve.abyssal-activation-restrictions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 950_001_i64;
    let filament_id = 950_002_i64;

    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
                && !s.stations.is_empty()
                && ctx
                    .sde
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == s.solar_system_id)
        })
        .expect("low-sec system with station and stargate")
        .solar_system_id;
    let sta_pos = ctx.sde.solar_systems[&low_sys].stations[0].position;
    let gate_pos = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == low_sys)
        .unwrap()
        .position;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = low_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 621, char_id, i64::from(low_sys), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            filament_id,
            make_item(filament_id, FIL_T1_ELEC, char_id, ship_id, InventoryFlag::Cargo, 10, false),
        );
    });

    let mut sess = SessionState::new(3611);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(low_sys);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: sta_pos + DVec3::new(400_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(621),
        )
        .await;

    // 1. Within 1,000 km of NPC station and stargate -> ProximityRestriction
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    let _ = sys_h.set_ball_position(ship_id, gate_pos + DVec3::new(400_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    // 2. Warp via live CmdWarpToStuff to open space -> post-warp invulnerability blocks filament (ShipInvulnerable)
    let open_pos = DVec3::new(8.0e10, 8.0e10, 8.0e10);
    let bm_open = 950_090_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_open,
            BookmarkRecord {
                bookmark_id: bm_open,
                owner_id: char_id,
                label: "Deep Safe".into(),
                solar_system_id: low_sys,
                x: open_pos.x,
                y: open_pos.y,
                z: open_pos.z,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(bm_open)],
        &kw,
        None,
    )
    .await
    .expect("Warp to deep safe");
    step_until_stopped(&ctx, low_sys, ship_id).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInvulnerable"
    );
    for _ in 0..35 {
        let _ = sys_h.step_manual(1.0).await;
    }

    // 3. Within 1,000 km of a Mobile Depot -> ProximityRestriction
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: 950_010,
                type_id: 33_474,
                position: open_pos + DVec3::new(500_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(33_474),
        )
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    sys_h.try_remove_ball(950_010);

    // 3b. Within 1,000 km of an Upwell structure in snap.structures -> ProximityRestriction
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            950_020,
            UpwellStructureRecord {
                structure_id: 950_020,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: low_sys,
                name: "Test Astrahus".into(),
                position: (open_pos + DVec3::new(500_000.0, 0.0, 0.0)).to_array(),
                state: "ShieldVulnerable".into(),
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                fuel_blocks: 5_000,
                quantum_core_installed: true,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&950_020);
    });

    // 4. Safe-logoff timer -> LogoffTimerActive; 15m capsuleer PvP log-off timer -> PvpTimerActive
    ctx.advance_secs(301);
    ctx.dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None)
        .await
        .expect("SafeLogoff");
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "LogoffTimerActive"
    );
    ctx.dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &kw, None)
        .await
        .expect("AbortSafeLogoff");

    let gun_caracal = 950_003_i64;
    let enemy_ship = 950_090_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_caracal, make_item(gun_caracal, 2881, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(950_004, make_item(950_004, 21898, char_id, ship_id, InventoryFlag::HiSlot0, 500, false));
        snap.module_online_states.insert(gun_caracal, true);
        snap.items.insert(enemy_ship, make_item(enemy_ship, 587, 90_000_002, i64::from(low_sys), InventoryFlag::None, 1, true));
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3311, 3413, 3426] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: enemy_ship,
                type_id: 587,
                position: open_pos + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(90_000_002, enemy_ship, 587),
            ctx.build_ship_slim_meta(90_000_002, 587),
        )
        .await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(enemy_ship)], &kw, None).await.unwrap();
    for _ in 0..30 {
        if sys_h.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == enemy_ship && l.locked) {
            break;
        }
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_caracal), PyRep::Integer(enemy_ship)], &kw, None)
        .await
        .expect("Live PvP aggression shot");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gun_caracal)], &kw, None).await;
    sys_h.try_remove_ball(enemy_ship);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "PvpTimerActive"
    );
    ctx.advance_secs(901);

    // 5. Deploy 1st Abyssal Trace in open space (succeeds), then < 1,000 km from trace -> ProximityRestriction,
    // and activate trace gate via AbyssalEntranceGateActivation to enter Abyssal Deadspace (32_000_000..=32_999_999)
    let dep = kv_map(
        &ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .expect("Open space AbyssalEntranceDeployment"),
    );
    let trace_ball_id = dep["traceBallID"].as_i64().expect("traceBallID");
    let _ = sys_h.set_ball_position(ship_id, open_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(filament_id), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    let _ = sys_h.set_ball_position(ship_id, open_pos).await;
    ctx.dispatch_rpc(
        &mut sess,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation enters Abyssal space");
    assert!(
        (32_000_000..=32_999_999).contains(&sess.solar_system_id.unwrap()),
        "Ship must transition into Abyssal solar system"
    );

    // 6. Real SDE-seeded Deadspace complex (DSG-30000142): acceleration gate entry, warp block, MWD SDE rule, and ShipInDungeon!
    let jita_sys = 30_000_142_u32;
    let rifter_id = 950_101_i64;
    let mwd_id = 950_102_i64;
    let ab_id = 950_103_i64;
    let fil_rifter = 950_104_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.active_ship_id = rifter_id;
        }
        snap.fleets.insert(
            701,
            FleetRecord {
                fleet_id: 701,
                boss_id: char_id,
                members: vec![char_id],
                motd: String::new(),
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        snap.items.insert(rifter_id, make_item(rifter_id, 587, char_id, i64::from(jita_sys), InventoryFlag::None, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 434, char_id, rifter_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, 439, char_id, rifter_id, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(950_105, make_item(950_105, 2881, char_id, rifter_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(950_106, make_item(950_106, 21898, char_id, rifter_id, InventoryFlag::HiSlot0, 2_000, false));
        snap.items.insert(fil_rifter, make_item(fil_rifter, FIL_T1_ELEC, char_id, rifter_id, InventoryFlag::Cargo, 6, false));
        snap.items.insert(950_107, make_item(950_107, 50_008, char_id, rifter_id, InventoryFlag::Cargo, 2, false));
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(ab_id, false);
        snap.module_online_states.insert(950_105, true);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3311, 3413, 3426, 3449, 3450, 3454, 3455] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(rifter_id);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(rifter_id);

    let jita_h = ctx.systems.get_or_spawn(jita_sys);
    let _ = jita_h
        .spawn_ball_with_combat(
            Ball {
                id: rifter_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 2.5,
                mass: 1_067_000.0,
                radius: 38.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;

    let agency_rep = ctx
        .dispatch_rpc(&mut sess, "custom_agency_provider", None, "get_content_data", &[], &kw, None)
        .await
        .expect("custom_agency_provider.get_content_data");
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSignatures", &[], &kw, None)
        .await
        .expect("scanMgr.GetSignatures");
    let PyRep::List(pieces) = agency_rep else { panic!() };
    let dsg_did = pieces
        .iter()
        .find(|p| p.dict_get("contentType").and_then(|v| v.as_str()) == Some("signature"))
        .and_then(|p| p.dict_get("dungeonID").and_then(|v| v.as_i64()))
        .expect("Deadspace signature dungeonID");
    let dsg_pos = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures[&jita_sys]
            .iter()
            .find(|sig| sig.signature_id == "DSG-30000142")
            .unwrap()
            .position
    });
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
        .await
        .unwrap();
    step_until_stopped(&ctx, jita_sys, rifter_id).await;
    for _ in 0..35 {
        let _ = jita_h.step_manual(1.0).await;
    }

    let entry_gate_id = jita_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .find(|b| {
            b.id != rifter_id
                && ctx.sde.types.get(&b.type_id).is_some_and(|t| t.group_id == 366)
                && b.position.distance(DVec3::from_array(dsg_pos)) < 50_000.0
        })
        .expect("Materialized entry acceleration gate ball")
        .id;
    let room1_id = dsg_did * 100 + 1;
    let bm_r2 = 950_190_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_r2,
            BookmarkRecord {
                bookmark_id: bm_r2,
                owner_id: char_id,
                label: "Gated Room 2".into(),
                solar_system_id: jita_sys,
                x: dsg_pos[0] + 1_200_000.0,
                y: dsg_pos[1],
                z: dsg_pos[2],
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let warp_r1_args = [PyRep::String("dungeon".into()), PyRep::Integer(room1_id)];
    let err_r1 = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r1_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_r1.code, "NaturalPhenomenaDisruptingWarp");

    let warp_r2_args = [PyRep::String("bookmark".into()), PyRep::Integer(bm_r2)];
    let err_r2 = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r2_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_r2.code, "NaturalPhenomenaDisruptingWarp");

    let g1 = kv_map(
        &ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
            .await
            .expect("Enter Room 1 via acceleration gate"),
    );
    let r1_dest = DVec3::new(g1["destinationX"].as_f64().unwrap(), g1["destinationY"].as_f64().unwrap(), g1["destinationZ"].as_f64().unwrap());
    let landed_r1 = jita_h.get_ball_snapshot(rifter_id).await.unwrap().unwrap().1.position;
    assert!(landed_r1.distance(r1_dest) < 1.0);

    // Inside deadspace pocket Room 1 -> both Abyssal and random jump filaments refused with ShipInDungeon!
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_rifter), PyRep::Integer(i64::from(FIL_T1_ELEC))], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(950_107)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );

    // Cross-pocket warp from Room 1 to Room 2 of the same deadspace complex -> NaturalPhenomenaDisruptingWarp!
    let err_cross = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &warp_r2_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_cross.code, "NaturalPhenomenaDisruptingWarp");

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleNotAllowedInDeadspace"
    );

    let PyRep::List(r1_npcs) = &g1["npcBallIDs"] else { panic!() };
    for nid in r1_npcs {
        let npc_bid = nid.as_i64().unwrap();
        let npc_pos = jita_h.get_ball_snapshot(npc_bid).await.unwrap().unwrap().1.position;
        let _ = jita_h.set_ball_position(rifter_id, npc_pos - DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_bid)], &kw, None).await.unwrap();
        for _ in 0..30 {
            if jita_h.get_target_locks(rifter_id).await.unwrap().iter().any(|l| l.target_id == npc_bid && l.locked) {
                break;
            }
            let _ = jita_h.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(950_105), PyRep::Integer(npc_bid)], &kw, None).await.unwrap();
        for _ in 0..60 {
            if jita_h.get_ball_snapshot(npc_bid).await.unwrap().is_none() {
                break;
            }
            let _ = jita_h.step_manual(1.0).await;
        }
        assert!(jita_h.get_ball_snapshot(npc_bid).await.unwrap().is_none());
    }
    let r1_gate_id = g1["nextGateBallID"].as_i64().unwrap();
    let r1_gate_pos = jita_h.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = jita_h.set_ball_position(rifter_id, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Enter gated Room 2");

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleNotAllowedInDeadspace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(rifter_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner allowed in MWD-restricted deadspace pocket");
}

}

// Folded submodule: part4
#[allow(unused_imports, dead_code)]
mod part4 {
    use super::*;

// Integration tests for Batch 36 open `feat:pve.*` parity rows (part 4: ESS grid warp & module restrictions).


use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn step_until_stopped(ctx: &ServerContext, sys_id: u32, ship_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_h.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) && b.velocity.length() < 0.5 {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_b36_ess_grid_warp_and_module_restrictions() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions
    eve_testkit::attest(&["feat:pve.ess-grid-warp-mwd-restrictions"]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status <= -0.1
                && s.solar_system_id < 31_000_000
                && ctx
                    .sde
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let stargate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .unwrap()
        .stargate_id as i64;

    let pilot_a = 100_001_i64;
    let ship_a = 960_001_i64;
    let frig_a = 960_003_i64;
    let mwd_id = 960_010_i64;
    let ab_id = 960_011_i64;
    let mjd_id = 960_012_i64;
    let cloak_id = 960_013_i64;
    let cyno_id = 960_014_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ca = snap.characters.values().next().cloned().expect("base char");
        ca.character_id = pilot_a;
        ca.user_id = 1;
        ca.solar_system_id = sys_id;
        ca.station_id = None;
        ca.active_ship_id = ship_a;
        ca.balance = IskCents(0);
        snap.characters.insert(pilot_a, ca);

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_001);
        st.dbs_multiplier = 1.35;

        snap.items.insert(ship_a, make_item(ship_a, 639, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(frig_a, make_item(frig_a, 587, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 12052, pilot_a, ship_a, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, 12056, pilot_a, ship_a, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(mjd_id, make_item(mjd_id, 33915, pilot_a, ship_a, InventoryFlag::MedSlot2, 1, true));
        snap.items.insert(cloak_id, make_item(cloak_id, 11370, pilot_a, ship_a, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(cyno_id, make_item(cyno_id, 21096, pilot_a, ship_a, InventoryFlag::HiSlot2, 1, true));
        snap.items.insert(960_019, make_item(960_019, 16273, pilot_a, ship_a, InventoryFlag::Cargo, 2_000, false));
        for mid in [mwd_id, mjd_id, cloak_id, cyno_id] {
            snap.module_online_states.insert(mid, true);
        }
        snap.module_online_states.insert(ab_id, false);
        let sk = snap.skills.entry(pilot_a).or_default();
        for sid in [3318_u32, 3413, 3418, 3426, 3432, 3449, 3450, 3454, 3455, 4385, 11579, 21603] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
        for tid in [639_u32, 12052, 12056, 33915, 11370, 21096] {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [182_u16, 183, 184] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let mut sess_a = SessionState::new(3612);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    // 1. ESS intrusion site & ballpark acceleration gate (V24 0-arg essMgr.GetDataForClientSolarSystem)
    let ess_info = kv_map(
        &ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert!(!ess_info.contains_key("essGateID"));
    let ess_id = ess_info["essID"].as_i64().unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ess_gate_ball = sys_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.type_id == 17_831)
        .expect("ESS acceleration gate ball in system ballpark");
    let ess_gate_id = ess_gate_ball.id;
    let gate_pos = ess_gate_ball.position;

    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 639,
                position: gate_pos + DVec3::new(300_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.12,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;

    // Outside 500 km no-cloak & 75 km warp bubble: MWD allowed; direct warp onto ESS grid blocked; warp to exterior ESS gate allowed!
    let ess_pos = sys_h.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(550_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("MWD allowed beyond 75 km");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None).await;

    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Warp to exterior ESS gate");
    step_until_stopped(&ctx, sys_id, ship_a).await;
    let gate_standoff = gate_pos + DVec3::new(500.0, 0.0, 0.0);
    let _ = sys_h.set_ball_position(ship_a, gate_standoff).await;
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(ship_a, gate_standoff.to_array());
    });

    // At ESS gate (200 km from ESS beacon, within 500 km no-cloak zone): Cloak blocked with EssIntrusionBubbleCloakBlocked!
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );

    // Hull class eligibility: Frigate (587), Marauder (28659), Mining Barge (17478), Industrial (648),
    // non-Tactical Destroyer (16242), Black Ops (22428), and Capital (23911) rejected; T3D (34317) admitted!
    for (bad_sid, bad_tid) in [(frig_a, 587_u32), (960_004, 28_659), (960_005, 17_478), (960_006, 648), (960_008, 16_242), (960_009, 22_428), (960_029, 23_911)] {
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(bad_sid, make_item(bad_sid, bad_tid, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
            s.characters.get_mut(&pilot_a).unwrap().active_ship_id = bad_sid;
        });
        let _ = sys_h.spawn_ball_with_combat(Ball { id: bad_sid, type_id: bad_tid, position: gate_standoff, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(bad_tid)).await;
        sess_a.ship_id = Some(bad_sid);
        assert_eq!(ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None).await.unwrap_err().code, "ShipRestrictedByEssGate");
    }
    let t3d_id = 960_007_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(t3d_id, make_item(t3d_id, 34_317, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        s.characters.get_mut(&pilot_a).unwrap().active_ship_id = t3d_id;
    });
    let _ = sys_h.spawn_ball_with_combat(Ball { id: t3d_id, type_id: 34_317, position: gate_standoff, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 2_000_000.0, radius: 65.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(34_317)).await;
    sess_a.ship_id = Some(t3d_id);
    ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None).await.expect("Tactical Destroyer (group 1305) admitted by ESS gate");

    // Warp-scrambled ship rejected; unscrambled ship initiates warp WITHOUT prematurely teleporting snap.item_positions!
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_a).unwrap().active_ship_id = ship_a);
    sess_a.ship_id = Some(ship_a);
    let _ = sys_h.set_ball_position(ship_a, gate_standoff).await;
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_a).await {
        cs.warp_scramble_strength = 2;
        let _ = sys_h.set_combat_state(ship_a, cs).await;
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_a).await {
        cs.warp_scramble_strength = 0;
        cs.is_cloaked = true;
        let _ = sys_h.set_combat_state(ship_a, cs).await;
    }
    ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Cruiser/Battleship enters ESS gate warp slide and decloaks");
    assert!(!sys_h.get_combat_state(ship_a).await.unwrap().unwrap().is_cloaked);

    // Pre-landing invariant (`B36-F06`): while warp slide is in flight, persisted DB position is still at exterior gate!
    let pre_land_db_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.item_positions[&ship_a]));
    assert!(
        pre_land_db_pos.distance(ess_pos) > 150_000.0,
        "DB item_positions must NOT prematurely teleport to ESS landing grid before warp completes: {pre_land_db_pos:?}"
    );
    assert!(matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // Post-landing invariant (`B36-F06`): once SystemActor finishes warp slide, DB item_positions updates to landing grid!
    step_until_stopped(&ctx, sys_id, ship_a).await;
    let post_land_db_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.item_positions[&ship_a]));
    assert!(
        post_land_db_pos.distance(ess_pos) < 5_000.0,
        "SystemActor must persist landed ship position inside ESS grid upon warp completion: {post_land_db_pos:?}"
    );

    // 2. Inside 75 km ESS bubble: Warp, MWD (T1 + meta/storyline), MJD, Cloak, Cyno, and Travel/Abyssal Filaments ALL refused; Afterburner allowed!
    let storyline_mwd_id = 960_020_i64;
    let needlejack_id = 960_021_i64;
    let abyssal_fil_id = 960_022_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(storyline_mwd_id, make_item(storyline_mwd_id, 5_975, pilot_a, ship_a, InventoryFlag::MedSlot3, 1, true));
        snap.items.insert(needlejack_id, make_item(needlejack_id, 50_008, pilot_a, ship_a, InventoryFlag::Cargo, 2, false));
        snap.items.insert(abyssal_fil_id, make_item(abyssal_fil_id, 47_765, pilot_a, ship_a, InventoryFlag::Cargo, 2, false));
        snap.module_online_states.insert(storyline_mwd_id, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(stargate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(storyline_mwd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(storyline_mwd_id), PyRep::String("online".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(storyline_mwd_id, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(needlejack_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    let cruiser_ess_id = 960_023_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cruiser_ess_id, make_item(cruiser_ess_id, 621, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.get_mut(&abyssal_fil_id).unwrap().location_id = cruiser_ess_id;
        snap.characters.get_mut(&pilot_a).unwrap().active_ship_id = cruiser_ess_id;
    });
    let _ = sys_h.spawn_ball_with_combat(Ball { id: cruiser_ess_id, type_id: 621, position: ess_pos + DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 10_000_000.0, radius: 120.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(621)).await;
    sess_a.ship_id = Some(cruiser_ess_id);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(abyssal_fil_id), PyRep::Integer(47_765)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&pilot_a).unwrap().active_ship_id = ship_a);
    sess_a.ship_id = Some(ship_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMjdBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_id), PyRep::String("cynosuralGeneration".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCynoBlocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner allowed inside 75 km ESS bubble");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into())], &kw, None).await;

    // 3. `F-04` regression: FleetWarp / WarpFleet enforces the 75 km ESS bubble for destinations and fleet members!
    let pilot_b = 100_002_i64;
    let ship_b = 960_040_i64; // Rifter Frigate (587) outside ESS bubble at 400 km
    ctx.db.with_mut_snapshot(|snap| {
        let mut cb = snap.characters[&pilot_a].clone();
        cb.character_id = pilot_b;
        cb.user_id = 2;
        cb.active_ship_id = ship_b;
        snap.characters.insert(pilot_b, cb);
        snap.items.insert(ship_b, make_item(ship_b, 587, pilot_b, i64::from(sys_id), InventoryFlag::None, 1, true));
    });
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_b,
                type_id: 587,
                position: ess_pos + DVec3::new(400_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let mut sess_b = SessionState::new(3699);
    crate::common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess_b, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(pilot_b)], &kw, None)
        .await
        .expect("SelectCharacterID pilot_b");
    ctx.db.clock().advance_secs(15);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.station_id = None;
    sess_b.ship_id = Some(ship_b);

    let cf = ctx
        .dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    let fid = cf.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or_else(|| sess_a.fleet_id.unwrap());
    ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(pilot_b), PyRep::None, PyRep::None, PyRep::Integer(4)], &kw, None)
        .await
        .expect("Invite pilot_b");
    let fb_b = ctx.bind_object(&mut sess_b, "fleetObjectHandler", fid).unwrap();
    ctx.dispatch_rpc(&mut sess_b, "fleetObjectHandler", Some(fb_b), "AcceptInvite", &[PyRep::Integer(fid)], &kw, None)
        .await
        .expect("AcceptInvite pilot_b");

    // (a) FleetWarp onto the ESS beacon / ESS grid is rejected with EssIntrusionBubbleWarpBlocked (never warps Frigate ship_b onto ESS grid!)
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(ess_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_b).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // (b) When ALL fleet ships are inside the 75 km ESS bubble, FleetWarp out to stargate fails with EssIntrusionBubbleWarpBlocked!
    let _ = sys_h.set_ball_position(ship_b, ess_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(stargate_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    // (c) Mixed fleet: FC ship_a inside 75 km ESS bubble + squadmate ship_b outside (400 km) ->
    // FleetWarp to exterior stargate skips ESS-bubbled ship_a and warps ONLY unbubbled ship_b!
    let _ = sys_h.set_ball_position(ship_b, ess_pos + DVec3::new(400_000.0, 0.0, 0.0)).await;
    let fw_mixed = ctx
        .dispatch_rpc(
            &mut sess_a,
            "fleetObjectHandler",
            None,
            "FleetWarp",
            &[PyRep::Integer(stargate_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("Mixed FleetWarp skips ESS-bubbled ship_a and warps exterior ship_b");
    assert_eq!(fw_mixed.dict_get("warpCount"), Some(&PyRep::Integer(1)));
    assert!(!matches!(
        sys_h.get_ball_snapshot(ship_a).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));
    assert!(matches!(
        sys_h.get_ball_snapshot(ship_b).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));
}

}

// Folded submodule: part5
#[allow(unused_imports, dead_code)]
mod part5 {
    use super::*;

// Integration tests for Batch 36 open `feat:pve.*` parity rows (part 5: ESS Main Bank autopayout,
// 1 Hz ballpark NPC ESS pooling, & SQLite-durable server-tick Reserve Bank key tiers).


use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, VirtualServerClock};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn count_char_event(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_b36_ess_main_autopayout_and_reserve_bank_key_tiers() {
    // parity: feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    eve_testkit::attest(&[
        "feat:pve.ess-main-bank-autopayout",
        "feat:pve.ess-reserve-bank-key-tiers",
    ]);
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_014_400)));
    let kw = HashMap::new();

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= -0.1
                && s.solar_system_id < 31_000_000
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 9)
        })
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let pilot_c = 100_003_i64;
    let ship_a = 965_001_i64;
    let ship_b = 965_002_i64;
    let ship_c = 965_003_i64;
    let wep_a = 965_015_i64;
    let ammo_a = 965_016_i64;
    let wep_b = 965_017_i64;
    let ammo_b = 965_018_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("base char");
        for (pid, uid, sid) in [(pilot_a, 1_u64, ship_a), (pilot_b, 2, ship_b), (pilot_c, 3, ship_c)] {
            let mut ch = base.clone();
            ch.character_id = pid;
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = sid;
            ch.balance = IskCents(0);
            snap.characters.insert(pid, ch);
            snap.items.insert(sid, make_item(sid, 639, pid, i64::from(sys_id), InventoryFlag::None, 1, true));
        }

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_001);
        st.dbs_multiplier = 1.35;
        st.ess_main_bank = IskCents(0);
        st.ess_reserve_bank = IskCents(0);

        snap.items.insert(wep_a, make_item(wep_a, 2961, pilot_a, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_a, make_item(ammo_a, 21894, pilot_a, ship_a, InventoryFlag::HiSlot0, 5_000, false));
        snap.items.insert(wep_b, make_item(wep_b, 2961, pilot_b, ship_b, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_b, make_item(ammo_b, 21894, pilot_b, ship_b, InventoryFlag::HiSlot0, 5_000, false));
        snap.module_online_states.insert(wep_a, true);
        snap.module_online_states.insert(wep_b, true);
        for pid in [pilot_a, pilot_b] {
            let sk = snap.skills.entry(pid).or_default();
            for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 20327, 21667] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
            }
            for tid in [639_u32, 2961, 21894] {
                if let Some(t) = ctx.sde.types.get(&tid) {
                    for sa in [182_u16, 183, 184] {
                        if let Some(&req_f) = t.default_attributes.get(&sa) {
                            let rsid = req_f as u32;
                            if rsid > 0 {
                                sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                            }
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);

    let mut sess_a = SessionState::new(3614);
    crate::common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess_a,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(pilot_a)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot_a");
    let _ = ctx.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    let mut sess_b = SessionState::new(3615);
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(pilot_b);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.ship_id = Some(ship_b);

    let ess_info = kv_map(
        &ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    let ess_id = ess_info["essID"].as_i64().unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ess_pos = sys_h.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let rat_spot = DVec3::new(-5.0e9, 0.0, 0.0);

    for (sid, pos) in [
        (ship_a, ess_pos + DVec3::new(1_500.0, 0.0, 0.0)),
        (ship_b, rat_spot),
        (ship_c, rat_spot + DVec3::new(50_000.0, 0.0, 0.0)),
    ] {
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 639,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(639),
            )
            .await;
    }

    // 1a (`F-04` regression): 1 Hz `NpcSpawnManager::handle_npc_destroyed` in null-sec splits bounties into
    // `ess_main_bank` + `ess_reserve_bank` and records `pilot_c` in `ess_main_bank_contributors`!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let first_belt_rat = ctx.npc_spawns.read().unwrap()[&sys_id]
        .entities
        .values()
        .filter(|e| e.bounty_isk > 0.0)
        .min_by_key(|e| e.ball_id)
        .expect("spawned null-sec belt rat with positive bounty")
        .ball_id;
    let belt_rat_bounty = ctx
        .npc_spawns
        .write()
        .unwrap()
        .get_mut(&sys_id)
        .unwrap()
        .handle_npc_destroyed(&ctx, sys_id, first_belt_rat, pilot_c, ctx.now_unix())
        .expect("1 Hz handle_npc_destroyed in null-sec");
    assert!(belt_rat_bounty > 0.0);
    let (c_bal_direct, main_from_1hz, res_from_1hz, c_contrib) = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&sys_id];
        (
            s.characters[&pilot_c].balance.0,
            st.ess_main_bank.0,
            st.ess_reserve_bank.0,
            st.ess_main_bank_contributors.get(&pilot_c).copied().unwrap_or(0),
        )
    });
    assert!(main_from_1hz > 0 && res_from_1hz > 0, "1 Hz null-sec NPC kill must pool into ESS Main & Reserve Banks");
    assert_eq!(c_contrib, main_from_1hz, "1 Hz null-sec NPC kill must record contributor share in ess_main_bank_contributors");
    assert!(c_bal_direct > 0 && (c_bal_direct as f64 / 100.0) < belt_rat_bounty * 1.35);

    // 1b. ESS Main Bank 2h45m autopayout via live NPC bounty combat (pilot_a 2 kills : pilot_b 1 kill) + active-link deferral
    let bounty_npc_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 11
                && (100_000.0..=250_000.0)
                    .contains(&t.default_attributes.get(&481).copied().unwrap_or(0.0))
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap();

    for (idx, (is_a, npc_bid)) in [(true, 965_501_i64), (true, 965_502), (false, 965_503)].into_iter().enumerate() {
        let origin = if is_a { ess_pos + DVec3::new(1_500.0, 0.0, 0.0) } else { rat_spot };
        let mut cs = ctx.build_type_combat_state(bounty_npc_tid);
        cs.shield_hp = 10.0;
        cs.armor_hp = 10.0;
        cs.structure_hp = 10.0;
        cs.signature_radius = 400.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: npc_bid,
                    type_id: bounty_npc_tid,
                    position: origin + DVec3::new(5_000.0 + (idx as f64) * 100.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 100_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
        let s_ref = if is_a { &mut sess_a } else { &mut sess_b };
        let w_id = if is_a { wep_a } else { wep_b };
        let attacker_sid = if is_a { ship_a } else { ship_b };
        ctx.dispatch_rpc(s_ref, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_bid)], &kw, None)
            .await
            .expect("AddTarget bounty NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(attacker_sid).await.unwrap().iter().any(|l| l.target_id == npc_bid && l.locked) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        for _ in 0..20 {
            if sys_h.get_ball_snapshot(npc_bid).await.unwrap().is_none() {
                break;
            }
            let r = kv_map(
                &ctx.dispatch_rpc(s_ref, "dogmaIM", None, "Activate", &[PyRep::Integer(w_id), PyRep::Integer(npc_bid)], &kw, None)
                    .await
                    .unwrap(),
            );
            if r.get("destroyed").and_then(|v| v.as_bool()) == Some(true) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        assert!(sys_h.get_ball_snapshot(npc_bid).await.unwrap().is_none());
    }

    ctx.db.clock().advance_secs(1_200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (main_pooled, bal_a_pre, bal_b_pre, bal_c_pre) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0,
            s.characters[&pilot_b].balance.0,
            s.characters[&pilot_c].balance.0,
        )
    });
    assert!(main_pooled > main_from_1hz);
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(500.0, 0.0, 0.0)).await;

    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank start");
    ctx.db.clock().advance_secs(9_900);
    let _ = ctx
        .dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
        .await
        .unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].ess_main_bank.0), main_pooled);

    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestMainBankUnlink", &[], &kw, None)
        .await
        .expect("RequestMainBankUnlink");
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (main_after, gain_a, gain_b, gain_c, has_j_a, has_j_b, has_j_c) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0 - bal_a_pre,
            s.characters[&pilot_b].balance.0 - bal_b_pre,
            s.characters[&pilot_c].balance.0 - bal_c_pre,
            s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSMainBankPayout"),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_b && j.entry_type == "ESSMainBankPayout"),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_c && j.entry_type == "ESSMainBankPayout"),
        )
    });
    assert_eq!(main_after, 0);
    assert_eq!(gain_a + gain_b + gain_c, main_pooled);
    assert!((gain_a - 2 * gain_b).abs() * 1_000 <= gain_a);
    assert!((gain_c - main_from_1hz).abs() <= 2);
    assert!(has_j_a && has_j_b && has_j_c);

    // 2. ESS Reserve Bank key tiers, duplicate-unlock guard, SQLite durability, & server-tick progressive streaming (`F-03`)!
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
        snap.item_positions.insert(ship_a, (ess_pos + DVec3::new(500.0, 0.0, 0.0)).to_array());
    });
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideEssLinkRange"
    );
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();

    let keys_15m = [57714_u32, 57716, 57717, 57720];
    let keys_45m = [57715_u32, 57719, 57718, 57721];
    let (mut wrong_quad_15m, mut matched_15m_tid) = (false, 0_u32);
    for (idx, &tid) in keys_15m.iter().enumerate() {
        let iid = 965_610 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if matched_15m_tid > 0 {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
            continue;
        }
        match ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            Ok(rep) => {
                assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(15));
                assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&iid)));
                let char_notifs = count_char_event(&ctx, pilot_a, "OnReserveBankUnlock");
                let sess_notifs: Vec<Vec<PyRep>> = sess_a
                    .drain_notifications()
                    .into_iter()
                    .filter(|(n, _)| n == "OnReserveBankUnlock")
                    .map(|(_, a)| a)
                    .collect();
                assert_eq!(char_notifs.len(), 1);
                assert_eq!(sess_notifs.len(), 1);
                assert!(sess_notifs[0][0].dict_get("regionID").is_some());
                matched_15m_tid = tid;
            }
            Err(e) => {
                assert_eq!(e.code, "EssReserveKeyWrongQuadrant");
                ctx.db.with_mut_snapshot(|snap| {
                    snap.items.remove(&iid);
                });
                wrong_quad_15m = true;
            }
        }
    }
    if !wrong_quad_15m {
        let wrong_tid = keys_15m.iter().copied().find(|&t| t != matched_15m_tid).unwrap();
        let wrong_iid = 965_619_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(wrong_iid, make_item(wrong_iid, wrong_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        // While already unlocked, duplicate RequestUnlockReserveBank rejects BEFORE consuming any key!
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(wrong_iid)], &kw, None)
                .await
                .unwrap_err()
                .code,
            "EssReserveBankAlreadyUnlocked"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&wrong_iid);
        });
    }
    assert!(matched_15m_tid > 0);

    // Duplicate `RequestUnlockReserveBank` while unlocked is rejected without consuming a second valid key (`F-03`)!
    let spare_key_iid = 965_650_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(spare_key_iid, make_item(spare_key_iid, matched_15m_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(spare_key_iid)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&spare_key_iid)), "Duplicate unlock attempt must NOT consume spare key");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&spare_key_iid);
    });

    // SQLite restart durability (`B36-F06`): reopen SQLite into a fresh ServerContext and stream pulses from server ticks!
    let saved_now = ctx.now_unix();
    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b36_ess_reserve_unlock"));
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(saved_now)));
    crate::common::login_pilot(&ctx2, &mut sess_a, "demo", "demo-secret").await;
    ctx2.dispatch_rpc(
        &mut sess_a,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(pilot_a)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot_a on reopened ctx2");
    let _ = ctx2.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    let ess_info_re = kv_map(
        &ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(ess_info_re["reserveBankPulsesRemaining"].as_i64(), Some(15));
    let details_re = kv_map(
        &ctx2.dispatch_rpc(&mut sess_a, "dynamicResourceCacheMgr", None, "GetESSSystemDetails", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(details_re["reserveTimeWindowState"].as_i64(), Some(2));

    let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
    let ess_pos2 = sys_h2.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let _ = sys_h2
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 639,
                position: ess_pos2 + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx2.build_type_combat_state(639),
        )
        .await;

    let bal_before_r15 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank initiates link after SQLite restart without consuming another key");
    {
        let now_u = ctx2.now_unix();
        let cw = ctx2.crimewatch.read().unwrap();
        let st = cw.states.get(&pilot_a).expect("crimewatch state");
        assert!(st.suspect_flag_until_unix >= now_u + 900);
        assert!(st.weapon_timer_until_unix >= now_u + 60);
    }

    // Duplicate unlock while actively linked is ALSO rejected without resetting `reserve_linked_character_id` (`F-03`)!
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.insert(spare_key_iid, make_item(spare_key_iid, matched_15m_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
    });
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(spare_key_iid)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(true)
    );
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.remove(&spare_key_iid);
    });
    let _ = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");

    // Stream Pulse 1 & Pulse 2 purely from `tick_background_cluster` (NO manual `AttemptLinkToReserveBank` polling!)
    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let pulse1 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15;
    assert!(pulse1 > 0, "Server tick must stream Reserve Bank pulse 1 automatically");
    assert!(ctx2.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSReserveBankPayout")));
    assert!(
        count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty(),
        "Intermediate pulse 1 must NOT emit OnReserveBankPlayerLinkDisconnected"
    );
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(true)
    );

    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let pulse2 = (ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15) - pulse1;
    assert!(pulse2 > pulse1, "Reserve Bank payouts must progressively spool up (pulse2={pulse2} > pulse1={pulse1})");
    assert!(
        count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty(),
        "Intermediate pulse 2 must NOT emit OnReserveBankPlayerLinkDisconnected"
    );

    // Moving beyond 2,500 m severs the Reserve Bank link on the next server tick and emits exactly 1 disconnect (`reason == 1`)!
    let _ = sys_h2.set_ball_position(ship_a, ess_pos2 + DVec3::new(3_200.0, 0.0, 0.0)).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let range_disc = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(range_disc.len(), 1, "Range break must emit exactly 1 OnReserveBankPlayerLinkDisconnected");
    assert_eq!(range_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(false)
    );

    // Move back within 2,500 m, re-link during remaining window, and stream remaining 13 pulses via `tick_background_cluster`!
    let _ = sys_h2.set_ball_position(ship_a, ess_pos2 + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("Re-link during remaining 15m Reserve Bank window");
    let mut per_pulse = vec![pulse1, pulse2];
    for step in 0..13 {
        let before = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
        ctx2.db.clock().advance_secs(60);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
        let gained = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - before;
        per_pulse.push(gained);
        if step < 12 {
            assert!(count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty());
        }
    }
    assert!(per_pulse[11] > per_pulse[0] && per_pulse[11] > per_pulse[14], "Reserve Bank curve must spool up to peak and drop down");
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15, 4_500_000);
    assert_eq!(ctx2.db.with_snapshot(|s| s.system_states[&sys_id].ess_reserve_bank.0), 0);
    let final_disc = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(final_disc.len(), 1, "Terminal pulse 15 must emit exactly 1 OnReserveBankPlayerLinkDisconnected");
    assert_eq!(final_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));

    // 45m Reserve Security Key tier unlocks cleanly once prior 15m window has terminated, and streams progressive pulse 1!
    ctx2.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
    });
    let mut matched_45m = false;
    for (idx, &tid) in keys_45m.iter().enumerate() {
        let iid = 965_620 + idx as i64;
        ctx2.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if matched_45m {
            ctx2.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
            continue;
        }
        if let Ok(rep) = ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(45));
            matched_45m = true;
        } else {
            ctx2.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
        }
    }
    assert!(matched_45m);

    let bal_before_r45 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let expected_45_p1 = eve_server::services::pve_ess_reserve_banks::compute_ess_reserve_pulse_payout_cents(4_500_000, 45, 45);
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r45, expected_45_p1);
}

}
