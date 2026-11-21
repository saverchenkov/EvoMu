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
    acquire, board_loaded_freighter, bom, buy, fuel_cargo, fund_pilots, install, int,
    items_at, login, move_item, nearest_system, owned_qty, real, rpc, split_off, train_skills, upwell_cargo, JITA_STATION,
    PERIMETER_SYS, PILOT_1,
};
use crate::industry_supercapital_tax_esi_e2e::{
    deploy_online_upwell_fast as deploy_online_upwell, dock_station_fast as dock_station,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_server::formulas::try_compute_manufacturing_material_qty;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::load_job_cost_meta;
use eve_server::world_seeder::seed_full_market_catalog;
use std::sync::{Arc, OnceLock};

fn fast_full_market_ctx() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    let (sde, snap) = BASE.get_or_init(|| {
        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        let _ = seed_full_market_catalog(&db, &sde);
        (sde, db.with_snapshot(|s| s.clone()))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = snap.clone());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(sde), db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

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

    let ctx = fast_full_market_ctx();
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

    let ctx = fast_full_market_ctx();
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

#[tokio::test]
async fn test_p0_1176_bp_37030_multirun_overflow_rejected_and_unlocked() {
    let ctx = fast_full_market_ctx();
    fund_pilots(&ctx, &[(PILOT_1, 100_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 9201, "demo", PILOT_1).await;
    train_skills(&ctx, &mut s1, &[3380, 3387, 3388, 3400], true).await;

    const STANDUP_VORTON_BP: u32 = 37_030;
    let bp_info = ctx.sde.blueprints[&STANDUP_VORTON_BP].clone();

    let (bp_item_id, jobs_before, items_before) = ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == PILOT_1 && it.type_id == 34));
        snap.next_entity_id += 1;
        let bp_id = snap.next_entity_id;
        snap.items.insert(
            bp_id,
            InventoryItemRecord {
                item_id: bp_id,
                type_id: STANDUP_VORTON_BP,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for m in &bp_info.materials {
            if m.type_id != 34 {
                let mut rem = m.quantity.saturating_mul(135);
                while rem > 0 {
                    let chunk = rem.min(i32::MAX as u64) as i32;
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.items.insert(
                        mid,
                        InventoryItemRecord {
                            item_id: mid,
                            type_id: m.type_id,
                            owner_id: PILOT_1,
                            location_id: JITA_STATION,
                            flag: InventoryFlag::Hangar,
                            quantity: chunk,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    rem -= chunk as u64;
                }
            }
        }
        (bp_id, snap.industry_jobs.len(), snap.items.len())
    });

    for (runs, expected_codes) in [
        (27_i64, &["InsufficientManufacturingMaterials"][..]),
        (135_i64, &["InvalidIndustryJobParameters", "InsufficientManufacturingMaterials"][..]),
    ] {
        let err = install(&ctx, &mut s1, bp_item_id, JITA_STATION, 1, runs, &[])
            .await
            .expect_err("37030 without Tritanium must be rejected cleanly");
        assert!(
            expected_codes.contains(&err.code.as_str()),
            "Unexpected error code {} for runs={runs}",
            err.code
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.industry_jobs.len(), jobs_before, "No job may be created on rejection");
            assert_eq!(snap.items.len(), items_before, "No items may be created on rejection");
            assert_eq!(
                snap.items[&bp_item_id].flag,
                InventoryFlag::Hangar,
                "Blueprint 37030 must remain unlocked"
            );
        });
    }

    // With 2.5B Tritanium split across valid i32 stacks, 27 runs (<= 30 days) consumes exact 2.16B
    // and CancelJob restores all 2.5B without i32 truncation.
    ctx.db.with_mut_snapshot(|snap| {
        for qty in [1_500_000_000_i32, 1_000_000_000_i32] {
            snap.next_entity_id += 1;
            let mid = snap.next_entity_id;
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 34,
                    owner_id: PILOT_1,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ok_job = install(&ctx, &mut s1, bp_item_id, JITA_STATION, 1, 27, &[])
        .await
        .expect("27 runs of 37030 with sufficient Tritanium stacks must succeed");
    let ok_jid = int(&ok_job, "jobID");
    assert_eq!(owned_qty(&ctx, PILOT_1, JITA_STATION, 34), 340_000_000);
    ctx.db.with_snapshot(|snap| {
        let meta = load_job_cost_meta(snap, ok_jid).expect("job cost meta");
        assert!(meta.consumed_materials.contains(&(34, 2_160_000_000)));
    });
    rpc(&ctx, &mut s1, "industryManager", "CancelJob", &[PyRep::Integer(ok_jid)])
        .await
        .expect("CancelJob refunds full 2.16B");
    assert_eq!(owned_qty(&ctx, PILOT_1, JITA_STATION, 34), 2_500_000_000);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&bp_item_id].flag, InventoryFlag::Hangar);
    });
}

#[tokio::test]
async fn test_p0_1176_sde_blueprints_property_sweep_no_u64_wrap_or_skip() {
    let ctx = eve_server::router::TestContext::from_server(fast_full_market_ctx())
        .with_rpc_rate_budget_disabled();
    fund_pilots(&ctx, &[(PILOT_1, 500_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 9202, "demo", PILOT_1).await;
    train_skills(
        &ctx,
        &mut s1,
        &[3380, 3387, 3388, 3400, 11_445, 11_452, 21_791, 45_746, 45_748, 45_749],
        true,
    )
    .await;

    const RUNS_SWEEP: [u32; 6] = [1, 27, 54, 108, 135, 1000];

    // 1. Exhaustive SDE sweep across all blueprints (activities 1, 8, 9) and run counts.
    let mut overflow_mfg_bps: Vec<u32> = Vec::new();
    let mut sample_reaction_bps: Vec<u32> = Vec::new();
    let mut sample_invention_bps: Vec<u32> = Vec::new();
    for (&bp_id, bp) in &ctx.sde.blueprints {
        for &runs in &RUNS_SWEEP {
            for m in &bp.materials {
                if m.quantity > 0 {
                    let q0 = try_compute_manufacturing_material_qty(runs, m.quantity, 0, 0.0, 0.0)
                        .expect("SDE mfg mat ME0 must not overflow u64");
                    let q10 =
                        try_compute_manufacturing_material_qty(runs, m.quantity, 10, 1.0, 2.0)
                            .expect("SDE mfg mat ME10 must not overflow u64");
                    assert!(q0 >= u64::from(runs) && q0 > 0 && q0 <= i64::MAX as u64);
                    assert!(q10 >= u64::from(runs) && q10 > 0 && q10 <= i64::MAX as u64);
                    if q0 > i32::MAX as u64 && !overflow_mfg_bps.contains(&bp_id) {
                        overflow_mfg_bps.push(bp_id);
                    }
                }
            }
            if let Some(act9) = bp.activities.get("reaction").or_else(|| bp.activities.get("9")) {
                if runs == 1 && !act9.materials.is_empty() {
                    sample_reaction_bps.push(bp_id);
                }
                for m in &act9.materials {
                    if m.quantity > 0 {
                        let qr =
                            try_compute_manufacturing_material_qty(runs, m.quantity, 0, 0.0, 0.0)
                                .expect("SDE reaction mat must not overflow u64");
                        assert!(qr >= u64::from(runs) && qr > 0 && qr <= i64::MAX as u64);
                    }
                }
            }
            if let Some(act8) = bp.activities.get("invention").or_else(|| bp.activities.get("8")) {
                if runs == 1 && !act8.materials.is_empty() {
                    sample_invention_bps.push(bp_id);
                }
                for m in &act8.materials {
                    let qi = m
                        .quantity
                        .max(1)
                        .checked_mul(u64::from(runs))
                        .expect("SDE invention mat must not overflow u64");
                    assert!(qi >= u64::from(runs) && qi > 0 && qi <= i64::MAX as u64);
                }
            }
        }
    }
    assert!(overflow_mfg_bps.len() >= 150, "Expected >=150 SDE overflow blueprints, found {}", overflow_mfg_bps.len());
    overflow_mfg_bps.sort_unstable();
    sample_reaction_bps.sort_unstable();
    sample_reaction_bps.truncate(12);
    sample_invention_bps.sort_unstable();
    sample_invention_bps.truncate(12);

    // 2. Dispatch real industryManager.InstallJob across all overflow manufacturing BPs with 0 hangar stock:
    // every call must reject cleanly before mutating jobs/items/unlocking blueprints.
    let (lowsec_sys, ls_athanor) = {
        let sys = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 30_100_000 && s.security_status > 0.05 && s.security_status < 0.40)
            .min_by_key(|s| s.solar_system_id)
            .unwrap()
            .solar_system_id;
        let corp_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].corporation_id);
        let st_id = ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.owner_id != PILOT_1 || i.location_id != JITA_STATION);
            snap.next_entity_id += 1;
            let ath = snap.next_entity_id;
            snap.structures.insert(
                ath,
                eve_db::UpwellStructureRecord {
                    structure_id: ath,
                    type_id: 35_835,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: "Sweep Athanor".into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    ..Default::default()
                },
            );
            for (slot, tid) in [
                (InventoryFlag::ServiceSlot0, 45_537_u32),
                (InventoryFlag::ServiceSlot1, 45_538_u32),
                (InventoryFlag::ServiceSlot2, 45_539_u32),
            ] {
                snap.next_entity_id += 1;
                let mid = snap.next_entity_id;
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: tid,
                        owner_id: corp_id,
                        location_id: ath,
                        flag: slot,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                snap.module_online_states.insert(mid, true);
            }
            ath
        });
        (sys, st_id)
    };

    for bp_tid in overflow_mfg_bps {
        let (bp_item_id, jobs_before, items_before) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: bp_tid,
                    owner_id: PILOT_1,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            (bid, snap.industry_jobs.len(), snap.items.len())
        });
        for runs in RUNS_SWEEP {
            let err = install(&ctx, &mut s1, bp_item_id, JITA_STATION, 1, i64::from(runs), &[])
                .await
                .expect_err("Zero-stock overflow mfg BP must never install");
            assert!(!err.code.is_empty());
            ctx.db.with_snapshot(|snap| {
                assert_eq!(snap.industry_jobs.len(), jobs_before);
                assert_eq!(snap.items.len(), items_before);
                assert_eq!(snap.items[&bp_item_id].flag, InventoryFlag::Hangar);
            });
        }
    }

    // 3. Invention (8) zero-stock BPC sweep across RUNS_SWEEP
    for bp_tid in sample_invention_bps {
        let (bpc_item_id, jobs_before, items_before) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: bp_tid,
                    owner_id: PILOT_1,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(1_000),
                    version: 1,
                    ..Default::default()
                },
            );
            (bid, snap.industry_jobs.len(), snap.items.len())
        });
        for runs in RUNS_SWEEP {
            let err = install(&ctx, &mut s1, bpc_item_id, JITA_STATION, 8, i64::from(runs), &[])
                .await
                .expect_err("Zero-stock invention must never install");
            assert!(!err.code.is_empty());
            ctx.db.with_snapshot(|snap| {
                assert_eq!(snap.industry_jobs.len(), jobs_before);
                assert_eq!(snap.items.len(), items_before);
            });
        }
    }

    // 4. Reaction (9) zero-stock sweep across RUNS_SWEEP + stocked exact u64 consumption check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
            ch.structure_id = Some(ls_athanor);
        }
    });
    s1.station_id = None;
    s1.structure_id = Some(ls_athanor);
    s1.solar_system_id = Some(lowsec_sys);
    for bp_tid in &sample_reaction_bps {
        let (rbp_id, jobs_before, items_before) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: *bp_tid,
                    owner_id: PILOT_1,
                    location_id: ls_athanor,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            (bid, snap.industry_jobs.len(), snap.items.len())
        });
        for runs in RUNS_SWEEP {
            let err = install(&ctx, &mut s1, rbp_id, ls_athanor, 9, i64::from(runs), &[])
                .await
                .expect_err("Zero-stock reaction must never install");
            assert!(!err.code.is_empty());
            ctx.db.with_snapshot(|snap| {
                assert_eq!(snap.industry_jobs.len(), jobs_before);
                assert_eq!(snap.items.len(), items_before);
            });
        }
    }
    if let Some(&first_react) = sample_reaction_bps.first() {
        let react_bp = &ctx.sde.blueprints[&first_react];
        let act9 = react_bp.activities.get("reaction").or_else(|| react_bp.activities.get("9")).unwrap();
        let (rbp_id, expected_consumed): (i64, Vec<(u32, i64)>) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: first_react,
                    owner_id: PILOT_1,
                    location_id: ls_athanor,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut exp = Vec::new();
            for m in &act9.materials {
                let q = try_compute_manufacturing_material_qty(27, m.quantity, 0, 0.0, 0.0).unwrap() as i64;
                let mut rem = q as u64;
                while rem > 0 {
                    let chunk = rem.min(i32::MAX as u64) as i32;
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.items.insert(
                        mid,
                        InventoryItemRecord {
                            item_id: mid,
                            type_id: m.type_id,
                            owner_id: PILOT_1,
                            location_id: ls_athanor,
                            flag: InventoryFlag::Hangar,
                            quantity: chunk,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    rem -= chunk as u64;
                }
                exp.push((m.type_id, q));
            }
            (bid, exp)
        });
        let r_job = install(&ctx, &mut s1, rbp_id, ls_athanor, 9, 27, &[])
            .await
            .expect("Stocked reaction within 30 days must succeed");
        let r_jid = int(&r_job, "jobID");
        ctx.db.with_snapshot(|snap| {
            let meta = load_job_cost_meta(snap, r_jid).expect("reaction job cost meta");
            for (mtid, mqty) in &expected_consumed {
                assert!(meta.consumed_materials.contains(&(*mtid, *mqty)));
            }
        });
        for (mtid, _) in expected_consumed {
            assert_eq!(owned_qty(&ctx, PILOT_1, ls_athanor, mtid), 0);
        }
    }
}
