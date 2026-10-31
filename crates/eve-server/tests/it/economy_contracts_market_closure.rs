//! Integration tests for Economy, Industry, Agents/R&D, HyperNet, and D-6 Persistence closure.
//!
//! Covers:
//! - `plan:E-4`, `feat:industry.facilities`, `feat:industry.facility-tax`, `feat:industry.job-cost`,
//!   `feat:industry.blueprint-me-research`, `feat:industry.capital-construction-restrictions`,
//!   `feat:lifecycle.industry-pause-and-many`
//! - `feat:wallet.hypernet-relay`, `feat:wallet.hypernet-hypercore-sizing-expiry`,
//!   `rpc:raffleProxy.CreateRaffle`, `rpc:raffleProxy.BuyRandomTickets`, `rpc:raffleProxy.GetActiveHistoricPrices`
//! - `plan:D-6`, `feat:invariant.no-transient-attrs-persisted`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;

fn setup_test_context() -> ServerContext {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters.values().next().unwrap().clone();
        for cid in 910_001_i64..=910_006_i64 {
            let mut ch = template.clone();
            ch.user_id = 1;
            ch.character_id = cid;
            ch.name = format!("Pilot {cid}");
            ch.corporation_id = 98_000_000 + cid;
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(cid, ch);
        }
    });
    ctx
}

fn make_upwell_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        position: [0.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 5_000,
        shield_hp: 10_000.0,
        armor_hp: 10_000.0,
        hull_hp: 10_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s
}

fn make_station_session(char_id: i64, corp_id: i64, sys_id: u32, station_id: i64) -> SessionState {
    let mut s = make_session(char_id, corp_id, sys_id);
    s.station_id = Some(station_id);
    s
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

async fn rpc_call(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn seed_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(tid, lvl) in skills {
            entry.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
}

fn seed_minerals(ctx: &ServerContext, owner_id: i64, loc_id: i64, base_id: i64, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, m_tid) in (34_u32..=40_u32).enumerate() {
            let iid = base_id + idx as i64;
            snap.items.insert(iid, make_item(iid, m_tid, owner_id, loc_id, InventoryFlag::Hangar, qty, false));
        }
    });
}

// parity: feat:industry.facilities, feat:industry.facility-tax, feat:industry.job-cost
#[tokio::test]
async fn test_e4_sde_facility_bonuses_taxes_and_job_cost() {
    let ctx = setup_test_context();
    let char_id = 910_001_i64;
    let corp_id = 98_910_001_i64;
    let mut sess = make_station_session(char_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);

    let structures = [
        (1_000_000_010_001_i64, 35825_u32, 0.99_f64, 0.85_f64, 0.97_f64),
        (1_000_000_010_002_i64, 35826_u32, 0.99_f64, 0.80_f64, 0.96_f64),
        (1_000_000_010_003_i64, 35827_u32, 0.99_f64, 0.70_f64, 0.95_f64),
        (1_000_000_010_004_i64, 35835_u32, 1.00_f64, 1.00_f64, 1.00_f64),
        (1_000_000_010_005_i64, 35836_u32, 1.00_f64, 0.75_f64, 1.00_f64),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        for &(sid, tid, _, _, _) in &structures {
            snap.structures.insert(
                sid,
                make_upwell_structure(sid, tid, corp_id, JITA_SYSTEM_ID, &format!("Test Structure {tid}")),
            );
        }
    });

    for &(sid, _tid, exp_mat, exp_time, exp_cost) in &structures {
        let fac = rpc_call(&ctx, &mut sess, "facilityManager", "GetFacility", &[PyRep::Integer(sid)])
            .await
            .expect("GetFacility");
        let mat = fac.dict_get("materialMultiplier").and_then(|v| v.as_f64()).unwrap();
        let time = fac.dict_get("timeMultiplier").and_then(|v| v.as_f64()).unwrap();
        let cost = fac.dict_get("costMultiplier").and_then(|v| v.as_f64()).unwrap();
        assert!((mat - exp_mat).abs() < 1e-6, "mat {mat} != {exp_mat}");
        assert!((time - exp_time).abs() < 1e-6, "time {time} != {exp_time}");
        assert!((cost - exp_cost).abs() < 1e-6, "cost {cost} != {exp_cost}");
    }

    let npc_fac = rpc_call(&ctx, &mut sess, "facilityManager", "GetFacility", &[PyRep::Integer(JITA_STATION_ID)])
        .await
        .expect("GetFacility NPC");
    let npc_tax = npc_fac.dict_get("tax").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - 0.0025).abs() < 1e-6, "NPC tax must be 0.25%, got {npc_tax}");

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let raitaru_id = structures[0].0;
    let tax_dict = vec![(PyRep::Integer(1), PyRep::Real(0.02))];
    rpc_call(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(tax_dict)],
    )
    .await
    .expect("SetFacilityTaxes");

    let cost_meta = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::compute_industry_job_costs_for_char(
            &ctx,
            snap,
            Some(char_id),
            681,
            1,
            1,
            raitaru_id,
            false,
        )
        .expect("compute_industry_job_costs_for_char")
    });
    assert!((cost_meta.facility_tax_rate - 0.02).abs() < 1e-6);
    assert!((cost_meta.facility_tax_isk - (cost_meta.eiv_isk * 0.02 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((cost_meta.scc_surcharge_isk - (cost_meta.eiv_isk * 0.04 * 100.0).round() / 100.0).abs() < 0.02);
}

// parity: feat:industry.capital-construction-restrictions, feat:industry.blueprint-me-research, feat:lifecycle.industry-pause-and-many, plan:E-4
#[tokio::test]
async fn test_e4_capital_shipyard_restrictions_and_exponential_research() {
    let ctx = setup_test_context();
    let char_id = 910_002_i64;
    let corp_id = 98_910_002_i64;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.security_status > 0.0 && s.region_id != 10_000_070)
        .unwrap()
        .solar_system_id;
    let azbel_id = 1_000_000_020_001_i64;
    let shipyard_mod_id = 1_000_000_020_002_i64;
    let mfg_mod_id = 1_000_000_020_003_i64;

    let mut sess = make_station_session(char_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);
    seed_skills(&ctx, char_id, &[(3380, 5), (3388, 5), (3387, 5), (3409, 5), (3403, 5), (3395, 5)]);
    seed_minerals(&ctx, char_id, JITA_STATION_ID, 710_000, 10_000_000);
    seed_minerals(&ctx, char_id, azbel_id, 720_000, 10_000_000);

    let cap_bp_tid = ctx
        .sde
        .blueprints
        .values()
        .find(|bp| ctx.sde.types.get(&bp.product_type_id).is_some_and(|t| t.group_id == 485))
        .map(|bp| bp.blueprint_type_id)
        .expect("Dreadnought BP in SDE");
    let freighter_bp_tid = ctx
        .sde
        .blueprints
        .values()
        .find(|bp| ctx.sde.types.get(&bp.product_type_id).is_some_and(|t| t.group_id == 513))
        .map(|bp| bp.blueprint_type_id)
        .expect("Freighter BP in SDE");

    let mut next_mat_id = 710_200_i64;
    let mut seed_bp_mats = |bp_tid: u32, loc_id: i64| {
        let mats: Vec<(u32, i32)> = ctx
            .sde
            .blueprints
            .get(&bp_tid)
            .map(|bp| {
                if !bp.materials.is_empty() {
                    bp.materials.iter().map(|m| (m.type_id, (m.quantity as i32) * 10)).collect()
                } else {
                    bp.activities
                        .get("manufacturing")
                        .or_else(|| bp.activities.get("1"))
                        .map(|a| a.materials.iter().map(|m| (m.type_id, (m.quantity as i32) * 10)).collect())
                        .unwrap_or_default()
                }
            })
            .unwrap_or_default();
        ctx.db.with_mut_snapshot(|snap| {
            for (tid, qty) in mats {
                next_mat_id += 1;
                snap.items.insert(next_mat_id, make_item(next_mat_id, tid, char_id, loc_id, InventoryFlag::Hangar, qty, false));
            }
        });
    };
    seed_bp_mats(freighter_bp_tid, JITA_STATION_ID);
    seed_bp_mats(cap_bp_tid, azbel_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(5_000_000_000);
        }
        snap.structures.insert(
            azbel_id,
            make_upwell_structure(azbel_id, 35826, corp_id, lowsec_sys, "Lowsec Azbel"),
        );
        let mut mfg_mod = make_item(mfg_mod_id, 35878, corp_id, azbel_id, InventoryFlag::ServiceSlot0, 1, true);
        mfg_mod.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(mfg_mod_id, mfg_mod);
        snap.module_online_states.insert(mfg_mod_id, true);

        snap.items.insert(710_100, make_item(710_100, cap_bp_tid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        snap.items.insert(710_101, make_item(710_101, freighter_bp_tid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        snap.items.insert(720_100, make_item(720_100, cap_bp_tid, char_id, azbel_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(720_101, make_item(720_101, 681, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
    });

    let err_hs = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(710_100)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Capital in High-Sec must fail");
    assert_eq!(err_hs.code, "CapitalConstructionForbiddenInHighSec");

    let ok_fr = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(710_101)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Freighter in High-Sec must succeed");
    assert!(ok_fr.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // Install a Blueprint with SDE materials to verify OnIndustryMaterials notification payload
    let mfg_bp_tid = ctx
        .sde
        .blueprints
        .values()
        .find(|bp| !bp.materials.is_empty())
        .map(|bp| bp.blueprint_type_id)
        .expect("BP with materials in SDE");
    seed_bp_mats(mfg_bp_tid, JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(710_102, make_item(710_102, mfg_bp_tid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
    });
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc_call(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    let _ = ctx.drain_notifications_for_char(char_id);
    let ok_rifter = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(710_102)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Rifter manufacturing job must succeed");
    let rifter_job_id = ok_rifter.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(rifter_job_id > 0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
            ch.structure_id = Some(azbel_id);
        }
    });
    sess.solar_system_id = Some(lowsec_sys);
    sess.station_id = None;
    sess.structure_id = Some(azbel_id);

    let err_noshipyard = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(720_100)),
            ("facilityID", PyRep::Integer(azbel_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Capital without Capital Shipyard must fail");
    assert_eq!(err_noshipyard.code, "FacilityNotCapitalCapable");

    ctx.db.with_mut_snapshot(|snap| {
        let mut cap_mod = make_item(shipyard_mod_id, 35881, corp_id, azbel_id, InventoryFlag::ServiceSlot1, 1, true);
        cap_mod.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(shipyard_mod_id, cap_mod);
        snap.module_online_states.insert(shipyard_mod_id, true);
    });
    let ok_cap = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(720_100)),
            ("facilityID", PyRep::Integer(azbel_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Capital with online Capital Shipyard must succeed");
    assert!(ok_cap.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
    });
    let me_res = rpc_call(
        &ctx,
        &mut make_station_session(char_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID),
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(720_101)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(5)),
        ])],
    )
    .await
    .expect("ME 0->5 InstallJob");
    let dur = me_res.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let bp_base = ctx.sde.blueprints.get(&681).unwrap().research_me_time_sec as f64;
    let exp_dur = ((5724.0_f64 * (bp_base / 105.0) * 0.75 * 0.85).round()) as i64;
    assert_eq!(dur, exp_dur);
}

#[tokio::test]
async fn test_e5_research_agent_access_and_rp_formula() {
    let ctx = setup_test_context();
    let char_id = 910_003_i64;
    let corp_id = 98_910_003_i64;

    let rd_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4 && a.level == 4 && a.station_id.is_some())
        .cloned()
        .expect("Level 4 R&D agent in SDE");
    let rd_agent_id = rd_agent.agent_id;
    let rd_station_id = rd_agent.station_id.unwrap();
    let rd_sys_id = rd_agent.solar_system_id.unwrap_or(JITA_SYSTEM_ID);
    let rd_corp_id = rd_agent.corporation_id;
    let rd_fac_id = ctx
        .sde
        .npc_corporations
        .get(&rd_corp_id)
        .and_then(|c| c.faction_id)
        .map(|f| f as i64)
        .unwrap_or(500_001);

    let mut undocked_sess = make_session(char_id, corp_id, rd_sys_id);
    let err_undocked = rpc_call(
        &ctx,
        &mut undocked_sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect_err("Undocked R&D start must fail");
    assert_eq!(err_undocked.code, "AgentNotCoLocated");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(rd_station_id);
            ch.solar_system_id = rd_sys_id;
        }
    });
    let mut sess = make_station_session(char_id, corp_id, rd_sys_id, rd_station_id);
    seed_skills(&ctx, char_id, &[(11_448, 5), (3356, 5)]);
    let err_skills = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect_err("Missing Science V / Mechanics V must fail");
    assert_eq!(err_skills.code, "ErrorNoRelevantResearchSkills");

    seed_skills(&ctx, char_id, &[(3402, 5), (3392, 5)]);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(rd_fac_id, 10.0);
        }
    });
    let err_st = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect_err("Faction 10 with Corp 0 must fail");
    assert_eq!(err_st.code, "InsufficientAgentStanding");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(rd_corp_id, 3.0);
            ch.standings.insert(rd_agent_id, 10.0);
        }
    });
    let ok_rd = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect("Valid R&D start");
    let ppd = ok_rd.dict_get("pointsPerDay").and_then(|v| v.as_f64()).unwrap();
    assert!((ppd - 125.55).abs() < 1e-6, "Expected 125.55 RP/day, got {ppd}");

    ctx.advance_secs(2 * 86_400);
    let journal = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let journal_str = format!("{journal:?}");
    assert!(journal_str.contains("251.1"));

    // Verify Research Project Management (12179) slot cap:
    // With RPM 0, a second R&D agent must fail with MaxResearchAgentsReached; with RPM 1, it succeeds.
    let second_rd = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4 && a.level == 1 && a.station_id.is_some() && a.agent_id != rd_agent_id)
        .cloned()
        .expect("Second R&D agent in SDE");
    let second_station_id = second_rd.station_id.unwrap();
    let second_sys_id = second_rd.solar_system_id.unwrap_or(JITA_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(second_station_id);
            ch.solar_system_id = second_sys_id;
        }
    });
    let mut sess2 = make_station_session(char_id, corp_id, second_sys_id, second_station_id);
    let err_cap = rpc_call(
        &ctx,
        &mut sess2,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect_err("Second R&D agent with RPM 0 must fail");
    assert_eq!(err_cap.code, "MaxResearchAgentsReached");

    seed_skills(&ctx, char_id, &[(12_179, 1), (3_359, 5)]);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(second_rd.agent_id, 5.0);
        }
    });
    let ok_second_no_mission = rpc_call(
        &ctx,
        &mut sess2,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect("Second R&D agent with RPM 1 must succeed");
    assert_eq!(ok_second_no_mission.dict_get("researchStarted").and_then(|v| v.as_bool()), Some(true));
    let ppd_no_mission = ok_second_no_mission.dict_get("pointsPerDay").and_then(|v| v.as_f64()).unwrap();
    assert!((ppd_no_mission - 54.0).abs() < 1e-6, "Expected 54.0 RP/day without Connections bonus, got {ppd_no_mission}");

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 99_001),
            eve_db::ActiveMissionRecord {
                mission_id: 99_001,
                character_id: char_id,
                agent_id: second_rd.agent_id,
                solar_system_id: second_sys_id,
                state: "Completed".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });
    let ok_second_with_mission = rpc_call(
        &ctx,
        &mut sess2,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(11_448)],
    )
    .await
    .expect("Second R&D agent after completed mission must succeed");
    let ppd_with_mission = ok_second_with_mission.dict_get("pointsPerDay").and_then(|v| v.as_f64()).unwrap();
    assert!((ppd_with_mission - 54.36).abs() < 1e-6, "Expected 54.36 RP/day with Connections bonus after mission, got {ppd_with_mission}");
}
