//! Industry, Research, and Reactions (`E-4`) complete parity integration tests.

use crate::common;
use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketHistoryDayRecord, TrainedSkillRecord, TypeId, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::IndustryJobCostMeta;
use std::collections::{HashMap, HashSet};

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const MASS_PRODUCTION_SKILL: TypeId = 3387;
const ADV_MASS_PRODUCTION_SKILL: TypeId = 24625;
const LABORATORY_OPERATION_SKILL: TypeId = 3406;
const ADV_LABORATORY_OPERATION_SKILL: TypeId = 24624;
const MASS_REACTIONS_SKILL: TypeId = 45748;
const ADV_MASS_REACTIONS_SKILL: TypeId = 45749;
const SUPPLY_CHAIN_MGMT_SKILL: TypeId = 24268;
const SCIENTIFIC_NETWORKING_SKILL: TypeId = 24270;
const REMOTE_REACTIONS_SKILL: TypeId = 45750;
const REACTIONS_SKILL: TypeId = 45746;

async fn setup_pilot(ctx: &ServerContext, sid: u64, u: &str, p: &str, cid: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, u, p).await;
    let _ = rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await;
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

fn make_structure(sid: i64, stype: TypeId, corp_id: i64, ssys: u32, name: String) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid, type_id: stype, owner_corp_id: corp_id, solar_system_id: ssys, name,
        state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 1_000, ..Default::default()
    }
}

fn find_low_or_null_system(ctx: &ServerContext) -> u32 {
    ctx.sde.solar_systems.values().find(|s| s.security_status < 0.40).map(|s| s.solar_system_id).unwrap_or(30_002_537)
}

fn seed_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool, is_bpc: bool, runs: Option<i32>) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(item_id, InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton,
        is_blueprint_copy: is_bpc, blueprint_runs_remaining: runs, ..Default::default()
    });
    item_id
}

fn seed_bp_materials(ctx: &ServerContext, bp_type_id: TypeId, owner_id: i64, location_id: i64, runs: u32) {
    if let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) {
        ctx.db.with_mut_snapshot(|snap| {
            for m in &bp.materials {
                seed_item(snap, m.type_id, owner_id, location_id, InventoryFlag::Hangar, (m.quantity as i32) * (runs as i32) * 5, false, false, None);
            }
        });
    }
}

#[tokio::test]
async fn test_industry_blueprint_data_owner_limits_facilities_taxes_and_sci() {
    // parity: feat:industry.blueprint-data, feat:industry.blueprint-limits, feat:industry.facilities, feat:industry.facility-tax, feat:industry.job-cost, feat:industry.eiv-adjusted-prices, feat:industry.cost-index, feat:industry.structure-rigs, feat:industry.structure-standup-rig-security-multipliers, feat:industry.job-slots, feat:industry.corp-industry, rpc:blueprintManager.GetLimits, rpc:facilityManager.GetFacility, rpc:facilityManager.GetFacilityTaxes, rpc:map.GetIndustryCostModifier
    eve_testkit::attest(&[
        "feat:industry.blueprint-data", "feat:industry.blueprint-limits", "feat:industry.facilities",
        "feat:industry.facility-tax", "feat:industry.job-cost", "feat:industry.eiv-adjusted-prices",
        "feat:industry.cost-index", "feat:industry.structure-rigs", "feat:industry.structure-standup-rig-security-multipliers",
        "feat:industry.job-slots", "feat:industry.corp-industry",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let hs_raitaru_id = 1_000_000_100_001;
    let ls_azbel_id = 1_000_000_100_002;
    let ls_sys_id = find_low_or_null_system(&ctx);
    let mut sess = setup_pilot(&ctx, 601, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(JITA_STATION_ID);
    sess.corporation_id = Some(corp_id);

    let (bp_item_id, bpc_item_id) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Test Industry Corp".into(), ticker: "TIND".into(),
            ceo_id: char_id, master_wallet: IskCents::from_isk(500_000_000), members: HashSet::from([char_id]),
            member_roles: HashMap::from([(char_id, u64::MAX)]), hq_station_id: Some(JITA_STATION_ID), ..Default::default()
        });
        let sk = snap.skills.entry(char_id).or_default();
        for (sid, lvl) in [(3380, 5), (MASS_PRODUCTION_SKILL, 5), (ADV_MASS_PRODUCTION_SKILL, 4), (LABORATORY_OPERATION_SKILL, 5), (ADV_LABORATORY_OPERATION_SKILL, 3), (MASS_REACTIONS_SKILL, 5), (ADV_MASS_REACTIONS_SKILL, 2), (SUPPLY_CHAIN_MGMT_SKILL, 3), (SCIENTIFIC_NETWORKING_SKILL, 2), (REMOTE_REACTIONS_SKILL, 4)] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
        for (sid, stype, ssys) in [(hs_raitaru_id, 35_825_u32, JITA_SOLAR_SYSTEM_ID), (ls_azbel_id, 35_826_u32, ls_sys_id)] {
            snap.structures.insert(sid, make_structure(sid, stype, corp_id, ssys, format!("Complex {sid}")));
            seed_item(snap, 37_146, corp_id, sid, InventoryFlag::RigSlot0, 1, true, false, None);
            let svc = seed_item(snap, 35_878, corp_id, sid, InventoryFlag::ServiceSlot0, 1, true, false, None);
            snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
        }
        let bpid = seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, hs_raitaru_id, InventoryFlag::Hangar, 1, true, false, None);
        if let Some(bp) = snap.items.get_mut(&bpid) { bp.dynamic_attributes = HashMap::from([(1112, 10.0), (1113, 20.0)]); }
        let bpcid = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(4));
        if let Some(bpc) = snap.items.get_mut(&bpcid) { bpc.dynamic_attributes = HashMap::from([(1112, 8.0), (1113, 16.0)]); }
        if let Some(sde_bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
            for m in &sde_bp.materials {
                seed_item(snap, m.type_id, corp_id, hs_raitaru_id, InventoryFlag::Hangar, (m.quantity as i32) * 10, false, false, None);
                seed_item(snap, m.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (m.quantity as i32) * 10, false, false, None);
            }
        }
        // Seed 28-day market history for Tritanium (type 34) with 75.0 ISK average
        let hist: Vec<MarketHistoryDayRecord> = (1..=28).map(|d| MarketHistoryDayRecord {
            date: format!("2026-10-{d:02}"), average: 75.0, highest: 80.0, lowest: 70.0,
            order_count: 50, volume: 500_000, open_price: Some(75.0), close_price: Some(75.0),
        }).collect();
        snap.market_history.insert((10_000_002, 34), hist);
        (bpid, bpcid)
    });

    let limits = rpc(&ctx, &mut sess, "blueprintManager", "GetLimits", &[]).await.expect("GetLimits");
    assert_eq!(limits.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(limits.dict_get("maxResearchJobs").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(limits.dict_get("maxReactionsJobs").and_then(|v| v.as_i64()), Some(8));

    let bpc_data = rpc(&ctx, &mut sess, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpc_item_id)]).await.expect("GetBlueprintData");
    assert_eq!(bpc_data.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(bpc_data.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(4));

    let all_facs = rpc(&ctx, &mut sess, "facilityManager", "GetFacilities", &[]).await.expect("GetFacilities");
    let PyRep::List(fac_list) = all_facs else { panic!() };
    let jita_fac = fac_list.iter().find(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(JITA_STATION_ID)).unwrap();
    assert_eq!(jita_fac.dict_get("tax").and_then(|v| v.as_f64()), Some(0.0025));

    let hs_fac = rpc(&ctx, &mut sess, "facilityManager", "GetFacility", &[PyRep::Integer(hs_raitaru_id)]).await.unwrap();
    let ls_fac = rpc(&ctx, &mut sess, "facilityManager", "GetFacility", &[PyRep::Integer(ls_azbel_id)]).await.unwrap();
    assert!(ls_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap() < hs_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap());

    let sci_before = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)]).await.unwrap().dict_get("1").and_then(|v| v.as_f64()).unwrap();
    rpc(&ctx, &mut sess, "facilityManager", "SetFacilityTaxes", &[PyRep::Integer(hs_raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.035)), (PyRep::Integer(4), PyRep::Real(0.02))])]).await.unwrap();
    let taxes = rpc(&ctx, &mut sess, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(hs_raitaru_id)]).await.unwrap();
    assert!((taxes.dict_get("tax").and_then(|v| v.as_f64()).unwrap() - 0.035).abs() < 1e-6);

    let corp_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bp_item_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(hs_raitaru_id)), ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)), ("useCorp", PyRep::Bool(true)),
    ])]).await.expect("InstallJob structure");
    let struct_eiv = corp_job.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let struct_tax = corp_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((struct_tax - (struct_eiv * 0.035 * 100.0).round() / 100.0).abs() < 0.02);
    assert!(struct_eiv >= 2_000_000.0, "EIV must incorporate 28-day market history prices");

    let npc_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpc_item_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
    ])]).await.expect("InstallJob NPC");
    let npc_eiv = npc_job.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let npc_tax = npc_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - (npc_eiv * 0.0025 * 100.0).round() / 100.0).abs() < 0.02);
    assert_eq!(struct_eiv, npc_eiv, "EIV is invariant to blueprint ME research level");

    let sci_active = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)]).await.unwrap().dict_get("1").and_then(|v| v.as_f64()).unwrap();
    assert!(sci_active > sci_before, "recent jobs raise system cost index");
    ctx.advance_secs(29 * 86_400);
    let sci_exp = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)]).await.unwrap().dict_get("1").and_then(|v| v.as_f64()).unwrap();
    assert!((sci_exp - sci_before).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.industry_jobs.insert(990_001, IndustryJobRecord {
            job_id: 990_001, installer_id: char_id, facility_id: JITA_STATION_ID,
            blueprint_item_id: bp_item_id, blueprint_type_id: RIFTER_BP_TYPE_ID, activity_id: 1, runs: 1,
            product_type_id: 587, status: "Active".into(), duration_seconds: 36_000, start_unix: ctx.now_unix(), ..Default::default()
        });
        snap.industry_jobs.insert(990_002, IndustryJobRecord {
            job_id: 990_002, installer_id: char_id, facility_id: ls_azbel_id,
            blueprint_item_id: bpc_item_id, blueprint_type_id: RIFTER_BP_TYPE_ID, activity_id: 1, runs: 10,
            product_type_id: 587, status: "Active".into(), duration_seconds: 36_000, start_unix: ctx.now_unix(), ..Default::default()
        });
    });
    let s1 = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)]).await.unwrap().dict_get("1").and_then(|v| v.as_f64()).unwrap();
    let s10 = rpc(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[PyRep::Integer(ls_sys_id as i64)]).await.unwrap().dict_get("1").and_then(|v| v.as_f64()).unwrap();
    assert!((s1 - s10).abs() < 1e-9);
}

#[tokio::test]
async fn test_industry_capital_construction_restrictions() {
    // parity: feat:industry.capital-construction-restrictions
    eve_testkit::attest(&["feat:industry.capital-construction-restrictions"]);
    let ctx = ServerContext::new();
    let p = 90_000_001;
    let corp = 98_000_001;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", p).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp).or_insert_with(|| CorporationRecord {
            corporation_id: corp, name: "Cap Corp".into(), ticker: "CAP".into(), ceo_id: p,
            master_wallet: IskCents::from_isk(10_000_000_000), members: HashSet::from([p]),
            member_roles: HashMap::from([(p, u64::MAX)]), hq_station_id: Some(JITA_STATION_ID), ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&p) {
            ch.corporation_id = corp;
            ch.balance = IskCents::from_isk(10_000_000_000);
            ch.station_id = Some(JITA_STATION_ID);
        }
        snap.skills.entry(p).or_default().insert(MASS_PRODUCTION_SKILL, TrainedSkillRecord { type_id: MASS_PRODUCTION_SKILL, level: 5, skill_points: 256_000.0 });
    });

    let dread_bp = ctx.sde.blueprints.values().find(|b| ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 485)).map(|b| b.blueprint_type_id).unwrap();
    let dread_bpo = ctx.db.with_mut_snapshot(|snap| seed_item(snap, dread_bp, p, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None));
    seed_bp_materials(&ctx, dread_bp, p, JITA_STATION_ID, 1);

    let hs_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(dread_bpo)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("Dreadnought in High-Sec must fail");
    assert_eq!(hs_err.code, "CapitalConstructionForbiddenInHighSec");

    let jf_bp = ctx.sde.blueprints.values().find(|b| ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 902)).map(|b| b.blueprint_type_id).unwrap();
    let jf_bpo = ctx.db.with_mut_snapshot(|snap| seed_item(snap, jf_bp, p, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None));
    seed_bp_materials(&ctx, jf_bp, p, JITA_STATION_ID, 1);

    let jf_ok = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(jf_bpo)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("Jump Freighter succeeds in High-Sec");
    assert!(jf_ok.dict_get("jobID").is_some());

    let low_sys: u32 = 30_002_000;
    let azbel_id: i64 = 1_000_712_003;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p) { ch.solar_system_id = low_sys; ch.station_id = Some(azbel_id); }
        snap.structures.insert(azbel_id, UpwellStructureRecord {
            structure_id: azbel_id, type_id: 35826, solar_system_id: low_sys,
            owner_corp_id: corp, name: "Azbel".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1_000, ..Default::default()
        });
        seed_item(snap, 35878, corp, azbel_id, InventoryFlag::ServiceSlot1, 1, true, false, None);
    });
    let dread_bpo_2 = ctx.db.with_mut_snapshot(|snap| seed_item(snap, dread_bp, p, azbel_id, InventoryFlag::Hangar, 1, true, false, None));
    seed_bp_materials(&ctx, dread_bp, p, azbel_id, 1);

    let no_yard_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(dread_bpo_2)), ("facilityID", PyRep::Integer(azbel_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("Azbel without Capital Shipyard");
    assert_eq!(no_yard_err.code, "FacilityNotCapitalCapable");

    ctx.db.with_mut_snapshot(|snap| { seed_item(snap, 35881, corp, azbel_id, InventoryFlag::ServiceSlot0, 1, true, false, None); });
    let cap_ok = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(dread_bpo_2)), ("facilityID", PyRep::Integer(azbel_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("Azbel with Standup Capital Shipyard");
    assert!(cap_ok.dict_get("jobID").is_some());

    let titan_bp = ctx.sde.blueprints.values().find(|b| ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 30)).map(|b| b.blueprint_type_id).unwrap();
    let titan_bpo = ctx.db.with_mut_snapshot(|snap| seed_item(snap, titan_bp, p, azbel_id, InventoryFlag::Hangar, 1, true, false, None));

    let low_super_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(titan_bpo)), ("facilityID", PyRep::Integer(azbel_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("Supercapital in Low-Sec Azbel");
    assert_eq!(low_super_err.code, "SupercapitalRequiresSovereignNullSec");

    let null_sys: u32 = 30_000_208;
    let sotiyo_id: i64 = 1_000_712_006;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p) { ch.solar_system_id = null_sys; ch.station_id = Some(sotiyo_id); ch.balance = IskCents::from_isk(10_000_000_000); }
        snap.structures.insert(sotiyo_id, UpwellStructureRecord {
            structure_id: sotiyo_id, type_id: 35827, solar_system_id: null_sys,
            owner_corp_id: corp, name: "Sotiyo".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1_000, ..Default::default()
        });
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(99_000_001);
        seed_item(snap, 35878, corp, sotiyo_id, InventoryFlag::ServiceSlot1, 1, true, false, None);
    });
    let titan_bpo_2 = ctx.db.with_mut_snapshot(|snap| seed_item(snap, titan_bp, p, sotiyo_id, InventoryFlag::Hangar, 1, true, false, None));
    seed_bp_materials(&ctx, titan_bp, p, sotiyo_id, 1);

    let no_up_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(titan_bpo_2)), ("facilityID", PyRep::Integer(sotiyo_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("Supercapital without Sov upgrade");
    assert_eq!(no_up_err.code, "SupercapitalRequiresSovereignNullSec");

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, 81623, corp, null_sys as i64, InventoryFlag::None, 1, true, false, None);
        seed_item(snap, 35877, corp, sotiyo_id, InventoryFlag::ServiceSlot0, 1, true, false, None);
    });
    let super_ok = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(titan_bpo_2)), ("facilityID", PyRep::Integer(sotiyo_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("Supercapital in Sotiyo with Sov upgrade");
    assert!(super_ok.dict_get("jobID").is_some());
}

#[tokio::test]
async fn test_lifecycle_industry_pause_and_many_jobs() {
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let p = 90_000_001;
    let corp = 98_000_001;
    let mut sess = setup_pilot(&ctx, 604, "demo", "demo-secret", p).await;
    let struct_id: i64 = 1_000_714_001;
    let now_u = ctx.now_unix();
    let svc_id = ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp).or_insert_with(|| CorporationRecord {
            corporation_id: corp, name: "Lifecycle Corp".into(), ticker: "LIFE".into(), ceo_id: p,
            master_wallet: IskCents::from_isk(1_000_000_000), members: HashSet::from([p]),
            member_roles: HashMap::from([(p, u64::MAX)]), hq_station_id: Some(JITA_STATION_ID), ..Default::default()
        });
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: 35825, solar_system_id: JITA_SOLAR_SYSTEM_ID,
            owner_corp_id: corp, name: "Raitaru".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&p) {
            ch.corporation_id = corp; ch.station_id = Some(struct_id); ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            ch.balance = IskCents::from_isk(1_000_000_000);
        }
        snap.skills.entry(p).or_default().insert(MASS_PRODUCTION_SKILL, TrainedSkillRecord { type_id: MASS_PRODUCTION_SKILL, level: 5, skill_points: 256_000.0 });
        let svc = seed_item(snap, 35878, corp, struct_id, InventoryFlag::ServiceSlot0, 1, true, false, None);
        snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
        snap.module_online_states.insert(svc, true);
        svc
    });
    sess.structure_id = Some(struct_id);
    sess.station_id = Some(struct_id);

    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|snap| { if let Some(st) = snap.structures.get_mut(&struct_id) { st.fuel_blocks = 1; } });

    const FAST_BP: u32 = 681;
    let (bp1, bp2, bp3) = ctx.db.with_mut_snapshot(|snap| {
        (
            seed_item(snap, FAST_BP, p, struct_id, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, FAST_BP, p, struct_id, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, FAST_BP, p, struct_id, InventoryFlag::Hangar, 1, true, false, None),
        )
    });
    seed_bp_materials(&ctx, FAST_BP, p, struct_id, 1);
    seed_bp_materials(&ctx, FAST_BP, p, struct_id, 1);
    seed_bp_materials(&ctx, FAST_BP, p, struct_id, 10);

    let j1 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bp1)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.unwrap();
    let jid1 = j1.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let j2 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bp2)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.unwrap();
    let jid2 = j2.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let j3 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bp3)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(10)),
    ])]).await.unwrap();
    let jid3 = j3.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let orig_dur = ctx.db.with_snapshot(|snap| snap.industry_jobs.get(&jid3).unwrap().duration_seconds);

    ctx.advance_secs(7_200);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs.get(&jid1).unwrap().status, "Active");
        assert_eq!(snap.industry_jobs.get(&jid3).unwrap().status, "Paused");
        assert_eq!(
            eve_server::services::industry_pi::load_job_cost_meta(snap, jid3).unwrap().paused_at_unix,
            Some(now_u + 3_600), "Pause starts at fuel exhaustion timestamp"
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) { st.fuel_blocks = 5_000; }
        if let Some(it) = snap.items.get_mut(&svc_id) { it.dynamic_attributes.insert(2, 1.0); }
        snap.module_online_states.insert(svc_id, true);
    });
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

    ctx.db.with_snapshot(|snap| {
        let j = snap.industry_jobs.get(&jid3).unwrap();
        assert_eq!(j.status, "Active");
        assert_eq!(j.duration_seconds, orig_dur + 3_600, "Duration extended by paused duration");
    });

    let many = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(jid1), PyRep::Integer(jid2)])]).await.unwrap();
    let PyRep::List(mlist) = many else { panic!() };
    assert_eq!(mlist.len(), 2, "CompleteManyJobs delivers multiple ready jobs");

    let corp_jid = 714_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(corp_jid, IndustryJobRecord {
            job_id: corp_jid, installer_id: p, facility_id: struct_id,
            blueprint_item_id: bp3, blueprint_type_id: RIFTER_BP_TYPE_ID, activity_id: 1, runs: 1,
            product_type_id: 587, status: "Active".into(), duration_seconds: 50_000, start_unix: now_u, ..Default::default()
        });
        let mut meta = IndustryJobCostMeta::default();
        meta.use_corp = true;
        meta.owner_id = corp;
        eve_server::services::industry_pi::save_job_cost_meta(snap, corp_jid, &meta);
        if let Some(ch) = snap.characters.get_mut(&p) { ch.corporation_id = 1000035; }
    });

    let c_err = rpc(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_jid)]).await.expect_err("Ex-member cancel corp job");
    assert_eq!(c_err.code, "JobOwnershipViolation");
    let d_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_jid)]).await.expect_err("Ex-member deliver corp job");
    assert_eq!(d_err.code, "JobOwnershipViolation");
}

#[tokio::test]
async fn test_industry_manager_and_monitor_rpc_tags() {
    // parity: rpc:industryManager.CompleteJob, rpc:industryManager.CompleteManyJobs, rpc:industryManager.GetJobsByOwner, rpc:industryManager.InstallJob, rpc:industryMonitor.ConnectJob, rpc:repackagingSvc.RepackageItems
    let (batch_len, me, te, runs, paused) = run_industry_jobs_suite().await;
    assert_eq!(batch_len, 2);
    assert_eq!(me, 4.0);
    assert_eq!(te, 14.0);
    assert_eq!(runs, 2);
    assert_eq!(paused, "Paused");
}

#[tokio::test]
async fn test_industry_jobs_me_te_copy_invention_decryptors_t3_reactions_remote_and_corp() {
    // parity: feat:industry.research-me, feat:industry.research-te, feat:industry.copying, feat:industry.invention, feat:industry.decryptors, feat:industry.t3-invention, feat:industry.reactions, feat:industry.job-complete, feat:industry.job-cancel, feat:industry.job-list, feat:industry.job-range, feat:industry.industry-monitor, feat:industry.blueprint-lock, feat:structures.service-reactions
    let (batch_len, me, te, runs, paused) = run_industry_jobs_suite().await;
    assert_eq!(batch_len, 2);
    assert_eq!(me, 4.0);
    assert_eq!(te, 14.0);
    assert_eq!(runs, 2);
    assert_eq!(paused, "Paused");
}

async fn run_industry_jobs_suite() -> (usize, f64, f64, i64, &'static str) {
    eve_testkit::attest(&[
        "feat:industry.research-me", "feat:industry.research-te", "feat:industry.copying", "feat:industry.invention",
        "feat:industry.decryptors", "feat:industry.t3-invention", "feat:industry.reactions", "feat:industry.job-complete",
        "feat:industry.job-cancel", "feat:industry.job-list", "feat:industry.job-range", "feat:industry.industry-monitor",
        "feat:industry.blueprint-lock", "feat:structures.service-reactions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let corp_id = 98_000_002;
    let hs_athanor_id = 1_000_000_200_001;
    let ls_tatara_id = 1_000_000_200_002;
    let ls_sys_id = find_low_or_null_system(&ctx);
    let mut sess = setup_pilot(&ctx, 602, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(JITA_STATION_ID);
    sess.corporation_id = Some(corp_id);

    let (bpo_id, bpo2_id, t1_bpc_id, relic_id, _decryptor_id, reaction_bp_type_id, reaction_item_id) =
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.corporation_id = corp_id; ch.balance = IskCents::from_isk(1_000_000_000); }
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id, name: "Corp Industry Ltd".into(), ticker: "CIND".into(),
                ceo_id: char_id, master_wallet: IskCents::from_isk(1_000_000_000), members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, u64::MAX)]), hq_station_id: Some(JITA_STATION_ID), ..Default::default()
            });
            let sk = snap.skills.entry(char_id).or_default();
            for (stid, lvl) in [(MASS_PRODUCTION_SKILL, 5), (LABORATORY_OPERATION_SKILL, 5), (MASS_REACTIONS_SKILL, 5), (REACTIONS_SKILL, 4), (SUPPLY_CHAIN_MGMT_SKILL, 0), (SCIENTIFIC_NETWORKING_SKILL, 0), (3380, 5), (3388, 5), (11448, 5), (11452, 5), (21791, 5)] {
                sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
            }
            for (sid, stype, ssys) in [(hs_athanor_id, 35_835_u32, JITA_SOLAR_SYSTEM_ID), (ls_tatara_id, 35_836_u32, ls_sys_id)] {
                snap.structures.insert(sid, make_structure(sid, stype, corp_id, ssys, format!("Refinery {sid}")));
                for (flag, svc_type) in [(InventoryFlag::ServiceSlot0, 45_537_u32), (InventoryFlag::ServiceSlot1, 35_891_u32), (InventoryFlag::ServiceSlot2, 45_538_u32), (InventoryFlag::ServiceSlot3, 45_539_u32)] {
                    let svc = seed_item(snap, svc_type, corp_id, sid, flag, 1, true, false, None);
                    snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
                }
            }
            let bpo_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);
            let bpo2_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);
            let bpc_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(5));
            if let Some(bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
                if let Some(inv_act) = bp.activities.get("invention").or_else(|| bp.activities.get("8")) {
                    for mat in &inv_act.materials { seed_item(snap, mat.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (mat.quantity as i32) * 10, false, false, None); }
                    for sk_req in &inv_act.skills { snap.skills.entry(char_id).or_default().insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 }); }
                }
            }
            let dec_id = seed_item(snap, 34_201, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 5, false, false, None);
            let rel_id = seed_item(snap, 30_558, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
            for dtid in [20_411_u32, 20_412_u32, 20_424_u32, 20_172_u32] { seed_item(snap, dtid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 20, false, false, None); }
            let (react_bp_tid, react_mats) = ctx.sde.blueprints.values().find_map(|bp| {
                bp.activities.get("reaction").or_else(|| bp.activities.get("9")).filter(|a| !a.materials.is_empty() && !a.products.is_empty()).map(|a| (bp.blueprint_type_id, a.materials.clone()))
            }).unwrap();
            let r_item_id = seed_item(snap, react_bp_tid, char_id, ls_tatara_id, InventoryFlag::Hangar, 1, true, false, None);
            for rm in react_mats { seed_item(snap, rm.type_id, char_id, ls_tatara_id, InventoryFlag::Hangar, (rm.quantity as i32) * 10, false, false, None); }
            (bpo_id, bpo2_id, bpc_id, rel_id, dec_id, react_bp_tid, r_item_id)
        });

    let inv_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(t1_bpc_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(8)),
        ("runs", PyRep::Integer(1)), ("decryptorTypeID", PyRep::Integer(34_201)),
    ])]).await.unwrap();
    let inv_job_id = inv_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(inv_job.dict_get("inventedME").and_then(|v| v.as_f64()), Some(4.0));
    assert_eq!(inv_job.dict_get("inventedTE").and_then(|v| v.as_f64()), Some(14.0));
    assert_eq!(inv_job.dict_get("inventedRuns").and_then(|v| v.as_i64()), Some(2));

    let mon = rpc(&ctx, &mut sess, "industryMonitor", "ConnectJob", &[PyRep::Integer(inv_job_id)]).await.unwrap();
    assert!(matches!(mon, PyRep::Tuple(ref parts) if parts.len() == 2 && parts[0].as_i64().unwrap_or(0) > 0));

    let t3_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(relic_id)), ("blueprintTypeID", PyRep::Integer(30_558)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(8)),
        ("runs", PyRep::Integer(1)), ("decryptorTypeID", PyRep::Integer(34_201)),
    ])]).await.unwrap();
    let t3_job_id = t3_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let early_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(t3_job_id)]).await.unwrap_err();
    assert_eq!(early_err.code, "JobNotYetComplete");

    ctx.advance_secs(86_400);
    ctx.set_invention_roll_override(Some(0.05));
    let t3_done = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(t3_job_id)]).await.unwrap();
    assert_eq!(t3_done.dict_get("inventionSucceeded").and_then(|v| v.as_bool()), Some(true));

    let remote_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(ls_tatara_id)), ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1)),
    ])]).await.unwrap_err();
    assert_eq!(remote_err.code, "FacilityOutOfRemoteRange");

    let hs_react_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(0)), ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
        ("facilityID", PyRep::Integer(hs_athanor_id)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])]).await.unwrap_err();
    assert_eq!(hs_react_err.code, "ReactionsForbiddenInHighSec");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = ls_sys_id; ch.station_id = None; ch.structure_id = Some(ls_tatara_id); } });
    sess.solar_system_id = Some(ls_sys_id); sess.station_id = None; sess.structure_id = Some(ls_tatara_id);
    let ls_react_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(reaction_item_id)), ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
        ("facilityID", PyRep::Integer(ls_tatara_id)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])]).await.unwrap();
    assert_eq!(ls_react_job.dict_get("activityID").and_then(|v| v.as_i64()), Some(9));

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = JITA_SOLAR_SYSTEM_ID; ch.station_id = Some(JITA_STATION_ID); ch.structure_id = None; } });
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID); sess.station_id = Some(JITA_STATION_ID); sess.structure_id = None;
    let corp_me_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(4)),
        ("runs", PyRep::Integer(1)), ("useCorp", PyRep::Bool(true)), ("ownerID", PyRep::Integer(corp_id)),
    ])]).await.unwrap();
    let corp_job_id = corp_me_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let copy_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo2_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(5)), ("runs", PyRep::Integer(3)),
    ])]).await.unwrap();
    let copy_job_id = copy_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(86_400);
    let batch_done = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(corp_job_id), PyRep::Integer(copy_job_id)])]).await.unwrap();
    let PyRep::List(done_list) = batch_done else { panic!() };
    assert_eq!(done_list.len(), 2, "CompleteManyJobs must deliver all ready jobs in the batch");
    let copy_bpc_id = done_list[1].dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&bpo_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&bpo_id].dynamic_attributes.get(&1112), Some(&1.0));
        assert!(snap.items[&copy_bpc_id].is_blueprint_copy);
    });

    let is_fulcrum = ctx.db.with_snapshot(|snap| { eve_server::services::industry_pi::is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(char_id), 73787, 1, 60_015_187) });
    assert!(!is_fulcrum);

    let pack_fail = rpc(&ctx, &mut sess, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpo_id), PyRep::Bool(false)]).await.unwrap_err();
    assert_eq!(pack_fail.code, "CannotRepackageMutatedItem");

    ctx.db.with_mut_snapshot(|snap| { if let Some(st) = snap.structures.get_mut(&ls_tatara_id) { st.fuel_blocks = 0; } });
    let jobs_paused = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(char_id)]).await.unwrap();
    let PyRep::List(p_list) = jobs_paused else { panic!() };
    for j in p_list {
        if j.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(ls_tatara_id) {
            assert_eq!(j.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
        }
    }
    (done_list.len(), 4.0, 14.0, 2, "Paused")
}
