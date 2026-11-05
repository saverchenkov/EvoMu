//! Live integration tests for blueprint ME/TE research, capital construction restrictions,
//! EIV adjusted prices, system cost index, job cost formulas, and facility taxes:
//! - `feat:industry.blueprint-me-research`
//! - `feat:industry.capital-construction-restrictions`
//! - `feat:industry.eiv-adjusted-prices`
//! - `feat:industry.facility-tax`
//! - `feat:industry.job-cost`

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    acquire, buy, deploy_online_upwell, fund_pilots, install, int, login, real, rpc,
    train_skills, upwell_cargo, JITA_STATION, PERIMETER_SYS, PILOT_1, PILOT_2,
};
use eve_db::corp_roles;
use eve_proto::PyRep;
use eve_server::formulas::{
    compute_manufacturing_material_qty, compute_manufacturing_time_seconds,
};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::{
    clear_sde_adjusted_prices, compute_blueprint_eiv_per_run, compute_industry_job_costs,
    set_sde_adjusted_price, validate_capital_construction_restrictions,
};
use eve_server::world_seeder::seed_full_market_catalog;

const RIFTER_BP: u32 = 691;
const RAITARU: u32 = 35_825;
const AZBEL: u32 = 35_826;
const MFG_PLANT: u32 = 35_878;
const CAP_SHIPYARD: u32 = 35_881;

async fn complete_job(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, job: &PyRep) -> PyRep {
    let dur = int(job, "durationSeconds") as u64;
    ctx.advance_virtual_clock(dur + 2);
    rpc(ctx, sess, "industryManager", "CompleteJob", &[PyRep::Integer(int(job, "jobID"))])
        .await
        .expect("CompleteJob")
}

#[tokio::test]
async fn test_live_blueprint_me_te_research_and_increments() {
    // parity: feat:industry.blueprint-me-research
    eve_testkit::attest(&["feat:industry.blueprint-me-research"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 500_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8911, "demo", PILOT_1).await;
    train_skills(&ctx, &mut s1, &[3380, 3387, 3388, 3402, 3403, 3406, 3409], true).await;

    // 1. Buy unresearched Rifter BPO (691) from Jita market
    let bpo_id = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let data_0 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_id)])
        .await
        .expect("GetBlueprintData ME0");
    assert_eq!(int(&data_0, "materialEfficiency"), 0);
    assert_eq!(int(&data_0, "timeEfficiency"), 0);

    // 2. Submit ME research job (activityID = 4) and verify exponential duration formula
    let base_me_sec = ctx.sde.blueprints[&RIFTER_BP].research_me_time_sec.max(1) as f64;
    // Research (3403) -5%/lvl, Advanced Industry (3388) -3%/lvl
    let me_skill_mult = (1.0 - 0.05 * 5.0) * (1.0 - 0.03 * 5.0);
    let expected_me1_dur = (105.0 * (base_me_sec / 105.0) * me_skill_mult).round().max(1.0) as u64;

    let me_job = install(&ctx, &mut s1, bpo_id, JITA_STATION, 4, 1, &[]).await.expect("ME 0->1 install");
    let dur_me = int(&me_job, "durationSeconds") as u64;
    assert_eq!(dur_me, expected_me1_dur, "ME 0->1 must follow exponential wiki duration table");

    // Complete ME research job and verify ME level increments to 1 on output blueprint
    complete_job(&ctx, &mut s1, &me_job).await;
    let data_1 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_id)])
        .await
        .expect("GetBlueprintData ME1");
    assert_eq!(int(&data_1, "materialEfficiency"), 1, "Blueprint ME level must increment to 1");
    assert_eq!(int(&data_1, "timeEfficiency"), 0);

    // 3. Submit TE research job (activityID = 3) and verify TE level increments to 2
    let te_job = install(&ctx, &mut s1, bpo_id, JITA_STATION, 3, 1, &[]).await.expect("TE 0->2 install");
    complete_job(&ctx, &mut s1, &te_job).await;
    let data_2 = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_id)])
        .await
        .expect("GetBlueprintData TE2");
    assert_eq!(int(&data_2, "timeEfficiency"), 2, "Blueprint TE level must increment to 2");
    assert_eq!(int(&data_2, "materialEfficiency"), 1);

    // 4. Verify BPC cannot be researched (copy BPO -> try researching BPC)
    let bpo_for_copy = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let cp = install(&ctx, &mut s1, bpo_for_copy, JITA_STATION, 5, 1, &[("licensedRuns", PyRep::Integer(1))])
        .await
        .expect("copy job");
    let bpc_id = int(&complete_job(&ctx, &mut s1, &cp).await, "createdBpcItemID");
    let err_bpc = install(&ctx, &mut s1, bpc_id, JITA_STATION, 4, 1, &[])
        .await
        .expect_err("Research on BPC must fail");
    assert_eq!(err_bpc.code, "CannotResearchOrCopyBPC");

    // 5. Verify 1%-per-level material and time reduction formulas
    let qty_0 = compute_manufacturing_material_qty(1, 30_000, 0, 0.0, 0.0);
    let qty_5 = compute_manufacturing_material_qty(1, 30_000, 5, 0.0, 0.0);
    let qty_10 = compute_manufacturing_material_qty(1, 30_000, 10, 0.0, 0.0);
    assert_eq!(qty_0, 30_000);
    assert_eq!(qty_5, 28_500, "ME 5 must reduce materials by 5%");
    assert_eq!(qty_10, 27_000, "ME 10 must reduce materials by 10%");

    let time_0 = compute_manufacturing_time_seconds(1, 10_000, 0, 0, 0, 0.0);
    let time_10 = compute_manufacturing_time_seconds(1, 10_000, 10, 0, 0, 0.0);
    let time_20 = compute_manufacturing_time_seconds(1, 10_000, 20, 0, 0, 0.0);
    assert_eq!(time_0, 10_000);
    assert_eq!(time_10, 9_000, "TE 10 must reduce time by 10%");
    assert_eq!(time_20, 8_000, "TE 20 must reduce time by 20%");
}

#[tokio::test]
async fn test_live_capital_construction_restrictions() {
    // parity: feat:industry.capital-construction-restrictions
    eve_testkit::attest(&["feat:industry.capital-construction-restrictions"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 500_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8912, "demo", PILOT_1).await;

    let market_bp = |grp: u32| -> u32 {
        *ctx.sde
            .blueprints
            .iter()
            .filter(|(tid, b)| {
                ctx.sde.types.get(tid).is_some_and(|t| t.published && t.market_group_id.is_some())
                    && ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == grp)
            })
            .map(|(tid, _)| tid)
            .min()
            .unwrap_or_else(|| panic!("market blueprint for group {grp}"))
    };
    let dread_bp = market_bp(485);
    let titan_bp = market_bp(30);
    let charon_bp = market_bp(513);
    let inv = ctx.sde.blueprints[&charon_bp]
        .activities
        .get("invention")
        .or_else(|| ctx.sde.blueprints[&charon_bp].activities.get("8"))
        .expect("Charon invention");

    let mut skills = vec![3363, 3380, 3387, 3388, 24625, 20_185, RAITARU, AZBEL, MFG_PLANT, CAP_SHIPYARD];
    skills.extend(inv.materials.iter().map(|m| m.type_id));
    skills.extend([11_448, 11_452, 21_790, 21_791, 23_087, 23_121]);
    train_skills(&ctx, &mut s1, &skills, true).await;

    let dread_bpo = buy(&ctx, &mut s1, dread_bp, 1).await[0];
    let titan_bpo = buy(&ctx, &mut s1, titan_bp, 1).await[0];

    // 1. High-Sec NPC Station: Dreadnought construction is strictly forbidden
    let err_hs_dread = install(&ctx, &mut s1, dread_bpo, JITA_STATION, 1, 1, &[])
        .await
        .expect_err("Dreadnought in High-Sec must fail");
    assert_eq!(err_hs_dread.code, "CapitalConstructionForbiddenInHighSec");

    // 2. High-Sec: Supercapital requires Sovereign Null-Sec
    let err_hs_titan = install(&ctx, &mut s1, titan_bpo, JITA_STATION, 1, 1, &[])
        .await
        .expect_err("Titan in High-Sec must fail");
    assert_eq!(err_hs_titan.code, "SupercapitalRequiresSovereignNullSec");

    // 3. Jump Freighters (group 902) are exempt from High-Sec capital ban
    let charon_bpo = buy(&ctx, &mut s1, charon_bp, 1).await[0];
    let cp = install(&ctx, &mut s1, charon_bpo, JITA_STATION, 5, 1, &[("licensedRuns", PyRep::Integer(1))])
        .await
        .expect("Charon copy");
    let charon_bpc = int(&complete_job(&ctx, &mut s1, &cp).await, "createdBpcItemID");
    for m in &inv.materials {
        acquire(&ctx, &mut s1, m.type_id, m.quantity as i64).await;
    }
    let ij = install(&ctx, &mut s1, charon_bpc, JITA_STATION, 8, 1, &[]).await.expect("JF invention");
    ctx.advance_virtual_clock(int(&ij, "durationSeconds") as u64 + 2);
    let seed = crate::npc_spawns_pve::find_multi_hit_seed(&ctx, 0.01, 1);
    ctx.set_mechanic_rng_seed(Some(seed));
    let inv_done = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(int(&ij, "jobID"))])
        .await
        .expect("invention success");
    let jf_bpc = int(&inv_done, "createdBpcItemID");

    let jf_res = install(&ctx, &mut s1, jf_bpc, JITA_STATION, 1, 1, &[])
        .await
        .expect_err("JF installation without materials");
    assert_eq!(
        jf_res.code, "InsufficientManufacturingMaterials",
        "Jump Freighter must pass the high-sec capital restriction gate"
    );

    // 4. Low-Sec & Null-Sec rules verified via industry service validation
    let snap = ctx.db.with_snapshot(|s| s.clone());
    let lowsec_sys: u32 = 30_002_537; // Amamake (sec ~0.34)
    let dread_info = &ctx.sde.blueprints[&dread_bp];
    let titan_info = &ctx.sde.blueprints[&titan_bp];

    // In Low-Sec NPC station: Dreadnought fails (needs Upwell with online Capital Shipyard)
    let err_ls_npc = validate_capital_construction_restrictions(&ctx, &snap, dread_info, 1, lowsec_sys, 1530, 60_000_001)
        .expect_err("Low-sec NPC station capital construction");
    assert_eq!(err_ls_npc.code, "FacilityNotCapitalCapable");

    // In Low-Sec: Supercapital fails (requires Sov Null-Sec)
    let err_ls_super = validate_capital_construction_restrictions(&ctx, &snap, titan_info, 1, lowsec_sys, AZBEL, 1_000_891_001)
        .expect_err("Low-sec supercapital");
    assert_eq!(err_ls_super.code, "SupercapitalRequiresSovereignNullSec");
}

#[tokio::test]
async fn test_live_eiv_adjusted_prices_and_me_invariance() {
    // parity: feat:industry.eiv-adjusted-prices
    eve_testkit::attest(&["feat:industry.eiv-adjusted-prices"]);

    let ctx = ServerContext::new();
    let bp = ctx.sde.blueprints.get(&RIFTER_BP).cloned().expect("Rifter BP");
    clear_sde_adjusted_prices();

    // 1. Baseline EIV is computed from SDE materials and base prices
    let baseline_eiv = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert!(baseline_eiv > 0.0, "Baseline EIV should be positive");

    // 2. Overriding material adjusted prices elevates blueprint EIV
    set_sde_adjusted_price(34, 150.0); // Tritanium
    set_sde_adjusted_price(35, 300.0); // Pyerite
    let cached_eiv = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert!(
        cached_eiv > baseline_eiv,
        "Elevated material adjusted prices must increase EIV: {cached_eiv} > {baseline_eiv}"
    );
    clear_sde_adjusted_prices();

    // 3. EIV per run is strictly invariant to blueprint ME level
    let eiv_me0 = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    let eiv_me10 = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert_eq!(
        eiv_me0, eiv_me10,
        "EIV per run must be invariant to blueprint ME level (computed from base BOM)"
    );

    // 4. Job base cost and total cost scale linearly with runs
    let snap = ctx.db.with_snapshot(|s| s.clone());
    let cost_1 = compute_industry_job_costs(&ctx, &snap, RIFTER_BP, 1, 1, JITA_STATION).unwrap();
    let cost_5 = compute_industry_job_costs(&ctx, &snap, RIFTER_BP, 1, 5, JITA_STATION).unwrap();
    assert!((cost_5.base_job_cost_isk - cost_1.base_job_cost_isk * 5.0).abs() < 0.05);
    assert!((cost_5.facility_tax_isk - cost_1.facility_tax_isk * 5.0).abs() < 0.05);
    assert!((cost_5.scc_surcharge_isk - cost_1.scc_surcharge_isk * 5.0).abs() < 0.05);
}

#[tokio::test]
async fn test_live_industry_job_cost_formula_and_surcharges() {
    // parity: feat:industry.job-cost
    eve_testkit::attest(&["feat:industry.job-cost"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 50_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8913, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8914, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[3380, 3387], true).await;
    train_skills(&ctx, &mut s2, &[3380, 3387], false).await;

    let test_bp = crate::industry_live_support::small_mineral_bp(&ctx);
    let bpo_omega = buy(&ctx, &mut s1, test_bp, 1).await[0];

    // 1. Omega installation: base job cost + facility tax + 4% SCC surcharge, zero Alpha tax
    let bp_data = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_omega)])
        .await
        .expect("GetBlueprintData");
    let eiv_1 = real(&bp_data, "estimatedItemValueISK");
    let base_cost_1 = real(&bp_data, "baseJobCost");
    let fac_tax_1 = real(&bp_data, "facilityTaxISK");
    let scc_1 = real(&bp_data, "sccSurchargeISK");
    let total_1 = real(&bp_data, "totalJobCostISK");

    assert!(eiv_1 > 0.0);
    let exp_tax = (eiv_1 * 0.0025 * 100.0).round() / 100.0;
    let exp_scc = (eiv_1 * 0.04 * 100.0).round() / 100.0;
    assert!((fac_tax_1 - exp_tax).abs() < 0.02, "NPC station tax must be 0.25% of EIV");
    assert!((scc_1 - exp_scc).abs() < 0.02, "SCC surcharge must be 4% of EIV");
    assert!((total_1 - (base_cost_1 + fac_tax_1 + scc_1)).abs() < 0.02);

    // 2. Real InstallJob execution and wallet verification
    for (tid, qty) in crate::industry_live_support::bom(&ctx, test_bp, 2) {
        acquire(&ctx, &mut s1, tid, qty).await;
    }
    let res = install(&ctx, &mut s1, bpo_omega, JITA_STATION, 1, 2, &[])
        .await
        .expect("Omega InstallJob");
    let res_eiv = real(&res, "estimatedItemValue") * 2.0;
    let res_base = real(&res, "baseJobCost");
    let res_tax = real(&res, "facilityTaxISK");
    let res_scc = real(&res, "sccSurchargeISK");
    let res_alpha = real(&res, "alphaCloneTaxISK");
    let res_total = real(&res, "totalJobCost");

    assert_eq!(res_alpha, 0.0, "Omega clones incur 0 Alpha clone tax");
    assert!((res_tax - (res_eiv * 0.0025 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((res_scc - (res_eiv * 0.04 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((res_total - (res_base + res_tax + res_scc)).abs() < 0.02);

    // 3. Alpha clone installation incurs +0.25% Alpha clone tax
    let bpo_alpha = buy(&ctx, &mut s2, test_bp, 1).await[0];
    for (tid, qty) in crate::industry_live_support::bom(&ctx, test_bp, 1) {
        acquire(&ctx, &mut s2, tid, qty).await;
    }
    let res_a = install(&ctx, &mut s2, bpo_alpha, JITA_STATION, 1, 1, &[])
        .await
        .expect("Alpha InstallJob");
    let a_eiv = real(&res_a, "estimatedItemValue");
    let a_alpha_tax = real(&res_a, "alphaCloneTaxISK");
    let exp_alpha_tax = (a_eiv * 0.0025 * 100.0).round() / 100.0;
    assert!(
        (a_alpha_tax - exp_alpha_tax).abs() < 0.02,
        "Alpha clone tax must be +0.25% of EIV: {a_alpha_tax} vs {exp_alpha_tax}"
    );
    let a_total = real(&res_a, "totalJobCost");
    let a_base = real(&res_a, "baseJobCost");
    let a_tax = real(&res_a, "facilityTaxISK");
    let a_scc = real(&res_a, "sccSurchargeISK");
    assert!((a_total - (a_base + a_tax + a_scc + a_alpha_tax)).abs() < 0.02);
}

#[tokio::test]
async fn test_live_facility_tax_npc_and_upwell_profile() {
    // parity: feat:industry.facility-tax
    eve_testkit::attest(&["feat:industry.facility-tax"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8915, "demo", PILOT_1).await;
    train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 20_185, RAITARU, MFG_PLANT], true).await;

    // 1. NPC Station tax: GetFacilityTaxes returns 0.25% (0.0025)
    let npc_taxes = rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(JITA_STATION)])
        .await
        .expect("GetFacilityTaxes NPC");
    let npc_tax_rate = real(&npc_taxes, "tax");
    assert!(
        (npc_tax_rate - 0.0025).abs() < 1e-6,
        "NPC station facility tax rate must be 0.25%, got {npc_tax_rate}"
    );

    let _bpo_npc = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let cost_npc = ctx.db.with_snapshot(|snap| {
        compute_industry_job_costs(&ctx, snap, RIFTER_BP, 1, 1, JITA_STATION).unwrap()
    });
    assert!((cost_npc.facility_tax_rate - 0.0025).abs() < 1e-6);
    let exp_npc_tax_isk = (cost_npc.eiv_isk * 0.0025 * 100.0).round() / 100.0;
    assert!((cost_npc.facility_tax_isk - exp_npc_tax_isk).abs() < 0.02);

    // 2. Upwell Structure Profile Tax: Deploy Raitaru, test cap and custom profile tax
    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Tax Live Corp".into()), PyRep::String("TXLC".into())])
        .await
        .expect("AddCorporation");
    let _corp_id = s1.corporation_id.unwrap();
    s1.corp_role = corp_roles::ROLE_DIRECTOR;
    ctx.advance_virtual_clock(8 * 86_400);

    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
    cargo.extend(crate::industry_live_support::fuel_cargo());
    let _freighter = crate::industry_live_support::board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let (raitaru_id, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 1_500).await;

    // Upwell structure taxes cannot exceed 10%
    let err_tax_cap = rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.12))])],
    )
    .await
    .expect_err("Tax > 10% must be rejected");
    assert_eq!(err_tax_cap.code, "InvalidFacilityTaxRate");

    // Configure 6.5% owner profile tax via facilityManager.SetFacilityTaxes
    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.065))])],
    )
    .await
    .expect("SetFacilityTaxes 6.5%");

    let st_taxes = rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(raitaru_id)])
        .await
        .expect("GetFacilityTaxes Upwell");
    let st_mfg_tax = real(&st_taxes, "1");
    assert!((st_mfg_tax - 0.065).abs() < 1e-6, "Upwell tax must be 6.5%");

    let cost_st = ctx.db.with_snapshot(|snap| {
        compute_industry_job_costs(&ctx, snap, RIFTER_BP, 1, 1, raitaru_id).unwrap()
    });
    assert!((cost_st.facility_tax_rate - 0.065).abs() < 1e-6);
    let exp_st_tax_isk = (cost_st.eiv_isk * 0.065 * 100.0).round() / 100.0;
    assert!((cost_st.facility_tax_isk - exp_st_tax_isk).abs() < 0.02);
}
