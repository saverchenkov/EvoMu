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

pub(crate) mod storyline_offer_counter {
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
            assert_eq!(eve_server::services::pve_rnd_storyline_sites::is_storyline_qualifying_agent_type(tid), expected);
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

pub(crate) mod abyssal_activation_deadspace_pocket_rules {
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

pub(crate) mod ess_grid_warp_module_restrictions {
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

pub(crate) mod ess_main_autopayout_reserve_bank {
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].ess_main_bank.0), main_pooled);

        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestMainBankUnlink", &[], &kw, None)
            .await
            .expect("RequestMainBankUnlink");
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

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
        let ctx2 = Arc::new(crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(&ctx, "b36_ess_reserve_unlock"));
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx2).await;
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx2).await;
        let pulse2 = (ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15) - pulse1;
        assert!(pulse2 > pulse1, "Reserve Bank payouts must progressively spool up (pulse2={pulse2} > pulse1={pulse1})");
        assert!(
            count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty(),
            "Intermediate pulse 2 must NOT emit OnReserveBankPlayerLinkDisconnected"
        );

        // Moving beyond 2,500 m severs the Reserve Bank link on the next server tick and emits exactly 1 disconnect (`reason == 1`)!
        let _ = sys_h2.set_ball_position(ship_a, ess_pos2 + DVec3::new(3_200.0, 0.0, 0.0)).await;
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx2).await;
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
            eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx2).await;
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx2).await;
        let expected_45_p1 = eve_server::services::pve_rnd_storyline_sites::compute_ess_reserve_pulse_payout_cents(4_500_000, 45, 45);
        assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r45, expected_45_p1);
    }
}

pub(crate) mod operations_tutorial_tasks_categories_progress {
    // Integration tests for batch 37 exclusive `feat:` parity rows:
    // - `feat:pve.operations-tutorial`
    // - `feat:pve.seasonal-login`
    // - `feat:structures.profiles`
    // - `feat:structures.structure-directory`


    use std::{collections::{HashMap, HashSet}, sync::Arc};
    use eve_db::{CharacterRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository, corp_roles};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{SessionState, service_roles};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_sde::SdeRegistry;
    use eve_server::router::ServerContext;
    use eve_server::services::{
        account_services,
        combat_crimewatch::combat_npc_ai::{destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity},
        operations_manager_helpers::{OperationDefinitions, get_operation_defs},
        station_structure::structure_upwell_ops::{load_ext_state, save_ext_state},
    };
    use glam::DVec3;
    use crate::common;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const JITA_SYS: u32 = 30_000_142;
    const AMARR_SYS: u32 = 30_002_187;
    const JITA_STATION: i64 = 60_003_760;

    fn new_ctx() -> ServerContext {
        let sde = Arc::new(SdeRegistry::open(&SdeRegistry::default_db_path()).expect("open SDE"));
        ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
    }

    async fn rpc_kw(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep], kw: &HashMap<String, PyRep>) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, m, args, kw, None).await
    }

    async fn login_demo_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(char_id as u64);
        let (user, pass) = if char_id == PILOT_2 { ("other", "other-secret") } else { ("demo", "demo-secret") };
        common::login_pilot(ctx, &mut sess, user, pass).await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
        sess.solar_system_id = Some(JITA_SYS);
        sess.station_id = Some(JITA_STATION);
        sess.structure_id = None;
        ctx.advance_secs(15);
        ctx.db.with_mut_snapshot(|snap| if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = JITA_SYS; ch.station_id = Some(JITA_STATION); ch.structure_id = None; ch.home_station_id = JITA_STATION;
        });
        sess
    }

    fn seed_corp_and_pilots(ctx: &ServerContext, corp_id: i64, ceo_id: i64, members_with_roles: &[(i64, u64, &str)]) {
        let uid_map: HashMap<i64, u64> = members_with_roles.iter().map(|&(cid, _, _)| (cid, ctx.db.provision_player_account(&format!("b37_{cid}"), "b37-pw"))).collect();
        ctx.db.with_mut_snapshot(|snap| {
            let base_char = snap.characters.get(&PILOT_1).expect("demo pilot 1").clone();
            let (mut member_ids, mut role_map) = (HashSet::new(), HashMap::new());
            for &(cid, roles, name) in members_with_roles {
                let (uid, ship_id) = (uid_map[&cid], 770_000_000 + cid);
                member_ids.insert(cid);
                role_map.insert(cid, roles);
                snap.items.entry(ship_id).or_insert(InventoryItemRecord { item_id: ship_id, type_id: 587, owner_id: cid, location_id: i64::from(JITA_SYS), flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
                let ch = snap.characters.entry(cid).or_insert_with(|| CharacterRecord {
                    character_id: cid, user_id: uid, name: name.to_string(), corporation_id: corp_id, solar_system_id: JITA_SYS, constellation_id: 20_000_020,
                    region_id: 10_000_002, active_ship_id: ship_id, home_station_id: JITA_STATION, balance: IskCents(100_000_000_00), race_id: 1, bloodline_id: 1, ancestry_id: 1, clone_grade: 2, ..base_char.clone()
                });
                ch.user_id = uid; ch.corporation_id = corp_id; ch.solar_system_id = JITA_SYS; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
            }
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id, name: format!("Corp {corp_id}"), ticker: "TST".into(), ceo_id, hq_station_id: Some(JITA_STATION),
                members: member_ids, member_roles: role_map, master_wallet: IskCents(500_000_000_00), ..Default::default()
            });
        });
    }

    async fn make_space_session(ctx: &ServerContext, char_id: i64, corp_id: i64, corp_role: u64) -> SessionState {
        let (uid, ship_id) = ctx.db.with_snapshot(|s| { let ch = s.characters.get(&char_id).unwrap(); (ch.user_id, ch.active_ship_id) });
        let _ = ctx.db.acquire_online_lock(uid, char_id as u64, Some(char_id), true, ctx.now_unix());
        let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        let mut sess = SessionState::new(char_id as u64);
        sess.user_id = Some(uid); sess.character_id = Some(char_id); sess.corporation_id = Some(corp_id); sess.corp_role = corp_role;
        sess.solar_system_id = Some(JITA_SYS); sess.constellation_id = Some(20_000_020); sess.region_id = Some(10_000_002); sess.ship_id = Some(ship_id);
        sess
    }

    async fn seed_structure_with_services(ctx: &ServerContext, structure_id: i64, owner_corp_id: i64, name: &str, state: &str, modules: &[(i64, u32, InventoryFlag, bool)]) {
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(structure_id, UpwellStructureRecord {
                structure_id, type_id: 35_833, owner_corp_id, solar_system_id: JITA_SYS, name: name.to_string(), position: [1_000.0, 0.0, 0.0], state: state.to_string(),
                fuel_blocks: 2_400, quantum_core_installed: true, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0, hull_hp: 1_000_000.0, reinforced_until_unix: if state == "Anchoring" { now + 3_600 } else { 0 }, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            let lf = ext.lifecycle.entry(structure_id).or_default();
            lf.last_fuel_tick_unix = Some(now);
            for &(item_id, type_id, flag, online) in modules {
                snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id: owner_corp_id, location_id: structure_id, flag, quantity: 1, singleton: true, version: 1, ..Default::default() });
                snap.module_online_states.insert(item_id, online);
                if online && !lf.online_modules.contains(&item_id) { lf.online_modules.push(item_id); }
            }
            save_ext_state(snap, &ext);
        });
        let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball { id: structure_id, type_id: 35_833, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0e9, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Rigid }).await;
    }

    fn pick_sde_type_matching(ctx: &ServerContext, conds: &HashMap<String, Vec<String>>, default_tid: u32) -> u32 {
        if let Some(tid) = conds.get("typeID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()) { return tid; }
        if let Some(gid) = conds.get("groupID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()) {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.group_id == gid) { return tid; }
        }
        default_tid
    }

    async fn ensure_dock_state(ctx: &ServerContext, sess: &mut SessionState, want_dock: bool) {
        if want_dock && sess.station_id.is_none() {
            ctx.advance_secs(15);
            let sid = sess.ship_id.unwrap();
            let sys = ctx.systems.get_or_spawn(JITA_SYS);
            let st_pos = sys.get_ball_snapshot(JITA_STATION).await.ok().flatten().map(|b| b.1.position).unwrap_or(DVec3::ZERO);
            let _ = sys.set_ball_position(sid, st_pos).await;
            rpc(ctx, sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION)]).await.unwrap();
        } else if !want_dock && sess.station_id.is_some() {
            ctx.advance_secs(15);
            rpc(ctx, sess, "stationSvc", "Undock", &[]).await.unwrap();
        }
    }

    async fn complete_op_authoritatively(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64, op_id: i64) {
        let defs = get_operation_defs(&ctx.sde);
        rpc(ctx, sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(JITA_SYS)), PyRep::Integer(cat_id)]).await.unwrap();
        for &tid in defs.op_tasks.get(&op_id).into_iter().flatten() {
            let ev = defs.task_event_names.get(&tid).cloned().unwrap_or_default();
            let target = defs.task_target_values.get(&tid).copied().unwrap_or(1);
            let conds = defs.task_event_conditions.get(&tid).cloned().unwrap_or_default();
            if ev.starts_with("client_") {
                let py_conds: Vec<(PyRep, PyRep)> = conds.iter().filter_map(|(k, vs)| {
                    let v = vs.first()?;
                    Some((PyRep::String(k.clone()), v.parse::<i64>().map(PyRep::Integer).unwrap_or_else(|_| PyRep::String(v.clone()))))
                }).collect();
                let mut kw = HashMap::new();
                if !py_conds.is_empty() { kw.insert("extra_data".into(), PyRep::Dict(py_conds)); }
                for _ in 0..target { rpc_kw(ctx, sess, "operationsManager", "process_client_event", &[PyRep::String(ev.clone())], &kw).await.unwrap(); }
                continue;
            }
            let saved_dock = sess.station_id.is_some();
            match ev.as_str() {
                "dockInStation" => { ensure_dock_state(ctx, sess, false).await; ensure_dock_state(ctx, sess, true).await; }
                "undockFromStation" => { ensure_dock_state(ctx, sess, true).await; ensure_dock_state(ctx, sess, false).await; }
                "shipBoarded" => {
                    let stid = pick_sde_type_matching(ctx, &conds, 587);
                    ensure_dock_state(ctx, sess, true).await;
                    let sid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: stid, owner_id: char_id, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                    ctx.advance_secs(15);
                    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(sid)]).await.unwrap();
                }
                "dungeonEntered" => {
                    ensure_dock_state(ctx, sess, false).await;
                    let site_id = defs.op_site_ids.get(&op_id).copied().or_else(|| conds.get("dungeonID").and_then(|v| v.first()).and_then(|s| s.parse::<i64>().ok())).unwrap_or(1);
                    rpc(ctx, sess, "operationsManager", "start_site", &[PyRep::Integer(op_id), PyRep::Integer(site_id)]).await.unwrap();
                }
                "fitItemCount" => {
                    ensure_dock_state(ctx, sess, true).await;
                    let mtid = pick_sde_type_matching(ctx, &conds, 439);
                    ctx.db.with_mut_snapshot(|s| {
                        if let Some(sid) = sess.ship_id { s.items.retain(|_, it| !(it.location_id == sid && matches!(it.flag as i64, 11..=34 | 92..=94))); }
                        let sk = s.skills.entry(char_id).or_default();
                        for (tid, _) in ctx.sde.required_skills_for_type(mtid).into_iter().chain(ctx.sde.required_skills_for_type(587)) {
                            sk.entry(tid).or_insert(eve_db::TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
                        }
                    });
                    let mut sid = sess.ship_id.unwrap();
                    ctx.invalidate_ship_fitting(sid);
                    let loc = sess.station_id.unwrap_or(sid);
                    let flag = if sess.station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::Cargo };
                    for _ in 0..target {
                        let mid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: mtid, owner_id: char_id, location_id: loc, flag, quantity: 1, singleton: false, version: 1, ..Default::default() }); id });
                        let (mut fitted, mut last_err) = (false, None);
                        for f in (27_i64..=34).chain(19..=26).chain(11..=18).chain(92..=94) {
                            match rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(sid), PyRep::Integer(f)]).await {
                                Ok(_) => { fitted = true; break; }
                                Err(e) => last_err = Some(e),
                            }
                        }
                        if !fitted {
                            let bsid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: 587, owner_id: char_id, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                            ctx.advance_secs(15);
                            rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(bsid)]).await.unwrap();
                            sid = sess.ship_id.unwrap();
                            for f in (27_i64..=34).chain(19..=26).chain(11..=18).chain(92..=94) {
                                match rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(sid), PyRep::Integer(f)]).await {
                                    Ok(_) => { fitted = true; break; }
                                    Err(e) => last_err = Some(e),
                                }
                            }
                        }
                        assert!(fitted, "failed to fit module {mtid} onto ship {sid}: {last_err:?}");
                    }
                }
                "loadChargesCount" => {
                    let ctid = pick_sde_type_matching(ctx, &conds, 222);
                    let sid = sess.ship_id.unwrap();
                    let loc = sess.station_id.unwrap_or(sid);
                    let flag = if sess.station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::Cargo };
                    let mut loaded = false;
                    for try_tid in [ctid, 222, 182, 252, 209] {
                        let cid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: try_tid, owner_id: char_id, location_id: loc, flag, quantity: target as i32, singleton: false, version: 1, ..Default::default() }); id });
                        if rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(cid), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::HiSlot0 as i64)]).await.is_ok() { loaded = true; break; }
                    }
                    assert!(loaded, "failed to load charges onto ship {sid}");
                }
                "skillTrainingStarted" => {
                    let sk_tid = conds.get("typeID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()).unwrap_or(3_300);
                    rpc(ctx, sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(i64::from(sk_tid)), PyRep::Integer(5)])])]).await.unwrap();
                }
                "timesWarped" | "warpToLocation" => {
                    ensure_dock_state(ctx, sess, false).await;
                    let sid = sess.ship_id.unwrap();
                    let sys = ctx.systems.get_or_spawn(JITA_SYS);
                    let _ = sys.set_ball_position(sid, DVec3::new(1_000_000_000.0, 0.0, 0.0)).await;
                    rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION), PyRep::Real(0.0)]).await.unwrap();
                }
                "moduleActivate" | "activateModule" | "moduleActivated" | "activateShipModule" => {
                    let mtid = pick_sde_type_matching(ctx, &conds, 439);
                    let sid = sess.ship_id.unwrap();
                    let mid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: mtid, owner_id: char_id, location_id: sid, flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                    ctx.invalidate_ship_fitting(sid);
                    ensure_dock_state(ctx, sess, false).await;
                    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(mid), PyRep::String("online".into())]).await.unwrap();
                }
                "lootItemCount" => {
                    ensure_dock_state(ctx, sess, false).await;
                    let sid = sess.ship_id.unwrap();
                    let ltid = pick_sde_type_matching(ctx, &conds, 34);
                    let (wreck_id, loot_id) = ctx.db.with_mut_snapshot(|s| {
                        s.next_entity_id += 1; let wid = s.next_entity_id;
                        s.items.insert(wid, InventoryItemRecord { item_id: wid, type_id: 26_557, owner_id: char_id, location_id: i64::from(JITA_SYS), flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
                        s.next_entity_id += 1; let lid = s.next_entity_id;
                        s.items.insert(lid, InventoryItemRecord { item_id: lid, type_id: ltid, owner_id: char_id, location_id: wid, flag: InventoryFlag::Cargo, quantity: target as i32, singleton: false, version: 1, ..Default::default() });
                        (wid, lid)
                    });
                    let sys = ctx.systems.get_or_spawn(JITA_SYS);
                    let ship_pos = sys.get_ball_snapshot(sid).await.ok().flatten().map(|b| b.1.position).unwrap_or(DVec3::ZERO);
                    let _ = sys.spawn_ball(eve_destiny::Ball { id: wreck_id, type_id: 26_557, position: ship_pos + DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 100.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }).await;
                    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
                }
                "somethingExplode" | "destroyHostileNPC" | "entityGroupDestroyed" => {
                    ensure_dock_state(ctx, sess, false).await;
                    let npc_tid = pick_sde_type_matching(ctx, &conds, 10_042);
                    for k in 0..target {
                        let bid = spawn_specialized_npc_entity(ctx, JITA_SYS, npc_tid, "BeltRat", 0, DVec3::ZERO).await;
                        let _ = destroy_specialized_npc_and_queue_bounty(ctx, JITA_SYS, bid, char_id).await;
                        if k == 0 && target >= 2 && conds.is_empty() && ev == "destroyHostileNPC" {
                            let PyRep::Dict(p) = rpc(ctx, sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
                            assert!(p.iter().find(|(id, _)| id.as_i64() == Some(tid)).and_then(|(_, v)| v.as_i64()).unwrap_or(0) < 100);
                        }
                    }
                }
                other => panic!("Unhandled SDE operation task event: {other}"),
            }
            ensure_dock_state(ctx, sess, saved_dock).await;
        }
    }

    async fn complete_tree_ops(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64) {
        let defs = get_operation_defs(&ctx.sde);
        let tree_ops = defs.cat_operation_trees.get(&cat_id).cloned().unwrap_or_default();
        let PyRep::List(done_list) = rpc(ctx, sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
        let mut done: HashSet<i64> = done_list.iter().filter_map(|x| x.as_i64()).collect();
        for _ in 0..tree_ops.len() {
            for &oid in &tree_ops {
                if !done.contains(&oid) && defs.are_prerequisites_met(oid, &done) {
                    complete_op_authoritatively(ctx, sess, char_id, cat_id, oid).await;
                    done.insert(oid);
                }
            }
        }
    }

    fn is_supported_op(defs: &OperationDefinitions, oid: i64) -> bool {
        let ok_one = |id: i64| defs.op_tasks.get(&id).is_some_and(|ts| ts.iter().all(|t| defs.task_event_names.get(t).is_some_and(|ev| {
            ev.starts_with("client_") || matches!(ev.as_str(), "dockInStation" | "undockFromStation" | "shipBoarded" | "dungeonEntered"
                | "fitItemCount" | "loadChargesCount" | "skillTrainingStarted" | "timesWarped" | "warpToLocation" | "moduleActivate"
                | "activateModule" | "moduleActivated" | "activateShipModule" | "lootItemCount" | "somethingExplode" | "destroyHostileNPC" | "entityGroupDestroyed")
        })));
        ok_one(oid) && defs.op_prerequisites.get(&oid).into_iter().flatten().chain(defs.op_tree_parents.get(&oid).into_iter().flatten()).all(|&p| ok_one(p))
    }

    async fn unlock_prereqs_for_op(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64, target_op: i64) {
        let defs = get_operation_defs(&ctx.sde);
        let PyRep::List(dl) = rpc(ctx, sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
        let (mut done, mut candidates, mut stack) = (dl.iter().filter_map(|x| x.as_i64()).collect::<HashSet<i64>>(), Vec::new(), vec![target_op]);
        for _ in 0..=defs.op_prerequisites.len() + defs.op_tree_parents.len() {
            let Some(cur) = stack.pop() else { break; };
            for &p in defs.op_prerequisites.get(&cur).into_iter().flatten().chain(defs.op_tree_parents.get(&cur).into_iter().flatten()) {
                if !candidates.contains(&p) { candidates.push(p); stack.push(p); }
            }
        }
        for _ in 0..candidates.len().max(1) {
            if defs.are_prerequisites_met(target_op, &done) { return; }
            for &oid in &candidates {
                if !done.contains(&oid) && defs.are_prerequisites_met(oid, &done) { complete_op_authoritatively(ctx, sess, char_id, cat_id, oid).await; done.insert(oid); }
            }
        }
    }

    #[tokio::test]
    // parity: feat:pve.operations-tutorial
    async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
        eve_testkit::attest(&["feat:pve.operations-tutorial"]);
        let ctx = new_ctx();
        common::pin_clock(&ctx);
        let mut anon = SessionState::new(77);
        assert!(rpc(&ctx, &mut anon, "operationsManager", "get_character_progress", &[]).await.is_err());

        let mut sess = login_demo_pilot(&ctx, PILOT_1).await;
        let defs = get_operation_defs(&ctx.sde);
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(i64::from(AMARR_SYS)), PyRep::Integer(31)]).await.unwrap_err().code, "CustomInfo");
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_main_tutorial_finished", &[PyRep::Integer(PILOT_2)]).await.unwrap_err().code, "UserError");
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_2), PyRep::Integer(31)]).await.unwrap_err().code, "InsufficientRole"); // rpc-lint: allow error code assertion
        let child_op = defs.sde_operations.iter().copied().find(|&oid| !defs.are_prerequisites_met(oid, &HashSet::new())).expect("child op with prereq");
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(child_op), PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap_err().code, "OperationPrerequisitesNotMet"); // rpc-lint: allow error code assertion

        // Intro tutorial Op 157 (Cat 31, SDE tasks [344=dockInStation, 345=shipBoarded, 346=client_agencyContentExpanded])
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(1));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(31)]).await.unwrap_err().code, "OperationCategoryIncomplete"); // rpc-lint: allow error code assertion

        let _ = ctx.drain_notifications_for_char(PILOT_1);
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(i64::from(JITA_SYS)), PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationCategoryChanged" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157)]));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[]).await.unwrap(), PyRep::Integer(31));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(2));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(157), PyRep::Integer(1)]).await.unwrap_err().code, "CustomInfo");

        rpc(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("dockInStation".into())]).await.unwrap(), PyRep::Bool(false));
        rpc(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationTaskCompleted" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]));
        let PyRep::Dict(prog_mid) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
        assert!((1..100).contains(&prog_mid.iter().find(|(k, _)| k.as_i64() == Some(157)).and_then(|(_, v)| v.as_i64()).unwrap_or(0)));

        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(345)]).await.unwrap_err().code, "TaskNotSkippable"); // rpc-lint: allow error code assertion
        let spoof_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("task_id".into()), PyRep::Integer(345))]))]);
        assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_progress".into())], &spoof_kw).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("operation_completed".into())]).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_claim_rewards_btn_click".into())]).await.unwrap(), PyRep::Bool(false));
        let board_sid = ctx.db.with_mut_snapshot(|s| {
            s.next_entity_id += 1;
            let id = s.next_entity_id;
            s.items.insert(id, InventoryItemRecord { item_id: id, type_id: 587, owner_id: PILOT_1, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
            id
        });
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(board_sid)]).await.expect("ship.Board");
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationTaskCompleted" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(345)]));

        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())]).await.unwrap(), PyRep::Bool(false));
        let bad_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("contentPieceType".into()), PyRep::Integer(99))]))]);
        assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())], &bad_kw).await.unwrap(), PyRep::Bool(false));
        let ok_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("contentPieceType".into()), PyRep::Integer(18))]))]);
        assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())], &ok_kw).await.unwrap(), PyRep::Bool(true));
        let PyRep::List(done_157) = rpc(&ctx, &mut sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
        assert!(done_157.contains(&PyRep::Integer(157)));

        complete_tree_ops(&ctx, &mut sess, PILOT_1, 31).await;
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationCategoryCompleted" && n.args == vec![PyRep::Integer(31)]));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(3));
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));

        let p1_race = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().race_id);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_2).unwrap().race_id = p1_race);
        let rw_item_op = defs.sde_operations.iter().copied().filter(|&oid| {
            defs.op_tasks.get(&oid).is_some_and(|ts| ts.iter().any(|t| defs.task_event_names.get(t).is_some_and(|e| !e.starts_with("client_"))))
                && (defs.op_completion_rewards.get(&oid).is_some_and(|rs| rs.iter().any(|r| r.race_id.is_none_or(|rc| rc == 0 || rc == p1_race)))
                    || defs.op_skill_rewards.get(&oid).is_some_and(|r| !r.is_empty()))
        }).min_by_key(|&oid| defs.op_default_category.get(&oid).and_then(|c| defs.cat_operation_trees.get(c)).map_or(99, Vec::len)).expect("SDE item/skill-rewarding op");
        let rw_op_cat = defs.op_default_category.get(&rw_item_op).copied().unwrap_or(31);
        unlock_prereqs_for_op(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
        let items_before: usize = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count());
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        complete_op_authoritatively(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
        let items_after: usize = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count());
        assert!(items_after > items_before, "docked completion of {rw_item_op} must deliver SDE item/skillbook rewards");
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnItemChange"), "docked item delivery must emit OnItemChange");
        complete_op_authoritatively(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count()), items_after);

        let mut sess2 = login_demo_pilot(&ctx, PILOT_2).await;
        ensure_dock_state(&ctx, &mut sess2, false).await;
        unlock_prereqs_for_op(&ctx, &mut sess2, PILOT_2, rw_op_cat, rw_item_op).await;
        let uid2 = sess2.user_id.unwrap();
        let toks_before = ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid2).map_or(0, Vec::len));
        let _ = ctx.drain_notifications_for_char(PILOT_2);
        complete_op_authoritatively(&ctx, &mut sess2, PILOT_2, rw_op_cat, rw_item_op).await;
        assert!(ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid2).map_or(0, Vec::len)) > toks_before);
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

        let PyRep::List(done_p2_list) = rpc(&ctx, &mut sess2, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
        let done_p2: HashSet<i64> = done_p2_list.iter().filter_map(|x| x.as_i64()).collect();
        if let Some((&rw_isk_op, &op_isk)) = defs.op_isk_rewards.iter().filter(|(&oid, &isk)| {
            isk > 0 && !done_p2.contains(&oid) && is_supported_op(defs, oid)
                && defs.op_tasks.get(&oid).is_some_and(|ts| ts.iter().any(|t| defs.task_event_names.get(t).is_some_and(|e| !e.starts_with("client_"))))
        }).min_by_key(|(oid, _)| *oid) {
            let isk_cat = defs.op_default_category.get(&rw_isk_op).copied().unwrap_or(31);
            unlock_prereqs_for_op(&ctx, &mut sess2, PILOT_2, isk_cat, rw_isk_op).await;
            let bal_pre = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.0);
            let _ = ctx.drain_notifications_for_char(PILOT_2);
            complete_op_authoritatively(&ctx, &mut sess2, PILOT_2, isk_cat, rw_isk_op).await;
            assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.0) - bal_pre, (op_isk as i64) * 100);
            assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnAccountChange"));
        }
        let rw_cat_id = defs.cat_isk_rewards.iter().filter(|(cid, isk)| **isk > 0 && defs.cat_operation_trees.get(cid).is_some_and(|t| !t.is_empty() && t.iter().all(|&o| is_supported_op(defs, o)))).map(|(c, _)| *c).min()
            .or_else(|| defs.cat_operation_trees.iter().filter(|(&cid, t)| cid != 31 && t.len() >= 2 && t.iter().all(|&o| is_supported_op(defs, o))).min_by_key(|(_, t)| t.len()).map(|(&c, _)| c)).expect("multi-op SDE category");
        let cat_isk = defs.cat_isk_rewards.get(&rw_cat_id).copied().unwrap_or(0);
        let bal_pre_cat = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap_err().code, "OperationCategoryIncomplete"); // rpc-lint: allow error code assertion
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0), bal_pre_cat);
        complete_tree_ops(&ctx, &mut sess, PILOT_1, rw_cat_id).await;
        let bal_before_claim = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap(), PyRep::Bool(true));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0) - bal_before_claim, (cat_isk as i64) * 100);
        if cat_isk > 0 { assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnAccountChange")); }
        rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap();
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0) - bal_before_claim, (cat_isk as i64) * 100);
    }

    #[tokio::test]
    // parity: feat:pve.seasonal-login
    async fn test_seasonal_login_campaign_active_claim_and_history() {
        eve_testkit::attest(&["feat:pve.seasonal-login"]);
        let ctx = new_ctx();
        ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_700_044_800)));

        let mut anon = SessionState::new(88);
        assert!(rpc(&ctx, &mut anon, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.is_err());

        let mut sess = login_demo_pilot(&ctx, PILOT_1).await;
        let uid = sess.user_id.expect("authenticated user_id");
        assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
        assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("no seasonal campaign"));

        assert_eq!(rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.unwrap_err().code, "InsufficientRole");
        let mut gm_sess = login_demo_pilot(&ctx, PILOT_2).await;
        gm_sess.role = service_roles::ROLE_GML | service_roles::ROLE_ADMIN;
        assert_eq!(rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 99999999 30 11:00:00".into())]).await.unwrap_err().code, "CampaignNotFound");

        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.expect("GM schedule 2323");
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&PILOT_1).unwrap().clone_grade = 1);

        let PyRep::Tuple(active_fields) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
        assert_eq!(active_fields.len(), 4);
        assert_eq!(active_fields[0], PyRep::Integer(2323));
        assert_eq!(active_fields[2], PyRep::String("11:00:00".into()));
        let initial_rem = active_fields[3].as_i64().expect("num_remaining_rewards");
        assert!(initial_rem > 0);

        let _ = ctx.drain_notifications_for_char(PILOT_1);
        assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnItemChange"), "docked seasonal claim must emit OnItemChange");
        assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 87_900)));
        assert!(!ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.type_id == 49_809)));
        assert!(!ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

        let PyRep::Dict(hist_day0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
        assert_eq!(hist_day0.len(), 1);
        let ord0 = hist_day0[0].0.as_i64().unwrap();
        assert_eq!(active_fields[1], PyRep::Integer(ord0 + 30), "end_date_ordinal before 11:00:00 UTC turnover must honor turnover_secs");
        assert_eq!(hist_day0[0].1, PyRep::Integer(0), "Alpha claim state must be CLONE_STATE_ALPHA (0)");
        let PyRep::Tuple(after_d0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
        assert_eq!(after_d0[3], PyRep::Integer(initial_rem - 1));

        ctx.advance_secs(600);
        assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("already claimed"));

        ctx.advance_secs(1_200);
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| { let ch = snap.characters.get_mut(&PILOT_1).unwrap(); ch.clone_grade = 2; ch.station_id = None; });
        let tokens_before = ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid).map_or(0, Vec::len));
        let _ = ctx.drain_notifications_for_char(PILOT_1);

        assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.db.with_snapshot(|s| {
            let toks = account_services::load_store(s).redeem_tokens.get(&uid).cloned().unwrap_or_default();
            toks.len() > tokens_before && toks.iter().any(|t| t.type_id == 49_809)
        }));
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

        let PyRep::Dict(hist_day1) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
        assert_eq!(hist_day1.len(), 2);
        assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0)).and_then(|(_, v)| v.as_i64()), Some(1));
        assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0 + 1)).and_then(|(_, v)| v.as_i64()), Some(1));

        ctx.advance_secs(31 * 86_400);
        assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
        assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("no seasonal campaign"));
    }

    #[tokio::test]
    // parity: feat:structures.profiles
    async fn test_structure_profiles_acl_mapping_and_delayed_apply() {
        eve_testkit::attest(&["feat:structures.profiles"]);
        let ctx = new_ctx();
        common::pin_clock(&ctx);
        const CORP_OWNER: i64 = 98_600_001;
        const CORP_GUEST: i64 = 98_600_002;
        const DIR_PILOT: i64 = 95_600_001;
        const GRUNT_PILOT: i64 = 95_600_002;
        const VIP_PILOT: i64 = 95_600_003;
        const PUB_PILOT: i64 = 95_600_004;
        const BLOCKED_CORP_PILOT: i64 = 95_600_005;
        const OUTSIDER_PILOT: i64 = 95_600_006;
        const ST_ID: i64 = 1_045_000_000_001;

        seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_PILOT, &[(DIR_PILOT, corp_roles::ROLE_DIRECTOR, "Owner Dir"), (GRUNT_PILOT, 0, "Owner Grunt")]);
        seed_corp_and_pilots(&ctx, CORP_GUEST, VIP_PILOT, &[(VIP_PILOT, corp_roles::ROLE_DIRECTOR, "VIP"), (PUB_PILOT, 0, "Pub"), (BLOCKED_CORP_PILOT, 0, "Blocked")]);
        seed_corp_and_pilots(&ctx, 98_600_003, OUTSIDER_PILOT, &[(OUTSIDER_PILOT, corp_roles::ROLE_DIRECTOR, "Outsider")]);
        seed_structure_with_services(&ctx, ST_ID, CORP_OWNER, "Profile Citadel", "ShieldVulnerable", &[
            (880_001, 35_892, InventoryFlag::ServiceSlot0, true), // Market (svc 1)
            (880_002, 35_894, InventoryFlag::ServiceSlot1, true), // Clone Bay (svc 2)
        ]).await;

        let mut dir_sess = make_space_session(&ctx, DIR_PILOT, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
        let mut grunt_sess = make_space_session(&ctx, GRUNT_PILOT, CORP_OWNER, 0).await;
        let mut vip_sess = make_space_session(&ctx, VIP_PILOT, CORP_GUEST, corp_roles::ROLE_DIRECTOR).await;
        let mut pub_sess = make_space_session(&ctx, PUB_PILOT, CORP_GUEST, 0).await;
        let mut blk_sess = make_space_session(&ctx, BLOCKED_CORP_PILOT, CORP_GUEST, 0).await;
        let mut out_sess = make_space_session(&ctx, OUTSIDER_PILOT, 98_600_003, corp_roles::ROLE_DIRECTOR).await;
        assert_eq!(rpc(&ctx, &mut grunt_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Unauthorized".into()), PyRep::String("".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

        let vip_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
        let pub_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Public ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
        let def_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Gunners ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(vip_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(CORP_GUEST)]), PyRep::Bool(false)]).await.unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(BLOCKED_CORP_PILOT)]), PyRep::Bool(true)]).await.unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();

        let prof_1 = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Tiered Trade".into()), PyRep::String("ACL tiered".into())]).await.unwrap().as_i64().unwrap();
        assert_eq!(rpc(&ctx, &mut vip_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), PyRep::Dict(vec![])]).await.unwrap_err().code, "StructureOwnershipViolation");
        let bad_tax = PyRep::Dict(vec![(PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(1.5))])]))]);
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), bad_tax]).await.unwrap_err().code, "InvalidStructureTaxRate");
        let bad_flat_fee = PyRep::Dict(vec![(PyRep::Integer(23), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(-10.0))])]))]);
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), bad_flat_fee]).await.unwrap_err().code, "InvalidStructureTaxRate");

        let v24_settings = PyRep::Dict(vec![
            (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Integer(1))])])),
            (PyRep::Integer(4), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Real(0.05))])])),
            (PyRep::Integer(23), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(500_000.0))])])),
            (PyRep::Integer(20), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(1_000_000.0))])])),
            (PyRep::Integer(17), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(def_acl)), ("value", PyRep::Integer(1))])])),
        ]);
        let _ = ctx.drain_notifications_for_char(DIR_PILOT);
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), v24_settings]).await.unwrap(), PyRep::Bool(true));
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));

        let dup_id = rpc(&ctx, &mut dir_sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_1)]).await.unwrap().as_i64().unwrap();
        let PyRep::List(profs) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfiles", &[]).await.unwrap() else { panic!("List") };
        assert_eq!(profs.len(), 2);
        let PyRep::Dict(saved_cfg) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_1)]).await.unwrap() else { panic!("Dict") };
        assert_eq!(saved_cfg.len(), 5);
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_id)]).await.unwrap(), PyRep::Bool(true));

        let _ = ctx.drain_notifications_for_char(DIR_PILOT);
        let _ = ctx.drain_notifications_for_char(VIP_PILOT);
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_ID)]), PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));
        assert!(ctx.drain_notifications_for_char(DIR_PILOT).iter().any(|n| n.event == "OnProfileSettingsChange" && n.args == vec![PyRep::List(vec![PyRep::Integer(ST_ID)])]));
        assert!(ctx.drain_notifications_for_char(VIP_PILOT).iter().any(|n| n.event == "OnDockingAccessChangedForCurrentSolarSystem"));
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_1)]).await.unwrap_err().code, "ProfileInUse");

        assert_eq!(rpc(&ctx, &mut blk_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
        assert_eq!(rpc(&ctx, &mut out_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
        if let Some(&undock_op) = get_operation_defs(&ctx.sde).op_tasks.iter().find_map(|(o, ts)| ts.first().filter(|t| get_operation_defs(&ctx.sde).task_event_names.get(t).map(|s| s.as_str()) == Some("undockFromStation")).map(|_| o)) {
            let mut gm_s = vip_sess.clone(); gm_s.role |= eve_machonet::service_roles::ROLE_GMH;
            let _ = rpc(&ctx, &mut gm_s, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(undock_op)]).await;
        }
        let _ = ctx.drain_notifications_for_char(VIP_PILOT);
        rpc(&ctx, &mut vip_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("VIP docks");
        assert!(ctx.drain_notifications_for_char(VIP_PILOT).iter().all(|n| n.event != "OnOperationTaskCompleted"));
        rpc(&ctx, &mut pub_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("Public corp member docks");

        assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(5)]).await.unwrap().as_f64().unwrap() - 0.01).abs() < 1e-6);
        assert!((rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(5)]).await.unwrap().as_f64().unwrap() - 0.05).abs() < 1e-6);
        assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap().as_f64().unwrap() - 500_000.0).abs() < 1e-3);
        assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap_err().code, "StructureServiceAccessDenied");

        let owner_wal_0 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
        rpc(&ctx, &mut vip_sess, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(ST_ID), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(100), PyRep::Integer(90)]).await.expect("VIP PlaceBuyOrder");
        let owner_wal_1 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
        assert_eq!(owner_wal_1 - owner_wal_0, 100_00);
        rpc(&ctx, &mut pub_sess, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(ST_ID), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(100), PyRep::Integer(90)]).await.expect("Pub PlaceBuyOrder");
        let owner_wal_2 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
        assert_eq!(owner_wal_2 - owner_wal_1, 500_00);
        assert_eq!(rpc(&ctx, &mut pub_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "DefenseAccessDenied");
        rpc(&ctx, &mut vip_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP gunner TakeControl");
        rpc(&ctx, &mut vip_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP ReleaseControl");
        let _ = ctx.drain_notifications_for_char(PUB_PILOT);
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(CORP_GUEST)])]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut pub_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_ID)])]).await.unwrap(), PyRep::List(vec![]));
    }

    #[tokio::test]
    // parity: feat:structures.structure-directory
    async fn test_structure_directory_accessible_structures_and_services() {
        eve_testkit::attest(&["feat:structures.structure-directory"]);
        let ctx = new_ctx();
        common::pin_clock(&ctx);
        const CORP_OWNER: i64 = 98_700_001; const CORP_ALLY: i64 = 98_700_002;
        const DIR_ID: i64 = 95_700_001; const GRUNT_ID: i64 = 95_700_002; const VIP_ID: i64 = 95_700_003; const MKT_ONLY_ID: i64 = 95_700_004; const OUT_ID: i64 = 95_700_005;
        const ST_HUB: i64 = 1_046_000_000_001; const ST_ANCHORING: i64 = 1_046_000_000_002; const ST_PRIVATE: i64 = 1_046_000_000_003;
        seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_ID, &[(DIR_ID, corp_roles::ROLE_DIRECTOR, "Dir"), (GRUNT_ID, 0, "Grunt")]);
        seed_corp_and_pilots(&ctx, CORP_ALLY, VIP_ID, &[(VIP_ID, corp_roles::ROLE_DIRECTOR, "VIP"), (MKT_ONLY_ID, 0, "MarketOnly")]);
        seed_corp_and_pilots(&ctx, 98_700_003, OUT_ID, &[(OUT_ID, corp_roles::ROLE_DIRECTOR, "Outsider")]);
        seed_structure_with_services(&ctx, ST_HUB, CORP_OWNER, "Public Trade Hub", "ShieldVulnerable", &[
            (890_001, 35_892, InventoryFlag::ServiceSlot0, true),  // Market (1) online
            (890_002, 35_899, InventoryFlag::ServiceSlot1, true),  // Reprocessing (3) online
            (890_003, 35_894, InventoryFlag::ServiceSlot2, false), // Clone Bay (2) OFFLINE
        ]).await;
        seed_structure_with_services(&ctx, ST_ANCHORING, CORP_OWNER, "Anchoring Raitaru", "Anchoring", &[]).await;
        seed_structure_with_services(&ctx, ST_PRIVATE, CORP_OWNER, "Private Sotiyo", "ShieldVulnerable", &[(890_004, 35_878, InventoryFlag::ServiceSlot0, true)]).await;
        let mut dir_sess = make_space_session(&ctx, DIR_ID, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
        let mut grunt_sess = make_space_session(&ctx, GRUNT_ID, CORP_OWNER, 0).await;
        let mut vip_sess = make_space_session(&ctx, VIP_ID, CORP_ALLY, corp_roles::ROLE_DIRECTOR).await;
        let mut mkt_sess = make_space_session(&ctx, MKT_ONLY_ID, CORP_ALLY, 0).await;
        let mut out_sess = make_space_session(&ctx, OUT_ID, 98_700_003, corp_roles::ROLE_DIRECTOR).await;
        let mut anon = SessionState::new(99);
        assert!(rpc(&ctx, &mut anon, "structureDirectory", "GetMyCharacterStructures", &[]).await.is_err());

        let g_vip = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
        let g_mkt = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("MktOnly".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_vip), PyRep::List(vec![PyRep::Integer(VIP_ID)])]).await.unwrap();
        rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_mkt), PyRep::List(vec![PyRep::Integer(MKT_ONLY_ID)])]).await.unwrap();

        let p_hub = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Hub ACL".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
        let hub_cfg = PyRep::Dict(vec![
            (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Integer(1))])])),
            (PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Real(0.03))])])),
            (PyRep::Integer(3), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.02))])])),
        ]);
        rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_hub), hub_cfg]).await.unwrap();
        rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_HUB)]), PyRep::Integer(p_hub)]).await.unwrap();
        out_sess.solar_system_id = Some(AMARR_SYS);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&OUT_ID).unwrap().solar_system_id = AMARR_SYS);
        assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_PRIVATE)]).await.unwrap_err().code, "StructureAccessDenied");
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(999_999_999)]).await.unwrap_err().code, "StructureNotFound");
        let PyRep::Dict(corp_map) = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap() else { panic!("Dict") };
        assert_eq!(corp_map.len(), 3);
        let hub_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_HUB)).unwrap().1;
        assert_eq!(hub_dir.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
        assert_eq!(hub_dir.dict_get("fuelBlocks"), Some(&PyRep::Integer(2_400)));
        assert_eq!(hub_dir.dict_get("profileID"), Some(&PyRep::Integer(p_hub)));
        let anch_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_ANCHORING)).unwrap().1;
        assert_eq!(anch_dir.dict_get("services"), Some(&PyRep::Dict(vec![])));
        assert_eq!(rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)]));
        assert_eq!(rpc(&ctx, &mut grunt_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_HUB)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));
        let PyRep::Dict(vip_map) = rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
        assert_eq!(vip_map.len(), 1);
        assert_eq!(vip_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
        assert_eq!(vip_map[0].1.dict_get("profileID"), Some(&PyRep::None));
        assert_eq!(rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));
        let PyRep::Dict(mkt_map) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
        assert_eq!(mkt_map.len(), 1);
        assert_eq!(mkt_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
        let PyRep::List(map_rows) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap() else { panic!("List") };
        assert_eq!(map_rows.len(), 3);
        let hub_map_row = map_rows.iter().find(|r| format!("{r:?}").contains(&ST_HUB.to_string())).unwrap();
        assert!(format!("{hub_map_row:?}").contains("5") && format!("{hub_map_row:?}").contains("4"));
        assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));
        assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![]));
        assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)])]).await.unwrap(), PyRep::List(vec![]));
    }
}

pub(crate) mod palatine_keepstar_cluster_uniqueness {
    // Integration tests for open `feat:structures.*` parity rows (Batch 38, Part 1).


    use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::station_structure::structure_lifecycle as life;
    use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use glam::DVec3;
    use std::collections::HashMap;

    const OWNER: i64 = 90_000_001;
    const OTHER_PILOT: i64 = 90_000_002;
    const JITA: u32 = 30_000_144;

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id, owner_id: owner, location_id: location,
                flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default()
            });
            id
        })
    }

    async fn pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("Select");
        ctx.advance_secs(12);
        rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())]).await.expect("AddCorporation");
        ctx.advance_secs(7 * 86_400 + 12);
        let ship_id = sess.ship_id.expect("ship");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = JITA;
                ch.station_id = None;
            }
            if let Some(it) = snap.items.get_mut(&ship_id) {
                it.location_id = i64::from(JITA);
            }
        });
        sess.solar_system_id = Some(JITA);
        sess.station_id = None;
        let pos = DVec3::new(0.0, 5.0e9, 0.0);
        let _ = ctx.systems.get_or_spawn(JITA).spawn_ball(Ball {
            id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        sess
    }

    async fn anchor_upwell(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
        let ship = sess.ship_id.expect("ship");
        let char_id = sess.character_id.expect("char");
        grant(ctx, char_id, ship, type_id, InventoryFlag::Cargo, 1);
        rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
    }

    fn extract_id(rep: &PyRep) -> i64 {
        rep.as_i64().unwrap_or_else(|| rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID"))
    }

    #[tokio::test]
    async fn test_palatine_keepstar_cluster_uniqueness() {

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 1, OWNER, "Palatine Corp", "PALA").await;
        let null_sys = ctx.sde.solar_systems.values()
            .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000 && s.region_id != life::POCHVEN_REGION_ID)
            .map(|s| s.solar_system_id)
            .expect("nullsec system");
        sess.solar_system_id = Some(null_sys);
        let ship = sess.ship_id.expect("ship");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.solar_system_id = null_sys;
            }
        });
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_meta(
            Ball { id: ship, type_id: 587, position: DVec3::new(9.0e12, 9.0e12, 9.0e12), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_ship_combat_state(OWNER, ship, 587),
            ctx.build_ship_slim_meta(OWNER, 587),
        ).await;

        let first = anchor_upwell(&ctx, &mut sess, life::PALATINE_KEEPSTAR_TYPE_ID, "Palatine One").await;
        assert!(first.is_ok(), "first Palatine Keepstar should anchor: {first:?}");

        ctx.systems.get_or_spawn(null_sys).set_ball_position(ship, DVec3::new(9.0e12, 9.0e12 + 5.0e9, 9.0e12)).await;

        let second = anchor_upwell(&ctx, &mut sess, life::PALATINE_KEEPSTAR_TYPE_ID, "Palatine Two").await;
        let err = second.expect_err("second Palatine Keepstar must be rejected");
        assert_eq!(err.code, "PalatineKeepstarLimitReached");
    }

    #[tokio::test]
    async fn test_quantum_core_gate_and_anchoring_onlining_phases() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 2, OWNER, "Core Gate Corp", "CORE").await;

        // Verify all 8 Upwell hulls require distinct matching Quantum Cores
        let hulls = [35832u32, 35833, 35834, 35825, 35826, 35827, 35835, 35836];
        let mut core_ids = std::collections::HashSet::new();
        for h in hulls {
            let cid = life::structure_profile(&ctx, h).expect("hull profile").core_type_id.expect("core_type_id");
            assert!(core_ids.insert(cid), "hull {h} must have a unique Quantum Core type");
        }

        // 1. Deployed structure starts in AnchorVulnerable (15 min) with hull HP only
        let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Alpha").await.expect("Anchor Astrahus"));
        let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
        let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_ANCHOR_VULNERABLE);
        assert_eq!(st.shield_hp, 0.0);
        assert_eq!(st.armor_hp, 0.0);
        assert_eq!(st.hull_hp, profile.hull_hp);

        // Pause while taking >= 10% of damage cap during AnchorVulnerable
        let before_until = st.reinforced_until_unix;
        let dmg_pause = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 600.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("damage");
            save_ext_state(snap, &ext);
            out
        });
        assert!(dmg_pause.repair_paused, "qualifying damage must pause AnchorVulnerable timer");
        assert!(dmg_pause.reinforced_until_unix > before_until);

        // Killable without timers during AnchorVulnerable
        let doomed_id = 770_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(doomed_id, UpwellStructureRecord {
                structure_id: doomed_id, type_id: 35832, owner_corp_id: st.owner_corp_id,
                solar_system_id: JITA, name: "Doomed Anchor".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_ANCHOR_VULNERABLE.into(), quantum_core_installed: false, fuel_blocks: 0,
                shield_hp: 0.0, armor_hp: 0.0, hull_hp: 100.0, reinforced_until_unix: ctx.now_unix() + 900,
                last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, doomed_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("kill");
            save_ext_state(snap, &ext);
            assert_eq!(out.state, life::STATE_DESTROYED);
            assert!(out.destruction.is_some(), "structure in AnchorVulnerable must die without reinforcement timers");
        });

        // 2. Advance past AnchorVulnerable -> Anchoring (24 h invulnerable)
        ctx.advance_secs(life::ANCHOR_VULNERABLE_SECS + 10);
        rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
        let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_ANCHORING);
        let invuln_err = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix())
        }).expect_err("Anchoring must be invulnerable");
        assert_eq!(invuln_err.code, "StructureReinforcedInvulnerable");

        // 3. Advance 24 h -> NoCore; service module cannot be brought online without Quantum Core
        ctx.advance_secs(life::ANCHORING_SECS + 1);
        rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
        let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_NO_CORE);

        let srv_id = grant(&ctx, st.owner_corp_id, st_id, 35894, InventoryFlag::ServiceSlot0, 1);
        let online_no_core = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(srv_id)]).await;
        assert_eq!(online_no_core.expect_err("service module must not online without Quantum Core").code, "QuantumCoreRequired");

        // Wrong-hull Quantum Core (Fortizar core in Astrahus) must NOT install
        let fortizar_core = life::structure_profile(&ctx, 35833).expect("Fortizar").core_type_id.expect("Fortizar core");
        let wrong_core_item = grant(&ctx, st.owner_corp_id, st_id, fortizar_core, InventoryFlag::StructureDeedBay, 1);
        rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
        let st_still_nocore = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st_still_nocore.state, life::STATE_NO_CORE);
        assert!(!st_still_nocore.quantum_core_installed);
        ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&wrong_core_item); });

        // 4. Install matching Astrahus Quantum Core (56201) -> Onlining (15 min)
        let core_type = profile.core_type_id.expect("Astrahus core type");
        grant(&ctx, st.owner_corp_id, st_id, core_type, InventoryFlag::StructureDeedBay, 1);
        rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
        let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_ONLINING);
        assert!(st.quantum_core_installed);

        // Pause while taking >= 10% of damage cap during Onlining
        let onlining_until = st.reinforced_until_unix;
        let online_pause = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_id, 600.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("damage");
            save_ext_state(snap, &ext);
            out
        });
        assert!(online_pause.repair_paused, "qualifying damage must pause Onlining timer");
        assert!(online_pause.reinforced_until_unix > onlining_until);

        // Killable without timers during Onlining
        let doomed_online_id = 770_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(doomed_online_id, UpwellStructureRecord {
                structure_id: doomed_online_id, type_id: 35832, owner_corp_id: st.owner_corp_id,
                solar_system_id: JITA, name: "Doomed Onlining".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_ONLINING.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 0.0, armor_hp: 0.0, hull_hp: 100.0, reinforced_until_unix: ctx.now_unix() + 900,
                last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, doomed_online_id, 500.0, 1.0, OTHER_PILOT, ctx.now_unix()).expect("kill");
            save_ext_state(snap, &ext);
            assert_eq!(out.state, life::STATE_DESTROYED);
            assert!(out.destruction.is_some(), "structure in Onlining must die without reinforcement timers");
        });

        // Advance past Onlining -> ShieldVulnerable (structure, armor, and shield fully healed)
        ctx.advance_secs(life::ONLINING_SECS + 10);
        rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await.expect("sync");
        let st = ctx.db.with_snapshot(|s| s.structures.get(&st_id).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
        assert!(st.shield_hp >= profile.shield_hp);
        assert!(st.armor_hp >= profile.armor_hp);
        assert!(st.hull_hp >= profile.hull_hp);

        // 5. Destroy structure -> Quantum Core drops with 100% certainty in space/wreck
        let destroy_out = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let out = life::destroy_structure(&ctx, snap, &mut ext, st_id, OTHER_PILOT, ctx.now_unix());
            save_ext_state(snap, &ext);
            out
        });
        let dropped_core = destroy_out.dropped_core_item_id.expect("Quantum Core must drop 100% on destruction");
        let core_exists = ctx.db.with_snapshot(|snap| snap.items.get(&dropped_core).is_some_and(|i| i.type_id == core_type));
        assert!(core_exists, "Quantum Core item must exist in dropped items");
    }

    #[tokio::test]
    async fn test_fitting_state_locks_and_pochven_anchoring() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 3, OWNER, "Fit Lock Corp", "FLCK").await;

        let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Fit").await.expect("Anchor"));
        let profile = life::structure_profile(&ctx, 35832).expect("profile");
        let mod_id = grant(&ctx, OWNER, st_id, 35894, InventoryFlag::Hangar, 1);

        // Refit refused while AnchorVulnerable -> StructureFittingLockedInState
        let err_anchor = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
            .expect_err("refit must be refused while AnchorVulnerable");
        assert_eq!(err_anchor.code, "StructureFittingLockedInState");

        // Refit refused while Onlining -> StructureFittingLockedInState
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&st_id) {
                st.state = life::STATE_ONLINING.into();
            }
        });
        let err_onlining = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
            .expect_err("refit must be refused while Onlining");
        assert_eq!(err_onlining.code, "StructureFittingLockedInState");

        // Refit refused while Reinforced -> StructureFittingLockedInState
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&st_id) {
                st.state = life::STATE_ARMOR_REINFORCED.into();
            }
        });
        let err_reinf = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
            .expect_err("refit must be refused while Reinforced");
        assert_eq!(err_reinf.code, "StructureFittingLockedInState");

        // Refit refused while Repairing -> StructureRepairingCannotRefit
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&st_id) {
                st.state = life::STATE_SHIELD_VULNERABLE.into();
                st.shield_hp = profile.shield_hp;
                st.armor_hp = profile.armor_hp;
                st.hull_hp = profile.hull_hp;
            }
            let mut ext = load_ext_state(snap);
            ext.lifecycle.entry(st_id).or_default().repair_until_unix = Some(ctx.now_unix() + 600);
            save_ext_state(snap, &ext);
        });
        let err_repairing = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
            .expect_err("refit must be refused while repairing");
        assert_eq!(err_repairing.code, "StructureRepairingCannotRefit");

        // Refit refused while damaged / under attack -> StructureDamagedCannotRefit
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            ext.lifecycle.entry(st_id).or_default().repair_until_unix = None;
            save_ext_state(snap, &ext);
            if let Some(st) = snap.structures.get_mut(&st_id) {
                st.shield_hp = profile.shield_hp * 0.85;
            }
        });
        let err_damaged = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await
            .expect_err("refit must be refused when shields < 100%");
        assert_eq!(err_damaged.code, "StructureDamagedCannotRefit");

        // Transition to ShieldVulnerable with full shields -> refit succeeds
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&st_id) {
                st.shield_hp = profile.shield_hp;
            }
        });
        let ok_refit = rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await;
        assert!(ok_refit.is_ok(), "refit must succeed at full shields in ShieldVulnerable: {ok_refit:?}");

        // Pochven anchoring restrictions: Upwell refused, existing ones remain, Customs Office requires >= 1.5 Triglavian standing
        let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).expect("Pochven system");
        let existing_poch_id = 770_099;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(existing_poch_id, UpwellStructureRecord {
                structure_id: existing_poch_id, type_id: 35832, owner_corp_id: 98_000_001,
                solar_system_id: pochven_sys, name: "Existing Pochven Astrahus".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: profile.shield_hp, armor_hp: profile.armor_hp, hull_hp: profile.hull_hp,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        });

        sess.solar_system_id = Some(pochven_sys);
        let ship = sess.ship_id.expect("ship");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.solar_system_id = pochven_sys;
            }
        });
        let _ = ctx.systems.get_or_spawn(pochven_sys).spawn_ball_with_meta(
            Ball { id: ship, type_id: 587, position: DVec3::new(9.0e12, 9.0e12, 9.0e12), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_ship_combat_state(OWNER, ship, 587),
            ctx.build_ship_slim_meta(OWNER, 587),
        ).await;
        let poch_upwell = anchor_upwell(&ctx, &mut sess, 35832, "Pochven Astrahus").await;
        assert_eq!(poch_upwell.expect_err("Pochven Upwell must fail").code, "AnchoringDisallowedInPochven");

        // Existing Pochven Upwell structure remains intact
        life::sync_all_structures(&ctx);
        let poch_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(existing_poch_id)]).await;
        assert!(poch_info.is_ok(), "existing Upwell structure in Pochven must remain: {poch_info:?}");

        let planet_pos = ctx.sde.celestials_in_system(pochven_sys)
            .into_iter()
            .find(|c| c.group_id == 7)
            .map(|c| c.position)
            .unwrap_or(DVec3::ZERO);
        ctx.systems.get_or_spawn(pochven_sys).set_ball_position(ship, planet_pos + DVec3::new(50_000_000.0, 0.0, 0.0)).await;
        let gantry_id = grant(&ctx, OWNER, ship, 3962, InventoryFlag::Cargo, 1);
        let low_standing = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id)]).await;
        assert_eq!(low_standing.expect_err("Pochven POCO with < 1.5 Trig standing must fail").code, "InsufficientTriglavianStanding");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.standings.insert(500_026, 2.0);
            }
        });
        let ok_standing = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id)]).await;
        assert!(ok_standing.is_ok(), "Pochven POCO with >= 1.5 Trig standing should succeed: {ok_standing:?}");
    }

    #[tokio::test]
    async fn test_reinforce_hour_hack_reveal_and_ewar_immunity() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 4, OWNER, "Hack Owner Corp", "HACK").await;
        let st_id = extract_id(&anchor_upwell(&ctx, &mut sess, 35832, "Astrahus Hack").await.expect("Anchor"));

        // Spawn structure ball & hacker ship ball in Jita
        let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures.get(&st_id).expect("st").position));
        let jita_sys = ctx.systems.get_or_spawn(JITA);
        let _ = jita_sys.spawn_ball(Ball {
            id: st_id, type_id: 35832, position: st_pos, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0e9, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OTHER_PILOT) {
                ch.user_id = 1;
                ch.solar_system_id = JITA;
                ch.station_id = None;
            }
        });
        let mut hacker_sess = SessionState::new(5);
        rpc(&ctx, &mut hacker_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
        rpc(&ctx, &mut hacker_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OTHER_PILOT)]).await.expect("Select");
        ctx.advance_secs(12);
        hacker_sess.solar_system_id = Some(JITA);
        hacker_sess.station_id = None;
        let hacker_ship = hacker_sess.ship_id.expect("hacker_ship");
        let _ = jita_sys.spawn_ball(Ball {
            id: hacker_ship, type_id: 587, position: st_pos + DVec3::new(900.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = jita_sys.add_target_lock(hacker_ship, eve_destiny::TargetLock {
            target_id: st_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
        }).await;

        // EWAR immunity: activate ECM, dampener, weapon disruptor, target painter, web, warp scrambler, and energy neutralizer
        let ewar_groups = [201u32, 208, 291, 379, 65, 52, 71];
        for (idx, grp) in ewar_groups.into_iter().enumerate() {
            let mod_tid = ctx.sde.types.values().find(|t| t.group_id == grp).map(|t| t.type_id).unwrap_or_else(|| panic!("missing EWAR group {grp}"));
            let flag = InventoryFlag::from_u16(19 + idx as u16);
            let mod_id = grant(&ctx, OTHER_PILOT, hacker_ship, mod_tid, flag, 1);
            let act = rpc(&ctx, &mut hacker_sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("default".into()), PyRep::Integer(st_id)]).await
                .unwrap_or_else(|e| panic!("EWAR group {grp} activate failed: {e:?}"));
            assert_eq!(act.dict_get("immune"), Some(&PyRep::Bool(true)), "structure must be immune to EWAR group {grp}");
            assert_eq!(act.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(false)), "EWAR group {grp} must have no effect");
        }

        // Non-member cannot see reinforce hour before hack
        let before = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo before");
        assert_eq!(before.dict_get("reinforceHour"), Some(&PyRep::None), "reinforceHour must be hidden before hack");

        // Data Analyzer hack reveals reinforce hour to hacker
        let _data_mod = grant(&ctx, OTHER_PILOT, hacker_ship, 22175, InventoryFlag::MedSlot7, 1);
        let start = rpc(&ctx, &mut hacker_sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect("StartNewGameInstance");
        assert_eq!(start.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
        let mut won = false;
        for _pass in 0..8 {
            for tile in 1..24 {
                for _ in 0..5 {
                    if let Ok(step) = rpc(&ctx, &mut hacker_sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(st_id), PyRep::Integer(tile)]).await {
                        if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                            won = true;
                            break;
                        }
                    } else {
                        break;
                    }
                }
                if won { break; }
            }
            if won { break; }
        }
        assert!(won, "Data Analyzer hack must complete with Won");

        let after = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("GetStructureInfo after");
        assert!(after.dict_get("reinforceHour").and_then(|v| v.as_i64()).is_some(), "reinforceHour must be revealed after hack");
    }
}

pub(crate) mod reinforce_durations_exit_time_rules {
    // Integration tests for open `feat:structures.*` parity rows (Batch 38, Part 2).


    use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::space_destiny::space_nav_travel::sync_tether;
    use eve_server::services::station_structure::structure_lifecycle as life;
    use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use glam::DVec3;
    use std::collections::HashMap;

    const OWNER: i64 = 90_000_001;
    const OTHER_PILOT: i64 = 90_000_002;
    const JITA: u32 = 30_000_142;
    const FULCRUM_STATION_ID: i64 = 60_015_187;

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id, owner_id: owner, location_id: location,
                flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default()
            });
            id
        })
    }

    async fn pilot_in_space_user(ctx: &ServerContext, sess_id: u64, user: &str, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String("demo-secret".into())]).await.expect("Login");
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("Select");
        ctx.advance_secs(12);
        rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())]).await.expect("AddCorporation");
        ctx.advance_secs(12);
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
        let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.position).unwrap_or(DVec3::ZERO);
        let pos = st_pos + DVec3::new(0.0, 5.0e9, 0.0);
        ctx.systems.get_or_spawn(JITA).set_ball_position(sess.ship_id.expect("ship"), pos).await;
        sess
    }

    async fn pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64, corp_name: &str, ticker: &str) -> SessionState {
        pilot_in_space_user(ctx, sess_id, "demo", char_id, corp_name, ticker).await
    }

    #[test]
    fn test_reinforce_durations_and_exit_time_rules() {

        // Armor reinforcement durations table
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(216_000));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(302_400));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(86_400));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(86_400));
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), None);
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::Abandoned), None);
        assert_eq!(life::armor_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::LowPower), None);

        // Hull reinforcement durations table
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::Wormhole, life::PowerState::FullPower), Some(129_600));
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(216_000));
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::XLarge, life::SpaceClass::HighSec, life::PowerState::FullPower), Some(388_800));
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::WarHq, life::PowerState::FullPower), Some(86_400));
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Flex, life::SpaceClass::LowNullSec, life::PowerState::FullPower), Some(1_800));
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Medium, life::SpaceClass::HighSec, life::PowerState::FullPower), None);
        assert_eq!(life::hull_reinforcement_secs(life::HullClass::Large, life::SpaceClass::HighSec, life::PowerState::Abandoned), None);

        // Jitter never rolling over UTC day
        let base_exit = 1_700_006_400;
        let jittered = life::apply_reinforce_jitter(base_exit, -10_800, 10_800);
        assert_eq!(jittered / 86_400, base_exit / 86_400, "jitter must never roll exit to previous UTC day");
        let late_exit = 1_700_092_000;
        let jittered_late = life::apply_reinforce_jitter(late_exit, 10_800, 10_800);
        assert_eq!(jittered_late / 86_400, late_exit / 86_400, "jitter must never roll exit to next UTC day");

        // In-flow transitions: Large structure (Fortizar 35833) in Low-sec with online service module
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let lowsec_sys = ctx.sde.solar_systems.values().find(|s| s.security_status > 0.0 && s.security_status < 0.45).map(|s| s.solar_system_id).expect("lowsec");
        let fort_prof = life::structure_profile(&ctx, 35833).expect("Fortizar profile");
        let fort_id = 780_001;
        let t0 = 1_700_010_000u64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(fort_id, UpwellStructureRecord {
                structure_id: fort_id, type_id: 35833, owner_corp_id: 98_000_001,
                solar_system_id: lowsec_sys, name: "Fortizar Flow".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 100.0, armor_hp: fort_prof.armor_hp, hull_hp: fort_prof.hull_hp,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(fort_id + 1, InventoryItemRecord {
                item_id: fort_id + 1, type_id: 35892, owner_id: 98_000_001, location_id: fort_id,
                flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(fort_id + 1, true);
            snap.structure_vulnerability_windows.insert(fort_id, (18, 0));
            let mut ext = load_ext_state(snap);
            // Simulate a 2-hour shield fight before depletion: exit time must be anchored to shield_fight_started_unix (t0), not fight duration
            ext.lifecycle.entry(fort_id).or_default().shield_fight_started_unix = Some(t0);
            let out_sh = life::apply_structure_damage(&ctx, snap, &mut ext, fort_id, 500.0, 1.0, OTHER_PILOT, t0 + 7_200).expect("deplete shield");
            assert_eq!(out_sh.state, life::STATE_ARMOR_REINFORCED);
            let unjittered_armor = ext.lifecycle.get(&fort_id).and_then(|l| l.unjittered_reinforce_exit_unix).expect("unjittered armor exit");
            assert_eq!((unjittered_armor % 86_400) / 3_600, 18, "exit hour must match owner's chosen hour (18:00 UTC)");
            assert_eq!(unjittered_armor, life::next_reinforce_exit(t0 + 86_400, 18), "armor exit must be computed from shield fight start, not fight duration");

            // Transition to ArmorVulnerable and deplete armor -> HullReinforced anchored to shield fight date (t0 + 24h + 2.5d)
            let armor_exit = out_sh.reinforced_until_unix;
            life::sync_structure(&ctx, snap, &mut ext, fort_id, armor_exit + 1);
            assert_eq!(snap.structures.get(&fort_id).unwrap().state, life::STATE_ARMOR_VULNERABLE);
            if let Some(s) = snap.structures.get_mut(&fort_id) { s.armor_hp = 100.0; }
            let out_ar = life::apply_structure_damage(&ctx, snap, &mut ext, fort_id, 500.0, 1.0, OTHER_PILOT, armor_exit + 3_600).expect("deplete armor");
            assert_eq!(out_ar.state, life::STATE_HULL_REINFORCED);
            let unjittered_hull = ext.lifecycle.get(&fort_id).and_then(|l| l.unjittered_reinforce_exit_unix).expect("unjittered hull exit");
            assert_eq!(unjittered_hull, life::next_reinforce_exit(t0 + 86_400 + 216_000, 18), "shield fight date must fix later hull timer date");
            save_ext_state(snap, &ext);
        });

        // Low-power Large structure skips armor reinforcement and goes straight to ArmorVulnerable
        let lp_id = 780_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(lp_id, UpwellStructureRecord {
                structure_id: lp_id, type_id: 35833, owner_corp_id: 98_000_001,
                solar_system_id: lowsec_sys, name: "LowPower Fortizar".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0,
                shield_hp: 100.0, armor_hp: fort_prof.armor_hp, hull_hp: fort_prof.hull_hp,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, lp_id, 500.0, 1.0, OTHER_PILOT, t0).expect("deplete low-power shield");
            assert_eq!(out.state, life::STATE_ARMOR_VULNERABLE, "low-power Large/XL must skip armor reinforcement");
            save_ext_state(snap, &ext);
        });

        // Medium structure (Astrahus 35832) has no hull timer -> ArmorVulnerable goes straight to HullVulnerable
        let med_id = 780_003;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(med_id, UpwellStructureRecord {
                structure_id: med_id, type_id: 35832, owner_corp_id: 98_000_001,
                solar_system_id: lowsec_sys, name: "Medium Astrahus".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_ARMOR_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 0.0, armor_hp: 100.0, hull_hp: 100_000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, med_id, 500.0, 1.0, OTHER_PILOT, t0).expect("deplete medium armor");
            assert_eq!(out.state, life::STATE_HULL_VULNERABLE, "Medium structure must skip hull reinforcement");
            save_ext_state(snap, &ext);
        });
    }

    #[test]
    fn test_repair_timer_pause_threshold_and_vulnerability_window() {

        assert_eq!(life::REPAIR_SHIELD_ARMOR_SECS, 900);
        assert_eq!(life::REPAIR_HULL_SECS, 1_800);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let profile = life::structure_profile(&ctx, 35832).expect("Astrahus profile");
        let now = ctx.now_unix();

        // 1. Armor vulnerability window (15 min): pauses on >= 10% damage cap, does NOT pause below 10%, repairs to 100%
        let st_armor = 781_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(st_armor, UpwellStructureRecord {
                structure_id: st_armor, type_id: 35832, owner_corp_id: 98_000_001,
                solar_system_id: JITA, name: "Armor Repair Test".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_ARMOR_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: 0.0, armor_hp: profile.armor_hp * 0.5, hull_hp: profile.hull_hp,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            ext.lifecycle.entry(st_armor).or_default().repair_until_unix = Some(now + life::REPAIR_SHIELD_ARMOR_SECS);
            // Qualifying damage (>= 10% damage cap) pauses repair timer
            let paused = life::apply_structure_damage(&ctx, snap, &mut ext, st_armor, 600.0, 1.0, OTHER_PILOT, now + 100).expect("qualifying");
            assert!(paused.repair_paused, "damage >= 10% of damage cap must pause armor repair timer");
            let after_pause = ext.lifecycle.get(&st_armor).and_then(|l| l.repair_until_unix).expect("repair_until");
            assert!(after_pause > now + life::REPAIR_SHIELD_ARMOR_SECS);

            // Below-threshold damage (< 10% damage cap) does NOT pause repair timer
            let not_paused = life::apply_structure_damage(&ctx, snap, &mut ext, st_armor, 200.0, 1.0, OTHER_PILOT, now + 105).expect("below threshold");
            assert!(!not_paused.repair_paused, "damage < 10% of damage cap must not pause repair timer");
            assert_eq!(ext.lifecycle.get(&st_armor).and_then(|l| l.repair_until_unix), Some(after_pause));

            // Uninterrupted countdown to 0 repairs all hull, armor, and shields back to 100% and returns to ShieldVulnerable
            life::sync_structure(&ctx, snap, &mut ext, st_armor, after_pause + 1);
            save_ext_state(snap, &ext);
        });
        let st = ctx.db.with_snapshot(|snap| snap.structures.get(&st_armor).cloned().expect("structure"));
        assert_eq!(st.state, life::STATE_SHIELD_VULNERABLE);
        assert!(st.shield_hp >= profile.shield_hp);
        assert!(st.armor_hp >= profile.armor_hp);
        assert!(st.hull_hp >= profile.hull_hp);

        // 2. Hull vulnerability window (30 min = 1800s) & Abandoned structure rules
        let st_hull = 781_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(st_hull, UpwellStructureRecord {
                structure_id: st_hull, type_id: 35832, owner_corp_id: 98_000_001,
                solar_system_id: JITA, name: "Abandoned Hull Repair Test".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_HULL_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 0,
                shield_hp: 0.0, armor_hp: 0.0, hull_hp: profile.hull_hp * 0.5,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            let mut ext = load_ext_state(snap);
            ext.lifecycle.entry(st_hull).or_default().low_power_since_unix = Some(now - life::ABANDON_AFTER_LOW_POWER_SECS - 10);
            assert_eq!(life::power_state(&ctx, snap, &ext, snap.structures.get(&st_hull).unwrap(), now), life::PowerState::Abandoned);
            let out = life::apply_structure_damage(&ctx, snap, &mut ext, st_hull, 200.0, 1.0, OTHER_PILOT, now).expect("abandoned hull damage");
            assert!(!out.repair_paused);
            let hull_repair_until = ext.lifecycle.get(&st_hull).and_then(|l| l.repair_until_unix).expect("hull repair_until");
            assert_eq!(hull_repair_until, now + life::REPAIR_HULL_SECS, "hull repair timer must be 30 min (1800s)");
            // Not repaired after 15 min
            life::sync_structure(&ctx, snap, &mut ext, st_hull, now + 900);
            assert_eq!(snap.structures.get(&st_hull).unwrap().state, life::STATE_HULL_VULNERABLE);
            // Repaired to 100% ShieldVulnerable after 30 min
            life::sync_structure(&ctx, snap, &mut ext, st_hull, hull_repair_until + 1);
            save_ext_state(snap, &ext);
        });
        let st_h = ctx.db.with_snapshot(|snap| snap.structures.get(&st_hull).cloned().expect("structure"));
        assert_eq!(st_h.state, life::STATE_SHIELD_VULNERABLE);
        assert!(st_h.shield_hp >= profile.shield_hp);
        assert!(st_h.armor_hp >= profile.armor_hp);
        assert!(st_h.hull_hp >= profile.hull_hp);
    }

    #[tokio::test]
    async fn test_tether_eligibility_restrictions_and_docking_lock() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 6, OWNER, "Tether Corp", "TETH").await;
        let ship = sess.ship_id.expect("ship");
        let st_pos = ctx.systems.get_or_spawn(JITA).get_ball_snapshot(ship).await.expect("q").expect("b").1.position;
        let profile = life::structure_profile(&ctx, 35832).expect("profile");
        let st_id = 782_001;
        let owner_corp = ctx.db.get_character(OWNER).expect("owner").corporation_id;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(st_id, UpwellStructureRecord {
                structure_id: st_id, type_id: 35832, owner_corp_id: owner_corp,
                solar_system_id: JITA, name: "Astrahus Tether".into(), position: st_pos.to_array(),
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                shield_hp: profile.shield_hp, armor_hp: profile.armor_hp, hull_hp: profile.hull_hp,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        });

        // Damage shield, armor, and hull (using expect, never if-let) and verify passive repair + untargetability
        let sys = ctx.systems.get_or_spawn(JITA);
        let mut cs = sys.get_combat_state(ship).await.expect("query").expect("combat state");
        cs.shield_hp = cs.max_shield_hp * 0.5;
        cs.armor_hp = cs.max_armor_hp * 0.5;
        cs.structure_hp = cs.max_structure_hp * 0.5;
        sys.set_combat_state(ship, cs).await.expect("set combat state");

        sync_tether(&ctx, OWNER, JITA, ship).await;
        let cs_teth = sys.get_combat_state(ship).await.expect("query").expect("cs");
        assert!(cs_teth.is_tethered, "ship with docking rights at cored structure must tether");
        assert!(cs_teth.shield_hp > cs_teth.max_shield_hp * 0.5, "tethered ship must receive passive shield repair");
        assert!(cs_teth.armor_hp > cs_teth.max_armor_hp * 0.5, "tethered ship must receive passive armor repair");
        assert!(cs_teth.structure_hp > cs_teth.max_structure_hp * 0.5, "tethered ship must receive passive hull repair");

        // Tethered ship cannot be targeted
        let hostile_ship = 999_001;
        let mut hostile_sess = SessionState::new(7);
        hostile_sess.user_id = Some(2);
        hostile_sess.character_id = Some(OTHER_PILOT);
        hostile_sess.solar_system_id = Some(JITA);
        hostile_sess.ship_id = Some(hostile_ship);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OTHER_PILOT) {
                ch.active_ship_id = hostile_ship;
                ch.solar_system_id = JITA;
            }
            snap.items.insert(hostile_ship, InventoryItemRecord {
                item_id: hostile_ship, type_id: 587, owner_id: OTHER_PILOT, location_id: i64::from(JITA),
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        });
        let _ = sys.spawn_ball_with_meta(
            Ball { id: hostile_ship, type_id: 587, position: st_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_ship_combat_state(OTHER_PILOT, hostile_ship, 587),
            ctx.build_ship_slim_meta(OTHER_PILOT, 587),
        ).await;
        let lock_err = rpc(&ctx, &mut hostile_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship)]).await
            .expect_err("tethered ship must not be targetable");
        assert_eq!(lock_err.code, "TargetInvulnerable");

        // Blocked actions while tethered: offensive module / smartbomb -> CantActivateWhileTethered
        let sb_tid = ctx.sde.types.values().find(|t| t.group_id == 72).map(|t| t.type_id).expect("smartbomb");
        let sb_id = grant(&ctx, OWNER, ship, sb_tid, InventoryFlag::HiSlot0, 1);
        let sb_err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_id), PyRep::String("default".into())]).await
            .expect_err("smartbomb must be refused while tethered");
        assert_eq!(sb_err.code, "CantActivateWhileTethered");

        // Tether breaks on: locking a target
        let _ = sys.add_target_lock(ship, eve_destiny::TargetLock {
            target_id: hostile_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
        }).await;
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break on locking a target");
        let _ = sys.remove_target_lock(ship, hostile_ship).await;
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered);

        // Tether breaks on: activating a bastion / cyno module (group 515 / 658)
        let cyno_tid = ctx.sde.types.values().find(|t| t.group_id == 658).map(|t| t.type_id).expect("cyno");
        let cyno_id = grant(&ctx, OWNER, ship, cyno_tid, InventoryFlag::HiSlot1, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(cyno_id); });
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break on cyno/bastion module");
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&cyno_id); });
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered);

        // Tether breaks on: leaving 10 km-from-docking-ring range
        sys.set_ball_position(ship, st_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether must break beyond docking perimeter");
        sys.set_ball_position(ship, st_pos).await;

        // Tether refused for: structure without quantum core, unpiloted ship, warp-scrambled ship
        ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false; });
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused without quantum core");
        ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true; });

        // Unpiloted ship (char_id == 0)
        sync_tether(&ctx, 0, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused for unpiloted ship");

        // Warp-scrambled ship
        let mut cs_scram = sys.get_combat_state(ship).await.expect("q").expect("cs");
        cs_scram.warp_scramble_strength = 2;
        sys.set_combat_state(ship, cs_scram).await.expect("set scram");
        sync_tether(&ctx, OWNER, JITA, ship).await;
        assert!(!sys.get_combat_state(ship).await.expect("q").expect("cs").is_tethered, "tether refused when warp-scrambled");
    }

    /// `feat:structures.fulcrum-alignment-services`
    #[tokio::test]
    async fn test_asset_safety_fulcrum_and_reprocessing_rigs() {
        // parity: feat:structures.fulcrum-alignment-services

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = pilot_in_space(&ctx, 8, OWNER, "Asset Fulcrum Corp", "ASFC").await;

        // 1. Asset safety: 20-day transit timer, 0.5% same-system fee, 15% cross-system/lowsec fee, wormhole loot drop
        assert_eq!(life::ASSET_SAFETY_AUTO_DELIVERY_SECS, 20 * 86_400);
        let lowsec_station_rec = ctx.sde.stations.values()
            .find(|st| ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45))
            .expect("lowsec station");
        let lowsec_sys = lowsec_station_rec.solar_system_id;
        let lowsec_station = lowsec_station_rec.station_id;
        let null_sys = 30_001_161; // HED-GP (stationless nullsec)
        let null_station_id = life::nearest_npc_station_from_pos(&ctx, null_sys, DVec3::ZERO).expect("null nearest lowsec");
        let null_station_sys = ctx.sde.stations.get(&null_station_id).unwrap().solar_system_id;
        assert_ne!(null_station_sys, null_sys, "null-sec auto delivery transits cross-system to low-sec");

        let st_same = 783_001;
        let st_auto = 783_002;
        let st_wh = 783_003;
        let wh_sys = ctx.sde.solar_systems.values().find(|s| s.solar_system_id >= 31_000_000).map(|s| s.solar_system_id).expect("wormhole");

        let (wrap_same, wrap_auto) = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.balance = eve_db::IskCents::from_f64_isk(1_000_000_000.0);
            }
            for (sid, sys_id) in [(st_same, lowsec_sys), (st_auto, null_sys), (st_wh, wh_sys)] {
                snap.structures.insert(sid, UpwellStructureRecord {
                    structure_id: sid, type_id: 35832, owner_corp_id: 98_000_001,
                    solar_system_id: sys_id, name: format!("AS {sid}"), position: [0.0, 0.0, 0.0],
                    state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
                    shield_hp: 100.0, armor_hp: 100.0, hull_hp: 100.0,
                    reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
                });
                snap.items.insert(sid + 50, InventoryItemRecord {
                    item_id: sid + 50, type_id: 587, owner_id: OWNER, location_id: sid,
                    flag: InventoryFlag::Hangar, quantity: 10, singleton: false, ..Default::default()
                });
            }
            let mut ext = load_ext_state(snap);
            let out_same = life::destroy_structure(&ctx, snap, &mut ext, st_same, OTHER_PILOT, ctx.now_unix());
            let out_auto = life::destroy_structure(&ctx, snap, &mut ext, st_auto, OTHER_PILOT, ctx.now_unix());
            let out_wh = life::destroy_structure(&ctx, snap, &mut ext, st_wh, OTHER_PILOT, ctx.now_unix());
            save_ext_state(snap, &ext);
            assert!(out_wh.asset_safety_wrap_ids.is_empty(), "wormhole structure must not create asset safety wraps");
            assert!(out_wh.dropped_item_ids.contains(&(st_wh + 50)), "wormhole structure items must drop as loot");
            (out_same.asset_safety_wrap_ids[0], out_auto.asset_safety_wrap_ids[0])
        });

        // Advance 6 days -> same-system claim charges 0.5% fee
        ctx.advance_secs(6 * 86_400);
        let bal_before_same = ctx.db.get_character(OWNER).unwrap().balance.to_wire_f64();
        let res_same = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_same), PyRep::Integer(lowsec_station)]).await.expect("same-system claim");
        let fee_same = res_same.dict_get("feePaidISK").and_then(|v| v.as_f64()).expect("feePaidISK");
        let bal_after_same = ctx.db.get_character(OWNER).unwrap().balance.to_wire_f64();
        assert!(fee_same > 0.0 && (bal_before_same - bal_after_same - fee_same).abs() < 0.01);

        // Advance 15 more days (total 21 days >= 20 days) -> auto-delivered cross-system to null_station_id, claim charges 15% fee (30x the 0.5% fee!)
        ctx.advance_secs(15 * 86_400);
        let res_auto = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_auto), PyRep::Integer(null_station_id)]).await.expect("auto-delivered claim");
        let fee_auto = res_auto.dict_get("feePaidISK").and_then(|v| v.as_f64()).expect("feePaidISK");
        assert!((fee_auto / fee_same - 30.0).abs() < 0.1, "15% fee must be 30x the 0.5% same-system fee: {fee_auto} vs {fee_same}");

        // 2. Fulcrum alignment services: real ship in space in Zarzakh
        let fulcrum_st = ctx.sde.stations.get(&FULCRUM_STATION_ID).expect("Fulcrum station in SDE");
        let zarzakh_sys = fulcrum_st.solar_system_id;
        let ship = sess.ship_id.expect("ship");
        sess.solar_system_id = Some(zarzakh_sys);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.solar_system_id = zarzakh_sys;
                ch.war_faction_id = Some(500_001); // Amarr militia
            }
        });
        let z_sys = ctx.systems.get_or_spawn(zarzakh_sys);
        let _ = z_sys.spawn_ball_with_meta(
            Ball { id: ship, type_id: 587, position: fulcrum_st.position + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1.0e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_ship_combat_state(OWNER, ship, 587),
            ctx.build_ship_slim_meta(OWNER, 587),
        ).await;

        // Both beyonce.CmdDock and stationSvc.Dock reject Empire militia (500_001..=500_004)
        let empire_dock_beyonce = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(empire_dock_beyonce.expect_err("Fulcrum must refuse Empire militia via beyonce").code, "FulcrumRefusesEmpireMilitia");
        let empire_dock_station = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID), PyRep::Integer(ship)]).await;
        assert_eq!(empire_dock_station.expect_err("Fulcrum must refuse Empire militia via stationSvc").code, "FulcrumRefusesEmpireMilitia");

        // Clear personal militia, enlist corporation in Gallente militia (500_004) -> still refused on both entrypoints
        let owner_corp_id = ctx.db.get_character(OWNER).unwrap().corporation_id;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.war_faction_id = None; }
            snap.fw_runtime.corp_war_factions.insert(owner_corp_id, 500_004);
        });
        assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await.expect_err("corp Empire militia beyonce").code, "FulcrumRefusesEmpireMilitia");
        assert_eq!(rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID), PyRep::Integer(ship)]).await.expect_err("corp Empire militia stationSvc").code, "FulcrumRefusesEmpireMilitia");

        // Clear corp militia -> neutral character with 60s weapons timer refuses docking on both entrypoints
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.corp_war_factions.remove(&owner_corp_id);
        });
        let t_now = ctx.now_unix();
        ctx.crimewatch.write().unwrap().states.entry(OWNER).or_default().weapon_timer_until_unix = t_now + 60;
        let wpn_dock_beyonce = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(wpn_dock_beyonce.expect_err("Fulcrum must refuse weapons timer via beyonce").code, "FulcrumRefusesTimerActive");
        let wpn_dock_station = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID), PyRep::Integer(ship)]).await;
        assert_eq!(wpn_dock_station.expect_err("Fulcrum must refuse weapons timer via stationSvc").code, "FulcrumRefusesTimerActive");
        ctx.crimewatch.write().unwrap().states.entry(OWNER).or_default().weapon_timer_until_unix = 0;

        // Retribution timer refuses docking on both entrypoints
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.disapproval_flags.insert(OWNER, t_now + 300);
        });
        let retrib_dock_beyonce = rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(retrib_dock_beyonce.expect_err("Fulcrum must refuse retribution timer via beyonce").code, "FulcrumRefusesTimerActive");
        let retrib_dock_station = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID), PyRep::Integer(ship)]).await;
        assert_eq!(retrib_dock_station.expect_err("Fulcrum must refuse retribution timer via stationSvc").code, "FulcrumRefusesTimerActive");
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.disapproval_flags.remove(&OWNER);
        });

        // Active 15-minute capsuleer PvP logoff timer (`pvp_timer_until_unix`) with expired weapons timer MUST allow docking
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.states.entry(OWNER).or_default();
            st.weapon_timer_until_unix = 0;
            st.pvp_timer_until_unix = t_now + 900;
            assert!(st.is_pvp_active(t_now) && !st.is_weapon_timer_active(t_now));
        }
        let ok_dock = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STATION_ID), PyRep::Integer(ship)]).await;
        assert!(ok_dock.is_ok(), "pilot with only 15m pvp_timer_until_unix must dock at Fulcrum: {ok_dock:?}");
        sess.station_id = Some(FULCRUM_STATION_ID);

        // Neutral character gets all 5 standard services: repair, fitting, market, insurance, industry
        // 1. Repair (service 13)
        let rep_standing = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(rep_standing.expect("repair standing check"), PyRep::Bool(true));
        let rep_all = rpc(&ctx, &mut sess, "repairSvc", "RepairAll", &[]).await;
        assert!(rep_all.is_ok(), "neutral repair at Fulcrum: {rep_all:?}");

        // 2. Fitting (service 17)
        let fit_standing = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(17), PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(fit_standing.expect("fitting standing check"), PyRep::Bool(true));

        // 3. Market (service 7)
        let mkt_standing = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(7), PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(mkt_standing.expect("market standing check"), PyRep::Bool(true));
        let mkt_tx = rpc(&ctx, &mut sess, "marketProxy", "CharGetTransactions", &[]).await;
        assert!(mkt_tx.is_ok(), "neutral market transactions at Fulcrum: {mkt_tx:?}");

        // 4. Insurance (service 21)
        let ins_standing = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(21), PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(ins_standing.expect("insurance standing check"), PyRep::Bool(true));
        let ins_prices = rpc(&ctx, &mut sess, "insuranceSvc", "GetInsurancePrices", &[]).await;
        assert!(ins_prices.is_ok(), "neutral insurance prices at Fulcrum: {ins_prices:?}");

        // 5. Industry (service 14)
        let ind_standing = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(14), PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(ind_standing.expect("industry standing check"), PyRep::Bool(true));
        let ind_jobs = rpc(&ctx, &mut sess, "industryManager", "GetJobCounts", &[]).await;
        assert!(ind_jobs.is_ok(), "neutral industry job counts at Fulcrum: {ind_jobs:?}");

        // Jump clone needs pirate-enlisted character; corp office needs pirate-enlisted corporation
        let clone_neutral = rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await;
        assert_eq!(clone_neutral.expect_err("Fulcrum clone requires pirate enlistment").code, "FulcrumCloneRequiresPirateEnlistment");
        let office_neutral = rpc(&ctx, &mut sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(office_neutral.expect_err("Fulcrum office requires pirate enlistment").code, "FulcrumOfficeRequiresPirateEnlistment");

        // Direct-enlisted character in Angels (500_011) inside an UN-ENLISTED corporation:
        // - jumpCloneSvc.InstallCloneInStation succeeds (character is pirate-enlisted)
        // - officeManager.RentOffice STILL fails with FulcrumOfficeRequiresPirateEnlistment!
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.war_faction_id = Some(500_011);
            }
            snap.fw_runtime.direct_enlisted_chars.insert(OWNER, 500_011);
            if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
                corp.set_division_balance(1, eve_db::IskCents::from_f64_isk(100_000_000.0));
            }
            snap.skills.entry(OWNER).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
        });
        assert!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await.is_ok(), "pirate-enlisted clone install should succeed");
        let office_char_only = rpc(&ctx, &mut sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION_ID)]).await;
        assert_eq!(office_char_only.expect_err("un-enlisted corp must fail RentOffice even if caller is direct-enlisted").code, "FulcrumOfficeRequiresPirateEnlistment");

        // Enlist the corporation itself in Angel Cartel (500_011) -> officeManager.RentOffice succeeds, deducts fee, creates office item
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.corp_war_factions.insert(owner_corp_id, 500_011);
        });
        let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&owner_corp_id].division_balance(1));
        assert!(rpc(&ctx, &mut sess, "officeManager", "RentOffice", &[PyRep::Integer(FULCRUM_STATION_ID)]).await.is_ok(), "pirate-enlisted corp office rent should succeed");
        ctx.db.with_snapshot(|s| {
            assert!(s.corporations[&owner_corp_id].division_balance(1) < corp_bal_before, "office rental fee deducted");
            assert!(s.items.values().any(|i| i.owner_id == owner_corp_id && i.location_id == FULCRUM_STATION_ID && i.type_id == 27), "office folder created");
        });

        // Authoritative unseeded Tranquility Angel Cartel (1_000_138 / 1_000_436) and Guristas (1_000_127 / 1_000_437) LP store offers
        let angel_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_138)]).await.expect("Angel LP store");
        let PyRep::List(ref a_offers) = angel_rep else { panic!("expected list of Angel offers"); };
        assert!(a_offers.len() >= 250, "Angel Cartel LP store must return real Tranquility catalog");
        let a_off = a_offers.iter().find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3838)).expect("Angel offer 3838");
        assert_eq!(a_off.dict_get("corpID").and_then(|v| v.as_i64()), Some(1_000_138));
        assert_eq!(a_off.dict_get("typeID").and_then(|v| v.as_i64()), Some(20_498));
        assert_eq!(a_off.dict_get("lpCost").and_then(|v| v.as_i64()), Some(23_625));
        assert_eq!(a_off.dict_get("iskCost").and_then(|v| v.as_f64()), Some(23_625_000.0));
        let PyRep::List(ref angel_militia) = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_436)]).await.expect("Malakim Zealots LP store") else { panic!("list"); };
        assert!(angel_militia.len() >= 300 && angel_militia.iter().any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3838)));

        let guristas_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_127)]).await.expect("Guristas LP store");
        let PyRep::List(ref g_offers) = guristas_rep else { panic!("expected list of Guristas offers"); };
        assert!(g_offers.len() >= 300, "Guristas LP store must return real Tranquility catalog");
        let g_off = g_offers.iter().find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3938)).expect("Guristas offer 3938");
        assert_eq!(g_off.dict_get("corpID").and_then(|v| v.as_i64()), Some(1_000_127));
        assert_eq!(g_off.dict_get("typeID").and_then(|v| v.as_i64()), Some(20_121));
        assert_eq!(g_off.dict_get("lpCost").and_then(|v| v.as_i64()), Some(23_625));
        assert_eq!(g_off.dict_get("iskCost").and_then(|v| v.as_f64()), Some(23_625_000.0));
        let PyRep::List(ref guri_militia) = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_437)]).await.expect("Commando Guri LP store") else { panic!("list"); };
        assert!(guri_militia.len() >= 350 && guri_militia.iter().any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3938)));

        // 3. Genuine rigged Upwell Athanor refinery (`35835`, `>= 1_000_000_000_000`) via `reprocessingSvc.GetQuotes`
        // In null-sec (`null_sys`), Athanor (`hull_mult = 1.02`) + T2 M-Set Asteroid Ore Grading Processor II (`46634`,
        // `attr 717 = 0.53`, `attr 2357 = 1.12`) yields `0.53 * 1.12 * 1.02 = 0.605472` (`24_218` Tritanium from `10_000` Veldspar).
        let refinery_id = 1_000_000_889_001i64;
        let ore_id = ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(refinery_id, UpwellStructureRecord {
                structure_id: refinery_id, type_id: 35835, owner_corp_id: owner_corp_id,
                solar_system_id: null_sys, name: "Rigged Null Athanor".into(), position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 200,
                shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(refinery_id + 1, InventoryItemRecord {
                item_id: refinery_id + 1, type_id: 35899, owner_id: owner_corp_id, location_id: refinery_id,
                flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            snap.items.insert(refinery_id + 2, InventoryItemRecord {
                item_id: refinery_id + 2, type_id: 46634, owner_id: owner_corp_id, location_id: refinery_id,
                flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(refinery_id + 1, true);
            snap.module_online_states.insert(refinery_id + 2, true);
            if let Some(ch) = snap.characters.get_mut(&OWNER) {
                ch.solar_system_id = null_sys;
                ch.station_id = None;
                ch.structure_id = Some(refinery_id);
            }
            if let Some(skills) = snap.skills.get_mut(&OWNER) {
                skills.remove(&3385);
                skills.remove(&3389);
                skills.remove(&12195);
            }
            snap.next_entity_id += 1;
            let oid = snap.next_entity_id;
            snap.items.insert(oid, InventoryItemRecord {
                item_id: oid, type_id: 1230, owner_id: OWNER, location_id: refinery_id,
                flag: InventoryFlag::Hangar, quantity: 10_000, singleton: false, ..Default::default()
            });
            oid
        });
        sess.solar_system_id = Some(null_sys);
        sess.station_id = None;
        sess.structure_id = Some(refinery_id);
        let PyRep::List(quotes) = rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(ore_id)])]).await.expect("rigged GetQuotes") else {
            panic!("expected List of quotes from reprocessingSvc.GetQuotes");
        };
        assert_eq!(quotes.len(), 1);
        let rigged_base = quotes[0].dict_get("stationBaseYield").and_then(|v| v.as_f64()).expect("stationBaseYield");
        let expected_rigged = 0.53 * 1.12 * 1.02;
        assert!((rigged_base - expected_rigged).abs() < 1e-6 && rigged_base > 0.51, "rigged base yield {rigged_base} must match {expected_rigged}");
        let PyRep::List(ref outputs) = quotes[0].dict_get("outputs").expect("outputs") else { panic!("outputs list"); };
        let trit_out = outputs.iter().find(|o| o.dict_get("typeID").and_then(|v| v.as_i64()) == Some(34)).expect("Tritanium output");
        assert_eq!(trit_out.dict_get("quantity").and_then(|v| v.as_i64()), Some(24_218));
    }
}

pub(crate) mod insurgency_laundering_roamer_drops_fob {
    // Integration tests for batch 41 exclusive `feat:` parity rows:
    // - `feat:pve.insurgency-laundering`
    // - `feat:pve.insurgency-mining-ambush`
    // - `feat:pve.insurgency-outpost-raid`
    // - `feat:pve.proving-grounds-queue-and-rewards`


    use crate::fw_warzone_insurgency::setup_warzone_chain;
    use crate::sovereignty_equinox_gameplay::{
        lock_target_via_rpc, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
    };
    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::MovementMode;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::with_combat_runtime;
    use eve_server::services::fw_services::{
        HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON,
    };
    use eve_server::services::insurgency_complex_ops::{
        load_ops_state, OutpostRaidSize, LARGE_ADV_OUTPOST_RESPAWN_SECS, MINING_AMBUSH_BASE_LP,
        MINING_AMBUSH_RANSOM_SECS, MINING_AMBUSH_RESPAWN_SECS, TYPE_ANGEL_INSURGENCY_DATA_SEQ,
        TYPE_FEDERATION_CI_OUTPOST_HUB, TYPE_GURISTAS_INSURGENCY_DATA_SEQ,
        TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_IMPERIAL_CI_OUTPOST_HUB, TYPE_ORE_RORQUAL,
        TYPE_ORE_RORQUAL_WRECK, TYPE_REPUBLIC_CI_OUTPOST_HUB, TYPE_STATE_CI_OUTPOST_HUB,
        TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
    };
    use eve_server::services::insurgency_services::{
        add_system_corruption, add_system_suppression, init_insurgency_campaign, FACTION_ANGEL_CARTEL,
        FORECASTING_DURATION_SECS,
    };
    use eve_server::services::pvp_filament_mgr::{get_pvp_filament_match_spec, load_pvp_store};
    use glam::DVec3;
    use std::collections::HashMap;

    fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
        ctx.clock().advance_secs(secs);
        let cur = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
        });
    }

    fn fit_small_railgun(ctx: &ServerContext, char_id: i64, ship_id: i64, gun_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 10_678, // 125mm Railgun I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                gun_id + 1,
                InventoryItemRecord {
                    item_id: gun_id + 1,
                    type_id: 222, // Antimatter Charge S
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 200,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
        });
    }

    async fn kill_npc_with_turret_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        sys_id: u32,
        ship_id: i64,
        gun_id: i64,
        npc_id: i64,
    ) {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_h.get_ball_snapshot(npc_id).await.unwrap().expect("NPC ball").1.position;
        let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;
        let _ = sys_h.set_ball_position(ship_id, npc_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
        let mut cs = sys_h.get_combat_state(npc_id).await.unwrap().unwrap();
        (cs.shield_hp, cs.armor_hp, cs.structure_hp, cs.shield_recharge_time_ms) = (0.0, 0.0, 1.0, 1.0e15);
        let _ = sys_h.set_combat_state(npc_id, cs).await;
        let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
        lock_target_via_rpc(ctx, sess, sys_id, ship_id, npc_id).await;
        let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;
        let _ = sys_h.set_ball_position(ship_id, npc_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
        rpc_call(
            ctx,
            sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        )
        .await
        .expect("dogmaIM.Activate on NPC");
        for _ in 0..20 {
            sys_h.step_manual(1.0).await.unwrap();
            if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
                break;
            }
        }
        assert!(sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none());
        let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    }

    async fn warp_ship_via_rpc(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64) {
        rpc_call(
            ctx,
            sess,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(target_id), PyRep::Real(0.0)],
        )
        .await
        .expect("beyonce.CmdWarpToStuff");
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        for _ in 0..300 {
            sys_h.step_manual(1.0).await.unwrap();
            if sys_h.get_ball_snapshot(ship_id).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
                break;
            }
        }
    }

    async fn bind_invbroker(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> u64 {
        let bind = ctx
            .dispatch_rpc(
                sess,
                "invbroker",
                None,
                "MachoBindObject",
                &[PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(15)]), PyRep::None],
                &HashMap::new(),
                None,
            )
            .await
            .expect("bind invbroker");
        match bind {
            PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
            PyRep::Integer(i) => i as u64,
            other => panic!("unexpected bind rep {other:?}"),
        }
    }

    async fn total_lp_via_rpc(ctx: &ServerContext, sess: &mut SessionState) -> i64 {
        let rep = rpc_call(ctx, sess, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[])
            .await
            .expect("LPSvc.GetAllMyCharacterWalletLPBalances");
        let PyRep::List(rows) = rep else { return 0 };
        rows.iter()
            .filter_map(|r| r.dict_get("lp").and_then(PyRep::as_i64))
            .sum()
    }

    fn find_recipe_for_trader(ctx: &ServerContext, trader_type_id: u32, input_type_id: u32) -> (u32, u32) {
        let (recipes, _) = ctx.sde.item_trader_component_for_type(trader_type_id).unwrap().expect("itemTrader component");
        let table = ctx.sde.load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary").unwrap();
        for rid in recipes {
            if let Some(rv) = table.get(&rid.to_string()) {
                let has_in = rv.get("inputItems").and_then(|v| v.as_object()).is_some_and(|m| m.contains_key(&input_type_id.to_string()));
                if has_in {
                    let out_tid = rv.get("outputItems").and_then(|v| v.as_object()).and_then(|m| m.keys().next()).and_then(|k| k.parse::<u32>().ok()).unwrap();
                    return (rid, out_tid);
                }
            }
        }
        panic!("no recipe on trader {trader_type_id} accepting {input_type_id}");
    }

    #[tokio::test]
    async fn test_b41_insurgency_laundering_roamer_drops_fob_and_war_hq_crates() {
        // parity: feat:pve.insurgency-laundering
        eve_testkit::attest(&["feat:pve.insurgency-laundering"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (_enemy_sys, front_sys, cmd_sys, rear_sys) = setup_warzone_chain(&ctx);

        let (pir_cid, pir_ship, emp_cid, emp_ship) = (91_100_001, 101_100_001, 91_100_002, 101_100_002);
        let mut s_pir = register_online_pilot(&ctx, 89_101, "b41_pir1", "pw", pir_cid, 98_100_001, None, front_sys, pir_ship, 621, DVec3::ZERO, 0).await;
        let mut s_emp = register_online_pilot(&ctx, 89_102, "b41_emp1", "pw", emp_cid, 98_100_002, None, front_sys, emp_ship, 621, DVec3::new(5_000.0, 0.0, 0.0), 0).await;
        rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
        fit_small_railgun(&ctx, pir_cid, pir_ship, 210_001);
        fit_small_railgun(&ctx, emp_cid, emp_ship, 210_011);

        let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let (gur_camp_id, gur_fob_sys) = ctx.db.with_snapshot(|s| {
            let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
            (cid, c.fob_solar_system_id)
        });
        init_insurgency_campaign(&ctx, 7_702, FACTION_ANGEL_CARTEL, 500_003, rear_sys, vec![rear_sys, cmd_sys]);
        fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);

        // Raise front_sys to Corruption Stage 3 & Suppression Stage 3 so named roamers can roll (seed 11 rolls < 0.35 on both draws).
        ctx.mechanic_rng.set_seed(Some(11));
        assert_eq!(add_system_corruption(&ctx, gur_camp_id, front_sys, 600).1, 3);
        assert_eq!(add_system_suppression(&ctx, gur_camp_id, front_sys, 600).1, 3);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        let (named_pir_id, named_enf_id) = with_combat_runtime(&ctx, |rt| {
            let p = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && (79_358..=79_364).contains(&n.type_id)).expect("named Guristas roamer").ball_id;
            let e = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && matches!(n.type_id, 79_389 | 79_394 | 79_395 | 79_396)).expect("named Enforcer roamer").ball_id;
            (p, e)
        });

        kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, pir_ship, 210_001, named_enf_id).await;
        kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, emp_ship, 210_011, named_pir_id).await;

        let (enf_wreck_id, cache_item_id, pir_wreck_id, seq_item_id) = ctx.db.with_snapshot(|s| {
            let c = s.items.values().find(|i| i.type_id == TYPE_UPWELL_CONSORTIUM_DATA_CACHE).expect("78848 dropped in Enforcer wreck");
            let q = s.items.values().find(|i| i.type_id == TYPE_GURISTAS_INSURGENCY_DATA_SEQ).expect("78857 dropped in Guristas wreck");
            (c.location_id, c.item_id, q.location_id, q.item_id)
        });

        let sys_h = ctx.systems.get_or_spawn(front_sys);
        let ew_pos = sys_h.get_ball_snapshot(enf_wreck_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(pir_ship, ew_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let mgr_pir = bind_invbroker(&ctx, &mut s_pir, front_sys).await;
        let pir_ship_inv = ctx.dispatch_rpc(&mut s_pir, "invbroker", Some(mgr_pir), "GetInventoryFromId", &[PyRep::Integer(pir_ship), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(&mut s_pir, "invbroker", Some(pir_ship_inv), "Add", &[PyRep::Integer(cache_item_id), PyRep::Integer(enf_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("loot 78848");

        let pw_pos = sys_h.get_ball_snapshot(pir_wreck_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(emp_ship, pw_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let mgr_emp = bind_invbroker(&ctx, &mut s_emp, front_sys).await;
        let emp_ship_inv = ctx.dispatch_rpc(&mut s_emp, "invbroker", Some(mgr_emp), "GetInventoryFromId", &[PyRep::Integer(emp_ship), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(&mut s_emp, "invbroker", Some(emp_ship_inv), "Add", &[PyRep::Integer(seq_item_id), PyRep::Integer(pir_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("loot 78857");

        // Move Pirate & Empire pilots to Guristas FOB system and test >= 3 negative itemTrader RPCs.
        set_pilot_location(&ctx, &mut s_pir, gur_fob_sys, None);
        place_ship_ball(&ctx, gur_fob_sys, pir_cid, pir_ship, 621, DVec3::ZERO).await;
        set_pilot_location(&ctx, &mut s_emp, gur_fob_sys, None);
        place_ship_ball(&ctx, gur_fob_sys, emp_cid, emp_ship, 621, DVec3::new(1_150_500.0, 0.0, 45_000.0)).await;

        let lf_id = 1_891_100_000 + gur_camp_id;
        let (lf_recipe, lf_crate_tid) = find_recipe_for_trader(&ctx, TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_UPWELL_CONSORTIUM_DATA_CACHE);
        let trade_args = |tid: i64, rec: u32| [PyRep::Integer(tid), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(rec as i64), PyRep::Integer(1)];

        assert_eq!(rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "ItemTraderOutOfRange");
        assert_eq!(rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "FactionNotAligned");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(1_899_999_001, InventoryItemRecord { item_id: 1_899_999_001, type_id: TYPE_STATE_CI_OUTPOST_HUB, owner_id: 500_001, location_id: i64::from(gur_fob_sys), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
            snap.item_positions.insert(1_899_999_001, [1_150_500.0, 0.0, 45_000.0]);
        });
        let (onnamon_recipe, onnamon_crate_tid) = find_recipe_for_trader(&ctx, TYPE_STATE_CI_OUTPOST_HUB, TYPE_GURISTAS_INSURGENCY_DATA_SEQ);
        assert_eq!(rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(1_899_999_001, onnamon_recipe)).await.unwrap_err().code, "InvalidWarHQLocation");

        // Positive FOB Laundering hand-in: Pirate warps to FOB Laundering Facility and exchanges 78848 -> crate + LP + corruption.
        warp_ship_via_rpc(&ctx, &mut s_pir, gur_fob_sys, pir_ship, lf_id).await;
        let fob_corr_before = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].corruption_points.get(&gur_fob_sys).copied().unwrap_or(0));
        rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.expect("Pirate FOB Laundering hand-in");
        assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.location_id == pir_ship && i.flag == InventoryFlag::Cargo && i.type_id == lf_crate_tid)));
        assert!(total_lp_via_rpc(&ctx, &mut s_pir).await >= 2_500);
        assert!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].corruption_points.get(&gur_fob_sys).copied().unwrap_or(0)) > fob_corr_before);
        assert_eq!(rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "InsufficientInputItems");

        // Positive Empire War HQ Counter-Insurgency Outpost hand-ins in all 4 War HQs (Onnamon, Intaki, Mehatoor, Amo).
        set_pilot_location(&ctx, &mut s_emp, HQ_SYSTEM_ONNAMON, None);
        place_ship_ball(&ctx, HQ_SYSTEM_ONNAMON, emp_cid, emp_ship, 621, DVec3::ZERO).await;
        warp_ship_via_rpc(&ctx, &mut s_emp, HQ_SYSTEM_ONNAMON, emp_ship, 1_891_200_003).await;
        let fob_supp_before = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].suppression_points.get(&gur_fob_sys).copied().unwrap_or(0));
        rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(1_891_200_003, onnamon_recipe)).await.expect("Onnamon Counter-Insurgency hand-in");
        assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.location_id == emp_ship && i.type_id == onnamon_crate_tid)));
        assert!(total_lp_via_rpc(&ctx, &mut s_emp).await >= 2_500);
        assert!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].suppression_points.get(&gur_fob_sys).copied().unwrap_or(0)) > fob_supp_before);

        for (idx, (hq_sys, hub_id, hub_tid, emp_fac, data_tid)) in [
            (HQ_SYSTEM_INTAKI, 1_891_200_001_i64, TYPE_FEDERATION_CI_OUTPOST_HUB, 500_004_i64, TYPE_GURISTAS_INSURGENCY_DATA_SEQ),
            (HQ_SYSTEM_MEHATOOR, 1_891_200_002, TYPE_IMPERIAL_CI_OUTPOST_HUB, 500_003, TYPE_ANGEL_INSURGENCY_DATA_SEQ),
            (HQ_SYSTEM_AMO, 1_891_200_004, TYPE_REPUBLIC_CI_OUTPOST_HUB, 500_002, TYPE_ANGEL_INSURGENCY_DATA_SEQ),
        ]
        .into_iter()
        .enumerate()
        {
            let (cid, sid) = (91_100_010 + idx as i64, 101_100_010 + idx as i64);
            let mut s = register_online_pilot(&ctx, 89_110 + idx as u64, &format!("b41_hq_{idx}"), "pw", cid, 98_100_010 + idx as i64, None, hq_sys, sid, 621, DVec3::new(1_150_500.0, 0.0, 35_000.0), 0).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.characters.get_mut(&cid).unwrap().war_faction_id = Some(emp_fac);
                snap.items.insert(880_100 + idx as i64, InventoryItemRecord { item_id: 880_100 + idx as i64, type_id: data_tid, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
            });
            let (rid, crate_tid) = find_recipe_for_trader(&ctx, hub_tid, data_tid);
            rpc_call(&ctx, &mut s, "beyonce", "CallComponentFromClient", &trade_args(hub_id, rid)).await.expect("War HQ Counter-Insurgency hand-in");
            assert!(ctx.db.with_snapshot(|sn| sn.items.values().any(|i| i.location_id == sid && i.type_id == crate_tid)));
        }
    }

    #[tokio::test]
    async fn test_b41_insurgency_mining_ambush_ransom_rescue_destroy_and_lp_split() {
        // parity: feat:pve.insurgency-mining-ambush
        eve_testkit::attest(&["feat:pve.insurgency-mining-ambush"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

        let mut pir_pilots = Vec::new();
        for i in 0..6_i64 {
            let (cid, sid) = (91_200_001 + i, 101_200_001 + i);
            let mut s = register_online_pilot(&ctx, 89_201 + i as u64, &format!("b41_amb_p{i}"), "pw", cid, 98_200_001 + i, None, front_sys, sid, 621, DVec3::ZERO, 0).await;
            rpc_call(&ctx, &mut s, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
            fit_small_railgun(&ctx, cid, sid, 220_001 + i * 10);
            pir_pilots.push((cid, sid, s));
        }
        let (emp_cid, emp_ship) = (91_200_099, 101_200_099);
        let mut s_emp = register_online_pilot(&ctx, 89_299, "b41_amb_emp", "pw", emp_cid, 98_200_099, None, front_sys, emp_ship, 621, DVec3::ZERO, 0).await;
        rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
        fit_small_railgun(&ctx, emp_cid, emp_ship, 220_901);

        let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
        fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
        add_system_corruption(&ctx, camp_id, front_sys, 200);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        // 1. Subcase A: 6 Pirate pilots complete ransom -> ORE Rorqual warps off, corruption +50, LP shared across >5 players (25,000 each).
        let amb1 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
        let sys_h = ctx.systems.get_or_spawn(front_sys);
        assert!(sys_h.is_ball_scrambled_sync(amb1.rorqual_ball_id));
        let mut pir_lp0 = Vec::new();
        for (_, sid, s) in &mut pir_pilots {
            pir_lp0.push(total_lp_via_rpc(&ctx, s).await);
            warp_ship_via_rpc(&ctx, s, front_sys, *sid, amb1.beacon_id).await;
        }
        let p0_sid = pir_pilots[0].1;
        for def_id in amb1.ore_defender_ids {
            kill_npc_with_turret_rpc(&ctx, &mut pir_pilots[0].2, front_sys, p0_sid, 220_001, def_id).await;
        }
        let p0_bal0 = ctx.db.with_snapshot(|s| s.characters[&pir_pilots[0].0].balance);
        let corr0 = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]);
        fast_forward_secs(&ctx, MINING_AMBUSH_RANSOM_SECS + 2);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        assert!(!sys_h.is_ball_scrambled_sync(amb1.rorqual_ball_id));
        assert!(matches!(sys_h.get_ball_snapshot(amb1.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), corr0 + 50);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&pir_pilots[0].0].balance), p0_bal0, "Ransom pays LP only, zero synthetic ISK");
        for (idx, (_, _, s)) in pir_pilots.iter_mut().enumerate() {
            assert_eq!(total_lp_via_rpc(&ctx, s).await - pir_lp0[idx], (MINING_AMBUSH_BASE_LP * 5) / 6);
        }

        // 2. Subcase B: Empire rescue -> kill all pirate tacklers, Rorqual warps off, suppression +50, full 30,000 LP for 1 pilot (<=5).
        for (_, sid, _) in &pir_pilots {
            let _ = sys_h.set_ball_position(*sid, DVec3::ZERO).await;
        }
        fast_forward_secs(&ctx, MINING_AMBUSH_RESPAWN_SECS + 20);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let amb2 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
        assert!(!amb2.resolved && sys_h.is_ball_scrambled_sync(amb2.rorqual_ball_id));
        let emp_lp0 = total_lp_via_rpc(&ctx, &mut s_emp).await;
        warp_ship_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, amb2.beacon_id).await;
        let supp0 = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points.get(&front_sys).copied().unwrap_or(0));
        for pt_id in amb2.pirate_tackler_ids {
            kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, emp_ship, 220_901, pt_id).await;
        }
        assert!(!sys_h.is_ball_scrambled_sync(amb2.rorqual_ball_id));
        assert!(matches!(sys_h.get_ball_snapshot(amb2.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points[&front_sys]), supp0 + 50);
        assert_eq!(total_lp_via_rpc(&ctx, &mut s_emp).await - emp_lp0, MINING_AMBUSH_BASE_LP);

        // 3. Subcase C: Destroying ORE Rorqual spawns lootable 79586 wreck, forfeits LP (0 delta), and keeps corruption (+50).
        let _ = sys_h.set_ball_position(emp_ship, DVec3::ZERO).await;
        fast_forward_secs(&ctx, MINING_AMBUSH_RESPAWN_SECS + 20);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let amb3 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
        assert_eq!(sys_h.get_ball_snapshot(amb3.rorqual_ball_id).await.unwrap().unwrap().1.type_id, TYPE_ORE_RORQUAL);

        let lp_before_kill = total_lp_via_rpc(&ctx, &mut pir_pilots[0].2).await;
        let corr_before_kill = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]);
        kill_npc_with_turret_rpc(&ctx, &mut pir_pilots[0].2, front_sys, p0_sid, 220_001, amb3.rorqual_ball_id).await;

        assert_eq!(total_lp_via_rpc(&ctx, &mut pir_pilots[0].2).await, lp_before_kill, "Destroying ORE Rorqual forfeits LP");
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), corr_before_kill + 50, "Destroying ORE Rorqual keeps corruption");

        let (rorq_wreck_id, rorq_loot_id) = ctx.db.with_snapshot(|s| {
            let w = s.items.values().find(|i| i.type_id == TYPE_ORE_RORQUAL_WRECK).expect("ORE Rorqual Wreck 79586");
            let l = s.items.values().find(|i| i.location_id == w.item_id && i.type_id == TYPE_UPWELL_CONSORTIUM_DATA_CACHE).expect("Rorqual wreck loot");
            (w.item_id, l.item_id)
        });
        let mgr = bind_invbroker(&ctx, &mut pir_pilots[0].2, front_sys).await;
        let _ = sys_h.set_ball_position(p0_sid, DVec3::ZERO).await;
        assert_eq!(ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(rorq_wreck_id), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap_err().code, "InventoryNotAccessible");

        let wpos = sys_h.get_ball_snapshot(rorq_wreck_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(p0_sid, wpos + DVec3::new(800.0, 0.0, 0.0)).await;
        let ship_inv = ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(p0_sid), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
        assert_eq!(ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(ship_inv), "Add", &[PyRep::Integer(rorq_loot_id), PyRep::Integer(rorq_wreck_id), PyRep::Integer(5), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap_err().code, "InvalidQuantity");
        ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(ship_inv), "Add", &[PyRep::Integer(rorq_loot_id), PyRep::Integer(rorq_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("Loot ORE Rorqual Wreck");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rorq_loot_id).unwrap().location_id), p0_sid);
    }

    #[tokio::test]
    async fn test_b41_insurgency_outpost_raid_sizes_timers_defenders_bc_and_respawn() {
        // parity: feat:pve.insurgency-outpost-raid
        eve_testkit::attest(&["feat:pve.insurgency-outpost-raid"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

        let mut s_neu = register_online_pilot(&ctx, 89_301, "b41_raid_neu", "pw", 91_300_001, 98_300_001, None, front_sys, 101_300_001, 587, DVec3::ZERO, 0).await;
        let mut s_pir = register_online_pilot(&ctx, 89_302, "b41_raid_pir", "pw", 91_300_002, 98_300_002, None, front_sys, 101_300_002, 587, DVec3::ZERO, 0).await;
        let mut s_emp = register_online_pilot(&ctx, 89_303, "b41_raid_emp", "pw", 91_300_003, 98_300_003, None, front_sys, 101_300_003, 621, DVec3::ZERO, 0).await;
        rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
        fit_small_railgun(&ctx, 91_300_002, 101_300_002, 230_001);
        fit_small_railgun(&ctx, 91_300_003, 101_300_003, 230_011);

        let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
        fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        let gate_for = |sz: OutpostRaidSize| 1_893_100_000 + i64::from(front_sys) * 10 + sz.slot_index();
        let (g_small, g_med, g_mod, g_ladv) = (gate_for(OutpostRaidSize::Small), gate_for(OutpostRaidSize::Medium), gate_for(OutpostRaidSize::Moderate), gate_for(OutpostRaidSize::LargeAdv));
        let sys_h = ctx.systems.get_or_spawn(front_sys);

        // >= 4 negative RPC tests on keeper.ActivateAccelerationGate.
        let _ = sys_h.set_ball_position(101_300_001, OutpostRaidSize::Small.gate_position()).await;
        assert_eq!(rpc_call(&ctx, &mut s_neu, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "NotEnlistedInMilitia");
        assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "TooFarFromGate");
        let _ = sys_h.set_ball_position(101_300_003, OutpostRaidSize::Small.gate_position()).await;
        assert_eq!(rpc_call(&ctx, &mut s_emp, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "ShipRestrictedForComplex");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(230_099, InventoryItemRecord { item_id: 230_099, type_id: 10_998, owner_id: 91_300_002, location_id: 101_300_002, flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, ..Default::default() });
        });
        let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::Small.gate_position()).await;
        assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "WarpCoreStabilizerRestricted");
        ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&230_099); });

        // 1. Small Outpost Raid (Pirate Frigate 587): Mordu's Legion defender (79545), 10-min (600s) capture, 0.25% contribution.
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.expect("enter Small raid");
        let mordus_small = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].mordus_defender_ball_id.unwrap());
        assert_eq!(sys_h.get_ball_snapshot(mordus_small).await.unwrap().unwrap().1.type_id, 79_545);
        fast_forward_secs(&ctx, 120);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs), 0.0);
        kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_small).await;
        fast_forward_secs(&ctx, 590);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].completed_on_cooldown));
        fast_forward_secs(&ctx, 15);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].completed_on_cooldown));
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 2);

        // 2. Medium Outpost Raid (Empire Cruiser 621): Pirate defender (79533), 15-min (900s) capture, 0.5% (+5 suppression pts).
        let _ = sys_h.set_ball_position(101_300_003, OutpostRaidSize::Medium.gate_position()).await;
        rpc_call(&ctx, &mut s_emp, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)]).await.expect("enter Medium raid");
        let pir_def_med = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_med].pirate_defender_ball_id.unwrap());
        assert_eq!(sys_h.get_ball_snapshot(pir_def_med).await.unwrap().unwrap().1.type_id, 79_533);
        kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, 101_300_003, 230_011, pir_def_med).await;
        fast_forward_secs(&ctx, 890);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_med].completed_on_cooldown));
        fast_forward_secs(&ctx, 15);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points[&front_sys]), 5);

        // 3. Moderate Outpost Raid: Battleship rejected; Battlecruiser (24698 Drake) gets SDE dbuff 2544 (+20% BC skill bonus), 15-min (900s) capture, 52,500 LP + 1.5% (+15 pts).
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 638);
        let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::Moderate.gate_position()).await;
        assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.unwrap_err().code, "ShipRestrictedForComplex");

        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 24_698);
        ctx.invalidate_ship_fitting(101_300_002);
        let bc_bonus_outside = ctx.get_or_rebuild_ship_fitting(91_300_002, 101_300_002).unwrap().evaluated_context(&ctx.sde).ship.get_attribute(743, &ctx.sde);
        let lp_before_mod = total_lp_via_rpc(&ctx, &mut s_pir).await;
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.expect("enter Moderate raid in Battlecruiser");
        let bc_bonus_inside = ctx.get_or_rebuild_ship_fitting(91_300_002, 101_300_002).unwrap().evaluated_context(&ctx.sde).ship.get_attribute(743, &ctx.sde);
        assert!((bc_bonus_inside - bc_bonus_outside * 1.20).abs() < 1e-6, "Moderate pocket applies SDE dbuff 2544 (+20% BC skill bonus): {bc_bonus_outside} -> {bc_bonus_inside}");
        let mordus_mod = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].mordus_defender_ball_id.unwrap());
        kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_mod).await;
        fast_forward_secs(&ctx, 725);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].completed_on_cooldown), "Moderate capture takes full 900s (no 1.25x speed)");
        fast_forward_secs(&ctx, 180);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].completed_on_cooldown));
        assert_eq!(total_lp_via_rpc(&ctx, &mut s_pir).await - lp_before_mod, 52_500);
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 17);

        // 4. Static Large ADV Outpost Raid (Battleship 638): 15-min (900s) capture, 1.0% (+10 pts), and static 40-min (2,400s) respawn.
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 638);
        let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::LargeAdv.gate_position()).await;
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)]).await.expect("enter Large ADV raid");
        let mordus_ladv = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].mordus_defender_ball_id.unwrap());
        kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_ladv).await;
        fast_forward_secs(&ctx, 905);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(ctx.db.with_snapshot(|s| { let r = &load_ops_state(s).outpost_raids[&g_ladv]; r.is_static && r.completed_on_cooldown }));
        assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 27);
        assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_none());

        fast_forward_secs(&ctx, LARGE_ADV_OUTPOST_RESPAWN_SECS - 15);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_none());

        fast_forward_secs(&ctx, 20);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_b41_proving_grounds_fleet_queue_restrictions_boundary_timeout_and_rewards() {
        // parity: feat:pve.proving-grounds-queue-and-rewards
        eve_testkit::attest(&["feat:pve.proving-grounds-queue-and-rewards"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let jita = 30_000_142_u32;
        let fil_tid = 72_984_u32; // 2v2 Battleships Proving Filament
        let spec = get_pvp_filament_match_spec(&ctx.sde, fil_tid);
        assert_eq!((spec.fleet_size, spec.number_of_fleets), (2, 2));

        let high_meta_mod_tid = ctx.sde.types.values().find(|t| t.category_id == 7 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec.max_meta_level)).unwrap().type_id;
        let high_meta_imp_tid = ctx.sde.types.values().find(|t| t.category_id == 20 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec.max_meta_level)).unwrap().type_id;

        let mut sessions = Vec::new();
        let mut ids = Vec::new();
        for i in 0..5_i64 {
            let (cid, sid, fid) = (91_400_001 + i, 101_400_001 + i, 884_001 + i);
            let s = register_online_pilot(&ctx, 89_401 + i as u64, &format!("b41_pg_{i}"), "pw", cid, 98_400_001 + i, None, jita, sid, 638, DVec3::new(50_000_000.0 + (i as f64) * 2_000.0, 0.0, 0.0), 0).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(fid, InventoryItemRecord { item_id: fid, type_id: fil_tid, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
            });
            ids.push((cid, sid, fid));
            sessions.push(s);
        }

        // >= 5 negative RPC tests on pvpFilamentMgr.JoinPVPQueue (verifying filament is NOT consumed).
        set_pilot_location(&ctx, &mut sessions[0], jita, Some(60_003_760));
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "MustBeInSpace");
        set_pilot_location(&ctx, &mut sessions[0], jita, None);

        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(501, FleetRecord { fleet_id: 501, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0], ..Default::default() });
            s.items.get_mut(&ids[0].1).unwrap().type_id = 606;
        });
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidShipForFilament");
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ids[0].1).unwrap().type_id = 638);

        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(884_101, InventoryItemRecord { item_id: 884_101, type_id: high_meta_mod_tid, owner_id: ids[0].0, location_id: ids[0].1, flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, ..Default::default() });
        });
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidModuleTierForFilament");
        ctx.db.with_mut_snapshot(|s| {
            s.items.remove(&884_101);
            s.active_implants.insert(ids[0].0, vec![high_meta_imp_tid]);
        });
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidImplantsForFilament");
        ctx.db.with_mut_snapshot(|s| {
            s.active_implants.remove(&ids[0].0);
            s.fleets.insert(501, FleetRecord { fleet_id: 501, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0, ids[4].0], ..Default::default() });
        });
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "FleetTooLargeForFilament");
        assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ids[0].2)));

        // Configure two valid 2-pilot fleets: Fleet 501 = [p0, p1], Fleet 502 = [p2, p3].
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(501, FleetRecord { fleet_id: 501, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0], ..Default::default() });
            s.fleets.insert(502, FleetRecord { fleet_id: 502, boss_id: ids[2].0, members: vec![ids[2].0, ids[3].0], ..Default::default() });
        });

        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(30));
        assert!(ctx.db.with_snapshot(|s| { let q = &load_pvp_store(s).queue; q.contains_key(&ids[0].0) && q.contains_key(&ids[1].0) }));
        rpc_call(&ctx, &mut sessions[1], "pvpFilamentMgr", "LeavePVPQueue", &[PyRep::Integer(ids[0].2)]).await.expect("fleet LeavePVPQueue");
        assert!(ctx.db.with_snapshot(|s| load_pvp_store(s).queue.is_empty()));
        let refunded_fid = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.owner_id == ids[0].0 && i.location_id == ids[0].1 && i.type_id == fil_tid).unwrap().item_id);
        assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(refunded_fid), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(30));

        // Fleet 502 boss activates 1 filament -> matches with Fleet 501 and spawns isolated arena pocket in 32000001!
        assert_eq!(rpc_call(&ctx, &mut sessions[2], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[2].2), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(5));
        let (mid, center) = ctx.db.with_snapshot(|s| {
            let st = load_pvp_store(s);
            let (&m_id, off) = st.match_arena_offsets.iter().next().unwrap();
            (m_id, DVec3::from_array(*off))
        });
        let arena_h = ctx.systems.get_or_spawn(32_000_001);
        let total_hp = |cs: &eve_destiny::CombatState| cs.shield_hp + cs.armor_hp + cs.structure_hp;

        // Inside 75 km boundary sphere before 900s timer -> zero environmental damage.
        let hp_inside_0 = total_hp(&arena_h.get_combat_state(ids[0].1).await.unwrap().unwrap());
        arena_h.step_manual(1.0).await.unwrap();
        let hp_inside_1 = total_hp(&arena_h.get_combat_state(ids[0].1).await.unwrap().unwrap());
        assert!((hp_inside_1 - hp_inside_0).abs() < 1e-6);

        // 1. Exiting 75 km boundary sphere inflicts escalating environmental damage until destruction!
        let _ = arena_h.set_ball_position(ids[2].1, center + DVec3::new(80_000.0, 0.0, 0.0)).await;
        let mut cs_p2 = arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap();
        (cs_p2.shield_hp, cs_p2.armor_hp, cs_p2.structure_hp, cs_p2.shield_recharge_time_ms) = (0.0, 0.0, 900.0, 1.0e15);
        let _ = arena_h.set_combat_state(ids[2].1, cs_p2).await;

        arena_h.step_manual(1.0).await.unwrap();
        let hp_b1 = total_hp(&arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap());
        let dmg_b1 = 900.0 - hp_b1;
        assert!(dmg_b1 > 0.0);

        arena_h.step_manual(1.0).await.unwrap();
        let hp_b2 = total_hp(&arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap());
        let dmg_b2 = hp_b1 - hp_b2;
        assert!(dmg_b2 > dmg_b1, "Boundary damage escalates ({dmg_b2} > {dmg_b1})");

        arena_h.step_manual(1.0).await.unwrap();
        assert!(arena_h.get_ball_snapshot(ids[2].1).await.unwrap().is_none());
        assert!(ctx.db.with_snapshot(|s| load_pvp_store(s).active_matches[&mid].active));

        // 2. Exceeding 900s arena timer inflicts escalating environmental damage until destruction & awards Proving Grounds victory rewards!
        let mut cs_p3 = arena_h.get_combat_state(ids[3].1).await.unwrap().unwrap();
        (cs_p3.shield_hp, cs_p3.armor_hp, cs_p3.structure_hp, cs_p3.shield_recharge_time_ms) = (0.0, 0.0, 500.0, 1.0e15);
        let _ = arena_h.set_combat_state(ids[3].1, cs_p3).await;
        let bal_p0_before = ctx.db.with_snapshot(|s| s.characters[&ids[0].0].balance);
        let items_p0_before = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == ids[0].0).count());

        fast_forward_secs(&ctx, 905);
        arena_h.step_manual(1.0).await.unwrap();
        let hp_t1 = total_hp(&arena_h.get_combat_state(ids[3].1).await.unwrap().unwrap());
        assert!(500.0 - hp_t1 > 0.0);

        arena_h.step_manual(1.0).await.unwrap();
        assert!(arena_h.get_ball_snapshot(ids[3].1).await.unwrap().is_none());

        let (m_active, m_winner, ev_id, got_reward) = ctx.db.with_snapshot(|s| {
            let st = load_pvp_store(s);
            let m = &st.active_matches[&mid];
            let bal_after = s.characters[&ids[0].0].balance;
            let items_after = s.items.values().filter(|i| i.owner_id == ids[0].0).count();
            (m.active, m.winner_char_id, m.event_id, bal_after > bal_p0_before || items_after > items_p0_before)
        });
        assert!(!m_active && m_winner == Some(ids[0].0) && got_reward);
        for &dead_cid in &[ids[2].0, ids[3].0] {
            let dead_notifs = ctx.drain_notifications_for_char(dead_cid);
            let sess_chg = dead_notifs.iter().find(|n| n.event == "OnSessionChanged").expect("pushed OnSessionChanged on arena pod respawn");
            let chg = &sess_chg.args[1];
            assert!(chg.dict_get("stationid").is_some() && chg.dict_get("solarsystemid2").is_some() && chg.dict_get("shipid").is_some());
        }
        let win_notifs = ctx.drain_notifications_for_char(ids[0].0);
        assert!(win_notifs.iter().any(|n| n.event == "OnPVPFilamentsCharacterStatistics"), "pushed OnPVPFilamentsCharacterStatistics");
        assert!(win_notifs.iter().any(|n| n.event == "OnPVPFilamentsLeaderboard"), "pushed OnPVPFilamentsLeaderboard");

        let stats_rep = rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(ev_id)]).await.unwrap();
        assert_eq!(stats_rep.dict_get("wins").and_then(PyRep::as_i64), Some(1));
        let exit_rep = rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70_000 + mid)]).await.expect("exit arena via end gate");
        assert_eq!(exit_rep.dict_get("originSystemID").and_then(PyRep::as_i64), Some(i64::from(jita)));
    }
}

pub(crate) mod outpost_raid_deadspace_warp_gate {
    // Additional integration tests for batch 41 `B41-P1-01..04` behavioral verification:
    // - `feat:pve.insurgency-outpost-raid`: deadspace warp block, gate-admitted occupancy, T3D/T3C, warzone militia
    // - `feat:pve.insurgency-laundering`: probabilistic `MechanicRng` Stage >= 3 named roamer promotion
    // - `feat:pve.proving-grounds-queue-and-rewards`: party size, boss, <= 30 km range, cargo module, jump-clone implant


    use crate::fw_warzone_insurgency::setup_warzone_chain;
    use crate::sovereignty_equinox_gameplay::{
        lock_target_via_rpc, register_online_pilot, rpc_call,
    };
    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};
    use eve_server::services::insurgency_complex_ops::{
        is_named_insurgency_roamer, load_ops_state, OutpostRaidSize,
    };
    use eve_server::services::insurgency_services::{
        add_system_corruption, add_system_suppression, FORECASTING_DURATION_SECS,
    };
    use eve_server::services::pvp_filament_mgr::{get_pvp_filament_match_spec, load_pvp_store};
    use glam::DVec3;

    fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
        ctx.clock().advance_secs(secs);
        let cur = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("__server_now_unix".into(), cur.to_string());
        });
    }

    fn fit_small_railgun(ctx: &ServerContext, char_id: i64, ship_id: i64, gun_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 10_678,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                gun_id + 1,
                InventoryItemRecord {
                    item_id: gun_id + 1,
                    type_id: 222,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 200,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
        });
    }

    async fn kill_npc_with_turret_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        sys_id: u32,
        ship_id: i64,
        gun_id: i64,
        npc_id: i64,
    ) {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_h
            .get_ball_snapshot(npc_id)
            .await
            .unwrap()
            .expect("NPC ball")
            .1
            .position;
        let _ = sys_h
            .set_ball_position(ship_id, npc_pos + DVec3::new(1_200.0, 0.0, 0.0))
            .await;
        let mut cs = sys_h.get_combat_state(npc_id).await.unwrap().unwrap();
        (
            cs.shield_hp,
            cs.armor_hp,
            cs.structure_hp,
            cs.shield_recharge_time_ms,
        ) = (0.0, 0.0, 1.0, 1.0e15);
        let _ = sys_h.set_combat_state(npc_id, cs).await;
        let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
        lock_target_via_rpc(ctx, sess, sys_id, ship_id, npc_id).await;
        rpc_call(
            ctx,
            sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(npc_id),
            ],
        )
        .await
        .expect("dogmaIM.Activate on NPC");
        for _ in 0..8 {
            sys_h.step_manual(1.0).await.unwrap();
            if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
                break;
            }
        }
        assert!(sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none());
        let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    }

    #[tokio::test]
    async fn test_b41_outpost_raid_deadspace_warp_gate_occupancy_t3_and_warzone_faction() {
        // parity: feat:pve.insurgency-outpost-raid
        eve_testkit::attest(&["feat:pve.insurgency-outpost-raid"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

        let mut s_pir = register_online_pilot(
            &ctx, 89_501, "b41p2_pir", "pw", 91_500_001, 98_500_001, None, front_sys, 101_500_001, 587,
            DVec3::ZERO, 0,
        )
        .await;
        let mut s_pir_bypass = register_online_pilot(
            &ctx, 89_502, "b41p2_byp", "pw", 91_500_002, 98_500_002, None, front_sys, 101_500_002, 587,
            DVec3::ZERO, 0,
        )
        .await;
        let mut s_amarr = register_online_pilot(
            &ctx, 89_503, "b41p2_ama", "pw", 91_500_003, 98_500_003, None, front_sys, 101_500_003, 587,
            DVec3::ZERO, 0,
        )
        .await;
        rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        rpc_call(&ctx, &mut s_pir_bypass, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        rpc_call(&ctx, &mut s_amarr, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_003)]).await.unwrap();
        fit_small_railgun(&ctx, 91_500_001, 101_500_001, 250_001);
        fit_small_railgun(&ctx, 91_500_002, 101_500_002, 250_011);

        let _ = rpc_call(&ctx, &mut s_pir, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        let gate_for = |sz: OutpostRaidSize| 1_893_100_000 + i64::from(front_sys) * 10 + sz.slot_index();
        let (g_small, g_med, g_mod, g_ladv) = (
            gate_for(OutpostRaidSize::Small),
            gate_for(OutpostRaidSize::Medium),
            gate_for(OutpostRaidSize::Moderate),
            gate_for(OutpostRaidSize::LargeAdv),
        );
        let sys_h = ctx.systems.get_or_spawn(front_sys);

        // 1. Direct warp from outside into Outpost Raid pocket is blocked (`NaturalPhenomenaDisruptingWarp`).
        let _ = sys_h.set_ball_position(101_500_002, OutpostRaidSize::Small.pocket_position()).await;
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(599, FleetRecord { fleet_id: 599, boss_id: 91_500_001, members: vec![91_500_001, 91_500_002], ..Default::default() });
            s.characters.get_mut(&91_500_001).unwrap().fleet_id = Some(599);
            s.characters.get_mut(&91_500_002).unwrap().fleet_id = Some(599);
        });
        s_pir.fleet_id = Some(599);
        let warp_err = rpc_call(
            &ctx,
            &mut s_pir,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("char".into()), PyRep::Integer(91_500_002), PyRep::Real(0.0)],
        )
        .await
        .unwrap_err();
        assert_eq!(warp_err.code, "NaturalPhenomenaDisruptingWarp");
        let _ = sys_h.set_ball_position(101_500_002, DVec3::ZERO).await;

        // 2. Cross-warzone Empire militia (Amarr 500_003 in Guristas 500_010 campaign) is rejected at gate.
        let _ = sys_h.set_ball_position(101_500_003, OutpostRaidSize::Small.gate_position()).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_amarr, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
                .await
                .unwrap_err()
                .code,
            "NotEnlistedInMilitia"
        );

        // 3. T3 Tactical Destroyer (34828 Jackdaw, group 1305): rejected at Small, allowed at Medium.
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 34_828);
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.gate_position()).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
                .await
                .unwrap_err()
                .code,
            "ShipRestrictedForComplex"
        );
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Medium.gate_position()).await;
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)])
            .await
            .expect("T3D allowed in Medium Outpost Raid");

        // 4. T3 Strategic Cruiser (29984 Tengu, group 963): rejected at Medium & Moderate, allowed at LargeAdv.
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 29_984);
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Medium.gate_position()).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)])
                .await
                .unwrap_err()
                .code,
            "ShipRestrictedForComplex"
        );
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Moderate.gate_position()).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)])
                .await
                .unwrap_err()
                .code,
            "ShipRestrictedForComplex"
        );
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::LargeAdv.gate_position()).await;
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)])
            .await
            .expect("T3C allowed in LargeAdv Outpost Raid");

        // 5. Gate-admitted occupancy & live WCS re-validation:
        // Admit s_pir via gate in Frigate (587) to spawn & clear Small defender, then move s_pir out while
        // placing s_pir_bypass (who never took the gate) inside Small pocket -> capture progress stays 0.0.
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 587);
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.gate_position()).await;
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
            .await
            .expect("enter Small raid in Frigate");
        let mordus_small = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].mordus_defender_ball_id.unwrap());
        kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_500_001, 250_001, mordus_small).await;
        let _ = sys_h.set_ball_position(101_500_001, DVec3::ZERO).await;
        let _ = sys_h.set_ball_position(101_500_002, OutpostRaidSize::Small.pocket_position()).await;
        fast_forward_secs(&ctx, 120);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs),
            0.0,
            "Pilot who bypassed acceleration gate cannot capture Outpost Raid"
        );

        // Return gate-admitted s_pir to Small pocket with WCS fitted -> capture progress stays 0.0.
        let _ = sys_h.set_ball_position(101_500_002, DVec3::ZERO).await;
        let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.pocket_position()).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                250_099,
                InventoryItemRecord {
                    item_id: 250_099,
                    type_id: 10_998,
                    owner_id: 91_500_001,
                    location_id: 101_500_001,
                    flag: InventoryFlag::LoSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        fast_forward_secs(&ctx, 120);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs),
            0.0,
            "Pilot with WCS fitted mid-pocket cannot progress Outpost Raid capture"
        );
    }

    #[tokio::test]
    async fn test_b41_insurgency_stage3_roamer_probabilistic_rng_promotion() {
        // parity: feat:pve.insurgency-laundering
        eve_testkit::attest(&["feat:pve.insurgency-laundering"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

        let mut s_emp = register_online_pilot(
            &ctx, 89_520, "b41p2_rng", "pw", 91_520_001, 98_520_001, None, front_sys, 101_520_001, 621,
            DVec3::ZERO, 0,
        )
        .await;
        rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let camp_id = ctx.db.with_snapshot(|s| {
            *s.insurgency_runtime.campaigns.iter().find(|(_, c)| c.pirate_faction_id == 500_010).unwrap().0
        });
        fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
        assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 600).1, 3);
        assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, 600).1, 3);

        ctx.mechanic_rng.set_seed(Some(42));
        let sys_h = ctx.systems.get_or_spawn(front_sys);
        let (mut named_pir, mut named_enf) = (0_usize, 0_usize);
        for _ in 0..20 {
            fast_forward_secs(&ctx, 2);
            eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
            let (pir_ball, pir_tid, enf_ball, enf_tid) = with_combat_ephemeral(&ctx, |rt| {
                let p = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && n.archetype == "InsurgencyPirateRoamer").unwrap();
                let e = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && n.archetype == "InsurgencyEnforcer").unwrap();
                (p.ball_id, p.type_id, e.ball_id, e.type_id)
            });
            if is_named_insurgency_roamer(pir_tid) {
                named_pir += 1;
            }
            if is_named_insurgency_roamer(enf_tid) {
                named_enf += 1;
            }
            for (bid, btid) in [(pir_ball, pir_tid), (enf_ball, enf_tid)] {
                let _ = sys_h.remove_ball(bid).await;
                with_mut_combat_ephemeral(&ctx, |rt| {
                    rt.npc_entities.remove(&bid);
                });
                eve_server::services::insurgency_complex_ops::on_insurgency_npc_destroyed(&ctx, front_sys, bid, btid, 0, 0).await;
            }
        }
        assert!(
            (1..20).contains(&named_pir) && (1..20).contains(&named_enf),
            "Stage >= 3 named roamer promotion must be probabilistic via MechanicRng (~35%), got pir={named_pir}/20 enf={named_enf}/20"
        );
    }

    #[tokio::test]
    async fn test_b41_proving_grounds_party_size_boss_range_cargo_implant_and_atomic_ticket() {
        // parity: feat:pve.proving-grounds-queue-and-rewards
        eve_testkit::attest(&["feat:pve.proving-grounds-queue-and-rewards"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let jita = 30_000_142_u32;
        let fil_2v2 = 72_984_u32; // 2v2 Battleships Proving Filament
        let (fil_1v1, ship_1v1) = ctx
            .sde
            .types
            .values()
            .find_map(|t| {
                let sp = get_pvp_filament_match_spec(&ctx.sde, t.type_id);
                if t.group_id != 4050 || t.name.contains("Expired") || sp.fleet_size != 1 {
                    return None;
                }
                let lid = sp.ship_restrictions_list_id?;
                let tl = ctx.sde.type_lists.get(&lid)?;
                ctx.sde.types.values().find(|st| tl.contains(st)).map(|st| (t.type_id, st.type_id))
            })
            .expect("1v1 Proving Grounds filament in SDE");
        let spec_2v2 = get_pvp_filament_match_spec(&ctx.sde, fil_2v2);
        let high_meta_mod_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 7 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level))
            .unwrap()
            .type_id;
        let high_meta_imp_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 20 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level))
            .unwrap()
            .type_id;

        let mut sessions = Vec::new();
        let mut ids = Vec::new();
        for i in 0..4_i64 {
            let (cid, sid, fid) = (91_530_001 + i, 101_530_001 + i, 885_301 + i);
            let s = register_online_pilot(
                &ctx,
                89_531 + i as u64,
                &format!("b41p2_pg_{i}"),
                "pw",
                cid,
                98_530_001 + i,
                None,
                jita,
                sid,
                638,
                DVec3::new(40_000_000.0 + (i as f64) * 2_000.0, 0.0, 0.0),
                0,
            )
            .await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(
                    fid,
                    InventoryItemRecord {
                        item_id: fid,
                        type_id: fil_2v2,
                        owner_id: cid,
                        location_id: sid,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        ..Default::default()
                    },
                );
                snap.items.insert(
                    fid + 100,
                    InventoryItemRecord {
                        item_id: fid + 100,
                        type_id: fil_1v1,
                        owner_id: cid,
                        location_id: sid,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        ..Default::default()
                    },
                );
            });
            ids.push((cid, sid, fid));
            sessions.push(s);
        }

        // 1. Solo pilot activating 2v2 filament -> FleetTooSmallForFilament.
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
                .await
                .unwrap_err()
                .code,
            "FleetTooSmallForFilament"
        );

        // 2. 2-person fleet: non-boss -> NotFleetBoss; 1v1 filament -> FleetTooLargeForFilament; > 30 km -> FleetMemberOutOfRange.
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(601, FleetRecord { fleet_id: 601, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0], ..Default::default() });
            s.fleets.insert(602, FleetRecord { fleet_id: 602, boss_id: ids[2].0, members: vec![ids[2].0, ids[3].0], ..Default::default() });
        });
        assert_eq!(
            rpc_call(&ctx, &mut sessions[1], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[1].2), PyRep::Integer(fil_2v2 as i64)])
                .await
                .unwrap_err()
                .code,
            "NotFleetBoss"
        );
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&ids[0].1).unwrap().type_id = ship_1v1;
            s.items.get_mut(&ids[1].1).unwrap().type_id = ship_1v1;
        });
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2 + 100), PyRep::Integer(fil_1v1 as i64)])
                .await
                .unwrap_err()
                .code,
            "FleetTooLargeForFilament"
        );
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&ids[0].1).unwrap().type_id = 638;
            s.items.get_mut(&ids[1].1).unwrap().type_id = 638;
        });

        let jita_h = ctx.systems.get_or_spawn(jita);
        let _ = jita_h.set_ball_position(ids[1].1, DVec3::new(40_035_000.0, 0.0, 0.0)).await;
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
                .await
                .unwrap_err()
                .code,
            "FleetMemberOutOfRange"
        );
        let _ = jita_h.set_ball_position(ids[1].1, DVec3::new(40_002_000.0, 0.0, 0.0)).await;

        // 3. High-meta module carried in Cargo -> InvalidModuleTierForFilament.
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                885_501,
                InventoryItemRecord {
                    item_id: 885_501,
                    type_id: high_meta_mod_tid,
                    owner_id: ids[1].0,
                    location_id: ids[1].1,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
                .await
                .unwrap_err()
                .code,
            "InvalidModuleTierForFilament"
        );
        ctx.db.with_mut_snapshot(|s| {
            s.items.remove(&885_501);
            // Active-body implant item (location_id == pilot_id) -> rejected.
            s.items.insert(
                885_502,
                InventoryItemRecord {
                    item_id: 885_502,
                    type_id: high_meta_imp_tid,
                    owner_id: ids[1].0,
                    location_id: ids[1].0,
                    flag: InventoryFlag::Implant,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
                .await
                .unwrap_err()
                .code,
            "InvalidImplantsForFilament"
        );

        // 4. Jump-clone implant (location_id == clone_id != pilot_id, owner_id == pilot_id) is ALLOWED & atomic 2v2 ticket match!
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&885_502).unwrap().location_id = 777_000_001; // stored jump clone ID
        });
        assert_eq!(
            rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)]).await.unwrap(),
            PyRep::Integer(30)
        );
        assert_eq!(
            rpc_call(&ctx, &mut sessions[2], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[2].2), PyRep::Integer(fil_2v2 as i64)]).await.unwrap(),
            PyRep::Integer(5)
        );
        ctx.db.with_snapshot(|s| {
            let st = load_pvp_store(s);
            assert!(st.queue.is_empty(), "whole-fleet tickets consumed atomically");
            let m = st.active_matches.values().next().unwrap();
            assert_eq!(m.team_a.len() + m.team_b.len(), 4);
            assert!(s.items.contains_key(&885_502), "jump-clone implant preserved");
        });
    }
}
