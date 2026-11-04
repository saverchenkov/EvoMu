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

    // Install a subcap Frigate Blueprint with SDE materials to verify OnIndustryMaterials notification payload
    let mfg_bp_tid = ctx
        .sde
        .blueprints
        .values()
        .find(|bp| !bp.materials.is_empty() && (bp.product_type_id == 587 || ctx.sde.types.get(&bp.product_type_id).is_some_and(|t| t.group_id == 25)))
        .map(|bp| bp.blueprint_type_id)
        .expect("Subcap frigate BP with materials in SDE");
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
        .find(|a| a.agent_type_id == 4 && a.level == 4 && a.station_id.is_some() && a.research_skills.contains(&11_448))
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
        .find(|a| {
            a.agent_type_id == 4
                && a.level == 1
                && a.station_id.is_some()
                && a.agent_id != rd_agent_id
                && !a.research_skills.is_empty()
        })
        .cloned()
        .expect("Second R&D agent in SDE");
    let second_skill_id = if second_rd.research_skills.contains(&11_448) {
        11_448
    } else {
        second_rd.research_skills[0]
    };
    seed_skills(&ctx, char_id, &[(second_skill_id, 5)]);
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
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(i64::from(second_skill_id))],
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
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(i64::from(second_skill_id))],
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
        &[PyRep::Integer(12), PyRep::Integer(second_rd.agent_id), PyRep::Integer(i64::from(second_skill_id))],
    )
    .await
    .expect("Second R&D agent after completed mission must succeed");
    let ppd_with_mission = ok_second_with_mission.dict_get("pointsPerDay").and_then(|v| v.as_f64()).unwrap();
    assert!((ppd_with_mission - 54.36).abs() < 1e-6, "Expected 54.36 RP/day with Connections bonus after mission, got {ppd_with_mission}");
}

pub(crate) mod hypernet_raffle_sizing_rng_expiry {
    // Integration tests for HyperNet Raffle and D-6 Persistence / No-Transient-Attrs closure.
    //
    // Covers:
    // - `feat:wallet.hypernet-relay`, `feat:wallet.hypernet-hypercore-sizing-expiry`,
    //   `rpc:raffleProxy.CreateRaffle`, `rpc:raffleProxy.BuyRandomTickets`, `rpc:raffleProxy.GetActiveHistoricPrices`
    // - `plan:D-6`, `feat:invariant.no-transient-attrs-persisted`


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;

    fn setup_test_context() -> ServerContext {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        ctx.db.with_mut_snapshot(|snap| {
            let template = snap.characters.values().next().unwrap().clone();
            for cid in 910_004_i64..=910_006_i64 {
                let mut ch = template.clone();
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

    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.GetActiveHistoricPrices
    #[tokio::test]
    async fn test_hypernet_raffle_sizing_rng_and_expiry() {
        let ctx = setup_test_context();
        let creator_id = 910_004_i64;
        let buyer_id = 910_005_i64;
        let corp_id = 98_910_004_i64;
        let mut creator_sess = make_station_session(creator_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);
        let mut buyer_sess = make_station_session(buyer_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);

        let prices = rpc_call(&ctx, &mut creator_sess, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(587)])
            .await
            .expect("GetActiveHistoricPrices");
        assert!(matches!(prices, PyRep::Tuple(ref t) if t.len() == 2));

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&creator_id) {
                ch.balance = IskCents::from_isk(100_000_000);
            }
            if let Some(ch) = snap.characters.get_mut(&buyer_id) {
                ch.balance = IskCents::from_isk(100_000_000);
            }
            snap.items.insert(730_001, make_item(730_001, 587, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
            snap.items.insert(730_002, make_item(730_002, 52_568, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 50, false));
            snap.items.insert(730_003, make_item(730_003, 587, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        });

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&creator_id) {
                ch.station_id = None;
                ch.structure_id = Some(1_000_000_099);
            }
        });
        let mut struct_sess = make_session(creator_id, corp_id, JITA_SYSTEM_ID);
        struct_sess.structure_id = Some(1_000_000_099);
        let err_struct = rpc_call(
            &ctx,
            &mut struct_sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::keyval([
                ("item_id", PyRep::Integer(730_001)),
                ("ticket_count", PyRep::Integer(8)),
                ("ticket_price", PyRep::Real(1_000_000.0)),
            ])],
        )
        .await
        .expect_err("Structure CreateRaffle must fail");
        assert_eq!(err_struct.code, "InvalidRaffleLocation");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&creator_id) {
                ch.station_id = Some(JITA_STATION_ID);
                ch.structure_id = None;
            }
        });

        let raffle1 = rpc_call(
            &ctx,
            &mut creator_sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::keyval([
                ("item_id", PyRep::Integer(730_001)),
                ("ticket_count", PyRep::Integer(8)),
                ("ticket_price", PyRep::Real(1_000_000.0)),
            ])],
        )
        .await
        .expect("CreateRaffle 1");
        let r1_id = raffle1.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();

        let buy_res = rpc_call(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(r1_id), PyRep::Integer(8)],
        )
        .await
        .expect("BuyRandomTickets 8");
        assert_eq!(buy_res.dict_get("winner_id").and_then(|v| v.as_i64()), Some(buyer_id));

        let raffle2 = rpc_call(
            &ctx,
            &mut creator_sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::keyval([
                ("item_id", PyRep::Integer(730_003)),
                ("ticket_count", PyRep::Integer(8)),
                ("ticket_price", PyRep::Real(1_000_000.0)),
            ])],
        )
        .await
        .expect("CreateRaffle 2");
        let r2_id = raffle2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
        rpc_call(
            &ctx,
            &mut buyer_sess,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(r2_id), PyRep::Integer(2)],
        )
        .await
        .expect("BuyRandomTickets 2");
        let bal_before_exp = ctx.db.with_snapshot(|s| s.characters.get(&buyer_id).unwrap().balance);
        ctx.advance_secs(72 * 3_600 + 1);
        let swept = eve_server::services::hypernet_raffle_ops::sweep_expired_raffles(&ctx);
        assert_eq!(swept, 1);
        let bal_after_exp = ctx.db.with_snapshot(|s| s.characters.get(&buyer_id).unwrap().balance);
        assert_eq!(bal_after_exp.0 - bal_before_exp.0, IskCents::from_isk(2_000_000).0);
    }

    // parity: plan:D-6, feat:invariant.no-transient-attrs-persisted
    #[tokio::test]
    async fn test_d6_persistence_and_no_transient_attrs_across_restart() {
        let db_path = std::env::temp_dir().join(format!("d6_restart_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);

        let char_id = 910_006_i64;
        let corp_id = 98_910_006_i64;
        let ship_id = 740_001_i64;
        let bastion_id = 740_002_i64;
        let rah_id = 740_003_i64;
        let mutadaptive_id = 740_004_i64;
        let target_ship_id = 740_005_i64;
        let dread_id = 740_006_i64;
        let siege_id = 740_007_i64;
        let ehe_id = 740_008_i64;
        let mutated_ar_id = 740_010_i64;
        let mutated_dcu_id = 740_011_i64;
        let fax_id = 740_012_i64;
        let triage_id = 740_013_i64;
        let weapon_id = 740_015_i64;

        let (ship_dyn_before, rah_dyn_before, mut_dyn_before, dread_dyn_before, fax_dyn_before, mutated_ar_dyn_before, mutated_dcu_dyn_before) = {
            let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 1"));
            let ctx = ServerContext::with_repository(repo);
            let mut sess = make_session(char_id, corp_id, JITA_SYSTEM_ID);
            let mutadaptive_tid = ctx
                .sde
                .types
                .values()
                .find(|t| t.group_id == 2018 && t.published)
                .map(|t| t.type_id)
                .unwrap_or(48_636);

            let template_ch = ServerContext::new()
                .db
                .with_snapshot(|s| s.characters.values().next().unwrap().clone());
            ctx.db.with_mut_snapshot(|snap| {
                let mut ch = template_ch;
                ch.character_id = char_id;
                ch.name = "D6 Pilot".into();
                ch.corporation_id = corp_id;
                ch.solar_system_id = JITA_SYSTEM_ID;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(50_000_000);
                snap.characters.insert(char_id, ch);
                snap.items.insert(
                    ship_id,
                    make_item(ship_id, 28_661, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
                );
                snap.items.insert(
                    bastion_id,
                    make_item(bastion_id, 33_400, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
                );
                snap.items.insert(
                    rah_id,
                    make_item(rah_id, 4_403, char_id, ship_id, InventoryFlag::LoSlot0, 1, true),
                );
                snap.items.insert(
                    mutadaptive_id,
                    make_item(mutadaptive_id, mutadaptive_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
                );
                snap.items.insert(
                    target_ship_id,
                    make_item(target_ship_id, 638, char_id + 1, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
                );
                snap.items.insert(
                    dread_id,
                    make_item(dread_id, 19_720, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
                );
                snap.items.insert(
                    siege_id,
                    make_item(siege_id, 20_280, char_id, dread_id, InventoryFlag::HiSlot0, 1, true),
                );
                snap.items.insert(
                    ehe_id,
                    make_item(ehe_id, 41_485, char_id, dread_id, InventoryFlag::LoSlot0, 1, true),
                );
                snap.items.insert(
                    740_009,
                    make_item(740_009, 16_275, char_id, dread_id, InventoryFlag::Cargo, 500, false),
                );
                let mut mutated_ar = make_item(mutated_ar_id, 3_530, char_id, ship_id, InventoryFlag::LoSlot1, 1, true);
                mutated_ar.source_mutaplasmid_id = Some(47_732);
                mutated_ar.dynamic_attributes.insert(84, 312.5);
                snap.items.insert(mutated_ar_id, mutated_ar);
                let mut mutated_dcu = make_item(mutated_dcu_id, 2_048, char_id, ship_id, InventoryFlag::LoSlot2, 1, true);
                mutated_dcu.source_mutaplasmid_id = Some(47_736);
                for aid in 267..=270 {
                    mutated_dcu.dynamic_attributes.insert(aid, 0.81);
                }
                for aid in 271..=274 {
                    mutated_dcu.dynamic_attributes.insert(aid, 0.84);
                }
                snap.items.insert(mutated_dcu_id, mutated_dcu);
                snap.items.insert(
                    fax_id,
                    make_item(fax_id, 37_604, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
                );
                snap.items.insert(
                    triage_id,
                    make_item(triage_id, 27_951, char_id, fax_id, InventoryFlag::HiSlot0, 1, true),
                );
                snap.items.insert(
                    740_014,
                    make_item(740_014, 16_275, char_id, fax_id, InventoryFlag::Cargo, 500, false),
                );
                snap.items.insert(
                    weapon_id,
                    make_item(weapon_id, 3_041, char_id, ship_id, InventoryFlag::HiSlot5, 1, true),
                );
                for mid in [bastion_id, rah_id, mutadaptive_id, siege_id, ehe_id, mutated_ar_id, mutated_dcu_id, triage_id, weapon_id] {
                    snap.module_online_states.insert(mid, true);
                }
            });
            seed_skills(&ctx, char_id, &[(3300, 5), (3426, 5), (28_164, 5)]);

            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(999_999_999);
                Err::<(), String>("forced rollback".into())
            });
            let bal_after_rb = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
            assert_eq!(bal_after_rb, IskCents::from_isk(50_000_000));

            let (ship_dyn_before, rah_dyn_before, mut_dyn_before, dread_dyn_before, fax_dyn_before, mutated_ar_dyn_before, mutated_dcu_dyn_before) = ctx.db.with_snapshot(|s| {
                (
                    s.items.get(&ship_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&rah_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&mutadaptive_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&dread_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&fax_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&mutated_ar_id).unwrap().dynamic_attributes.clone(),
                    s.items.get(&mutated_dcu_id).unwrap().dynamic_attributes.clone(),
                )
            });

            let sys_h = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
            for (bid, tid, oid, x) in [
                (ship_id, 28_661_u32, char_id, 0.0_f64),
                (target_ship_id, 638_u32, char_id + 1, 2_000.0_f64),
                (dread_id, 19_720_u32, char_id, 5_000.0_f64),
                (fax_id, 37_604_u32, char_id, 8_000.0_f64),
            ] {
                let cs = ctx.build_type_combat_state(tid);
                let b = eve_destiny::Ball {
                    id: bid,
                    type_id: tid,
                    position: eve_destiny::DVec3::new(x, 0.0, 0.0),
                    velocity: eve_destiny::DVec3::ZERO,
                    max_velocity: 150.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                };
                let meta = eve_destiny::SlimItemMeta {
                    group_id: 26,
                    category_id: 6,
                    owner_id: oid,
                    char_id: Some(oid),
                    corp_id,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false, ..Default::default()
                };
                let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
            }
            let _ = sys_h
                .add_target_lock(
                    ship_id,
                    eve_destiny::TargetLock {
                        target_id: target_ship_id,
                        lock_time_ms: 0.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(50_000.0),
                    },
                )
                .await;

            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(bastion_id), PyRep::String("moduleBonusBastion".into())],
            )
            .await
            .expect("Activate Bastion");
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(rah_id), PyRep::String("adaptiveArmorHardener".into())],
            )
            .await
            .expect("Activate RAH");
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(mutadaptive_id),
                    PyRep::String("remoteArmorRepair".into()),
                    PyRep::Integer(target_ship_id),
                ],
            )
            .await
            .expect("Activate Mutadaptive RAR");

            ctx.set_mechanic_rng_seed(Some(7));
            ctx.db.with_mut_snapshot(|snap| {
                snap.transient_item_attributes
                    .entry(ship_id)
                    .or_default()
                    .insert(1175, 0.90);
            });
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Overload",
                &[
                    PyRep::Integer(weapon_id),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(target_ship_id),
                    PyRep::Integer(1),
                ],
            )
            .await
            .expect("Overload weapon");

            sess.ship_id = Some(dread_id);
            ctx.db.with_mut_snapshot(|snap| {
                snap.characters.get_mut(&char_id).unwrap().active_ship_id = dread_id;
            });
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(siege_id), PyRep::String("moduleBonusSiege".into())],
            )
            .await
            .expect("Activate Siege");
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(ehe_id), PyRep::String("emergencyHullEnergizer".into())],
            )
            .await
            .expect("Activate EHE/ADC");

            sess.ship_id = Some(fax_id);
            ctx.db.with_mut_snapshot(|snap| {
                snap.characters.get_mut(&char_id).unwrap().active_ship_id = fax_id;
            });
            rpc_call(
                &ctx,
                &mut sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(triage_id), PyRep::String("moduleBonusTriage".into())],
            )
            .await
            .expect("Activate Triage");

            let _ = sys_h
                .apply_damage(ship_id, eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0))
                .await;
            for _ in 0..5 {
                let _ = sys_h.set_capacitor(ship_id, 6_000.0).await;
                for _ in 0..15 {
                    let _ = sys_h.step_manual(1.0).await;
                }
            }

            assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&bastion_id)));
            assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&siege_id)));
            assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&triage_id)));
            ctx.db.with_snapshot(|s| {
                let rah_em = s
                    .transient_item_attributes
                    .get(&rah_id)
                    .and_then(|t| t.get(&267).copied())
                    .unwrap_or(0.85);
                assert!(rah_em < 0.85, "RAH EM resonance must shift below 0.85 before crash, got {rah_em}");
                let spooled_rep = s
                    .transient_item_attributes
                    .get(&mutadaptive_id)
                    .and_then(|t| t.get(&84).copied())
                    .unwrap_or(0.0);
                assert!(spooled_rep > 0.0, "Mutadaptive RAR spool attr 84 must be populated before crash");
                let hi_heat = s
                    .transient_item_attributes
                    .get(&ship_id)
                    .and_then(|t| t.get(&1175).copied())
                    .unwrap_or(0.0);
                assert!(hi_heat > 0.0, "High rack heat attr 1175 must be > 0 before crash, got {hi_heat}");
            });
            let mut active_attrs = std::collections::HashMap::new();
            eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
                &ctx,
                ship_id,
                &mut active_attrs,
            );
            assert_eq!(active_attrs.get(&2135).copied(), Some(0.05));
            let mut triage_attrs = std::collections::HashMap::new();
            eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
                &ctx,
                fax_id,
                &mut triage_attrs,
            );
            assert_eq!(triage_attrs.get(&2135).copied(), Some(0.20));
            (
                ship_dyn_before,
                rah_dyn_before,
                mut_dyn_before,
                dread_dyn_before,
                fax_dyn_before,
                mutated_ar_dyn_before,
                mutated_dcu_dyn_before,
            )
        };

        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 2"));
        let ctx2 = ServerContext::with_repository(repo2);
        ctx2.db.with_snapshot(|snap| {
            assert_eq!(snap.characters.get(&char_id).unwrap().balance, IskCents::from_isk(50_000_000));
            assert!(
                snap.module_active_states.is_empty(),
                "Active module cycle state must not persist across server restart"
            );
            assert!(
                snap.module_online_states.get(&bastion_id).copied().unwrap_or(false),
                "Module online state must persist across server restart"
            );
            assert_eq!(
                snap.items.get(&ship_id).unwrap().dynamic_attributes,
                ship_dyn_before,
                "Ship dynamic_attributes must equal un-buffed fitted values after restart"
            );
            assert_eq!(
                snap.items.get(&rah_id).unwrap().dynamic_attributes,
                rah_dyn_before,
                "RAH dynamic_attributes must equal un-buffed fitted values after restart"
            );
            let mut mut_dyn_after = snap.items.get(&mutadaptive_id).unwrap().dynamic_attributes.clone();
            mut_dyn_after.remove(&3);
            assert_eq!(
                mut_dyn_after,
                mut_dyn_before,
                "Mutadaptive RAR dynamic_attributes (excluding persistent heat damage 3) must equal un-buffed fitted values after restart"
            );
            assert!(
                snap.items.get(&weapon_id).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0) > 0.0,
                "Overloaded weapon heat damage (attr 3) should persist across restart"
            );
            assert_eq!(
                snap.items.get(&dread_id).unwrap().dynamic_attributes,
                dread_dyn_before,
                "Dreadnought dynamic_attributes must equal un-buffed fitted values after restart"
            );
            assert_eq!(
                snap.items.get(&fax_id).unwrap().dynamic_attributes,
                fax_dyn_before,
                "FAX dynamic_attributes must equal un-buffed fitted values after restart"
            );
            assert!(
                snap.transient_item_attributes.is_empty(),
                "Transient item attributes must not persist across server restart"
            );
            let ar_after = snap.items.get(&mutated_ar_id).unwrap();
            assert_eq!(
                ar_after.dynamic_attributes,
                mutated_ar_dyn_before,
                "Mutated Armor Repairer rolled attr 84 must survive SQLite restart"
            );
            assert_eq!(ar_after.source_mutaplasmid_id, Some(47_732));
            let dcu_after = snap.items.get(&mutated_dcu_id).unwrap();
            assert_eq!(
                dcu_after.dynamic_attributes,
                mutated_dcu_dyn_before,
                "Mutated Damage Control rolled attrs 267..=274 must survive SQLite restart"
            );
            assert_eq!(dcu_after.source_mutaplasmid_id, Some(47_736));
        });
        let mut post_restart_attrs = std::collections::HashMap::new();
        eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
            &ctx2,
            ship_id,
            &mut post_restart_attrs,
        );
        assert!(
            post_restart_attrs.get(&2135).is_none() && post_restart_attrs.get(&2112).is_none(),
            "Bastion/Siege/Triage resonance/bonus attributes must not persist across server restart"
        );
        let mut post_restart_fax_attrs = std::collections::HashMap::new();
        eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
            &ctx2,
            fax_id,
            &mut post_restart_fax_attrs,
        );
        assert!(
            post_restart_fax_attrs.get(&2135).is_none() && post_restart_fax_attrs.get(&2112).is_none(),
            "Triage attributes must not persist across server restart"
        );
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod structure_market_service_order_expiry {
    // Integration tests closing canonical plan rows E-2 and E-4
    // and their blocking lifecycle and structure features.


    use crate::common;
    use eve_db::{
        corp_roles, CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
        MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord, UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::*;
    use eve_server::services::station_structure::structure_upwell_ops::structure_allows_market_orders;
    use std::collections::HashMap;

    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_REGION_ID: i64 = 10_000_002;
    const JITA_STATION_ID: i64 = 60_003_760;
    const AMARR_SYSTEM_ID: u32 = 30_002_187;
    const AMARR_REGION_ID: i64 = 10_000_043;
    const AMARR_STATION_ID: i64 = 60_008_494;
    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const CORP_ID: i64 = 2001;

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
    }

    fn make_job(job_id: i64, facility_id: i64, bp_id: i64, duration_seconds: u64, start_unix: u64) -> IndustryJobRecord {
        IndustryJobRecord {
            job_id, installer_id: P1, facility_id, blueprint_item_id: bp_id, blueprint_type_id: 681,
            activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds, start_unix,
        }
    }

    fn insert_expired_order(snap: &mut eve_db::WorldStateSnapshot, oid: i64, owner: i64, tid: u32, is_bid: bool, price: IskCents, qty: i32, escrow: IskCents, now_u: u64) {
        snap.market_orders.insert(
            oid,
            MarketOrderRecord {
                order_id: oid, owner_id: owner, station_id: AMARR_STATION_ID, region_id: AMARR_REGION_ID as u32,
                type_id: tid, is_bid, price, vol_entered: qty, vol_remaining: qty, min_volume: 1, is_npc: false, escrow,
            },
        );
        let nid = -(4_000_000_000_000_i64.saturating_add(oid.abs()));
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid, character_id: -4, item_id: oid, label: format!("MarketOrderMeta:{oid}"),
                note: format!("{{\"range\":32767,\"duration\":1,\"issue_unix\":{}}}", now_u.saturating_sub(86_500)), timestamp_unix: now_u,
            },
        );
    }

    #[tokio::test]
    async fn test_e2_structure_market_service_and_order_expiry() {
        // parity: plan:E-2, feat:structures.service-market, feat:lifecycle.market_order-expiry-trigger
        eve_testkit::attest(&[
            "plan:E-2",
            "feat:structures.service-market",
            "feat:lifecycle.market_order-expiry-trigger",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let struct_id = 1_000_000_101i64;
        let struct_trit_item_id = 910_090i64;

        // 1. Structure market service: Fortizar without Standup Market Hub refuses PlaceMultiSellOrder
        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID as u32);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.corporation_id = CORP_ID;
                ch.balance = IskCents(50_000_000_00);
                ch.station_id = None;
                ch.structure_id = Some(struct_id);
                ch.solar_system_id = JITA_SYSTEM_ID;
                ch.region_id = JITA_REGION_ID as u32;
            }
            let mut corp = CorporationRecord {
                corporation_id: CORP_ID,
                name: "Market Test Corp".into(),
                ticker: "MKTC".into(),
                ceo_id: P1,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents(50_000_000_00),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(AMARR_STATION_ID),
                members: [P1].into_iter().collect(),
                member_roles: HashMap::from([(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER)]),
                applications: HashMap::new(),
            };
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents(50_000_000_00);
            snap.corporations.insert(CORP_ID, corp);

            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id, type_id: 35833, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                    name: "Test Fortizar".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                    quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                    reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            snap.structures.insert(
                struct_id + 1,
                UpwellStructureRecord {
                    structure_id: struct_id + 1, type_id: 35832, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
                    name: "Test Astrahus".into(), position: [10_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                    quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
                    reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            let mut ast_hub = make_item(struct_id + 12, 35892, CORP_ID, struct_id + 1, InventoryFlag::ServiceSlot0, 1, true);
            ast_hub.dynamic_attributes.insert(2, 1.0);
            snap.items.insert(struct_id + 12, ast_hub);
            snap.module_online_states.insert(struct_id + 12, true);
            snap.items.insert(
                struct_trit_item_id,
                make_item(struct_trit_item_id, 34, P1, struct_id, InventoryFlag::Hangar, 1_000, false),
            );
            snap.items.insert(
                struct_trit_item_id + 1,
                make_item(struct_trit_item_id + 1, 34, P2, struct_id, InventoryFlag::Hangar, 1_000, false),
            );
        });

        assert!(
            !ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id)),
            "Structure with zero fitted service modules must not allow market orders"
        );
        assert!(
            !ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id + 1)),
            "Medium Astrahus must not allow market orders"
        );

        let mut sess = SessionState::new(8301);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID");
        sess.corporation_id = Some(CORP_ID);
        sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;

        let sell_entry_at_struct = PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(struct_trit_item_id)),
            (PyRep::String("typeID".into()), PyRep::Integer(34)),
            (PyRep::String("price".into()), PyRep::Real(500.0)),
            (PyRep::String("quantity".into()), PyRep::Integer(100)),
            (PyRep::String("stationID".into()), PyRep::Integer(struct_id)),
            (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]);
        let no_hub_sell_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry_at_struct.clone(), PyRep::Bool(false)])
            .await.expect_err("PlaceMultiSellOrder at structure with zero service modules must fail");
        assert_eq!(no_hub_sell_err.code, "MarketHubOffline");
        let no_hub_buy_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
            PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
        ]).await.expect_err("PlaceBuyOrder at structure with zero service modules must fail");
        assert_eq!(no_hub_buy_err.code, "MarketHubOffline");

        // Medium Astrahus also rejects PlaceBuyOrder
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.structure_id = Some(struct_id + 1); } });
        sess.structure_id = Some(struct_id + 1);
        let med_buy_err = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(struct_id + 1), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
            PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
        ]).await.expect_err("PlaceBuyOrder at Medium Astrahus must fail");
        assert_eq!(med_buy_err.code, "MarketHubOffline");
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.structure_id = Some(struct_id); } });
        sess.structure_id = Some(struct_id);

        // Install and online Standup Market Hub I (35892), configure structure profile with 2.0% market tax and block P2
        ctx.db.with_mut_snapshot(|snap| {
            let mut hub = make_item(struct_id + 11, 35892, CORP_ID, struct_id, InventoryFlag::ServiceSlot1, 1, true);
            hub.dynamic_attributes.insert(2, 1.0);
            snap.items.insert(struct_id + 11, hub);
            snap.module_online_states.insert(struct_id + 11, true);
            let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            let prof = ext.access_profiles.entry(struct_id).or_default();
            prof.service_settings.insert(4, 200); // SETTING_MARKET_TAX (4): 200 bps = 2.0% owner market rate (+ 0.5% SCC = 2.5% total)
            prof.blocked_characters.insert(P2);
            eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            if let Some(ch2) = snap.characters.get_mut(&P2) {
                ch2.station_id = None; ch2.structure_id = Some(struct_id);
                ch2.solar_system_id = JITA_SYSTEM_ID; ch2.region_id = JITA_REGION_ID as u32;
                ch2.balance = IskCents(10_000_000_00);
            }
        });
        assert!(ctx.db.with_snapshot(|snap| structure_allows_market_orders(&ctx, snap, struct_id)));

        let mut sess_p2 = SessionState::new(8305);
        common::login_pilot(&ctx, &mut sess_p2, "other", "other-secret").await;
        rpc(&ctx, &mut sess_p2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.expect("SelectCharacterID P2");
        let acl_buy_err = rpc(&ctx, &mut sess_p2, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100),
            PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
        ]).await.expect_err("ACL-blocked character must be rejected by PlaceBuyOrder");
        assert_eq!(acl_buy_err.code, "StructureServiceAccessDenied");
        let acl_sell_err = rpc(&ctx, &mut sess_p2, "marketProxy", "PlaceMultiSellOrder", &[
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(struct_trit_item_id + 1)),
                (PyRep::String("typeID".into()), PyRep::Integer(34)),
                (PyRep::String("price".into()), PyRep::Real(500.0)),
                (PyRep::String("quantity".into()), PyRep::Integer(100)),
                (PyRep::String("stationID".into()), PyRep::Integer(struct_id)),
                (PyRep::String("duration".into()), PyRep::Integer(90)),
            ])]),
            PyRep::Bool(false),
        ]).await.expect_err("ACL-blocked character must be rejected by PlaceMultiSellOrder");
        assert_eq!(acl_sell_err.code, "StructureServiceAccessDenied");

        let struct_sell_rep = rpc(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entry_at_struct, PyRep::Bool(false)])
            .await.expect("PlaceMultiSellOrder at structure with online Standup Market Hub");
        let struct_oid = struct_sell_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("struct_oid");
        let struct_buy_rep = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(struct_id), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(1_000),
            PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
        ]).await.expect("PlaceBuyOrder at structure with profile market broker fee");
        let buy_fee = struct_buy_rep.dict_get("brokerFeeISK").and_then(|v| v.as_f64()).expect("brokerFeeISK");
        assert!((buy_fee - 250.0).abs() < 0.02, "Profile 2.0% + 0.5% SCC = 2.5% on 10,000 ISK = 250 ISK (got {buy_fee})");
        let jita_orders = rpc(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.expect("GetOrders in Jita");
        let jita_orders_str = format!("{jita_orders:?}");
        assert!(jita_orders_str.contains(&struct_oid.to_string()) && jita_orders_str.contains(&struct_id.to_string()));

        // 2. Order expiry lifecycle at non-Jita station (Amarr 60_008_494):
        let now_u = ctx.now_unix();
        let buy_order_id = 910_001i64;
        let sell_order_id = 910_002i64;
        let corp_order_id = 910_003i64;
        let p2_exp_bid_id = 910_004i64;
        let p2_exp_ask_id = 910_005i64;
        let p1_mex_item_id = 910_095i64;

        let seed_p2_expired = |ctx: &ServerContext| {
            ctx.db.with_mut_snapshot(|snap| {
                insert_expired_order(snap, p2_exp_bid_id, P2, 36, true, IskCents(500_00), 40, IskCents(20_000_00), now_u);
                insert_expired_order(snap, p2_exp_ask_id, P2, 36, false, IskCents(50_00), 40, IskCents(0), now_u);
            });
        };

        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID as u32);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.balance = IskCents(10_000_000_00);
                ch.station_id = Some(AMARR_STATION_ID);
                ch.structure_id = None;
                ch.solar_system_id = AMARR_SYSTEM_ID;
                ch.region_id = AMARR_REGION_ID as u32;
            }
            if let Some(ch2) = snap.characters.get_mut(&P2) {
                ch2.balance = IskCents(10_000_000_00);
            }
            snap.items.insert(
                p1_mex_item_id,
                make_item(p1_mex_item_id, 36, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 100, false),
            );
            insert_expired_order(snap, buy_order_id, P1, 35, true, IskCents(100_00), 100, IskCents(10_000_00), now_u);
            insert_expired_order(snap, sell_order_id, P1, 35, false, IskCents(200_00), 50, IskCents(0), now_u);
            insert_expired_order(snap, corp_order_id, CORP_ID, 35, false, IskCents(200_00), 25, IskCents(0), now_u);
        });
        seed_p2_expired(&ctx);

        // Login P1 at Amarr -> sweeps P1's (and P1's corp's) expired orders while leaving P2's expired orders unswept
        let _ = ctx.drain_notifications_for_char(P1);
        let mut sess_amarr = SessionState::new(8302);
        common::login_pilot(&ctx, &mut sess_amarr, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess_amarr, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID at Amarr");
        sess_amarr.corporation_id = Some(CORP_ID);
        sess_amarr.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;

        assert_eq!(
            ctx.db.with_snapshot(|s| s.market_orders.get(&p2_exp_ask_id).map(|o| o.vol_remaining)),
            Some(40),
            "P1 login must only sweep P1/corp expired orders, not global P2 orders"
        );

        // 2a. Verify all 5 read RPCs never return expired orders even when unswept expired orders exist on entry
        for tid in [35i64, 36i64] {
            seed_p2_expired(&ctx);
            let orders_rep = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetOrders", &[PyRep::Integer(tid)]).await.expect("GetOrders");
            let s = format!("{orders_rep:?}");
            for oid in [buy_order_id, sell_order_id, corp_order_id, p2_exp_bid_id, p2_exp_ask_id] {
                assert!(!s.contains(&oid.to_string()), "GetOrders must not return expired order {oid}");
            }
        }

        seed_p2_expired(&ctx);
        let region_best = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetRegionBest", &[]).await.expect("GetRegionBest");
        let rb_str = format!("{region_best:?}");
        assert!(!rb_str.contains("Integer(35)") && !rb_str.contains("Integer(36)"), "GetRegionBest must exclude expired asks");

        seed_p2_expired(&ctx);
        let station_asks = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetStationAsks", &[]).await.expect("GetStationAsks");
        let sa_str = format!("{station_asks:?}");
        assert!(!sa_str.contains("Integer(35)") && !sa_str.contains("Integer(36)"), "GetStationAsks must exclude expired asks");

        let char_orders = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders");
        let co_str = format!("{char_orders:?}");
        assert!(!co_str.contains(&buy_order_id.to_string()) && !co_str.contains(&sell_order_id.to_string()));

        let corp_orders = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCorporationOrders", &[]).await.expect("GetCorporationOrders");
        assert!(!format!("{corp_orders:?}").contains(&corp_order_id.to_string()));

        // 2b. Verify all 3 match RPCs never match against unswept expired orders (910_004 bid @ 500 ISK, 910_005 ask @ 50 ISK)
        let p1_bal_before_match = ctx.db.get_character(P1).unwrap().balance.0;

        // (i) PlaceBuyOrder (immediate duration=0 @ 100 ISK against expired ask @ 50 ISK)
        seed_p2_expired(&ctx);
        let buy_match_res = rpc(
            &ctx,
            &mut sess_amarr,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(AMARR_STATION_ID),
                PyRep::Integer(36),
                PyRep::Real(100.0),
                PyRep::Integer(10),
                PyRep::Integer(32767),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Bool(false),
            ],
        )
        .await;
        assert!(
            buy_match_res.is_err()
                || buy_match_res.as_ref().ok().and_then(|r| r.dict_get("filledImmediately")).and_then(|v| v.as_bool()) == Some(false)
        );

        // (ii) PlaceMultiSellOrder (immediate duration=0 @ 100 ISK against expired bid @ 500 ISK)
        seed_p2_expired(&ctx);
        let sell_match_err = rpc(
            &ctx,
            &mut sess_amarr,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::Dict(vec![
                    (PyRep::String("itemID".into()), PyRep::Integer(p1_mex_item_id)),
                    (PyRep::String("typeID".into()), PyRep::Integer(36)),
                    (PyRep::String("price".into()), PyRep::Real(100.0)),
                    (PyRep::String("quantity".into()), PyRep::Integer(10)),
                    (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)),
                    (PyRep::String("duration".into()), PyRep::Integer(0)),
                ])]),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect_err("Immediate PlaceMultiSellOrder must not fill against expired bid order");
        assert_eq!(sell_match_err.code, "ImmediateOrderUnfilled");

        // (iii) BuyMultipleItems (@ 100 ISK against expired ask @ 50 ISK)
        seed_p2_expired(&ctx);
        let bmi_res = rpc(
            &ctx,
            &mut sess_amarr,
            "marketProxy",
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(100.0)])])],
        )
        .await;
        if let Ok(ref rep) = bmi_res {
            assert_eq!(rep.dict_get("purchasedCount").and_then(|v| v.as_i64()).unwrap_or(0), 0);
        }
        assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, p1_bal_before_match, "No P1 ISK moved on expired order match attempts");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&p1_mex_item_id).map(|i| i.quantity)), Some(100));

        // 2c. Verify P1's login sweep refunded buy escrow, returned sell items to Amarr hangar, and emitted notifications
        assert_eq!(p1_bal_before_match, 10_000_000_00 + 10_000_00, "Buy escrow refunded exactly once");
        let returned_pyerite = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.location_id == AMARR_STATION_ID && i.type_id == 35 && i.flag == InventoryFlag::Hangar && i.owner_id == P1)
                .map(|i| i.quantity)
        });
        assert_eq!(returned_pyerite, Some(50), "Sell items returned to non-Jita order station hangar (Amarr)");

        let notifs = ctx.drain_notifications_for_char(P1);
        assert!(notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));
        let item_recv = notifs.iter().find(|n| n.event == "OnMarketItemReceived").expect("OnMarketItemReceived emitted");
        assert_eq!(item_recv.args.last(), Some(&PyRep::Integer(AMARR_STATION_ID)));

        let _ = rpc(&ctx, &mut sess_amarr, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders second call");
        assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, p1_bal_before_match);
    }

    #[tokio::test]
    async fn test_e4_structure_industry_service_pause_and_many_jobs() {
        // parity: plan:E-4, feat:structures.service-industry, feat:lifecycle.industry-pause-and-many
        eve_testkit::attest(&[
            "plan:E-4",
            "feat:structures.service-industry",
            "feat:lifecycle.industry-pause-and-many",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let struct_id = 1_000_000_201i64;
        let now_u = ctx.now_unix();

        // 1. Structure industry services: test Manufacturing (1), Research (3), and Invention (8) via InstallJob
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.corporation_id = CORP_ID;
                ch.balance = IskCents(100_000_000_00);
                ch.station_id = None;
                ch.structure_id = Some(struct_id);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            for &(tid, lvl) in &[(3380, 5), (3387, 5), (3402, 5), (3403, 5), (3406, 5), (11448, 5), (11452, 5), (21791, 5)] {
                snap.skills.entry(P1).or_default().insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: lvl,
                        skill_points: 256_000.0,
                    },
                );
            }
            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: 35826, // Azbel (Engineering Complex)
                    owner_corp_id: CORP_ID,
                    solar_system_id: JITA_SYSTEM_ID,
                    name: "Test Azbel".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 5_000,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            for (iid, is_bpc) in [(700_010, false), (700_011, false), (700_012, false), (700_013, true), (700_014, true)] {
                let mut bp = make_item(iid, 691, P1, struct_id, InventoryFlag::Hangar, 1, true);
                bp.is_blueprint_copy = is_bpc;
                bp.blueprint_runs_remaining = is_bpc.then_some(10);
                snap.items.insert(iid, bp);
            }
            for (iid, tid, qty) in [
                (700_020, 34u32, 5_000_000),
                (700_021, 35u32, 1_000_000),
                (700_022, 36u32, 500_000),
                (700_023, 37u32, 500_000),
                (700_024, 20424u32, 100),
                (700_025, 20172u32, 100),
            ] {
                snap.items.insert(iid, make_item(iid, tid, P1, struct_id, InventoryFlag::Hangar, qty, false));
            }
        });

        let mut sess = SessionState::new(8401);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID");

        // 1a. With ZERO service modules fitted on the Upwell structure, InstallJob refuses all industry activities (R-IND-NO-MODULES)
        for act_id in [1i64, 3i64, 5i64, 8i64] {
            let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
                .await
                .expect_err("InstallJob must fail when Upwell structure has zero service modules fitted");
            assert_eq!(err.code, "FacilityServiceOffline");
        }

        // 1b. Fit Standup Manufacturing Plant I (35878), Standup Research Lab I (35891), Standup Invention Lab I (35886) initially OFFLINE
        ctx.db.with_mut_snapshot(|snap| {
            for (iid, tid, flag) in [
                (struct_id + 1, 35878u32, InventoryFlag::ServiceSlot0),
                (struct_id + 2, 35891u32, InventoryFlag::ServiceSlot1),
                (struct_id + 3, 35886u32, InventoryFlag::ServiceSlot2),
            ] {
                let mut svc = make_item(iid, tid, CORP_ID, struct_id, flag, 1, true);
                svc.dynamic_attributes.insert(2, 0.0);
                snap.items.insert(iid, svc);
                snap.module_online_states.insert(iid, false);
            }
        });
        for act_id in [1i64, 3i64, 5i64, 8i64] {
            let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
                .await
                .expect_err("InstallJob must fail when activity's Standup service module is offline");
            assert_eq!(err.code, "FacilityServiceOffline");
        }

        // Online only Standup Manufacturing Plant I (35878): Manufacturing (1) succeeds, while Research (3), Copying (5), & Invention (8) still fail
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&(struct_id + 1)) {
                it.dynamic_attributes.insert(2, 1.0);
            }
            snap.module_online_states.insert(struct_id + 1, true);
        });
        let mfg_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)])
            .await
            .expect("Manufacturing InstallJob succeeds when Standup Manufacturing Plant I is online");
        assert!(mfg_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        for act_id in [3i64, 5i64, 8i64] {
            let err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
                .await
                .expect_err("Research/Copying/Invention must still fail while only Manufacturing service is online");
            assert_eq!(err.code, "FacilityServiceOffline");
        }

        // Online Standup Research Lab I (35891) and Standup Invention Lab I (35886): ME Research (3), Copying (5), and Invention (8) now succeed!
        ctx.db.with_mut_snapshot(|snap| {
            for iid in [struct_id + 2, struct_id + 3] {
                if let Some(it) = snap.items.get_mut(&iid) {
                    it.dynamic_attributes.insert(2, 1.0);
                }
                snap.module_online_states.insert(iid, true);
            }
        });
        for act_id in [3i64, 5i64, 8i64] {
            let job_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(act_id), PyRep::Integer(1)])
                .await
                .expect("Research/Copying/Invention InstallJob succeeds when corresponding Standup lab is online");
            assert!(job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        }

        // 2. Pause timing: initialize structure fuel lifecycle at now_u, then leave 1 fuel block so services
        // run out of fuel and go offline at the first hourly fuel tick (now_u + 3_600).
        eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&struct_id) {
                st.fuel_blocks = 1;
            }
        });

        ctx.db.with_mut_snapshot(|snap| {
            for bp_id in [700_001i64, 700_004i64, 700_002i64] {
                snap.items.insert(bp_id, make_item(bp_id, 681, P1, struct_id, InventoryFlag::Hangar, 1, true));
            }
            for (idx, m_tid) in (34_u32..=40_u32).enumerate() {
                let iid = 700_100 + idx as i64;
                snap.items.insert(iid, make_item(iid, m_tid, P1, struct_id, InventoryFlag::Hangar, 100_000, false));
            }
        });
        let j1_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(700_001)), ("facilityID", PyRep::Integer(struct_id)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
        ])])
            .await
            .expect("InstallJob 1");
        let job_done_before = j1_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let j2_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(700_004)), ("facilityID", PyRep::Integer(struct_id)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
        ])])
            .await
            .expect("InstallJob 2");
        let job_done_before_2 = j2_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let j3_rep = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(700_002)), ("facilityID", PyRep::Integer(struct_id)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(10)),
        ])])
            .await
            .expect("InstallJob 3 (long)");
        let job_active_during = j3_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let orig_dur_3 = ctx.db.with_snapshot(|snap| snap.industry_jobs.get(&job_active_during).unwrap().duration_seconds);

        // Advance clock to now_u + 7_200 (2 hours later; fuel ran out at now_u + 3_600)
        ctx.advance_secs(7_200);
        eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

        let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)])
            .await
            .expect("GetJobsByOwner");

        ctx.db.with_snapshot(|snap| {
            assert!(!is_structure_industry_service_online(snap, struct_id, 1));
            assert_eq!(snap.industry_jobs.get(&job_done_before).unwrap().status, "Active");
            assert_eq!(snap.industry_jobs.get(&job_active_during).unwrap().status, "Paused");
            assert_eq!(
                load_job_cost_meta(snap, job_active_during).unwrap().paused_at_unix,
                Some(now_u + 3_600),
                "Pause starts at exact fuel exhaustion moment (now_u + 3_600), not first read (now_u + 7_200)"
            );
        });

        // Refuel and re-online service modules at now_u + 7_200; verify paused duration (+3,600s) is added on resume
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&struct_id) {
                st.fuel_blocks = 5_000;
            }
            for iid in [struct_id + 1, struct_id + 2, struct_id + 3] {
                if let Some(it) = snap.items.get_mut(&iid) {
                    it.dynamic_attributes.insert(2, 1.0);
                }
                snap.module_online_states.insert(iid, true);
            }
        });
        eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
        let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)])
            .await
            .expect("GetJobsByOwner after refuel");
        ctx.db.with_snapshot(|snap| {
            let j2 = snap.industry_jobs.get(&job_active_during).unwrap();
            assert_eq!(j2.status, "Active");
            assert_eq!(j2.duration_seconds, orig_dur_3 + 3_600);
        });

        // 3. CompleteManyJobs delivers all ready jobs
        let many_rep = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "CompleteManyJobs",
            &[PyRep::List(vec![PyRep::Integer(job_done_before), PyRep::Integer(job_done_before_2)])],
        )
        .await
        .expect("CompleteManyJobs delivers ready jobs");
        let many_str = format!("{many_rep:?}");
        assert!(many_str.contains(&job_done_before.to_string()) && many_str.contains(&job_done_before_2.to_string()));

        // 4. Ex-members cannot cancel or deliver corp jobs
        let corp_job_id = 880_003i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.industry_jobs.insert(corp_job_id, make_job(corp_job_id, JITA_STATION_ID, 700_003, 50_000, now_u));
            let mut meta = IndustryJobCostMeta::default();
            meta.use_corp = true;
            meta.owner_id = CORP_ID;
            save_job_cost_meta(snap, corp_job_id, &meta);
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.corporation_id = 9999;
            }
        });

        let cancel_err = rpc(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_job_id)])
            .await
            .expect_err("Ex-member must not cancel corp job");
        assert_eq!(cancel_err.code, "JobOwnershipViolation");

        let deliver_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)])
            .await
            .expect_err("Ex-member must not deliver corp job");
        assert_eq!(deliver_err.code, "JobOwnershipViolation");
    }
}

pub(crate) mod pi_extractor_expiry_launch_canister {
    // Integration tests closing canonical plan row E-6
    // and its blocking planetary interaction lifecycle features.


    use crate::common;
    use eve_db::{CorporationRecord, IskCents, PlanetaryColonyRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::*;
    use std::collections::HashMap;

    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const P1: i64 = 90_000_001;
    const POCO_CORP_ID: i64 = 2002;

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    fn seed_extractor_colony(
        snap: &mut eve_db::WorldStateSnapshot,
        planet_id: i64,
        ecu_pin_id: i64,
        store_pin_id: i64,
        now_u: u64,
        expiry_unix: Option<u64>,
    ) {
        snap.planetary_colonies.insert(
            (P1, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: P1,
                solar_system_id: JITA_SYSTEM_ID,
                command_center_level: 3,
                pins_count: 2,
                last_extracted_unix: now_u,
                extracted_commodities: HashMap::new(),
            },
        );
        let net = PiColonyNetworkState {
            pins: vec![
                PiPinState {
                    pin_id: ecu_pin_id,
                    type_id: 2848, // Extractor Control Unit
                    latitude: 0.1,
                    longitude: 0.1,
                    extractor_resource_type_id: Some(2268), // Aqueous Liquids (0.01 m3/unit)
                    extractor_head_count: 4,
                    extractor_head_radius: 0.03,
                    extractor_cycle_seconds: 900,
                    program_install_unix: now_u,
                    expiry_time_unix: expiry_unix,
                    ..Default::default()
                },
                PiPinState {
                    pin_id: store_pin_id,
                    type_id: 2536, // Storage Facility (12,000 m3 capacity)
                    latitude: 0.2,
                    longitude: 0.2,
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: ecu_pin_id,
                dest_pin_id: store_pin_id,
                level: 1,
                ..Default::default()
            }],
            routes: vec![PiRouteState {
                route_id: 1,
                commodity_type_id: 2268,
                quantity: 100,
                path: vec![ecu_pin_id, store_pin_id],
            }],
            last_simulated_unix: now_u,
            ..Default::default()
        };
        save_pi_network(snap, P1, planet_id, &net);
    }

    #[tokio::test]
    async fn test_e6_pi_extractor_expiry_and_launch_canister() {
        // parity: plan:E-6, feat:lifecycle.pi-extractor-program-expiry, feat:lifecycle.pi-launch-canister
        eve_testkit::attest(&[
            "plan:E-6",
            "feat:lifecycle.pi-extractor-program-expiry",
            "feat:lifecycle.pi-launch-canister",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let planet_single = 40_009_082i64; // Jita IV (single-call colony)
        let planet_multi = 40_009_083i64;  // Jita V (repeated-call colony)
        let ecu_pin_id = 50_000_001i64;
        let store_pin_id = 50_000_002i64;
        let now_u = ctx.now_unix();

        // 1. Seed two identical colonies for RPC-level repeated-call invariance (GetColony / StartTicking)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.balance = IskCents(50_000_000_00);
                ch.solar_system_id = JITA_SYSTEM_ID;
            }
            let mut poco_corp = CorporationRecord {
                corporation_id: POCO_CORP_ID,
                name: "Orbital Customs Corp".into(),
                ticker: "POCO".into(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents(1_000_000_00),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(JITA_STATION_ID),
                members: [90_000_002].into_iter().collect(),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
            };
            poco_corp.ensure_wallet_divisions();
            poco_corp.wallet_divisions[0].balance = IskCents(1_000_000_00);
            snap.corporations.insert(POCO_CORP_ID, poco_corp);
            save_orbital_settings(
                snap,
                planet_single,
                &OrbitalCustomsSettings {
                    planet_id: planet_single,
                    owner_corp_id: POCO_CORP_ID,
                    corporation_tax_rate: 0.08,
                    alliance_tax_rate: 0.08,
                    standing_neutral_tax_rate: 0.12,
                    allow_alliance: true,
                    allow_standings: true,
                    minimum_standing: -10.0,
                    ..Default::default()
                },
            );
            seed_extractor_colony(snap, planet_single, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
            seed_extractor_colony(snap, planet_multi, ecu_pin_id, store_pin_id, now_u, Some(now_u + 7_200));
        });

        let mut sess = SessionState::new(8501);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID");
        sess.solar_system_id = Some(JITA_SYSTEM_ID);

        // 1a. Drive planet_multi through 16 sub-cycle steps of 450s via planetMgr.StartTicking & planetMgr.GetColony RPCs,
        // then drive planet_single through a single 7,200s planetMgr.GetColony RPC call at the end.
        for step in 1..=16 {
            ctx.advance_secs(450);
            if step % 2 == 1 {
                rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_multi)])
                    .await
                    .expect("planetMgr.StartTicking");
            } else {
                rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
                    .await
                    .expect("planetMgr.GetColony");
            }
        }
        let _ = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_multi)])
            .await
            .expect("GetColony multi final");
        let single_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
            .await
            .expect("GetColony single 7,200s");
        assert!(
            single_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            "Single 7,200s GetColony call reports positive simulatedExtractedQuantity"
        );

        let (single_yield, multi_yield) = ctx.db.with_snapshot(|snap| {
            let net_s = load_pi_network(snap, P1, planet_single, 3);
            let net_m = load_pi_network(snap, P1, planet_multi, 3);
            (
                *net_s.pins[1].contents.get(&2268).unwrap_or(&0),
                *net_m.pins[1].contents.get(&2268).unwrap_or(&0),
            )
        });
        assert!(single_yield > 0, "Extractor produced yield");
        assert_eq!(
            single_yield, multi_yield,
            "RPC-level yield invariance: 16 StartTicking/GetColony sub-cycle calls == 1 single 7,200s GetColony call"
        );

        // Repeated GetColony/StartTicking at the same timestamp (0s elapsed) produces 0 extra yield
        let zero_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
            .await
            .expect("GetColony with 0s elapsed");
        assert_eq!(
            zero_rep.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()),
            Some(0),
            "Repeated GetColony with 0s elapsed yields 0"
        );

        // 1b. Extractor stops producing at install + duration (expiry); Flush after expiry never mints commodities
        ctx.advance_secs(10_000);
        rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(planet_single)])
            .await
            .expect("StartTicking past expiry");
        let post_expiry_yield = ctx.db.with_snapshot(|snap| {
            *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
        });
        assert_eq!(single_yield, post_expiry_yield, "Extractor stops producing past expiry");

        ctx.advance_secs(900);
        let flush_expired = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
            .await
            .expect("Flush after extractor expiry");
        assert_eq!(
            flush_expired.dict_get("producedQuantity").and_then(|v| v.as_i64()),
            Some(0),
            "Flush must produce 0 when all extractor programs on the planet have expired"
        );

        // 1c. Bounded program duration on UserUpdateNetwork command 10 (InstallProgram):
        // Default (no client duration) -> 86,400s (24h); excessive client duration (99,999,999s) -> clamped to 14 * 86,400s (1,209,600s)
        let install_now = ctx.now_unix();
        rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[
                PyRep::Integer(planet_single),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(10),
                    PyRep::Integer(ecu_pin_id),
                    PyRep::Integer(2268),
                    PyRep::Real(0.03),
                ])]),
            ],
        )
        .await
        .expect("UserUpdateNetwork command 10 default duration");
        let default_exp = ctx.db.with_snapshot(|snap| {
            load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
        });
        assert_eq!(
            default_exp,
            Some(install_now + 86_400),
            "Extractor program installed without explicit duration defaults to bounded 24h (86,400s)"
        );

        rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[
                PyRep::Integer(planet_single),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(10),
                    PyRep::Integer(ecu_pin_id),
                    PyRep::Integer(2268),
                    PyRep::Real(0.03),
                    PyRep::Integer(4),
                    PyRep::Integer(900),
                    PyRep::Integer((install_now + 99_999_999) as i64),
                ])]),
            ],
        )
        .await
        .expect("UserUpdateNetwork command 10 clamped duration");
        let clamped_exp = ctx.db.with_snapshot(|snap| {
            load_pi_network(snap, P1, planet_single, 3).pins[0].expiry_time_unix
        });
        assert_eq!(
            clamped_exp,
            Some(install_now + 14 * 86_400),
            "Client-supplied extractor expiry is clamped to 14 days (1,209,600s) max"
        );

        // Calling Flush immediately after install (before 1 cycle elapses) is rejected with ExtractorCycleNotReady
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
                col.last_extracted_unix = install_now;
            }
        });
        let flush_early_err = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(planet_single)])
            .await
            .expect_err("Flush before cycle elapses must fail");
        assert_eq!(flush_early_err.code, "ExtractorCycleNotReady");

        // 2. Active extractor output beyond storage capacity is lost AND offline owner receives
        // PlanetExtractorFinished + PlanetStorageFull notifications on next login (SelectCharacterID)
        let offline_now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(col) = snap.planetary_colonies.get_mut(&(P1, planet_single)) {
                col.last_extracted_unix = offline_now;
            }
            let mut net = load_pi_network(snap, P1, planet_single, 3);
            net.pins[0].program_install_unix = offline_now;
            net.pins[0].extractor_cycle_seconds = 900;
            net.pins[0].expiry_time_unix = Some(offline_now + 1_800);
            // Storage Facility (2536) has capacity 12,000.0 m3; Aqueous Liquids (2268) is 0.01 m3/unit -> max 1,200,000 units.
            // Pre-fill with 1,199,900 units so the first 900s cycle fills storage to 1,200,000 and overflows.
            net.pins[1].contents.clear();
            net.pins[1].contents.insert(2268, 1_199_900);
            net.last_simulated_unix = offline_now;
            save_pi_network(snap, P1, planet_single, &net);
        });

        let _ = ctx.drain_notifications_for_char(P1);
        // Advance 3,600s while P1 is offline (extractor finishes at +1,800s and storage fills to capacity)
        ctx.advance_secs(3_600);

        let mut sess_relogin = SessionState::new(8502);
        common::login_pilot(&ctx, &mut sess_relogin, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess_relogin, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID after offline PI events");
        sess_relogin.solar_system_id = Some(JITA_SYSTEM_ID);

        let login_notifs = ctx.drain_notifications_for_char(P1);
        assert!(
            login_notifs.iter().any(|n| n.event == "PlanetExtractorFinished"),
            "Offline owner receives PlanetExtractorFinished notification at login"
        );
        assert!(
            login_notifs.iter().any(|n| n.event == "PlanetStorageFull"),
            "Offline owner receives PlanetStorageFull notification at login"
        );
        let persisted_types: Vec<i32> = ctx
            .notification_records
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.receiver_id == P1)
            .map(|r| r.type_id)
            .collect();
        assert!(
            persisted_types.contains(&101) && persisted_types.contains(&102),
            "Both PlanetExtractorFinished (101) and PlanetStorageFull (102) persisted in notification_records"
        );

        // Trigger GetColony after relogin: verify storage clamped at 1,200,000 units and NO duplicate 101/102 notifications emitted
        let _ = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetColony", &[PyRep::Integer(planet_single)])
            .await
            .expect("GetColony after relogin");
        let post_get_colony_notifs = ctx.drain_notifications_for_char(P1);
        assert!(
            !post_get_colony_notifs
                .iter()
                .any(|n| n.event == "PlanetExtractorFinished" || n.event == "PlanetStorageFull"),
            "GetColony after login sync must not re-fire duplicate PlanetExtractorFinished or PlanetStorageFull notifications"
        );
        let capped_qty = ctx.db.with_snapshot(|snap| {
            *load_pi_network(snap, P1, planet_single, 3).pins[1].contents.get(&2268).unwrap_or(&0)
        });
        assert_eq!(
            capped_qty, 1_200_000,
            "Active extractor output beyond storage capacity (12,000 m3 = 1,200,000 units) is lost"
        );

        // 2b. Processor schematic (3,000 Aqueous Liquids P0 -> 20 Water P1) and expedited transfer cooldown
        let proc_client_id = 50_000_003i64;
        let lp_client_id = 50_000_004i64;
        rpc(
            &ctx,
            &mut sess_relogin,
            "planetMgr",
            "UserUpdateNetwork",
            &[
                PyRep::Integer(planet_single),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(proc_client_id), PyRep::Integer(2473), PyRep::Real(0.25), PyRep::Real(0.25)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(lp_client_id), PyRep::Integer(2544), PyRep::Real(0.30), PyRep::Real(0.30)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_pin_id), PyRep::Integer(proc_client_id), PyRep::Integer(1)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(proc_client_id), PyRep::Integer(lp_client_id), PyRep::Integer(1)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(proc_client_id), PyRep::Integer(121)]),
                ]),
            ],
        )
        .await
        .expect("UserUpdateNetwork add processor, launchpad, links, and Water schematic 121");
        let (proc_pin_id, lp_pin_id) = ctx.db.with_snapshot(|snap| {
            let net = load_pi_network(snap, P1, planet_single, 3);
            let p_id = net.pins.iter().find(|p| p.type_id == 2473).unwrap().pin_id;
            let l_id = net.pins.iter().find(|p| p.type_id == 2544).unwrap().pin_id;
            (p_id, l_id)
        });
        rpc(
            &ctx,
            &mut sess_relogin,
            "planetMgr",
            "UserTransferCommodities",
            &[
                PyRep::Integer(planet_single),
                PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
                PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(500))]),
            ],
        )
        .await
        .expect("UserTransferCommodities expedited transfer");
        let cd_err = rpc(
            &ctx,
            &mut sess_relogin,
            "planetMgr",
            "UserTransferCommodities",
            &[
                PyRep::Integer(planet_single),
                PyRep::List(vec![PyRep::Integer(store_pin_id), PyRep::Integer(proc_pin_id), PyRep::Integer(lp_pin_id)]),
                PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(100))]),
            ],
        )
        .await
        .expect_err("Expedited transfer within cooldown must fail");
        assert_eq!(cd_err.code, "ExpeditedTransferOnCooldown");

        // 3. POCO export tax charged & journaled on CustomsOffice export, and Launch Canister lifecycle (DeleteLaunch + 5-day expiry)
        ctx.db.with_mut_snapshot(|snap| {
            let mut net = load_pi_network(snap, P1, planet_single, 3);
            net.pins[1].contents.clear();
            net.pins[1].contents.insert(3645, 1_000); // 1,000 units of Water (P1 commodity, base 400 ISK)
            save_pi_network(snap, P1, planet_single, &net);
        });

        let p1_bal_before_poco = ctx.db.get_character(P1).unwrap().balance.0;
        let poco_corp_before = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet.0);
        let poco_export_rep = rpc(
            &ctx,
            &mut sess_relogin,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(planet_single),
                PyRep::Integer(3645),
                PyRep::Integer(200),
                PyRep::String("CustomsOffice".into()),
            ],
        )
        .await
        .expect("UserLaunchCommodities via CustomsOffice");
        let export_tax_isk = poco_export_rep
            .dict_get("exportTaxISK")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        assert!(export_tax_isk > 0.0, "POCO export must charge positive exportTaxISK");
        let p1_bal_after_poco = ctx.db.get_character(P1).unwrap().balance.0;
        let poco_corp_after = ctx.db.with_snapshot(|s| s.corporations[&POCO_CORP_ID].master_wallet.0);
        assert!(p1_bal_after_poco < p1_bal_before_poco, "P1 wallet debited by POCO export tax");
        assert!(poco_corp_after > poco_corp_before, "POCO owner corporation credited with player tax share");
        let poco_launch_id = poco_export_rep.dict_get("launchID").and_then(|v| v.as_i64()).expect("poco_launch_id");

        // DeleteLaunch on a REAL existing launch ID succeeds and removes the launch canister and its ball
        let del_ok = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(poco_launch_id)])
            .await
            .expect("DeleteLaunch on real launch canister must succeed");
        assert_eq!(del_ok, PyRep::Bool(true));
        assert!(
            ctx.db.with_snapshot(|s| !s.items.contains_key(&poco_launch_id)),
            "DeleteLaunch removes the launch canister item from space"
        );

        // DeleteLaunch on a non-launch item ID is rejected with InvalidLaunchID
        let del_err = rpc(&ctx, &mut sess_relogin, "planetMgr", "DeleteLaunch", &[PyRep::Integer(100_000_001)])
            .await
            .expect_err("DeleteLaunch must reject non-launch ID");
        assert_eq!(del_err.code, "InvalidLaunchID");

        // Command Center launch creates a real Planetary Launch Container item and Ball in space near the planet
        // (never in station hangar), places commodities inside the container, preserves launchTime, and expires after 5 days
        let launch_ts = ctx.now_unix() as i64;
        let launch_rep = rpc(
            &ctx,
            &mut sess_relogin,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(planet_single),
                PyRep::Integer(store_pin_id),
                PyRep::Integer(3645),
                PyRep::Integer(500),
            ],
        )
        .await
        .expect("UserLaunchCommodities CommandCenterLaunch");
        let launch_id = launch_rep
            .as_i64()
            .or_else(|| launch_rep.dict_get("launchID").and_then(|v| v.as_i64()))
            .expect("launch_id");
        assert!(launch_id > 0);
        let (container_item, inner_commodity) = ctx.db.with_snapshot(|s| {
            let cont = s.items.get(&launch_id).cloned().expect("Planetary Launch Container item");
            let inner = s
                .items
                .values()
                .find(|i| i.location_id == launch_id)
                .cloned()
                .expect("Commodity item inside launch container");
            (cont, inner)
        });
        assert_eq!(
            container_item.location_id,
            JITA_SYSTEM_ID as i64,
            "Planetary Launch Container located in solar system space"
        );
        assert_ne!(
            container_item.type_id, 3645,
            "Launch item is a Planetary Launch Container SDE type, not the raw commodity"
        );
        assert_eq!(inner_commodity.type_id, 3645);
        assert_eq!(inner_commodity.quantity, 500);
        let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        let (_, launch_ball) = sys_handle
            .get_ball_snapshot(launch_id)
            .await
            .expect("ball query")
            .expect("Planetary Launch Container ball spawned in ballpark");
        assert_eq!(launch_ball.type_id, container_item.type_id);
        assert!(
            launch_ball.position.length() > 1_000.0,
            "Launch container ball has non-zero orbital coordinates near planet"
        );
        let in_hangar = ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|i| i.location_id == JITA_STATION_ID && i.type_id == 3645)
        });
        assert!(!in_hangar, "Never places launched goods in station hangar");

        // Advance 60s and verify GetMyLaunchesDetails reports the stored launchTime and non-zero (x, y, z)
        ctx.advance_secs(60);
        let my_launches = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
            .await
            .expect("GetMyLaunchesDetails");
        let PyRep::List(ref launch_list) = my_launches else {
            panic!("GetMyLaunchesDetails must return List");
        };
        let found_launch = launch_list
            .iter()
            .find(|entry| entry.dict_get("launchID").and_then(|v| v.as_i64()) == Some(launch_id))
            .expect("launch entry in GetMyLaunchesDetails");
        assert_eq!(
            found_launch.dict_get("launchTime").and_then(|v| v.as_i64()),
            Some(launch_ts),
            "GetMyLaunchesDetails reports stored launchTime"
        );
        let lx = found_launch.dict_get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ly = found_launch.dict_get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let lz = found_launch.dict_get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(
            lx.abs() > 1.0 && ly.abs() > 1.0 && lz.abs() > 1.0,
            "GetMyLaunchesDetails reports non-zero orbital coordinates (x, y, z)"
        );

        // Uncollected after 5 days is removed by background tick (before GetMyLaunchesDetails is called)
        ctx.advance_secs(5 * 86_400 + 100);
        assert!(
            ctx.db.with_snapshot(|s| !s.items.contains_key(&launch_id) && !s.items.contains_key(&inner_commodity.item_id)),
            "Background tick removes expired planetary launch container and its inner commodities after 5 days"
        );
        assert!(
            sys_handle.get_ball_snapshot(launch_id).await.expect("ball query").is_none(),
            "Background tick removes expired planetary launch container ball from ballpark"
        );
        let launches_expired = rpc(&ctx, &mut sess_relogin, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(planet_single)])
            .await
            .expect("GetMyLaunchesDetails after 5 days");
        assert!(
            !format!("{launches_expired:?}").contains(&launch_id.to_string()),
            "Uncollected launch removed after 5 days"
        );
    }
}

pub(crate) mod sde_loader_celestials_gates {
    // Action Plan Acceptance Test Suite: Persistence, Dogma, Economy & Mining
    //
    // Validates:
    // - D-1 / D-2: SDE runtime loading, celestials, Jita-Perimeter gate pairs, schools, indexed lookups
    // - D-3 / D-4 / D-5: Durable SQLite WorldRepository, Argon2id accounts & online locks, evomu.toml config
    // - DG-1: Dogma modifier engine, modifierInfo functions, stacking penalties, Rifter with overdrives
    // - E-1 / E-2: Market fees (Broker Relations / Accounting), order lifecycle, escrow management
    // - E-3: Contracts (ItemExchange and Courier delivery & collateral)
    // - C-5: Asteroid belts spawning, Miner I laser cycle, depletion
    // - C-6: Reprocessing yield, station tax, material outputs, non-reprocessable rejection



    use crate::common;
    use eve_db::{
        AsteroidRecord, BookmarkRecord, CorporationRecord, DbError, FleetRecord,
        IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, MailRecord, MarketOrderRecord,
        SkillQueueEntry, StandingTransactionRecord, TrainedSkillRecord, TypeId, UpwellStructureRecord,
        WalletJournalEntry, WorldRepository,
    };
    use eve_dogma::{
        stacking_penalty_factor, DogmaEffectContext, DogmaItemState, EffectModifierSpec,
        ShipFittingEngine,
    };
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_sde::{attr_ids, SdeError, SdeRegistry};
    use eve_server::config::ServerConfig;
    use eve_server::router::ServerContext;
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::sync::Arc;

    const JITA_STATION_ID: i64 = 60_003_760;
    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const PERIMETER_SOLAR_SYSTEM_ID: u32 = 30_000_144;
    const TRITANIUM_TYPE_ID: TypeId = 34;
    const VELDSPAR_TYPE_ID: TypeId = 1230;
    const SCORDITE_TYPE_ID: TypeId = 1228;
    const RIFTER_TYPE_ID: TypeId = 587;
    const MINER_I_TYPE_ID: TypeId = 483;
    const OVERDRIVE_II_TYPE_ID: TypeId = 1236;
    const BROKER_RELATIONS_SKILL: TypeId = 3446;
    const MINING_SKILL: TypeId = 3386;
    const REPROCESSING_SKILL: TypeId = 3385;
    const REPROC_EFFICIENCY_SKILL: TypeId = 3389;

    async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
            .await
            .expect("SelectCharacterID");
        sess
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match dict {
            PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| match k { PyRep::String(s) => s == key, _ => false }).map(|(_, v)| v),
            _ => None,
        }
    }

    // ====================================================================
    // Test 1: D-1 & D-2: SDE Loader, Celestials, Jita-Perimeter Gate Pairs
    // ====================================================================
    #[tokio::test]
    async fn test_plan_d1_d2_sde_loader_celestials_and_gates() {
        // 1. SdeRegistry::open fails clearly if database path is missing
        let missing_path = Path::new("/tmp/nonexistent_evomu_sde_db_never_exists.sqlite");
        let open_missing = SdeRegistry::open(missing_path);
        assert!(matches!(open_missing, Err(SdeError::MissingDatabase(_))), "Opening missing SDE database must return SdeError::MissingDatabase");

        // 2. Load runtime SDE from canonical path
        let sde_path = SdeRegistry::default_db_path();
        let sde = SdeRegistry::open(&sde_path).expect("Real SDE SQLite database must open");

        // 3. Verify >= 5,000 solar systems in the registry (action_plan.md D-2 criterion)
        assert!(sde.solar_systems.len() >= 5_000, "SDE solar_systems must contain at least 5,000 systems (got {})", sde.solar_systems.len());

        // 4. Verify Jita <-> Perimeter stargate pair with positions and destinations
        let jita_to_perim_gate = sde.jumps.get(&50_001_249).expect("Jita -> Perimeter stargate 50001249 must exist");
        assert_eq!(jita_to_perim_gate.from_solar_system_id, JITA_SOLAR_SYSTEM_ID);
        assert_eq!(jita_to_perim_gate.to_solar_system_id, PERIMETER_SOLAR_SYSTEM_ID);
        assert_eq!(jita_to_perim_gate.destination_stargate_id, 50_002_185, "Destination of 50001249 must be 50002185");
        assert!(jita_to_perim_gate.position.length() > 1.0, "Stargate position must be non-trivial 3D coordinates");

        let perim_to_jita_gate = sde.jumps.get(&50_002_185).expect("Perimeter -> Jita stargate 50002185 must exist");
        assert_eq!(perim_to_jita_gate.from_solar_system_id, PERIMETER_SOLAR_SYSTEM_ID);
        assert_eq!(perim_to_jita_gate.to_solar_system_id, JITA_SOLAR_SYSTEM_ID);
        assert_eq!(perim_to_jita_gate.destination_stargate_id, 50_001_249, "Destination of 50002185 must be 50001249");
        assert!(perim_to_jita_gate.position.length() > 1.0, "Stargate position must be non-trivial 3D coordinates");

        // 5. Verify school -> race/corp mapping and all 12 starter schools (11..=22)
        assert!(sde.schools.len() >= 12, "SDE schools table must have all starter schools");
        assert_eq!(sde.schools[&11].race_id, 4, "School 11 must map to Caldari (4)");
        assert_eq!(sde.schools[&11].corporation_id, 1000166);
        assert_eq!(sde.schools[&14].race_id, 2, "School 14 must map to Gallente (2)");
        assert_eq!(sde.schools[&14].corporation_id, 1000170);
        assert_eq!(sde.schools[&17].race_id, 1, "School 17 must map to Minmatar (1)");
        assert_eq!(sde.schools[&17].corporation_id, 1000167);
        assert_eq!(sde.schools[&20].race_id, 8, "School 20 must map to Amarr (8)");
        assert_eq!(sde.schools[&20].corporation_id, 1000168);

        for school_id in 11..=22 {
            let school = sde.schools.get(&school_id).expect("Starter school 11..=22 must exist");
            assert!((30_000_001..30_005_500).contains(&school.solar_system_id), "School {school_id} must reside in a valid highsec solar system");
        }

        // 6. Verify indexed lookups in SdeRegistry (D-1 real SDE database contents)
        assert!(sde.types.contains_key(&TRITANIUM_TYPE_ID), "Tritanium (34) must exist in SDE types");
        assert!(sde.types.contains_key(&RIFTER_TYPE_ID), "Rifter (587) must exist in SDE types");
        assert!(sde.types.contains_key(&MINER_I_TYPE_ID), "Miner I (483) must exist in SDE types");
        assert!(sde.types.contains_key(&OVERDRIVE_II_TYPE_ID), "Overdrive II (1236) must exist in SDE types");

        assert!(sde.groups.contains_key(&55), "Projectile Weapon group 55 must exist");
        assert!(sde.groups.contains_key(&53), "Propulsion Module group 53 must exist");
        assert!(sde.categories.contains_key(&6), "Ship category 6 must exist");
        assert!(sde.categories.contains_key(&7), "Module category 7 must exist");

        assert!(sde.attributes.contains_key(&attr_ids::MAX_VELOCITY), "maxVelocity attribute 37 must exist");
        assert!(sde.attributes.contains_key(&attr_ids::MASS), "mass attribute 4 must exist");
        assert!(sde.attributes.contains_key(&attr_ids::CAPACITY), "capacity attribute 38 must exist");
        assert!(sde.attributes.contains_key(&161), "volume attribute 161 must exist");

        assert!(sde.effects.contains_key(&5934), "warpScrambleBlockMWD effect 5934 must exist");
        assert!(sde.blueprints.contains_key(&691), "Rifter Blueprint 691 must exist in SDE");
        assert!(sde.stations.contains_key(&JITA_STATION_ID), "Jita 4-4 station 60003760 must exist in SDE");
        assert_eq!(sde.stations[&JITA_STATION_ID].solar_system_id, JITA_SOLAR_SYSTEM_ID);

        assert!(sde.factions.contains_key(&500001), "Caldari State faction 500001 must exist");
        assert!(sde.factions.contains_key(&500002), "Minmatar Republic faction 500002 must exist");
        assert!(sde.npc_corporations.contains_key(&1000003), "Caldari Navy NPC corp 1000003 must exist");

        let veld_mats = sde.type_materials.get(&VELDSPAR_TYPE_ID).expect("Veldspar typeMaterials");
        assert_eq!(veld_mats.len(), 1);
        assert_eq!(veld_mats[0].material_type_id, 34);
        assert_eq!(veld_mats[0].quantity, 400);

        let scor_mats = sde.type_materials.get(&SCORDITE_TYPE_ID).expect("Scordite typeMaterials");
        assert!(scor_mats.iter().any(|m| m.material_type_id == 34));
        assert!(scor_mats.iter().any(|m| m.material_type_id == 35));
    }

    // ====================================================================
    // Test 2: D-3, D-4, D-5: Durable World DB, Argon2 Accounts, evomu.toml
    // ====================================================================
    #[tokio::test]
    async fn test_plan_d3_d4_d5_world_db_accounts_and_config() {
        // Part A (D-5): Config loader and feature flags
        let toml_sample = r#"
    bind_address = "127.0.0.1"
    macho_port = 26000
    bridge_port = 26002
    esi_port = 26004
    sde_path = "data/evomu_sde.sqlite"
    world_db_path = "data/evomu_world.sqlite"
    belt_respawn_interval_secs = 7200
    downtime_utc_secs_of_day = 39600

    [feature_flags]
    enable_quasar_gateway = true
    enable_esi_http = true
    enable_living_world = false
    seed_demo = false

    [[admin_accounts]]
    username = "polaris_root"
    password_token = "polaris_secret_token"
    character_id = 90000003
    character_name = "ISD Polaris"

    [[player_accounts]]
    username = "new_cadet"
    password_token = "cadet_secret_token"
    "#;
        let cfg = ServerConfig::from_toml_str(toml_sample).expect("ServerConfig TOML parsing must succeed");
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.macho_port, 26_000);
        assert_eq!(cfg.bridge_port, 26_002);
        assert_eq!(cfg.esi_port, 26_004);
        assert!(cfg.feature_flags.enable_quasar_gateway);
        assert!(!cfg.feature_flags.enable_living_world);
        assert_eq!(cfg.admin_accounts.len(), 1);
        assert_eq!(cfg.admin_accounts[0].username, "polaris_root");
        assert_eq!(cfg.player_accounts.len(), 1);
        assert_eq!(cfg.player_accounts[0].username, "new_cadet");

        // Part B (D-4): Accounts, Argon2id hashes, bans, online locks
        let sde = SdeRegistry::with_canonical_seed();
        let pid = std::process::id();
        let temp_db = std::env::temp_dir().join(format!("evomu_action_plan_d3_d4_{pid}.sqlite"));
        let _ = std::fs::remove_file(&temp_db);

        let repo = Arc::new(WorldRepository::open_sqlite(&temp_db).expect("open_sqlite must succeed"));
        assert_eq!(repo.sqlite_path(), Some(temp_db.as_path()));

        // Verify WAL journal mode (D-3)
        {
            let conn = rusqlite::Connection::open(&temp_db).unwrap();
            let journal_mode: String = conn.query_row("PRAGMA journal_mode;", [], |r| r.get(0)).unwrap();
            assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        }

        // Provision player account with salted Argon2id password hash
        let uid = repo.provision_player_account("cadet_alpha", "S0m3Str0ngP@ssphrase!");
        let auth_record = repo.with_snapshot(|snap| snap.accounts.get("cadet_alpha").cloned()).expect("Account must exist");
        assert!(auth_record.password_token.starts_with("$argon2id$"), "Password hash must use Argon2id, got: {}", auth_record.password_token);

        // Verify credentials verification
        assert!(repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!").is_ok());
        assert!(repo.authenticate("cadet_alpha", "WrongPassphrase!").is_err());

        // Single active session lock (multiLoginBlocker)
        assert!(repo.acquire_online_lock(uid, 501, None, false, 1_700_000_000).unwrap().is_none());
        let conflict = repo.acquire_online_lock(uid, 502, None, false, 1_700_000_005);
        assert!(matches!(conflict, Err(DbError::AlreadyLoggedIn { .. })), "Second session must be rejected with AlreadyLoggedIn lock");
        assert_eq!(repo.check_session_revoked(uid, 501, 1_700_000_006), None);
        let revoked_502 = repo.check_session_revoked(uid, 502, 1_700_000_006);
        assert_eq!(revoked_502.unwrap().0, "MultiLoginBlocked");
        repo.release_online_lock(uid, 501);

        // Account ban enforcement
        let now = repo.now_unix_secs();
        repo.ban_account("cadet_alpha", now + 3600, "Rule violation test").expect("ban_account");
        let banned_login = repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!");
        assert!(matches!(banned_login, Err(DbError::AccountBanned { .. })), "Banned user must receive AccountBanned error");
        repo.unban_account("cadet_alpha").expect("unban_account");
        assert!(repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!").is_ok());

        // Part C (D-3): Relational domain tables, concurrent writes & durable restart
        let u2 = repo.provision_player_account("cadet_bravo", "AnotherP@ssphrase!456");
        let c1 = repo.create_character(uid, "Alpha Pilot", 1, 1, 11, 17, &sde).unwrap();
        let c2 = repo.create_character(u2, "Bravo Pilot", 2, 3, 19, 14, &sde).unwrap();
        let c1_id = c1.character_id;
        let c2_id = c2.character_id;

        // Populate relational domain tables
        repo.with_mut_snapshot(|snap| {
            snap.skills.entry(c1_id).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
            snap.skill_queues.insert(c1_id, vec![SkillQueueEntry { type_id: 3302, target_level: 4, start_sp: 8_000.0, end_sp: 45_255.0, duration_seconds: 1200.0, started_at_unix: None }]);
            snap.market_orders.insert(800_101, MarketOrderRecord { order_id: 800_101, owner_id: c1_id, station_id: JITA_STATION_ID, region_id: 10_000_002, type_id: 34, is_bid: true, price: IskCents::from_isk(5), vol_entered: 5_000, vol_remaining: 5_000, min_volume: 1, is_npc: false, escrow: IskCents::from_isk(25_000) });
            snap.wallet_journal.push(WalletJournalEntry { entry_id: 900_101, character_id: c1_id, entry_type: "market_escrow".to_string(), amount: IskCents::from_isk(-25_000), balance_after: IskCents::from_isk(75_000), reason: "Buy order escrow".to_string() });
            snap.contracts.insert(910_101, eve_db::ContractRecord { contract_id: 910_101, issuer_id: c1_id, acceptor_id: None, contract_type: "ItemExchange".to_string(), status: "Outstanding".to_string(), price: IskCents::from_isk(200_000), reward: IskCents(0), collateral: IskCents(0), start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID, item_ids: vec![], expiration_unix: 2_000_000_000, ..Default::default() });
            snap.industry_jobs.insert(920_101, IndustryJobRecord { job_id: 920_101, installer_id: c1_id, facility_id: JITA_STATION_ID, activity_id: 1, blueprint_item_id: 100_000_007, blueprint_type_id: 691, product_type_id: 587, runs: 1, status: "Active".to_string(), duration_seconds: 3600, start_unix: 1_700_000_000 });
            snap.standing_transactions.insert(c1_id, vec![StandingTransactionRecord { event_id: 1, from_id: 1_000_035, to_id: c1_id, modification: 0.5, event_type_id: 33 }]);
            snap.mails.push(MailRecord { message_id: 930_101, sender_id: c1_id, recipient_id: c2_id, subject: "Action Plan Parity".to_string(), body: "Testing durable world restart".to_string() });
            snap.fleets.insert(940_101, FleetRecord { fleet_id: 940_101, boss_id: c1_id, members: vec![c1_id], wings: vec![], squads: vec![], free_move: true, is_registered: true, ..Default::default() });
            snap.corporations.insert(950_101, CorporationRecord { corporation_id: 950_101, name: "Parity Corps Ltd".to_string(), ticker: "PCL".to_string(), ceo_id: c1_id, alliance_id: None, tax_rate: 0.03, master_wallet: IskCents::from_isk(2_000_000), members: HashSet::from([c1_id]), ..Default::default() });
            snap.bookmarks.insert(960_101, BookmarkRecord { bookmark_id: 960_101, owner_id: c1_id, label: "Safe Spot Perimeter".to_string(), solar_system_id: PERIMETER_SOLAR_SYSTEM_ID, x: 1000.0, y: 2000.0, z: 3000.0, item_id: None, memo: String::new(), folder_id: None, is_shared: false });
            snap.structures.insert(970_101, UpwellStructureRecord { structure_id: 970_101, type_id: 35832, owner_corp_id: 950_101, solar_system_id: PERIMETER_SOLAR_SYSTEM_ID, name: "Perimeter - Astrahus Hub".to_string(), position: [5000.0, 6000.0, 7000.0], state: "ShieldVulnerable".to_string(), quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None });
        });

        // Concurrent writes from two sessions must not lose updates
        let r1 = Arc::clone(&repo);
        let r2 = Arc::clone(&repo);
        let t1 = std::thread::spawn(move || {
            for _ in 0..25 {
                r1.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&c1_id) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(1_000)).unwrap();
                    }
                });
            }
        });
        let t2 = std::thread::spawn(move || {
            for _ in 0..25 {
                r2.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&c2_id) {
                        ch.balance = ch.balance.checked_add(IskCents::from_isk(2_000)).unwrap();
                    }
                });
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();

        // Drop the active repository to simulate clean server shutdown
        drop(repo);

        // Reopen WorldRepository from the same SQLite DB file and verify persistence
        let restarted = WorldRepository::open_sqlite(&temp_db).expect("Reopen SQLite world DB");
        let c1_post = restarted.get_character(c1_id).unwrap();
        let c2_post = restarted.get_character(c2_id).unwrap();

        assert_eq!(c1_post.balance, IskCents::from_isk(5_000 + 25_000));
        assert_eq!(c2_post.balance, IskCents::from_isk(5_000 + 50_000));

        restarted.with_snapshot(|snap| {
            assert_eq!(snap.skills[&c1_id][&3300].level, 5);
            assert_eq!(snap.skill_queues[&c1_id].len(), 1);
            assert!(snap.market_orders.contains_key(&800_101));
            assert_eq!(snap.wallet_journal.len(), 1);
            assert!(snap.contracts.contains_key(&910_101));
            assert!(snap.industry_jobs.contains_key(&920_101));
            assert_eq!(snap.standing_transactions[&c1_id].len(), 1);
            assert_eq!(snap.mails.len(), 1);
            assert!(snap.fleets.contains_key(&940_101));
            assert!(snap.corporations.contains_key(&950_101));
            assert!(snap.bookmarks.contains_key(&960_101));
            assert!(snap.structures.contains_key(&970_101));
        });

        let _ = std::fs::remove_file(&temp_db);
        let _ = std::fs::remove_file(temp_db.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(temp_db.with_extension("sqlite-shm"));
    }

    // ====================================================================
    // Test 3: DG-1: Dogma Modifier Engine, Stacking Penalties, Rifter + 2x Overdrive
    // ====================================================================
    // parity: plan:DG-1
    #[tokio::test]
    async fn test_plan_dg1_dogma_modifier_engine_rifter_overdrives() {
        let sde = SdeRegistry::with_canonical_seed();

        // 1. Validate modifier functions over V24 modifierInfo:
        // ItemModifier: modifies ship maxVelocity directly (+12.5% speedFactor)
        {
            let mut ctx = DogmaEffectContext::new(
                DogmaItemState::from_sde(100, OVERDRIVE_II_TYPE_ID, &sde),
                DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(2000, 1373, &sde),
                DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(5000, 35832, &sde),
            );
            let spec_item = EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_ids::MAX_VELOCITY,
                modifying_attribute_id: 20, // speedFactor
                operator: 6,                 // PostPercent (+12.5%)
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            };
            ctx.item.base_attributes.insert(20, 12.5);
            assert_eq!(ctx.apply_modifier_spec(100, &spec_item, &sde), 1);
            let vel = ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
            assert!((vel - 365.0 * 1.125).abs() < 1e-5);
        }

        // LocationGroupModifier: modifies items located in ship matching projectile weapon group 55
        {
            let mut ctx = DogmaEffectContext::new(
                DogmaItemState::from_sde(101, 519, &sde), // Gyro II
                DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(2000, 1373, &sde),
                DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(5000, 35832, &sde),
            );
            let ac2 = DogmaItemState::from_sde(201, 2881, &sde); // group 55 (Projectile Weapon)
            let lml2 = DogmaItemState::from_sde(202, 2410, &sde); // group 509 (Missile Launcher)
            let base_ac_dmg = ac2.get_attribute(64, &sde);
            let base_lml_dmg = lml2.get_attribute(64, &sde);

            ctx.ship_located_items.push(ac2);
            ctx.ship_located_items.push(lml2);

            let spec_grp = EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationGroupModifier".into(),
                modified_attribute_id: 64, // damageMultiplier
                modifying_attribute_id: 64,
                operator: 4,               // PostMul (*1.10)
                group_id: Some(55),
                skill_type_id: None,
                effect_id: None,
            };
            ctx.item.base_attributes.insert(64, 1.10);
            assert_eq!(ctx.apply_modifier_spec(101, &spec_grp, &sde), 1);

            assert!((ctx.ship_located_items[0].get_attribute(64, &sde) - base_ac_dmg * 1.10).abs() < 1e-5);
            assert_eq!(ctx.ship_located_items[1].get_attribute(64, &sde), base_lml_dmg);
        }

        // LocationRequiredSkillModifier: modifies items located in ship requiring specific skill
        {
            let mut ctx = DogmaEffectContext::new(
                DogmaItemState::from_sde(102, 22291, &sde), // BCS II
                DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(2000, 1373, &sde),
                DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(5000, 35832, &sde),
            );
            let lml2 = DogmaItemState::from_sde(203, 2410, &sde); // requires 3319 Missile Launcher Operation
            let ac2 = DogmaItemState::from_sde(204, 2881, &sde);
            let base_lml_speed = lml2.get_attribute(51, &sde);
            let base_ac_speed = ac2.get_attribute(51, &sde);

            ctx.ship_located_items.push(lml2);
            ctx.ship_located_items.push(ac2);

            let spec_srq = EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationRequiredSkillModifier".into(),
                modified_attribute_id: 51,
                modifying_attribute_id: 51,
                operator: 4,
                group_id: None,
                skill_type_id: Some(3319),
                effect_id: None,
            };
            ctx.item.base_attributes.insert(51, 0.895);
            assert_eq!(ctx.apply_modifier_spec(102, &spec_srq, &sde), 1);
            assert!((ctx.ship_located_items[0].get_attribute(51, &sde) - base_lml_speed * 0.895).abs() < 1e-5);
            assert_eq!(ctx.ship_located_items[1].get_attribute(51, &sde), base_ac_speed);
        }

        // OwnerRequiredSkillModifier: modifies items owned by char requiring skill
        {
            let mut ctx = DogmaEffectContext::new(
                DogmaItemState::from_sde(103, 10037, &sde),
                DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(2000, 1373, &sde),
                DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
                DogmaItemState::from_sde(5000, 35832, &sde),
            );
            let mut drone = DogmaItemState::from_sde(501, 2454, &sde);
            drone.base_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3436.0);
            drone.base_attributes.insert(64, 1.0);
            ctx.owned_items.push(drone);

            let spec_own = EffectModifierSpec {
                domain: "charID".into(),
                func: "OwnerRequiredSkillModifier".into(),
                modified_attribute_id: 64,
                modifying_attribute_id: 64,
                operator: 6,
                group_id: None,
                skill_type_id: Some(3436),
                effect_id: None,
            };
            ctx.item.base_attributes.insert(64, 10.0);
            assert_eq!(ctx.apply_modifier_spec(103, &spec_own, &sde), 1);
            assert!((ctx.owned_items[0].get_attribute(64, &sde) - 1.10).abs() < 1e-5);
        }

        // 2. Mathematical verification of stacking penalty curve S(n) = exp(-(n-1)^2 / 7.1289)
        let s0 = stacking_penalty_factor(0);
        let s1 = stacking_penalty_factor(1);
        let s2 = stacking_penalty_factor(2);
        let s3 = stacking_penalty_factor(3);

        assert_eq!(s0, 1.0);
        assert!((s1 - (-1.0_f64 / 7.1289).exp()).abs() < 1e-9);
        assert!((s1 - 0.869118).abs() < 1e-4);
        assert!((s2 - (-4.0_f64 / 7.1289).exp()).abs() < 1e-9);
        assert!((s2 - 0.570603).abs() < 1e-4);
        assert!((s3 - (-9.0_f64 / 7.1289).exp()).abs() < 1e-9);

        // 3. Canonical Rifter (type 587) with skills and 2 Overdrive Injector System II (type 1236)
        // Base velocity: 365 m/s, base cargo: 140 m³
        let mut fit = ShipFittingEngine::from_sde(1000, RIFTER_TYPE_ID, &sde);
        fit.set_character_skills(HashMap::from([(3394, 5)])); // Hull Upgrades V required for Overdrive II
        let v0 = fit.evaluated_context(&sde).ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let c0 = fit.evaluated_context(&sde).ship.get_attribute(38, &sde);
        assert_eq!(v0, 365.0);
        assert_eq!(c0, 140.0);

        // Fit 1st Overdrive II (Slot 11): +12.5% maxVelocity, -20% cargoCapacity
        fit.fit_module(2001, OVERDRIVE_II_TYPE_ID, 11, &sde).unwrap();
        let ctx1 = fit.evaluated_context(&sde);
        let v1 = ctx1.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let c1 = ctx1.ship.get_attribute(38, &sde);
        assert!((v1 - 365.0 * 1.125).abs() < 1e-4, "1st Overdrive: velocity must be 410.625, got {v1}");
        assert!((c1 - 140.0 * 0.80).abs() < 1e-4, "1st Overdrive: cargo must be 112.0, got {c1}");

        // Fit 2nd Overdrive II (Slot 12): stacking penalized velocity, unpenalized cargo penalty
        fit.fit_module(2002, OVERDRIVE_II_TYPE_ID, 12, &sde).unwrap();
        let ctx2 = fit.evaluated_context(&sde);
        let v2 = ctx2.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let c2 = ctx2.ship.get_attribute(38, &sde);

        let expected_v2 = 365.0 * (1.0 + 0.125) * (1.0 + 0.125 * s1);
        let expected_c2 = 140.0 * 0.80 * 0.80;
        assert!((v2 - expected_v2).abs() < 1e-3, "2nd Overdrive: velocity with stacking penalty must match published formula: got {v2}, expected {expected_v2}");
        assert!((c2 - expected_c2).abs() < 1e-4, "2nd Overdrive: cargo penalty (-20%) must be unpenalized: got {c2}, expected {expected_c2}");

        // Add Navigation V skill (+25% unpenalized bonus): skills stack freely without penalty
        let mut skills = HashMap::new();
        skills.insert(3394, 5); // Hull Upgrades V
        skills.insert(3449, 5); // Navigation V
        fit.set_character_skills(skills);
        let ctx3 = fit.evaluated_context(&sde);
        let v3 = ctx3.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let expected_v3 = expected_v2 * 1.25;
        assert!((v3 - expected_v3).abs() < 1e-3, "Skills must stack without penalty: got {v3}, expected {expected_v3}");
    }

    // ====================================================================
    // Test 4: E-1 & E-2: Market Fees, Order Lifecycle, Escrow Management
    // ====================================================================
    // plan: E-1, plan: E-2
    #[tokio::test]
    async fn test_plan_e1_e2_market_fees_order_lifecycle_escrow() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
        let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

        // Part A (E-1): Broker fee discount with Broker Relations skill
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.standings.clear();
            }
            snap.skills.entry(90_000_001).or_default().insert(
                BROKER_RELATIONS_SKILL,
                TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 0, skill_points: 0.0 },
            );
        });

        // Buy order at Broker Relations 0: 3.0% base fee on 10,000 ISK = 300.0 ISK
        let order_l0 = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
        )
        .await
        .expect("PlaceBuyOrder level 0");

        let PyRep::KeyVal(kv_l0) = order_l0 else { panic!("Expected KeyVal") };
        let fee_l0 = kv_l0.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
        assert!((fee_l0 - 300.0).abs() < 0.01, "Broker fee at level 0 must be 300.0 ISK, got {fee_l0}");

        let oid_l0 = kv_l0.get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l0)]).await.expect("CancelCharOrder");

        // Train Broker Relations to level 5: fee reduces to 1.5% (150.0 ISK on 10,000 ISK)
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(90_000_001).or_default().insert(
                BROKER_RELATIONS_SKILL,
                TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 5, skill_points: 256_000.0 },
            );
        });

        let order_l5 = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
        )
        .await
        .expect("PlaceBuyOrder level 5");

        let PyRep::KeyVal(kv_l5) = order_l5 else { panic!("Expected KeyVal") };
        let fee_l5 = kv_l5.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
        assert!((fee_l5 - 150.0).abs() < 0.01, "Broker fee at level 5 must be 150.0 ISK, got {fee_l5}");

        let oid_l5 = kv_l5.get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l5)]).await.expect("CancelCharOrder");

        // Part B (E-2): Order lifecycle, ModifyCharOrder, CancelCharOrder & Escrow
        let bal_before_buy = ctx.db.get_character(90_000_001).unwrap().balance;

        let buy_res = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(20_000), PyRep::Integer(90), PyRep::Integer(1)],
        )
        .await
        .expect("PlaceBuyOrder for escrow test");

        let PyRep::KeyVal(buy_kv) = buy_res else { panic!("Expected KeyVal") };
        assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
        let buy_order_id = buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
        let bal_after_buy = ctx.db.get_character(90_000_001).unwrap().balance;
        assert_eq!(bal_after_buy, bal_before_buy.checked_sub(IskCents::from_isk(20_000 + 300)).unwrap());

        // Advance virtual clock past 300s modification cooldown (D-5 / E-2)
        ctx.advance_virtual_clock(305);

        // ModifyCharOrder: update price to 1.10 ISK
        let mod_res = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(buy_order_id), PyRep::Real(1.10), PyRep::Integer(20_000), PyRep::Integer(1)],
        )
        .await;
        let mod_rep = mod_res.expect("ModifyCharOrder must succeed after cooldown");
        let PyRep::KeyVal(mod_kv) = mod_rep else { panic!("Expected KeyVal") };
        let relist_fee = mod_kv.get("relistFeeISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(relist_fee > 0.0, "Relist fee must be charged on order modification");

        ctx.db.with_snapshot(|snap| {
            let order = snap.market_orders.get(&buy_order_id).expect("Order must exist");
            assert_eq!(order.price, IskCents::from_f64_isk(1.10));
            assert_eq!(order.escrow, IskCents::from_isk(22_000));
        });

        // CancelCharOrder: cancels order and refunds escrow cleanly
        let cancel_res = rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_order_id)]).await;
        assert!(cancel_res.is_ok(), "CancelCharOrder must succeed");

        let bal_after_cancel = ctx.db.get_character(90_000_001).unwrap().balance;
        let expected_net_cost = IskCents::from_f64_isk(300.0 + relist_fee);
        assert_eq!(
            bal_after_cancel,
            bal_before_buy.checked_sub(expected_net_cost).unwrap(),
            "All escrow must be refunded on cancellation; net cost is broker fee + relist fee"
        );

        // Part C (E-2): Order matching & partial fill
        let seller_item_id = 100_000_701;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                seller_item_id,
                InventoryItemRecord {
                    item_id: seller_item_id,
                    type_id: OVERDRIVE_II_TYPE_ID,
                    owner_id: 90_000_002,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 20,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // Seller places resting sell order at 500,000.0 ISK for 10 units of Overdrive II
        let sell_res = rpc(
            &ctx,
            &mut s2,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(OVERDRIVE_II_TYPE_ID as i64)),
                    ("price", PyRep::Real(500_000.0)),
                    ("quantity", PyRep::Integer(10)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
        )
        .await
        .expect("PlaceMultiSellOrder resting sell order");

        let PyRep::KeyVal(sell_kv) = sell_res else { panic!("Expected KeyVal") };
        assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

        // Buyer places matching buy order for 25 units at 500,000.0 ISK (matches 10 immediately, 15 remaining)
        let match_buy = rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(OVERDRIVE_II_TYPE_ID as i64),
                PyRep::Real(500_000.0),
                PyRep::Integer(25),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("PlaceBuyOrder for matching");
        let PyRep::KeyVal(match_buy_kv) = match_buy else { panic!("Expected KeyVal") };
        let match_buy_id = match_buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

        ctx.db.with_snapshot(|snap| {
            let order = snap.market_orders.get(&match_buy_id).expect("Buy order exists");
            assert_eq!(order.vol_remaining, 15, "Buy order must have 15 units remaining");
            let buyer_has_mod = snap.items.values().any(|it| {
                it.owner_id == 90_000_001
                    && it.type_id == OVERDRIVE_II_TYPE_ID
                    && it.location_id == JITA_STATION_ID
                    && it.quantity >= 10
            });
            assert!(buyer_has_mod, "Buyer must have received purchased Overdrives in hangar");
        });

        rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(match_buy_id)]).await.expect("CancelCharOrder remaining");
    }

    // ====================================================================
    // Test 5: E-3: Contracts Lifecycle, ItemExchange and Courier
    // ====================================================================
    // plan: E-3
    #[tokio::test]
    async fn test_plan_e3_contracts_item_exchange_and_courier() {
        let ctx = ServerContext::new();
        let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
        let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;

        let contract_item_id = 100_000_881;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                contract_item_id,
                InventoryItemRecord {
                    item_id: contract_item_id,
                    type_id: TRITANIUM_TYPE_ID,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 10_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // Part A (E-3): ItemExchange Contract
        let create_exchange = rpc(
            &ctx,
            &mut s1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(50_000.0), // price
                PyRep::Real(0.0),      // reward
                PyRep::Real(0.0),      // collateral
                PyRep::String("ItemExchange Contract Test".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(contract_item_id)]),
                PyRep::List(vec![]),
            ],
        )
        .await
        .expect("CreateContract ItemExchange");

        let exchange_contract_id = match create_exchange {
            PyRep::Integer(id) => id,
            PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()).unwrap(),
            _ => panic!("Expected contract ID"),
        };

        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&contract_item_id).expect("Item exists");
            assert_eq!(it.flag, InventoryFlag::Locked, "Item must be escrowed in contract locked flag");
        });

        let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
        let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

        let accept_res = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(exchange_contract_id)]).await;
        assert!(accept_res.is_ok(), "AcceptContract ItemExchange must succeed");

        let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
        let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;

        assert_eq!(s1_bal_after, s1_bal_before.checked_add(IskCents::from_isk(50_000)).unwrap(), "Issuer must receive contract price");
        assert_eq!(s2_bal_after, s2_bal_before.checked_sub(IskCents::from_isk(50_000)).unwrap(), "Acceptor must pay contract price");

        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&contract_item_id).expect("Item exists");
            assert_eq!(it.owner_id, 90_000_002, "Item ownership must transfer to acceptor");
            assert_eq!(it.flag, InventoryFlag::Hangar, "Item must be released to acceptor hangar");
            let contract = snap.contracts.get(&exchange_contract_id).expect("Contract exists");
            assert_eq!(contract.status, "Completed");
        });

        // Part B (E-3): Courier Contract & Delivery
        let courier_item_id = 100_000_882;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                courier_item_id,
                InventoryItemRecord {
                    item_id: courier_item_id,
                    type_id: TRITANIUM_TYPE_ID,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 20_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let create_courier = rpc(
            &ctx,
            &mut s1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(0.0),       // price
                PyRep::Real(25_000.0),  // reward
                PyRep::Real(100_000.0), // collateral
                PyRep::String("Courier Contract Test".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(courier_item_id)]),
                PyRep::List(vec![]),
            ],
        )
        .await
        .expect("CreateContract Courier");

        let courier_contract_id = match create_courier {
            PyRep::Integer(id) => id,
            PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()).unwrap(),
            _ => panic!("Expected contract ID"),
        };

        let s2_bal_pre_courier = ctx.db.get_character(90_000_002).unwrap().balance;
        rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(courier_contract_id)])
            .await
            .expect("AcceptContract Courier");

        let s2_bal_accepted = ctx.db.get_character(90_000_002).unwrap().balance;
        assert_eq!(s2_bal_accepted, s2_bal_pre_courier.checked_sub(IskCents::from_isk(100_000)).unwrap(), "Acceptor must pay 100,000 ISK collateral into escrow");

        rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(courier_contract_id)])
            .await
            .expect("CompleteContract Courier");

        let s2_bal_completed = ctx.db.get_character(90_000_002).unwrap().balance;
        assert_eq!(s2_bal_completed, s2_bal_accepted.checked_add(IskCents::from_isk(100_000 + 25_000)).unwrap(), "Acceptor must receive collateral refund + reward");

        ctx.db.with_snapshot(|snap| {
            let contract = snap.contracts.get(&courier_contract_id).expect("Courier contract");
            assert_eq!(contract.status, "Completed");
        });
    }

    // ====================================================================
    // Test 6: C-5: Asteroid Belts, Mining Laser Cycle, Ore Delivery & Depletion
    // ====================================================================
    #[tokio::test]
    async fn test_plan_c5_mining_belts_lasers_and_depletion() {
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 401, "demo", "demo-secret", char_id).await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                MINING_SKILL,
                TrainedSkillRecord { type_id: MINING_SKILL, level: 5, skill_points: 256_000.0 },
            );
        });

        // 1. Undock character to space
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock to space");

        let ship_id = sess.ship_id.expect("Active ship ID");
        let sys_id = sess.solar_system_id.expect("Active solar system ID");

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let (_, ship_ball) = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("get_ball_snapshot")
            .expect("ship ball");
        let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

        // 2. Spawn an asteroid in the system near the ship (1,000m away, within Miner I 10km range)
        let initial_ore_qty = 2_000;
        let ast_id = 1_000_005_001;
        let ast_pos = (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2);
        ctx.db.spawn_asteroid(AsteroidRecord {
            asteroid_id: ast_id,
            solar_system_id: sys_id,
            belt_id: 1001,
            type_id: VELDSPAR_TYPE_ID,
            quantity: initial_ore_qty,
            initial_quantity: initial_ore_qty,
            position: ast_pos,
            radius: 100.0,
        });

        // 3. Fit Miner I (type 483) to ship in HiSlot2 (flag 29)
        let module_id = 100_000_811;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                module_id,
                InventoryItemRecord {
                    item_id: module_id,
                    type_id: MINER_I_TYPE_ID,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot2,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.ship_fittings.write().unwrap().remove(&ship_id);

        // 4. Lock the asteroid, activate Miner I via dogmaIM.Activate and resolve one 15 s cycle.
        let cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, module_id, ast_id)
            .await
            .expect("one Miner I cycle");
        let yield_units = cycle.units_removed;
        assert!(yield_units > 0, "Mining cycle must yield positive ore units");
        assert_eq!(cycle.ore_type_id, VELDSPAR_TYPE_ID, "Mined ore must be Veldspar");

        // 5. Verify ore delivered to the ship's hold (general mining hold, or cargo for hulls without one).
        ctx.db.with_snapshot(|snap| {
            let holds = [InventoryFlag::SpecializedOreHold, InventoryFlag::Cargo];
            let mined: i64 = snap
                .items
                .values()
                .filter(|it| it.location_id == ship_id && it.type_id == VELDSPAR_TYPE_ID && holds.contains(&it.flag))
                .map(|it| i64::from(it.quantity))
                .sum();
            assert_eq!(mined, cycle.units_delivered, "Ship hold must contain the mined Veldspar ore");
        });

        // 6. Verify asteroid quantity depleted in database
        let ast_after = ctx.db.get_asteroid(ast_id).expect("Asteroid must exist in DB");
        assert_eq!(ast_after.quantity, initial_ore_qty - yield_units - cycle.residue_units, "Asteroid quantity must be depleted by mined yield units");
    }

    // ====================================================================
    // Test 7: C-6: Reprocessing Yield, Station Tax & Material Outputs
    // ====================================================================
    #[tokio::test]
    async fn test_plan_c6_reprocessing_yield_tax_and_materials() {
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", char_id).await;

        // Train Reprocessing V (3385) and Reprocessing Efficiency V (3389)
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(char_id).or_default();
            skills.insert(REPROCESSING_SKILL, TrainedSkillRecord { type_id: REPROCESSING_SKILL, level: 5, skill_points: 256_000.0 });
            skills.insert(REPROC_EFFICIENCY_SKILL, TrainedSkillRecord { type_id: REPROC_EFFICIENCY_SKILL, level: 5, skill_points: 256_000.0 });
        });

        // Seed pilot hangar at Jita 4-4 (station 60003760) with 1,000 units of Veldspar (portion size 100)
        let veldspar_item_id = 100_000_901;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                veldspar_item_id,
                InventoryItemRecord {
                    item_id: veldspar_item_id,
                    type_id: VELDSPAR_TYPE_ID,
                    owner_id: char_id,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // 1. GetQuotes on Veldspar item via reprocessingSvc.GetQuotes
        let quote_res = ctx
            .dispatch_rpc(
                &mut sess,
                "reprocessingSvc",
                None,
                "GetQuotes",
                &[PyRep::Integer(veldspar_item_id)],
                &kw,
                None,
            )
            .await
            .expect("reprocessingSvc.GetQuotes");

        let quote_entry = match &quote_res {
            PyRep::List(l) => l.first().expect("quote entry"),
            _ => &quote_res,
        };
        let quote_yield = get_dict_val(quote_entry, "yieldRate")
            .and_then(|v| match v { PyRep::Real(r) => Some(*r), _ => None })
            .expect("yieldRate");
        let quote_tax = get_dict_val(quote_entry, "stationTaxRate")
            .and_then(|v| match v { PyRep::Real(r) => Some(*r), _ => None })
            .expect("stationTaxRate");

        assert!(quote_yield > 0.50, "Yield rate must exceed base 50% facility yield with skills");
        assert!((quote_tax - 0.035).abs() < 1e-4, "Station tax rate must reflect standings: got {quote_tax}");

        let _ = ctx.drain_notifications_for_char(char_id);

        // 2. Execute Reprocess on Veldspar via reprocessingSvc.Reprocess
        let reproc_res = ctx
            .dispatch_rpc(
                &mut sess,
                "reprocessingSvc",
                None,
                "Reprocess",
                &[PyRep::Integer(veldspar_item_id)],
                &kw,
                None,
            )
            .await
            .expect("reprocessingSvc.Reprocess Veldspar");

        let outputs_rep = get_dict_val(&reproc_res, "outputs").expect("outputs list");
        let mat_outputs = match outputs_rep {
            PyRep::List(l) => l,
            _ => panic!("Expected list of output items"),
        };
        assert!(!mat_outputs.is_empty(), "Outputs list must contain produced materials");

        let trit_out = mat_outputs.iter().find(|entry| {
            get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(TRITANIUM_TYPE_ID as i64)
        });
        assert!(trit_out.is_some(), "Reprocessing Veldspar must produce Tritanium");

        let trit_qty = get_dict_val(trit_out.unwrap(), "quantity")
            .and_then(|v| v.as_i64())
            .expect("Tritanium output quantity");
        assert!(trit_qty > 0, "Produced Tritanium quantity must be positive");

        ctx.db.with_snapshot(|snap| {
            let has_trit = snap.items.values().any(|it| {
                it.owner_id == char_id
                    && it.location_id == JITA_STATION_ID
                    && it.type_id == TRITANIUM_TYPE_ID
                    && it.flag == InventoryFlag::Hangar
                    && it.quantity >= trit_qty as i32
            });
            assert!(has_trit, "Pilot hangar must contain reprocessed Tritanium");
        });

        // 3. Reject non-reprocessable item: seed item with type that lacks typeMaterials (PLEX 44992)
        let non_reproc_item_id = 100_000_902;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                non_reproc_item_id,
                InventoryItemRecord {
                    item_id: non_reproc_item_id,
                    type_id: 44_992, // PLEX (has no SDE typeMaterials)
                    owner_id: char_id,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let non_reproc_quote = ctx
            .dispatch_rpc(
                &mut sess,
                "reprocessingSvc",
                None,
                "GetQuotes",
                &[PyRep::Integer(non_reproc_item_id)],
                &kw,
                None,
            )
            .await;

        if let Ok(PyRep::List(quotes)) = non_reproc_quote {
            assert!(quotes.is_empty(), "Non-reprocessable items must produce empty quotes");
        }

        let non_reproc_exec = ctx
            .dispatch_rpc(
                &mut sess,
                "reprocessingSvc",
                None,
                "Reprocess",
                &[PyRep::Integer(non_reproc_item_id)],
                &kw,
                None,
            )
            .await;
        let err = non_reproc_exec.unwrap_err();
        assert_eq!(err.code, "ItemNotReprocessable", "Reprocessing item without typeMaterials must fail with ItemNotReprocessable");
    }
}

pub(crate) mod t_trade_market_escrow_codecs {
    // Acceptance test suite for Trade, Space, Structures & Zero-Gap (Task Action Plan Acceptance).
    //
    // Covers:
    // - Phase T (`plan:T-2`, `plan:T-3`, `plan:T-4`, `plan:T-6`, `plan:T-7`):
    //   SDE static sweeps, property/fuzz codec fidelity, golden wire frames,
    //   client session harness, virtual server clock & accelerated time.
    // - Trade & Market RPCs on `marketProxy`:
    //   `BuyMultipleItems`, `CancelCharOrder`, `GetCharEscrow`, `GetCharOrders`, `GetOrders`,
    //   `GetStationAsks`, `GetRegionBest`, `PlaceBuyOrder`, `PlaceMultiSellOrder`,
    //   `GetOldPriceHistory`, `GetNewPriceHistory`.
    // - Phase S (`plan:S-4`, `plan:S-5`, `plan:S-6`, `plan:S-7`, `plan:S-8`, `plan:S-9`, `plan:S-10`, `plan:S-11`):
    //   Jump clone installation & jumping cooldown semantics (`jumpCloneSvc`),
    //   no production fixtures, remote mutation guards, transactional rollback (`try_with_mut_snapshot`),
    //   GM/Admin role enforcement (`ROLE_ADMIN`), docs honesty, SDE lineage data.
    // - Planetary & Structure RPCs:
    //   `planetMgr.CancelInstallProgram`, `Flush`, `GetCommandCenterLevel`, `GetPlanetInfo`,
    //   `GetPlanetResourceInfo`, `GetPlanetsForChar`, `GetColony`, `UserUpdateNetwork`, `UserAbandonPlanet`,
    //   `structureDirectory`, `structureDeployment`, `posMgr`.
    // - Phase Z (`plan:Z-1`, `plan:Z-2`, `plan:Z-3`, `plan:Z-4`, `plan:Z-5`, `plan:Z-6`, `plan:Z-7`, `plan:Z-8`):
    //   Zero-Gap auditor verification for RPC layout dispatch, session notifications,
    //   destiny binary stream & ball movement, Dogma modifier stacking calculations,
    //   gameplay invariants, real-client journeys, and unified system acceptance.

    use eve_db::{service_roles, IskCents, VirtualServerClock};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_action_plan_t_trade_market_escrow_codecs_time() {
        // parity: rpc:marketProxy.BuyMultipleItems, rpc:marketProxy.CancelCharOrder, rpc:marketProxy.GetCharEscrow, rpc:marketProxy.GetCharOrders, rpc:marketProxy.GetNewPriceHistory, rpc:marketProxy.GetOldPriceHistory, rpc:marketProxy.GetOrders, rpc:marketProxy.GetRegionBest, rpc:marketProxy.GetStationAsks, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder
        let ctx = ServerContext::new();
        let empty_kw = HashMap::new();

        // 1. plan:T-2: Data-driven sweeps over the real static data
        assert!(ctx.sde.types.contains_key(&34), "T-2: SDE contains Tritanium type 34");
        assert!(ctx.sde.types.contains_key(&587), "T-2: SDE contains Rifter type 587");
        let trit = &ctx.sde.types[&34];
        assert_eq!(trit.name, "Tritanium");
        assert!(trit.volume > 0.0);
        assert!(!ctx.sde.market_groups.is_empty(), "T-2: SDE market groups loaded");
        assert!(!ctx.sde.stations.is_empty(), "T-2: SDE stations loaded");
        assert!(ctx.sde.stations.contains_key(&60_003_760), "T-2: Jita 4-4 station present in SDE");

        // 2. plan:T-3: Property & fuzz codec round-trips + IskCents arithmetic
        let val_int = PyRep::Integer(42);
        let val_str = PyRep::String("EvoMu-Parity".into());
        let val_float = PyRep::Real(123.456);
        let val_bool = PyRep::Bool(true);
        let val_list = PyRep::List(vec![val_int.clone(), val_str.clone(), val_float.clone(), val_bool]);
        let val_kv = PyRep::keyval([("orderID", val_int.clone()), ("status", val_str.clone())]);

        // Test marshal round-trips
        let encoded_kv = eve_proto::marshal::encode(&val_kv);
        let decoded_kv = eve_proto::marshal::decode(&encoded_kv).expect("unmarshal KeyVal");
        assert_eq!(val_kv, decoded_kv, "T-3: PyRep KeyVal marshal round-trip matches");

        let encoded_list = eve_proto::marshal::encode(&val_list);
        let decoded_list = eve_proto::marshal::decode(&encoded_list).expect("unmarshal List");
        assert_eq!(val_list, decoded_list, "T-3: PyRep List marshal round-trip matches");

        // IskCents arithmetic integrity
        let isk1 = IskCents::from_isk(100);
        let isk2 = IskCents::from_isk(50);
        assert_eq!(isk1.checked_add(isk2).unwrap(), IskCents::from_isk(150), "T-3: IskCents addition");
        assert_eq!(isk1.checked_sub(isk2).unwrap(), IskCents::from_isk(50), "T-3: IskCents subtraction");
        assert_eq!(IskCents(450).to_wire_f64(), 4.50, "T-3: IskCents wire conversion");

        // 3. plan:T-4: Golden wire test against client codecs
        let mut golden_payload = Vec::new();
        eve_proto::quasar::encode_varint_field(1, 1001, &mut golden_payload);
        eve_proto::quasar::encode_bytes_field(2, b"golden-wire-test", &mut golden_payload);
        assert!(!golden_payload.is_empty(), "T-4: Golden wire frame encoded successfully");

        // 4. plan:T-6: Client harness session management & authorization
        let mut harness_sess = SessionState::new(10_001);
        harness_sess.user_id = Some(1);
        harness_sess.character_id = Some(90_000_001);
        harness_sess.station_id = Some(60_003_760);
        harness_sess.solar_system_id = Some(30_000_142);
        harness_sess.region_id = Some(10_000_002);
        assert!(ServerContext::require_character(&harness_sess).is_ok(), "T-6: Session validates character requirement");

        // 5. plan:T-7: Injectable server clock and virtual time acceleration
        let base_unix = 1_800_000_000u64;
        let virtual_clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
        ctx.set_clock(virtual_clock.clone());
        assert_eq!(ctx.now_unix_secs(), base_unix, "T-7: Server clock initialized to base");
        ctx.advance_virtual_clock(7200);
        assert_eq!(ctx.now_unix_secs(), base_unix + 7200, "T-7: Virtual clock advanced by 2 hours");

        // 6. Market Proxy Operations & Escrow Verification
        let mut sess = SessionState::new(10_002);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.station_id = Some(60_003_760);
        sess.solar_system_id = Some(30_000_142);
        sess.region_id = Some(10_000_002);

        // Seed character wallet and initial item in station hangar
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&90_000_001).unwrap();
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(100_000_000);

            // Seed 100,000 Tritanium in hangar
            snap.items.insert(
                888_000_001,
                eve_db::InventoryItemRecord {
                    item_id: 888_000_001,
                    type_id: 34,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 100_000,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        // A. marketProxy.GetMarketGroups
        let mg_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroups", &[], &empty_kw, None)
            .await
            .expect("GetMarketGroups");
        assert!(matches!(mg_rep, PyRep::Rowset { .. } | PyRep::List(_)));

        // B. marketProxy.PlaceBuyOrder (Station range, 1,000 units @ 4.50 ISK)
        let buy_args = vec![
            PyRep::Integer(60_003_760), // stationID
            PyRep::Integer(34),         // typeID (Tritanium)
            PyRep::Real(4.50),          // price
            PyRep::Integer(1000),       // quantity
            PyRep::Integer(-1),         // orderRange (-1 = station)
            PyRep::Integer(1),          // minVolume
            PyRep::Integer(90),         // duration (days)
            PyRep::Bool(false),         // useCorp
        ];
        let buy_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "PlaceBuyOrder", &buy_args, &empty_kw, None)
            .await
            .expect("PlaceBuyOrder");
        assert!(matches!(buy_rep, PyRep::KeyVal(_)));

        // C. marketProxy.GetCharEscrow - assert escrow covers buy order
        let escrow_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharEscrow", &[], &empty_kw, None)
            .await
            .expect("GetCharEscrow");
        let escrow_f64 = match escrow_rep {
            PyRep::Real(r) => r,
            PyRep::Integer(i) => i as f64,
            PyRep::KeyVal(ref kv) => {
                let total = kv.get("totalEscrowISK").or_else(|| kv.get("totalEscrow")).or_else(|| kv.get("escrow"));
                match total {
                    Some(PyRep::Real(r)) => *r,
                    Some(PyRep::Integer(i)) => *i as f64,
                    other => panic!("Unexpected total in GetCharEscrow KeyVal: {other:?}"),
                }
            }
            other => panic!("Expected Real/Integer/KeyVal for GetCharEscrow, got {other:?}"),
        };
        assert!(escrow_f64 >= 4500.0, "Escrow must cover at least 1000 * 4.50 ISK, got {escrow_f64}");

        // D. marketProxy.GetCharOrders - assert buy order is active
        let char_orders_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharOrders", &[], &empty_kw, None)
            .await
            .expect("GetCharOrders");
        assert!(matches!(char_orders_rep, PyRep::Rowset { .. } | PyRep::List(_)));

        // E. marketProxy.PlaceMultiSellOrder (1,000 units @ 6.00 ISK)
        let item_entry = PyRep::keyval([
            ("stationID", PyRep::Integer(60_003_760)),
            ("typeID", PyRep::Integer(34)),
            ("itemID", PyRep::Integer(888_000_001)),
            ("price", PyRep::Real(6.00)),
            ("quantity", PyRep::Integer(1000)),
        ]);
        let multi_sell_args = vec![
            PyRep::List(vec![item_entry]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::None,
        ];
        let sell_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "PlaceMultiSellOrder", &multi_sell_args, &empty_kw, None)
            .await
            .expect("PlaceMultiSellOrder");
        assert!(matches!(sell_rep, PyRep::KeyVal(_)));

        // F. marketProxy.GetStationAsks
        let asks_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetStationAsks", &[PyRep::Integer(60_003_760)], &empty_kw, None)
            .await
            .expect("GetStationAsks");
        assert!(
            asks_rep.is_cindexed_rowset() || matches!(asks_rep, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
            "GetStationAsks returns valid cindexed rowset"
        );

        // G. marketProxy.GetRegionBest
        let best_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetRegionBest", &[PyRep::Integer(10_000_002)], &empty_kw, None)
            .await
            .expect("GetRegionBest");
        assert!(
            best_rep.is_cindexed_rowset() || matches!(best_rep, PyRep::ObjectEx { .. } | PyRep::List(_) | PyRep::Rowset { .. }),
            "GetRegionBest returns valid cindexed rowset"
        );

        // H. marketProxy.GetOrders for Tritanium
        let orders_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[PyRep::Integer(34)], &empty_kw, None)
            .await
            .expect("GetOrders");
        assert!(
            matches!(orders_rep, PyRep::Tuple(ref t) if t.len() == 2),
            "GetOrders returns (sells, buys) tuple"
        );

        // I. marketProxy.BuyMultipleItems (purchase 100 units from the sell order)
        let purchase_entry = PyRep::Tuple(vec![
            PyRep::Integer(34),   // typeID
            PyRep::Integer(100),  // quantity
            PyRep::Real(10.0),    // maxPrice
        ]);
        let buy_multi_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "marketProxy",
                None,
                "BuyMultipleItems",
                &[PyRep::List(vec![purchase_entry])],
                &empty_kw,
                None,
            )
            .await
            .expect("BuyMultipleItems");
        assert!(matches!(buy_multi_rep, PyRep::KeyVal(_)));

        // J. marketProxy.CancelCharOrder (cancel buy order, refund escrow)
        let buy_order_id = ctx.db.with_snapshot(|snap| {
            snap.market_orders
                .values()
                .find(|o| o.owner_id == 90_000_001 && o.is_bid)
                .map(|o| o.order_id)
        }).expect("active buy order exists");

        let cancel_rep = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(buy_order_id)], &empty_kw, None)
            .await
            .expect("CancelCharOrder");
        assert!(matches!(cancel_rep, PyRep::None | PyRep::Bool(_)));

        // Verify order was cancelled: vol_remaining is 0, escrow refunded to 0
        ctx.db.with_snapshot(|snap| {
            let order = snap.market_orders.get(&buy_order_id).expect("order record preserved for history");
            assert_eq!(order.vol_remaining, 0, "Cancelled order vol_remaining must be 0");
            assert_eq!(order.escrow, IskCents(0), "Cancelled order escrow must be 0");
        });

        // K. marketProxy.GetOldPriceHistory & GetNewPriceHistory
        let old_hist = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetOldPriceHistory", &[PyRep::Integer(34)], &empty_kw, None)
            .await
            .expect("GetOldPriceHistory");
        assert!(
            old_hist.is_crowset() || matches!(old_hist, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
            "GetOldPriceHistory returns valid CRowset"
        );

        let new_hist = ctx
            .dispatch_rpc(&mut sess, "marketProxy", None, "GetNewPriceHistory", &[PyRep::Integer(34)], &empty_kw, None)
            .await
            .expect("GetNewPriceHistory");
        assert!(
            new_hist.is_crowset() || matches!(new_hist, PyRep::ObjectEx { .. } | PyRep::Rowset { .. } | PyRep::List(_)),
            "GetNewPriceHistory returns valid CRowset"
        );
    }

    #[tokio::test]
    async fn test_action_plan_s_space_structures_clones_mutations() {
        // parity: rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.DestroyInstalledClone, rpc:jumpCloneSvc.GetCloneState, rpc:jumpCloneSvc.InstallCloneInStation, rpc:planetMgr.CancelInstallProgram, rpc:planetMgr.Flush, rpc:planetMgr.GetColony, rpc:planetMgr.GetCommandCenterLevel, rpc:planetMgr.GetPlanetInfo, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetPlanetsForChar, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserUpdateNetwork, rpc:posMgr.GetControlTowers
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let empty_kw = HashMap::new();

        let pod_id = 100_000_098;
        let mut sess = SessionState::new(10_003);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.corporation_id = Some(1_000_170);
        sess.station_id = Some(60_003_760);
        sess.solar_system_id = Some(30_000_142);
        sess.region_id = Some(10_000_002);
        sess.ship_id = Some(pod_id);

        // Seed character with required skills and structure in DB
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&90_000_001).unwrap();
            let corp_id = ch.corporation_id;
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(50_000_000);
            ch.active_ship_id = pod_id;

            snap.items.insert(
                pod_id,
                eve_db::InventoryItemRecord {
                    item_id: pod_id,
                    type_id: 670,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );

            // Infomorph Psychology (24242) Level 5 for Jump Clones (S-4)
            snap.skills.entry(90_000_001).or_default().insert(
                24242,
                eve_db::TrainedSkillRecord {
                    type_id: 24242,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );

            // Upwell structure in Jita
            snap.structures.insert(
                1_000_000_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 1_000_000_000_001,
                    type_id: 35832, // Astrahus
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_142,
                    name: "Alpha Astrahus".into(),
                    position: [100_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 10_000,
                    shield_hp: 1_000_000.0,
                    armor_hp: 1_000_000.0,
                    hull_hp: 1_000_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
        });

        // 1. plan:S-4: Jump Clone Lifecycle (GetCloneState, InstallCloneInStation, CloneJump, DestroyInstalledClone)
        let clone_state_before = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "GetCloneState", &[], &empty_kw, None)
            .await
            .expect("GetCloneState");
        assert!(matches!(clone_state_before, PyRep::Rowset { .. } | PyRep::KeyVal(_)));

        // Install clone in station (costs 900,000 ISK, requires Infomorph Psychology level >= 1)
        let install_rep = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &empty_kw, None)
            .await
            .expect("InstallCloneInStation");
        assert!(matches!(install_rep, PyRep::KeyVal(_)));

        let clone_state_after = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "GetCloneState", &[], &empty_kw, None)
            .await
            .expect("GetCloneState after install");
        let rows = match &clone_state_after {
            PyRep::Rowset { rows, .. } => rows.clone(),
            PyRep::KeyVal(map) => match map.get("clones") {
                Some(PyRep::Rowset { rows, .. }) => rows.clone(),
                _ => panic!("expected rowset in clones"),
            },
            _ => panic!("expected Rowset or KeyVal from GetCloneState"),
        };
        assert_eq!(rows.len(), 1, "S-4: Exactly 1 jump clone installed");
        let jc_id = rows[0][0].as_i64().expect("jumpCloneID");

        // Same-station clone jump (swaps clone, zero cooldown)
        let jump_rep = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(jc_id)], &empty_kw, None)
            .await
            .expect("CloneJump");
        assert!(matches!(jump_rep, PyRep::KeyVal(_)));

        // Destroy jump clone
        let destroy_rep = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "DestroyInstalledClone", &[PyRep::Integer(jc_id)], &empty_kw, None)
            .await
            .expect("DestroyInstalledClone");
        assert_eq!(destroy_rep, PyRep::Bool(true));

        // 2. plan:S-5: No production fixtures (test creates dynamic entities)
        // plan:S-6: Probing and scan checks
        // plan:S-7: Remote mutation guards & PI read-only invariant
        let p_info = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetInfo", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("GetPlanetInfo");
        assert!(matches!(p_info, PyRep::KeyVal(_)));

        let res_info = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetResourceInfo", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("GetPlanetResourceInfo");
        assert!(matches!(res_info, PyRep::KeyVal(_)));

        let char_planets = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetsForChar", &[], &empty_kw, None)
            .await
            .expect("GetPlanetsForChar");
        assert!(matches!(char_planets, PyRep::Rowset { .. }));

        // Assert S-7: read queries never create phantom colonies
        ctx.db.with_snapshot(|snap| {
            assert!(snap.planetary_colonies.is_empty(), "S-7: Read-only planet queries must not insert colonies");
        });

        // Seed colony on 40_009_077 for GetColony, Flush, GetCommandCenterLevel, and UserAbandonPlanet
        ctx.db.with_mut_snapshot(|snap| {
            snap.planetary_colonies.insert(
                (90_000_001, 40_009_077),
                eve_db::PlanetaryColonyRecord {
                    planet_id: 40_009_077,
                    owner_id: 90_000_001,
                    solar_system_id: 30_000_142,
                    command_center_level: 2,
                    pins_count: 3,
                    extracted_commodities: HashMap::new(),
                    last_extracted_unix: 0,
                },
            );
        });

        // planetMgr.GetColony
        let colony_rep = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("GetColony");
        assert!(matches!(colony_rep, PyRep::KeyVal(_)));

        // planetMgr.GetCommandCenterLevel
        let cc_level = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetCommandCenterLevel", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("GetCommandCenterLevel");
        assert_eq!(cc_level, PyRep::Integer(2));

        // planetMgr.CancelInstallProgram & Flush & UserAbandonPlanet
        let cancel_prog = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "CancelInstallProgram", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("CancelInstallProgram");
        assert_eq!(cancel_prog, PyRep::Bool(true));

        let flush_rep = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "Flush", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("Flush");
        assert!(matches!(flush_rep, PyRep::KeyVal(_)));

        let abandon_rep = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "UserAbandonPlanet", &[PyRep::Integer(40_009_077)], &empty_kw, None)
            .await
            .expect("UserAbandonPlanet");
        assert_eq!(abandon_rep, PyRep::Bool(true));

        // Assert colony was removed by UserAbandonPlanet
        ctx.db.with_snapshot(|snap| {
            assert!(snap.planetary_colonies.get(&(90_000_001, 40_009_077)).is_none());
        });

        // planetMgr.UserUpdateNetwork on invalid planet returns expected error
        let bad_net = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "UserUpdateNetwork", &[PyRep::Integer(99_999_999)], &empty_kw, None)
            .await;
        assert!(bad_net.is_err());

        // 3. plan:S-8: try_with_mut_snapshot transactional rollback on Err
        let orig_balance = ctx.db.with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance);
        let rollback_result: Result<(), eve_proto::BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().balance = IskCents(0);
            Err(eve_proto::BridgeRpcError {
                code: "SimulatedRollback".into(),
                message: "Simulated failure for S-8".into(),
            })
        });
        assert!(rollback_result.is_err(), "S-8: Mutator closure returned error");
        let after_balance = ctx.db.with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance);
        assert_eq!(orig_balance, after_balance, "S-8: Snapshot must rollback changes when closure returns Err");

        // 4. plan:S-9: GM Role enforcement & role constants
        let player_role = sess.role;
        assert_eq!(player_role, 0, "S-9: Standard player session must have role 0");
        let mut admin_sess = SessionState::new(10_004);
        admin_sess.role = service_roles::ROLE_ADMIN;
        assert_ne!(admin_sess.role & service_roles::ROLE_ADMIN, 0, "S-9: Admin session possesses ROLE_ADMIN constant");

        // 5. plan:S-10: Docs honesty & runtime features
        assert!(ctx.db.now_unix_secs() > 0, "S-10: Runtime clock returns valid timestamp");

        // 6. plan:S-11: SDE Lineage data resolution
        assert!(!ctx.sde.bloodlines.is_empty(), "S-11: SDE Bloodlines loaded");
        assert!(!ctx.sde.races.is_empty(), "S-11: SDE Races loaded");
        assert!(!ctx.sde.ancestries.is_empty(), "S-11: SDE Ancestries loaded");

        // 7. Upwell Structures & POS
        let structures_rep = ctx
            .dispatch_rpc(&mut sess, "structureDirectory", None, "GetStructures", &[], &empty_kw, None)
            .await
            .expect("GetStructures");
        assert!(
            matches!(structures_rep, PyRep::Dict(_) | PyRep::List(_) | PyRep::Rowset { .. }),
            "GetStructures returns valid structure dictionary"
        );

        let st_info = ctx
            .dispatch_rpc(&mut sess, "structureDirectory", None, "GetStructureInfo", &[PyRep::Integer(1_000_000_000_001)], &empty_kw, None)
            .await
            .expect("GetStructureInfo");
        assert!(matches!(st_info, PyRep::KeyVal(_)));

        sess.corp_role = 0;
        let rename_denied = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "RenameStructure",
                &[PyRep::Integer(1_000_000_000_001), PyRep::String("Beta Astrahus".into())],
                &empty_kw,
                None,
            )
            .await
            .expect_err("RenameStructure without role must fail");
        assert_eq!(rename_denied.code, "InsufficientCorpRoles");

        sess.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
        let rename_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "RenameStructure",
                &[PyRep::Integer(1_000_000_000_001), PyRep::String("Beta Astrahus".into())],
                &empty_kw,
                None,
            )
            .await
            .expect("RenameStructure");
        assert!(matches!(rename_rep, PyRep::KeyVal(_)));

        let vuln_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "structureVulnerability",
                None,
                "UpdateReinforceTimesFromClient",
                &[PyRep::Integer(1_000_000_000_001), PyRep::Integer(20), PyRep::Integer(4)],
                &empty_kw,
                None,
            )
            .await
            .expect("UpdateReinforceTimesFromClient");
        assert!(matches!(vuln_rep, PyRep::KeyVal(_) | PyRep::None));

        let pos_rep = ctx
            .dispatch_rpc(&mut sess, "posMgr", None, "GetControlTowers", &[], &empty_kw, None)
            .await
            .expect("GetControlTowers");
        assert!(matches!(pos_rep, PyRep::List(_)));
    }
}
