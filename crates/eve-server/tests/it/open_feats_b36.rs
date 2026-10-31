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
