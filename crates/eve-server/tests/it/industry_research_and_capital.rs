//! Live end-to-end integration tests for blueprint ME research exponential curves,
//! lab bonuses, material reduction, and capital/Jump Freighter/supercapital construction gates:
//! - `plan:E-4`
//!
//! Industrialists are regular player sessions: blueprints come from the Jita market (the Jump
//! Freighter BPC is invented from a market Charon BPO), and every Upwell facility is hauled by
//! freighter through stargates, anchored, fueled and fitted live.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    acquire, board_loaded_freighter, bom, buy, deploy_online_upwell, dock_station, fuel_cargo, fund_pilots, install, int,
    items_at, login, move_item, nearest_system, owned_qty, real, rpc, split_off, train_skills, upwell_cargo, JITA_STATION,
    PERIMETER_SYS, PILOT_1,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;

const RIFTER_BP: u32 = 691;
const RAITARU: u32 = 35_825;
const AZBEL: u32 = 35_826;
const MFG_PLANT: u32 = 35_878;
const RESEARCH_LAB: u32 = 35_891;
const CAP_SHIPYARD: u32 = 35_881;

async fn complete(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, job: &PyRep) -> PyRep {
    ctx.advance_virtual_clock(int(job, "durationSeconds") as u64 + 2);
    rpc(ctx, sess, "industryManager", "CompleteJob", &[PyRep::Integer(int(job, "jobID"))]).await.expect("CompleteJob")
}

#[tokio::test]
async fn test_live_exponential_me_research_durations_costs_and_material_reduction() {
    // parity: plan:E-4
    eve_testkit::attest(&["plan:E-4"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 100_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8801, "demo", PILOT_1).await;
    train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 3402, 3403, 3406, 3409, 3388, 20_185, RAITARU, MFG_PLANT, RESEARCH_LAB], true).await;
    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("ME Lab Corp".into()), PyRep::String("MLAB".into())])
        .await
        .expect("AddCorporation");
    ctx.advance_virtual_clock(8 * 86_400);

    let base_me_sec = ctx.sde.blueprints[&RIFTER_BP].research_me_time_sec.max(1) as f64;
    // Research (3403) -5%/lvl and Advanced Industry (3388) -3%/lvl research time.
    let me_skill_mult = (1.0 - 0.05 * 5.0) * (1.0 - 0.03 * 5.0);

    let bpo_for_copy = acquire(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let cp = install(&ctx, &mut s1, bpo_for_copy, JITA_STATION, 5, 1, &[]).await.expect("copy job");
    let bpc_id = int(&complete(&ctx, &mut s1, &cp).await, "createdBpcItemID");
    let err_bpc = install(&ctx, &mut s1, bpc_id, JITA_STATION, 4, 1, &[]).await.expect_err("BPC ME research must fail");
    assert_eq!(err_bpc.code, "CannotResearchOrCopyBPC");

    // Wiki research level table: 105, 250, 595, 1414, 3360, 8000, 19000, 45255, 107700, 256000 (x base/105).
    const LVL_SECS: [u64; 10] = [105, 250, 595, 1_414, 3_360, 8_000, 19_000, 45_255, 107_700, 256_000];
    let bpo_10 = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let mut durations = Vec::new();
    let mut scc_fees = Vec::new();
    for (step, lvl_secs) in LVL_SECS.iter().enumerate() {
        let rj = install(&ctx, &mut s1, bpo_10, JITA_STATION, 4, 1, &[])
            .await
            .unwrap_or_else(|e| panic!("ME step {step}->{} failed: {e:?}", step + 1));
        let dur = int(&rj, "durationSeconds") as u64;
        let expected_dur = ((*lvl_secs as f64) * (base_me_sec / 105.0) * me_skill_mult).round().max(1.0) as u64;
        assert_eq!(dur, expected_dur, "Step {step}->{} must follow exponential LVL_SECS table", step + 1);
        durations.push(dur);
        scc_fees.push(real(&rj, "sccSurchargeISK"));
        complete(&ctx, &mut s1, &rj).await;
    }
    assert!(durations[9] > durations[0] * 1_500);
    assert!(scc_fees[9] > scc_fees[0] * 1_500.0, "research EIV fee scales with the level time multiplier");
    let err_11 = install(&ctx, &mut s1, bpo_10, JITA_STATION, 4, 1, &[]).await.expect_err("ME > 10 must fail");
    assert_eq!(err_11.code, "MaxMaterialEfficiencyReached");

    // Raitaru with Research Lab: multi-level ME 0->5 is faster than the NPC 0->5 sum; research SCI rises.
    let mut cargo = upwell_cargo(&ctx, RAITARU, &[RESEARCH_LAB, MFG_PLANT]);
    cargo.extend(fuel_cargo());
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    for _ in 0..2 {
        let bp = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
        move_item(&ctx, &mut s1, bp, freighter, 5).await;
    }
    let (raitaru_id, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[RESEARCH_LAB, MFG_PLANT], 2_500).await;
    let bpo_5 = split_off(&ctx, &mut s1, items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0, raitaru_id, 4, 1).await;
    let r5 = install(&ctx, &mut s1, bpo_5, raitaru_id, 4, 5, &[]).await.expect("ME 0->5 on Raitaru lab");
    assert!((int(&r5, "durationSeconds") as u64) < durations[0..5].iter().sum::<u64>());
    let sci_before = real(&r5, "systemCostIndex");
    complete(&ctx, &mut s1, &r5).await;
    let bpo_sci = split_off(&ctx, &mut s1, items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0, raitaru_id, 4, 1).await;
    let r_next = install(&ctx, &mut s1, bpo_sci, raitaru_id, 4, 1, &[]).await.expect("next research job");
    let sci_after = real(&r_next, "systemCostIndex");
    assert!(sci_after > sci_before, "Research SCI must rise after completed research work ({sci_after} > {sci_before})");

    // Haul the ME5 BPO home, then compare one-run Tritanium consumption for ME 0 / 5 / 10.
    move_item(&ctx, &mut s1, bpo_5, freighter, 5).await;
    dock_station(&ctx, &mut s1, JITA_STATION).await;
    move_item(&ctx, &mut s1, bpo_5, JITA_STATION, 4).await;
    let bpo_0 = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let trit_per_run = ctx.sde.blueprints[&RIFTER_BP].materials.iter().find(|m| m.type_id == 34).unwrap().quantity as i64;
    let mut consumed = Vec::new();
    for bpo in [bpo_0, bpo_5, bpo_10] {
        for (tid, qty) in bom(&ctx, RIFTER_BP, 1) {
            acquire(&ctx, &mut s1, tid, qty).await;
        }
        let trit_before = owned_qty(&ctx, PILOT_1, JITA_STATION, 34);
        install(&ctx, &mut s1, bpo, JITA_STATION, 1, 1, &[]).await.expect("1-run mfg job");
        consumed.push(trit_before - owned_qty(&ctx, PILOT_1, JITA_STATION, 34));
    }
    assert_eq!(consumed[0], trit_per_run);
    assert_eq!(consumed[1], (trit_per_run as f64 * 0.95).ceil() as i64);
    assert_eq!(consumed[2], (trit_per_run as f64 * 0.90).ceil() as i64);
}

#[tokio::test]
async fn test_live_capital_and_jump_freighter_construction_matrix() {
    // parity: plan:E-4
    eve_testkit::attest(&["plan:E-4"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 400_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8802, "demo", PILOT_1).await;

    let market_bp_for_group = |grp: u32| -> u32 {
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
    let (dread_bp, titan_bp, charon_bp) = (market_bp_for_group(485), market_bp_for_group(30), market_bp_for_group(513));
    let charon = &ctx.sde.blueprints[&charon_bp];
    let inv = charon.activities.get("invention").or_else(|| charon.activities.get("8")).expect("Charon invention");
    let jf_bp = inv.products.first().expect("invention product").type_id;
    assert_eq!(ctx.sde.blueprints.get(&jf_bp).and_then(|b| ctx.sde.types.get(&b.product_type_id)).map(|t| t.group_id), Some(902));

    let mut skills: Vec<u32> = vec![3363, 3380, 3387, 3388, 24625, 20_185, RAITARU, AZBEL, MFG_PLANT, CAP_SHIPYARD];
    for bp in [dread_bp, titan_bp, charon_bp, jf_bp] {
        for act in ["manufacturing", "copying", "invention"] {
            if let Some(a) = ctx.sde.blueprints[&bp].activities.get(act) {
                skills.extend(a.skills.iter().map(|s| s.type_id));
            }
        }
    }
    // Datacores pull in their science skills; racial Encryption Methods cover the invention roll.
    skills.extend(inv.materials.iter().map(|m| m.type_id));
    skills.extend([11_448, 11_452, 21_790, 21_791, 23_087, 23_121]);
    train_skills(&ctx, &mut s1, &skills, true).await;
    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Shipyard Corp".into()), PyRep::String("SYRD".into())])
        .await
        .expect("AddCorporation");
    ctx.advance_virtual_clock(8 * 86_400);

    // Invent a Jump Freighter BPC: copy a market Charon BPO, then a seeded-success invention roll.
    let charon_bpo = buy(&ctx, &mut s1, charon_bp, 1).await[0];
    let cp = install(&ctx, &mut s1, charon_bpo, JITA_STATION, 5, 1, &[("licensedRuns", PyRep::Integer(1))]).await.expect("Charon copy");
    let charon_bpc = int(&complete(&ctx, &mut s1, &cp).await, "createdBpcItemID");
    for m in &inv.materials {
        acquire(&ctx, &mut s1, m.type_id, m.quantity as i64).await;
    }
    let ij = install(&ctx, &mut s1, charon_bpc, JITA_STATION, 8, 1, &[]).await.expect("JF invention");
    ctx.advance_virtual_clock(int(&ij, "durationSeconds") as u64 + 2);
    let seed = crate::npc_spawns_pve::find_multi_hit_seed(&ctx, 0.01, 1);
    ctx.set_mechanic_rng_seed(Some(seed));
    let inv_done = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(int(&ij, "jobID"))]).await.expect("invention success");
    let jf_bpc = int(&inv_done, "createdBpcItemID");

    let low_sys = nearest_system(&ctx, PERIMETER_SYS, |sid| {
        ctx.sde.solar_systems.get(&sid).is_some_and(|s| s.security_status > 0.05 && s.security_status < 0.45 && s.region_id != 10_000_070)
    });
    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
    cargo.extend(upwell_cargo(&ctx, AZBEL, &[MFG_PLANT, CAP_SHIPYARD]));
    cargo.extend(fuel_cargo());
    cargo.extend([(dread_bp, 1), (titan_bp, 1)]);
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    move_item(&ctx, &mut s1, jf_bpc, freighter, 5).await;
    let (dread_bpo, titan_bpo) = (items_at(&ctx, PILOT_1, freighter, dread_bp)[0].0, items_at(&ctx, PILOT_1, freighter, titan_bp)[0].0);

    // 1. High-sec Raitaru: Jump Freighters are exempt from capital restrictions; Dreadnoughts/Titans are not.
    let (hs_raitaru, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 900).await;
    for bp in [jf_bpc, dread_bpo, titan_bpo] {
        move_item(&ctx, &mut s1, bp, hs_raitaru, 4).await;
    }
    let jf_err = install(&ctx, &mut s1, jf_bpc, hs_raitaru, 1, 1, &[]).await.expect_err("JF without hull/components");
    assert_eq!(jf_err.code, "InsufficientManufacturingMaterials", "JF passes the capital gate in High-Sec and only lacks materials");
    let err_hs_cap = install(&ctx, &mut s1, dread_bpo, hs_raitaru, 1, 1, &[]).await.expect_err("Dreadnought in High-Sec Upwell must fail");
    assert_eq!(err_hs_cap.code, "CapitalConstructionForbiddenInHighSec");
    let err_hs_super = install(&ctx, &mut s1, titan_bpo, hs_raitaru, 1, 1, &[]).await.expect_err("Titan in High-Sec must fail");
    assert_eq!(err_hs_super.code, "SupercapitalRequiresSovereignNullSec");
    for bp in [dread_bpo, titan_bpo] {
        move_item(&ctx, &mut s1, bp, freighter, 5).await;
    }

    // 2. Low-sec Azbel: capitals need an online Standup Capital Shipyard I (35881).
    let (ls_azbel, _) = deploy_online_upwell(&ctx, &mut s1, low_sys, AZBEL, &[MFG_PLANT], 3_000).await;
    for bp in [dread_bpo, titan_bpo] {
        move_item(&ctx, &mut s1, bp, ls_azbel, 4).await;
    }
    let err_no_cap_mod = install(&ctx, &mut s1, dread_bpo, ls_azbel, 1, 1, &[]).await.expect_err("Dreadnought without 35881 must fail");
    assert_eq!(err_no_cap_mod.code, "FacilityNotCapitalCapable");
    let cap_item = items_at(&ctx, PILOT_1, freighter, CAP_SHIPYARD)[0].0;
    let fit = rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(cap_item), PyRep::Integer(ls_azbel), PyRep::Integer(165)])
        .await
        .expect("Fit Standup Capital Shipyard I");
    assert_eq!(fit.dict_get("online").and_then(|v| v.as_bool()), Some(true));
    let cap_mod_id = int(&fit, "itemID");
    let gate_ok = install(&ctx, &mut s1, dread_bpo, ls_azbel, 1, 1, &[]).await.expect_err("no capital components hauled");
    assert_eq!(gate_ok.code, "InsufficientManufacturingMaterials", "online 35881 clears the capital gate in Low-Sec Azbel");
    rpc(&ctx, &mut s1, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(ls_azbel), PyRep::Integer(cap_mod_id)])
        .await
        .expect("Offline 35881");
    let err_off_cap = install(&ctx, &mut s1, dread_bpo, ls_azbel, 1, 1, &[]).await.expect_err("Offline 35881 must block capital job");
    assert_eq!(err_off_cap.code, "FacilityNotCapitalCapable");
    let err_ls_super = install(&ctx, &mut s1, titan_bpo, ls_azbel, 1, 1, &[]).await.expect_err("Titan in Low-Sec must fail");
    assert_eq!(err_ls_super.code, "SupercapitalRequiresSovereignNullSec");
}
