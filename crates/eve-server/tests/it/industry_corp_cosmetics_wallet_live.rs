//! End-to-end live integration tests for Blueprint ME Research, Capital Construction
//! Restrictions, Facility Taxes, Industry Job Costs, Corporation Project Contribution Methods,
//! Member Role Audit, Structure Reinforce Default, and NPC-NPC / Derived Standings.

#![forbid(unsafe_code)]

use crate::common;
use crate::corp_member_audit_reinforce_and_quit::found_corp_with_member;
use crate::fleet_boss_formations_corp_projects_live::{build_project_create_msg, quasar_call};
use crate::industry_live_support::{
    acquire, board_loaded_freighter, buy, deploy_online_upwell, dock_station, dock_structure,
    fuel_cargo, fund_pilots, install, int, items_at, login, move_item, nearest_system, owned_qty,
    real, rpc, small_mineral_bp, train_skills, upwell_cargo, JITA_STATION, PERIMETER_SYS, PILOT_1,
    PILOT_2,
};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::formulas::compute_manufacturing_material_qty;
use eve_server::quasar_gateway::STATUS_FORBIDDEN;
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::load_mission_meta;
use eve_server::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id;
use eve_server::world_seeder::seed_full_market_catalog;

const RIFTER_BP: u32 = 691;
const TRITANIUM: u32 = 34;
const PYERITE: u32 = 35;
const VELDSPAR: u32 = 1230;
const RAITARU: u32 = 35_825;
const AZBEL: u32 = 35_826;
const SOTIYO: u32 = 35_827;
const MFG_PLANT: u32 = 35_878;
const CAP_SHIPYARD: u32 = 35_881;
const SUPERCAP_SHIPYARD: u32 = 35_877;

async fn finish_job(ctx: &ServerContext, sess: &mut SessionState, job: &PyRep) -> PyRep {
    let dur = int(job, "durationSeconds") as u64;
    ctx.advance_virtual_clock(dur + 2);
    rpc(ctx, sess, "industryManager", "CompleteJob", &[PyRep::Integer(int(job, "jobID"))])
        .await
        .expect("CompleteJob")
}

#[tokio::test]
async fn test_live_industry_blueprint_me_research_and_production_savings() {
    // parity: feat:industry.blueprint-me-research
    eve_testkit::attest(&["feat:industry.blueprint-me-research"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 50_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 9101, "demo", PILOT_1).await;
    train_skills(&ctx, &mut s1, &[3380, 3387, 3388, 3403, 3406, 3409], true).await;

    let bpo_res = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];

    let base_me_sec = ctx.sde.blueprints[&RIFTER_BP].research_me_time_sec.max(1) as f64;
    let me_skill_mult = (1.0 - 0.05 * 5.0) * (1.0 - 0.03 * 5.0);

    // 1. Research ME 0 -> 3 (exponential steps 105 + 250 + 595 = 950s scaled)
    let job_0_3 = install(&ctx, &mut s1, bpo_res, JITA_STATION, 4, 3, &[])
        .await
        .expect("Install ME 0->3");
    let exp_0_3 = (950.0 * (base_me_sec / 105.0) * me_skill_mult).round().max(1.0) as i64;
    assert_eq!(int(&job_0_3, "durationSeconds"), exp_0_3);
    finish_job(&ctx, &mut s1, &job_0_3).await;

    let data_me3 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_res)])
        .await
        .expect("GetBlueprintData ME3");
    assert_eq!(int(&data_me3, "materialEfficiency"), 3);

    // 2. Research ME 3 -> 5 (exponential steps 1414 + 3360 = 4774s scaled)
    let job_3_5 = install(&ctx, &mut s1, bpo_res, JITA_STATION, 4, 2, &[])
        .await
        .expect("Install ME 3->5");
    let exp_3_5 = (4_774.0 * (base_me_sec / 105.0) * me_skill_mult).round().max(1.0) as i64;
    assert_eq!(int(&job_3_5, "durationSeconds"), exp_3_5);
    finish_job(&ctx, &mut s1, &job_3_5).await;

    let data_me5 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_res)])
        .await
        .expect("GetBlueprintData ME5");
    assert_eq!(int(&data_me5, "materialEfficiency"), 5);

    // 3. Verify 5% mineral reduction in a real 10-run manufacturing job vs ME=0 BPO
    let mfg_bp = small_mineral_bp(&ctx);
    let bpo_mfg_res = buy(&ctx, &mut s1, mfg_bp, 1).await[0];
    let job_mfg_me5 = install(&ctx, &mut s1, bpo_mfg_res, JITA_STATION, 4, 5, &[])
        .await
        .expect("Research ME 0->5 on module BPO");
    finish_job(&ctx, &mut s1, &job_mfg_me5).await;
    let bpo_mfg_me0 = buy(&ctx, &mut s1, mfg_bp, 1).await[0];

    let mfg_mats = ctx.sde.blueprints[&mfg_bp].materials.clone();
    let runs = 10_i64;
    for m in &mfg_mats {
        let need_me0 = compute_manufacturing_material_qty(runs as u32, m.quantity, 0, 0.0, 0.0) as i64;
        acquire(&ctx, &mut s1, m.type_id, need_me0 * 2).await;
    }

    let before_mat = owned_qty(&ctx, PILOT_1, JITA_STATION, mfg_mats[0].type_id);
    let base_mat = mfg_mats[0].quantity;
    let exp_consumed_me5 = compute_manufacturing_material_qty(runs as u32, base_mat, 5, 0.0, 0.0) as i64;
    let exp_consumed_me0 = compute_manufacturing_material_qty(runs as u32, base_mat, 0, 0.0, 0.0) as i64;
    assert!(exp_consumed_me5 < exp_consumed_me0);

    let mfg_me5 = install(&ctx, &mut s1, bpo_mfg_res, JITA_STATION, 1, runs, &[])
        .await
        .expect("Install ME=5 manufacturing job");
    let mid_mat = owned_qty(&ctx, PILOT_1, JITA_STATION, mfg_mats[0].type_id);
    assert_eq!(before_mat - mid_mat, exp_consumed_me5, "ME=5 job must deduct 5% fewer materials");
    finish_job(&ctx, &mut s1, &mfg_me5).await;

    let mfg_me0 = install(&ctx, &mut s1, bpo_mfg_me0, JITA_STATION, 1, runs, &[])
        .await
        .expect("Install ME=0 manufacturing job");
    let after_mat = owned_qty(&ctx, PILOT_1, JITA_STATION, mfg_mats[0].type_id);
    assert_eq!(mid_mat - after_mat, exp_consumed_me0, "ME=0 job must deduct unreduced base materials");
    finish_job(&ctx, &mut s1, &mfg_me0).await;

    // 4. Research ME 5 -> 10 (max cap) and reject ME > 10
    let job_5_10 = install(&ctx, &mut s1, bpo_res, JITA_STATION, 4, 5, &[])
        .await
        .expect("Install ME 5->10");
    finish_job(&ctx, &mut s1, &job_5_10).await;
    let data_me10 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_res)])
        .await
        .expect("GetBlueprintData ME10");
    assert_eq!(int(&data_me10, "materialEfficiency"), 10);
    let over_cap = install(&ctx, &mut s1, bpo_res, JITA_STATION, 4, 1, &[]).await;
    assert!(over_cap.is_err(), "Researching beyond ME 10 must fail");
}

#[tokio::test]
async fn test_live_industry_capital_and_supercapital_restrictions() {
    // parity: feat:industry.capital-construction-restrictions
    eve_testkit::attest(&["feat:industry.capital-construction-restrictions"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 150_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;
    let mut s1 = login(&ctx, 9103, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9104, "other", PILOT_2).await;

    let find_bp = |grp: u32| -> u32 {
        *ctx.sde
            .blueprints
            .iter()
            .filter(|(tid, b)| {
                ctx.sde.types.get(tid).is_some_and(|t| t.published && t.market_group_id.is_some())
                    && ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == grp)
            })
            .map(|(tid, _)| tid)
            .min()
            .unwrap()
    };
    let dread_bp = find_bp(485);
    let titan_bp = find_bp(30);
    let charon_bp = find_bp(513);

    let mut skills = vec![3363, 3380, 3387, 3388, 24625, 20_185, AZBEL, SOTIYO, MFG_PLANT, CAP_SHIPYARD, SUPERCAP_SHIPYARD, dread_bp, titan_bp, charon_bp];
    skills.extend([11_448, 11_452, 21_790, 21_791, 23_087, 23_121]);
    train_skills(&ctx, &mut s1, &skills, true).await;

    let dread_bpo = buy(&ctx, &mut s1, dread_bp, 1).await[0];
    let titan_bpo = buy(&ctx, &mut s1, titan_bp, 1).await[0];

    // 1. High-Sec NPC Station rejects Dreadnought and Titan
    let hs_dread = install(&ctx, &mut s1, dread_bpo, JITA_STATION, 1, 1, &[])
        .await
        .expect_err("Dreadnought forbidden in High-Sec");
    assert_eq!(hs_dread.code, "CapitalConstructionForbiddenInHighSec");

    let hs_titan = install(&ctx, &mut s1, titan_bpo, JITA_STATION, 1, 1, &[])
        .await
        .expect_err("Titan forbidden in High-Sec");
    assert_eq!(hs_titan.code, "SupercapitalRequiresSovereignNullSec");

    // 2. Jump Freighter BPC is exempt from High-Sec capital ban
    let charon_bpo = buy(&ctx, &mut s1, charon_bp, 1).await[0];
    let cp = install(&ctx, &mut s1, charon_bpo, JITA_STATION, 5, 1, &[("licensedRuns", PyRep::Integer(1))])
        .await
        .expect("Copy Charon BPO");
    let charon_bpc = int(&finish_job(&ctx, &mut s1, &cp).await, "createdBpcItemID");
    let inv = ctx.sde.blueprints[&charon_bp]
        .activities
        .get("invention")
        .or_else(|| ctx.sde.blueprints[&charon_bp].activities.get("8"))
        .unwrap()
        .clone();
    for m in &inv.materials {
        acquire(&ctx, &mut s1, m.type_id, m.quantity as i64).await;
    }
    let ij = install(&ctx, &mut s1, charon_bpc, JITA_STATION, 8, 1, &[]).await.expect("Invent JF");
    ctx.set_mechanic_rng_seed(Some(crate::npc_spawns_pve::find_multi_hit_seed(&ctx, 0.01, 1)));
    let jf_bpc = int(&finish_job(&ctx, &mut s1, &ij).await, "createdBpcItemID");
    let jf_check = install(&ctx, &mut s1, jf_bpc, JITA_STATION, 1, 1, &[]).await.expect_err("JF needs materials");
    assert_eq!(jf_check.code, "InsufficientManufacturingMaterials");

    // 3. Low-Sec Azbel: rejected without Standup Capital Shipyard I (35881), accepted once fitted
    found_corp_with_member(&ctx, &mut s1, &mut s2, "CAPC").await;
    ctx.advance_virtual_clock(8 * 86_400);
    let lowsec_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| {
        ctx.sde.solar_systems.get(&sys).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id != 10_000_070)
    });
    let mut cargo = upwell_cargo(&ctx, AZBEL, &[MFG_PLANT, CAP_SHIPYARD]);
    cargo.extend([(dread_bp, 1), (titan_bp, 1)]);
    cargo.extend(fuel_cargo());
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let ls_dread_bpo = items_at(&ctx, PILOT_1, freighter, dread_bp)[0].0;
    let ls_titan_bpo = items_at(&ctx, PILOT_1, freighter, titan_bp)[0].0;

    let (azbel_id, _) = deploy_online_upwell(&ctx, &mut s1, lowsec_sys, AZBEL, &[MFG_PLANT], 3_000).await;
    let no_shipyard = install(&ctx, &mut s1, ls_dread_bpo, azbel_id, 1, 1, &[])
        .await
        .expect_err("Azbel without Capital Shipyard must reject capital");
    assert_eq!(no_shipyard.code, "FacilityNotCapitalCapable");

    // Fit & online Standup Capital Shipyard I into slot 165 -> capital construction gate now passes
    let shipyard_item = items_at(&ctx, PILOT_1, freighter, CAP_SHIPYARD)[0].0;
    let fit = rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(shipyard_item), PyRep::Integer(azbel_id), PyRep::Integer(165)])
        .await
        .expect("Fit Capital Shipyard");
    assert_eq!(fit.dict_get("online").and_then(|v| v.as_bool()), Some(true));
    let with_shipyard = install(&ctx, &mut s1, ls_dread_bpo, azbel_id, 1, 1, &[])
        .await
        .expect_err("Capital gate passed, awaits materials");
    assert_eq!(with_shipyard.code, "InsufficientManufacturingMaterials");

    let ls_super = install(&ctx, &mut s1, ls_titan_bpo, azbel_id, 1, 1, &[])
        .await
        .expect_err("Titan forbidden in Low-Sec Azbel");
    assert_eq!(ls_super.code, "SupercapitalRequiresSovereignNullSec");
}

#[tokio::test]
async fn test_live_industry_facility_tax_and_job_cost_formula() {
    // parity: feat:industry.facility-tax, feat:industry.job-cost
    eve_testkit::attest(&["feat:industry.facility-tax", "feat:industry.job-cost"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;
    let mut s1 = login(&ctx, 9105, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9106, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 3388, 20_185, RAITARU, MFG_PLANT], true).await;

    // 1. Verify NPC station facility tax = 0.25%
    let npc_tax_rep = rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(JITA_STATION)])
        .await
        .expect("GetFacilityTaxes Jita");
    assert!((real(&npc_tax_rep, "tax") - 0.0025).abs() < 1e-6);

    let tax_bp = small_mineral_bp(&ctx);
    let bpo = buy(&ctx, &mut s1, tax_bp, 1).await[0];
    let mats = ctx.sde.blueprints[&tax_bp].materials.clone();
    for m in &mats {
        acquire(&ctx, &mut s1, m.type_id, (m.quantity * 20) as i64).await;
    }

    let jita_job = install(&ctx, &mut s1, bpo, JITA_STATION, 1, 5, &[])
        .await
        .expect("Install 5-run job in Jita");
    let jita_eiv = real(&jita_job, "estimatedItemValueISK") * 5.0;
    let jita_fac_tax = real(&jita_job, "facilityTaxISK");
    let jita_scc = real(&jita_job, "sccSurchargeISK");
    let jita_base = real(&jita_job, "baseJobCost");
    let jita_total = real(&jita_job, "totalJobCostISK");
    assert!((jita_fac_tax - ((jita_eiv * 0.0025 * 100.0).round() / 100.0)).abs() < 0.05);
    assert!((jita_total - (jita_base + jita_fac_tax + jita_scc)).abs() < 0.05);
    finish_job(&ctx, &mut s1, &jita_job).await;

    // 2. Deploy Raitaru in Perimeter, test role-gated SetFacilityTaxes (4.0%), verify corp tax credit + SCI rise
    let corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "TAXC").await;
    ctx.advance_virtual_clock(8 * 86_400);

    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
    cargo.push((tax_bp, 1));
    for m in &mats {
        cargo.push((m.type_id, (m.quantity * 25) as i64));
    }
    cargo.extend(fuel_cargo());
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let st_bpo = items_at(&ctx, PILOT_1, freighter, tax_bp)[0].0;
    let (raitaru_id, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 1_500).await;

    let denied = rpc(
        &ctx,
        &mut s2,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.05))])],
    )
    .await
    .expect_err("Non-director cannot set facility tax");
    assert_eq!(denied.code, "InsufficientCorpRoles");

    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.04))])],
    )
    .await
    .expect("SetFacilityTaxes 4%");
    let raitaru_tax = rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(raitaru_id)])
        .await
        .expect("GetFacilityTaxes Raitaru");
    assert!((real(&raitaru_tax, "1") - 0.04).abs() < 1e-6);

    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.raw_cents());
    let pilot_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());

    let job_1 = install(&ctx, &mut s1, st_bpo, raitaru_id, 1, 5, &[])
        .await
        .expect("Install 5-run job in Raitaru");
    let st_eiv = real(&job_1, "estimatedItemValueISK") * 5.0;
    let st_tax_isk = real(&job_1, "facilityTaxISK");
    let sci_1 = real(&job_1, "systemCostIndex");
    assert!((st_tax_isk - ((st_eiv * 0.04 * 100.0).round() / 100.0)).abs() < 0.05);

    let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.raw_cents());
    let pilot_bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());
    let tax_cents = (st_tax_isk * 100.0).round() as i64;
    let total_cents = (real(&job_1, "totalJobCostISK") * 100.0).round() as i64;
    assert_eq!(corp_bal_after - corp_bal_before, tax_cents, "Owner corp wallet must receive facility tax");
    assert_eq!(pilot_bal_before - pilot_bal_after, total_cents, "Pilot wallet must pay full job installation cost");
    finish_job(&ctx, &mut s1, &job_1).await;

    // 3. Subsequent manufacturing job in Perimeter observes increased System Cost Index
    let job_2 = install(&ctx, &mut s1, st_bpo, raitaru_id, 1, 5, &[])
        .await
        .expect("Install second job in Raitaru");
    let sci_2 = real(&job_2, "systemCostIndex");
    assert!(sci_2 > sci_1, "System cost index must increase with recent manufacturing volume");
    finish_job(&ctx, &mut s1, &job_2).await;
}

#[tokio::test]
async fn test_live_corp_projects_audit_reinforce_and_npc_standings() {
    // parity: feat:corporation.corp-project-contribution-methods, feat:corporation.member-audit, feat:corporation.structure-reinforce-default, feat:standings.npc-npc-standings
    eve_testkit::attest(&[
        "feat:corporation.corp-project-contribution-methods",
        "feat:corporation.member-audit",
        "feat:corporation.structure-reinforce-default",
        "feat:standings.npc-npc-standings",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 60_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 9107, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9108, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[20_185, RAITARU], true).await;

    let corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "PRJ1").await;
    ctx.advance_virtual_clock(8 * 86_400);

    // 1. Structure Reinforce Default + Member Role Audit
    let denied = rpc(&ctx, &mut s2, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(21)])
        .await
        .expect_err("Non-director rejected on reinforce default");
    assert_eq!(denied.code, "CorpRoleDenied");

    rpc(&ctx, &mut s1, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(21)])
        .await
        .expect("Director sets reinforce default 21");
    let def_hr = rpc(&ctx, &mut s2, "corpRegistry", "GetStructureReinforceDefault", &[])
        .await
        .expect("GetStructureReinforceDefault");
    assert_eq!(def_hr.as_i64(), Some(21));

    let cur_roles = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::String("Logistics Officer".into()),
            PyRep::Integer(corp_roles::ROLE_STATION_MANAGER as i64),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Grant ROLE_STATION_MANAGER");
    assert_eq!(cur_roles.dict_get("roles").and_then(|v| v.as_i64()), Some(corp_roles::ROLE_STATION_MANAGER as i64));

    let audit_log = rpc(&ctx, &mut s1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("corpmgr.AuditMember");
    let PyRep::Rowset { rows, .. } = audit_log else {
        panic!("AuditMember must return Rowset");
    };
    assert!(
        rows.iter().any(|r| {
            r.first().and_then(|v| v.as_i64()) == Some(PILOT_2)
                && r.get(3).and_then(|v| v.as_i64()) == Some(corp_roles::ROLE_STATION_MANAGER as i64)
        }),
        "Member audit log must record role change for PILOT_2"
    );

    // 2. Corporation Project Contribution Methods (DeliverItem + MineMaterials + anti-spoof)
    let mut spoof_req = Vec::new();
    let mut fake_uuid = Vec::new();
    encode_bytes_field(1, &[0xBB; 16], &mut fake_uuid);
    encode_bytes_field(1, &fake_uuid, &mut spoof_req);
    encode_varint_field(3, 500, &mut spoof_req);
    let spoof_rsp = quasar_call(&ctx, &s2, "eve_public.goal.api.SetCurrentProgressRequest", spoof_req);
    assert_eq!(spoof_rsp.status_code, STATUS_FORBIDDEN, "Client progress spoofing must fail closed");

    let deliv_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.goal.api.CreateRequest",
        build_project_create_msg("Supply Jita Tritanium", 200, "DeliverItem", TRITANIUM as i64),
    );
    let pid_deliv = goal_uuid_to_project_id(&deliv_rsp.payload.unwrap().value[4..20]).unwrap();

    let mine_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.goal.api.CreateRequest",
        build_project_create_msg("Mine Belt Veldspar", 2_000, "MineMaterials", VELDSPAR as i64),
    );
    let pid_mine = goal_uuid_to_project_id(&mine_rsp.payload.unwrap().value[4..20]).unwrap();

    // Deliver wrong item -> no progress on Tritanium project; deliver 200 Tritanium -> completed
    let pyer = buy(&ctx, &mut s1, PYERITE, 10).await[0];
    rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(pyer), PyRep::Integer(1)])
        .await
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_deliv].current_quantity), 0);

    acquire(&ctx, &mut s1, TRITANIUM, 200).await;
    let trit = items_at(&ctx, PILOT_1, JITA_STATION, TRITANIUM)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(trit), PyRep::Integer(1)])
        .await
        .expect("Deliver 200 Tritanium to corp hangar");
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_deliv].current_quantity), 200);

    // Trigger live mining gameplay event for corp member PILOT_2 -> advances MineMaterials project
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(
        &ctx,
        PILOT_2,
        2_000,
        PERIMETER_SYS,
        VELDSPAR,
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_mine].current_quantity), 2_000);

    // 3. NPC-NPC Standings Rowset & Derived Faction Standings from Agent Mission Completion
    let npc_matrix = rpc(&ctx, &mut s1, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { rows, .. } = npc_matrix else {
        panic!("Expected Rowset from GetNPCNPCStandings");
    };
    let npc_rel = |f: i64, t: i64| -> f64 {
        rows.iter()
            .find(|r| r.first().and_then(|v| v.as_i64()) == Some(f) && r.get(1).and_then(|v| v.as_i64()) == Some(t))
            .and_then(|r| r.get(2).and_then(|v| v.as_f64()))
            .unwrap()
    };
    assert!((npc_rel(500_003, 500_001) - 5.0).abs() < 1e-6);
    assert!((npc_rel(500_003, 500_004) - (-3.0)).abs() < 1e-6);

    // Complete a Caldari agent mission and verify derived faction standing delta
    let (&caldari_agent, agent_station) = ctx
        .sde
        .agents
        .iter()
        .filter(|(_, a)| {
            a.level == 1
                && !matches!(a.agent_type_id, 4 | 5 | 7 | 8 | 10 | 12 | 13)
                && a.division_id != 37
                && a.station_id.is_some()
                && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001)
        })
        .min_by_key(|(id, _)| *id)
        .map(|(id, a)| (id, a.station_id.unwrap()))
        .expect("Caldari L1 non-career mission agent");
    if agent_station != JITA_STATION {
        dock_station(&ctx, &mut s1, agent_station).await;
    }

    let offer = rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(caldari_agent)])
        .await
        .expect("Request Caldari agent mission");
    let mid = int(&offer, "missionID");
    rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)])
        .await
        .expect("Accept Caldari agent mission");
    if let Some((tid, qty, dest_st, granted_cid)) = ctx.db.with_snapshot(|s| {
        load_mission_meta(s, PILOT_1, mid as u32).map(|m| {
            (
                m.required_delivery_type_id.unwrap_or(0),
                m.required_delivery_qty.max(1),
                m.destination_station_id,
                m.granted_courier_item_id,
            )
        })
    }) {
        if let Some(cid_item) = granted_cid {
            if let Some(ship_id) = s1.ship_id {
                move_item(&ctx, &mut s1, cid_item, ship_id, 5).await;
            }
        } else if tid > 0 {
            s1.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
            let kv = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String(format!("/giveitem {tid} {qty}"))])
                .await
                .unwrap();
            s1.role = 0;
            if let Some(ship_id) = s1.ship_id {
                move_item(&ctx, &mut s1, int(&kv, "itemID"), ship_id, 5).await;
            }
        }
        if let Some(dest) = dest_st.filter(|&d| d > 0 && d != s1.station_id.unwrap_or(0)) {
            dock_station(&ctx, &mut s1, dest).await;
        }
    }
    let (ball_ids, m_sys) = ctx.db.with_snapshot(|s| {
        s.active_missions
            .get(&(PILOT_1, mid as u32))
            .map(|m| (m.spawned_npc_ball_ids.clone(), m.solar_system_id))
            .unwrap_or((Vec::new(), 30_000_142))
    });
    for bid in ball_ids {
        eve_server::services::agents_missions::on_mission_npc_destroyed_core(&ctx.db, m_sys, bid, Some(PILOT_1));
    }

    let tx_before = ctx.db.with_snapshot(|s| s.standing_transactions.get(&PILOT_1).map_or(0, |v| v.len()));
    rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)])
        .await
        .expect("Complete Caldari agent mission");
    let derived_events: Vec<(i64, f64)> = ctx.db.with_snapshot(|s| {
        s.standing_transactions
            .get(&PILOT_1)
            .map(|v| v[tx_before..].iter().map(|e| (e.from_id, e.modification)).collect())
            .unwrap_or_default()
    });
    assert!(
        derived_events.iter().any(|&(f, d)| f == 500_003 && d > 0.0)
            && derived_events.iter().any(|&(f, d)| f == 500_004 && d < 0.0),
        "Caldari mission must propagate positive derived standing to Amarr (500003) and negative to Gallente (500004): {derived_events:?}"
    );
    let _ = (corp_id, dock_station, dock_structure, SOTIYO, SUPERCAP_SHIPYARD);
}
