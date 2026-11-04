//! Integration tests for industry job cost, facility tax, EIV, capital construction, & ME/TE research:
//! - `feat:industry.eiv-adjusted-prices`
//! - `feat:industry.job-cost`
//! - `feat:industry.facility-tax`
//! - `feat:industry.capital-construction-restrictions`
//! - `feat:industry.blueprint-me-research`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MarketHistoryDayRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{compute_manufacturing_material_qty, compute_manufacturing_time_seconds};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::{
    clear_sde_adjusted_prices, compute_blueprint_eiv_per_run,
    compute_blueprint_eiv_per_run_with_snap, compute_industry_job_costs, load_facility_taxes,
    resolve_structure_rig_bonuses_full, set_sde_adjusted_price,
};

const PILOT: i64 = 90_000_001;
const CORP: i64 = 98_000_001;
const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const RIFTER_BP: u32 = 687;

async fn setup_pilot(ctx: &ServerContext, sess_id: u64, char_id: i64, station_id: i64, sys_id: u32) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);
    sess.structure_id = None;
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn add_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn seed_materials(ctx: &ServerContext, bp_tid: u32, owner: i64, loc: i64, runs: i32, base_id: i64) {
    let mats: Vec<(u32, u64)> = ctx
        .sde
        .blueprints
        .get(&bp_tid)
        .map(|bp| {
            if !bp.materials.is_empty() {
                bp.materials.iter().map(|m| (m.type_id, m.quantity)).collect()
            } else {
                bp.activities
                    .get("manufacturing")
                    .or_else(|| bp.activities.get("1"))
                    .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default();
    for (i, (tid, qty)) in mats.into_iter().enumerate() {
        add_item(ctx, base_id + i as i64, tid, owner, loc, InventoryFlag::Hangar, (qty as i32) * runs, false);
    }
}

#[tokio::test]
async fn test_industry_eiv_adjusted_prices() {
    // parity: feat:industry.eiv-adjusted-prices
    let ctx = ServerContext::new();
    let bp = ctx.sde.blueprints.get(&RIFTER_BP).cloned().expect("Rifter BP");
    clear_sde_adjusted_prices();

    let baseline_eiv = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert!(baseline_eiv > 0.0, "Baseline EIV should be positive");

    // 1. Explicit market adjusted prices via cache override
    set_sde_adjusted_price(34, 150.0); // Tritanium @ 150 ISK
    set_sde_adjusted_price(35, 300.0); // Pyerite @ 300 ISK
    let cached_eiv = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert!(cached_eiv > baseline_eiv, "Cached adjusted prices must elevate EIV");
    clear_sde_adjusted_prices();

    // 2. 28-day daily rolling average from market history (clamped EMA)
    ctx.db.with_mut_snapshot(|snap| {
        let hist: Vec<MarketHistoryDayRecord> = (1..=28)
            .map(|d| MarketHistoryDayRecord {
                date: format!("2026-10-{d:02}"),
                average: 75.0,
                highest: 80.0,
                lowest: 70.0,
                order_count: 50,
                volume: 500_000,
                open_price: Some(75.0),
                close_price: Some(75.0),
            })
            .collect();
        snap.market_history.insert((10_000_002, 34), hist);
    });

    let hist_eiv = ctx.db.with_snapshot(|snap| {
        compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), &bp, 1)
    });
    assert!(hist_eiv > baseline_eiv, "28d market history must compute rolling adjusted price EIV");

    // 3. EIV is strictly invariant to blueprint ME level (computed from ME-0 base BOM)
    let eiv_me0 = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    let mut bpo_item = InventoryItemRecord {
        item_id: 885_000_001,
        type_id: RIFTER_BP,
        owner_id: PILOT,
        location_id: JITA_STATION,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        ..Default::default()
    };
    bpo_item.dynamic_attributes.insert(1112, 10.0); // ME 10
    let eiv_me10 = compute_blueprint_eiv_per_run(&ctx, &bp, 1);
    assert_eq!(eiv_me0, eiv_me10, "EIV per run must be invariant to blueprint ME level");
}

#[tokio::test]
async fn test_industry_job_cost_formula_and_surcharges() {
    // parity: feat:industry.job-cost
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 801, PILOT, JITA_STATION, JITA_SYS).await;
    let bpo_id = 885_000_101;
    add_item(&ctx, bpo_id, RIFTER_BP, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_materials(&ctx, RIFTER_BP, PILOT, JITA_STATION, 4, 885_000_200);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            3387,
            TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.balance = IskCents::from_isk(1_000_000_000);
            ch.clone_grade = 2; // Omega
        }
    });

    // 1. Omega installation: base cost + facility tax + 4% SCC surcharge, zero Alpha tax
    let res = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
    )
    .await
    .expect("Omega InstallJob");

    let eiv_per_run = res.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let total_eiv = eiv_per_run * 2.0;
    let sci = res.dict_get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
    let base_cost = res.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let fac_tax = res.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let scc = res.dict_get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
    let alpha_tax = res.dict_get("alphaCloneTaxISK").and_then(|v| v.as_f64()).unwrap();
    let total = res.dict_get("totalJobCost").and_then(|v| v.as_f64()).unwrap();

    let exp_base = (total_eiv * sci * 1.0 * 100.0).round() / 100.0;
    let exp_scc = (total_eiv * 0.04 * 100.0).round() / 100.0;
    assert!((base_cost - exp_base).abs() < 0.02);
    assert!((scc - exp_scc).abs() < 0.02, "SCC surcharge in empire highsec must be 4%");
    assert_eq!(alpha_tax, 0.0, "Omega pilots incur 0 Alpha clone tax");
    assert!((total - (base_cost + fac_tax + scc)).abs() < 0.02);

    // 2. Alpha clone installation: +0.25% Alpha clone tax added to fee
    let bpo_alpha = 885_000_102;
    add_item(&ctx, bpo_alpha, RIFTER_BP, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.clone_grade = 1; // Alpha
        }
    });

    let res_alpha = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_alpha)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
    )
    .await
    .expect("Alpha InstallJob");

    let alpha_fee = res_alpha.dict_get("alphaCloneTaxISK").and_then(|v| v.as_f64()).unwrap();
    let exp_alpha = (total_eiv * 0.0025 * 100.0).round() / 100.0;
    assert!((alpha_fee - exp_alpha).abs() < 0.02, "Alpha clone tax must be +0.25% of EIV");
    let total_alpha = res_alpha.dict_get("totalJobCost").and_then(|v| v.as_f64()).unwrap();
    let base_a = res_alpha.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let tax_a = res_alpha.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let scc_a = res_alpha.dict_get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((total_alpha - (base_a + tax_a + scc_a + alpha_fee)).abs() < 0.02);
}

#[tokio::test]
async fn test_industry_facility_tax_npc_and_upwell_profiles() {
    // parity: feat:industry.facility-tax
    let ctx = ServerContext::new();
    let npc_station: i64 = 60_000_001; // Non-Jita SDE NPC Station
    let mut sess = setup_pilot(&ctx, 802, PILOT, npc_station, 30_000_001).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            3387,
            TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.balance = IskCents::from_isk(1_000_000_000);
            ch.station_id = Some(npc_station);
            ch.solar_system_id = 30_000_001;
        }
    });

    // 1. Standard NPC station default facility tax rate is 10%
    let npc_taxes = ctx.db.with_snapshot(|snap| load_facility_taxes(snap, npc_station));
    let npc_rate = npc_taxes.get(&1).copied().unwrap_or(0.0);
    assert!((npc_rate - 0.10).abs() < 1e-6, "Default NPC station industry tax must be 10%, got {npc_rate}");

    let bpo_npc = 886_000_001;
    add_item(&ctx, bpo_npc, RIFTER_BP, PILOT, npc_station, InventoryFlag::Hangar, 1, true);
    seed_materials(&ctx, RIFTER_BP, PILOT, npc_station, 1, 886_000_100);

    let cost_meta = ctx.db.with_snapshot(|snap| {
        compute_industry_job_costs(&ctx, snap, RIFTER_BP, 1, 1, npc_station)
    })
    .expect("compute NPC job costs");
    assert!((cost_meta.facility_tax_rate - 0.10).abs() < 1e-6);
    let exp_tax_isk = (cost_meta.eiv_isk * 0.10 * 100.0).round() / 100.0;
    assert!((cost_meta.facility_tax_isk - exp_tax_isk).abs() < 0.02);

    // 2. Upwell structure owner-configured profile tax
    let raitaru_id: i64 = 1_000_886_001;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(CORP).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: CORP,
            name: "TaxCorp".into(),
            ticker: "TXC".into(),
            ceo_id: PILOT,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.corporation_id = CORP;
        }
        snap.structures.insert(
            raitaru_id,
            UpwellStructureRecord {
                structure_id: raitaru_id,
                type_id: 35825,
                solar_system_id: 30_000_001,
                owner_corp_id: CORP,
                name: "Custom Tax Raitaru".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                ..Default::default()
            },
        );
    });
    add_item(&ctx, 886_000_099, 35878, CORP, raitaru_id, InventoryFlag::ServiceSlot0, 1, true);

    // Upwell structure taxes cannot exceed 10%
    let err_tax = rpc(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.12))])],
    )
    .await
    .expect_err("Tax > 10% must fail");
    assert_eq!(err_tax.code, "InvalidFacilityTaxRate");

    // Configure 6.5% owner profile tax
    rpc(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.065))])],
    )
    .await
    .expect("SetFacilityTaxes 6.5%");

    let bpo_st = 886_000_002;
    add_item(&ctx, bpo_st, RIFTER_BP, PILOT, raitaru_id, InventoryFlag::Hangar, 1, true);
    seed_materials(&ctx, RIFTER_BP, PILOT, raitaru_id, 1, 886_000_200);

    let st_costs = ctx.db.with_snapshot(|snap| {
        compute_industry_job_costs(&ctx, snap, RIFTER_BP, 1, 1, raitaru_id)
    })
    .expect("compute Upwell structure job costs");
    assert!((st_costs.facility_tax_rate - 0.065).abs() < 1e-6);
    let exp_st_tax = (st_costs.eiv_isk * 0.065 * 100.0).round() / 100.0;
    assert!((st_costs.facility_tax_isk - exp_st_tax).abs() < 0.02);
}

#[tokio::test]
async fn test_industry_capital_construction_restrictions_and_sovereignty() {
    // parity: feat:industry.capital-construction-restrictions
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 803, PILOT, JITA_STATION, JITA_SYS).await;

    let (dread_bp_tid, titan_bp_tid, jf_bp_tid) = {
        let mut d = 0;
        let mut t = 0;
        let mut j = 0;
        for (&b_tid, b) in &ctx.sde.blueprints {
            if let Some(prod) = ctx.sde.types.get(&b.product_type_id) {
                if prod.group_id == 485 && d == 0 { d = b_tid; }
                if prod.group_id == 30 && t == 0 { t = b_tid; }
                if prod.group_id == 902 && j == 0 { j = b_tid; }
            }
        }
        (d, t, j)
    };
    assert!(dread_bp_tid > 0 && titan_bp_tid > 0 && jf_bp_tid > 0);

    let dread_bpo = 887_000_001;
    let jf_bpo = 887_000_002;
    add_item(&ctx, dread_bpo, dread_bp_tid, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    add_item(&ctx, jf_bpo, jf_bp_tid, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_materials(&ctx, jf_bp_tid, PILOT, JITA_STATION, 1, 887_000_100);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            3387,
            TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.balance = IskCents::from_isk(10_000_000_000);
        }
    });

    // 1. Capital ships cannot be built in High-Sec space
    let err_hs = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(dread_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Capital ship in High-Sec must fail");
    assert_eq!(err_hs.code, "CapitalConstructionForbiddenInHighSec");

    // 2. Jump Freighters (group 902) are allowed in High-Sec
    let jf_ok = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(jf_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Jump Freighter in High-Sec must succeed");
    assert!(jf_ok.dict_get("jobID").is_some());

    // 3. Supercapitals require Sov Nullsec, Sotiyo (35827), upgrade, and Standup Supercapital Shipyard
    let null_sys: u32 = 30_000_208;
    let sotiyo_id: i64 = 1_000_887_001;
    sess.solar_system_id = Some(null_sys);
    sess.station_id = Some(sotiyo_id);
    sess.corporation_id = Some(CORP);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(CORP).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: CORP,
            name: "Cap Corp".into(),
            ticker: "CAP".into(),
            ceo_id: PILOT,
            master_wallet: IskCents::from_isk(10_000_000_000),
            members: std::collections::HashSet::from([PILOT]),
            member_roles: HashMap::from([(PILOT, u64::MAX)]),
            ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.corporation_id = CORP;
            ch.solar_system_id = null_sys;
            ch.station_id = Some(sotiyo_id);
        }
        snap.structures.insert(
            sotiyo_id,
            UpwellStructureRecord {
                structure_id: sotiyo_id,
                type_id: 35827,
                solar_system_id: null_sys,
                owner_corp_id: CORP,
                name: "Sov Sotiyo".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                ..Default::default()
            },
        );
    });
    add_item(&ctx, 887_000_098, 35878, CORP, sotiyo_id, InventoryFlag::ServiceSlot1, 1, true);

    let titan_bpo = 887_000_003;
    add_item(&ctx, titan_bpo, titan_bp_tid, PILOT, sotiyo_id, InventoryFlag::Hangar, 1, true);

    // Missing Sov: fails
    let err_no_sov = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(titan_bpo)),
            ("facilityID", PyRep::Integer(sotiyo_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Supercapital without Sov");
    assert_eq!(err_no_sov.code, "SupercapitalRequiresSovereignNullSec");

    // Add Sov + Supercapital Facilities Upgrade + Standup Supercapital Shipyard
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(99_000_001);
    });
    add_item(&ctx, 887_000_097, 81623, CORP, null_sys as i64, InventoryFlag::None, 1, true);
    add_item(&ctx, 887_000_096, 35877, CORP, sotiyo_id, InventoryFlag::ServiceSlot0, 1, true);
    seed_materials(&ctx, titan_bp_tid, PILOT, sotiyo_id, 1, 887_000_300);

    let super_ok = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(titan_bpo)),
            ("facilityID", PyRep::Integer(sotiyo_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Supercapital in Sotiyo with Sov and Shipyard");
    assert!(super_ok.dict_get("jobID").is_some());

    // 4. Sotiyo rig Nullsec security multiplier (2.1x)
    let (_, _, _, sec_mult) = ctx.db.with_snapshot(|snap| {
        resolve_structure_rig_bonuses_full(&ctx, snap, sotiyo_id, null_sys)
    });
    assert!((sec_mult - 2.1).abs() < 1e-6, "Null-Sec rig security multiplier must be 2.1x");
}

#[tokio::test]
async fn test_industry_blueprint_me_research() {
    // parity: feat:industry.blueprint-me-research
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 804, PILOT, JITA_STATION, JITA_SYS).await;
    let bpo_id = 888_000_001;
    let bpo_unresearched = 888_000_002;
    add_item(&ctx, bpo_id, RIFTER_BP, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    add_item(&ctx, bpo_unresearched, RIFTER_BP, PILOT, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_materials(&ctx, RIFTER_BP, PILOT, JITA_STATION, 10, 888_000_100);

    ctx.db.with_mut_snapshot(|snap| {
        for sid in [3387, 3406, 3380, 3402, 3409] {
            snap.skills.entry(PILOT).or_default().insert(
                sid,
                TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.balance = IskCents::from_isk(5_000_000_000);
        }
    });

    // 1. ME research requires a BPO, cannot research a BPC
    let bpc_id = 888_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: RIFTER_BP,
                owner_id: PILOT,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
                ..Default::default()
            },
        );
    });
    let err_bpc = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpc_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("ME research on BPC must fail");
    assert_eq!(err_bpc.code, "CannotResearchOrCopyBPC");

    // 2. ME levels 1-10 reduce manufacturing material requirement by 1% per level (10% at ME 10)
    let qty_me0 = compute_manufacturing_material_qty(10, 32_000, 0, 0.0, 0.0);
    let qty_me5 = compute_manufacturing_material_qty(10, 32_000, 5, 0.0, 0.0);
    let qty_me10 = compute_manufacturing_material_qty(10, 32_000, 10, 0.0, 0.0);
    assert_eq!(qty_me0, 320_000);
    assert_eq!(qty_me5, 304_000, "ME 5 must reduce materials by 5%");
    assert_eq!(qty_me10, 288_000, "ME 10 must reduce materials by 10%");

    // 3. TE levels 1-20 reduce manufacturing time by 1% per level (20% at TE 20)
    let time_te0 = compute_manufacturing_time_seconds(1, 6_000, 0, 0, 0, 0.0);
    let time_te10 = compute_manufacturing_time_seconds(1, 6_000, 10, 0, 0, 0.0);
    let time_te20 = compute_manufacturing_time_seconds(1, 6_000, 20, 0, 0, 0.0);
    assert_eq!(time_te0, 6_000);
    assert_eq!(time_te10, 5_400, "TE 10 must reduce manufacturing time by 10%");
    assert_eq!(time_te20, 4_800, "TE 20 must reduce manufacturing time by 20%");

    // 4. Installing research past ME 10 fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bpo) = snap.items.get_mut(&bpo_id) {
            bpo.dynamic_attributes.insert(1112, 10.0); // Already ME 10
        }
    });
    let err_max_me = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Research past ME 10 must fail");
    assert_eq!(err_max_me.code, "MaxMaterialEfficiencyReached");
}
