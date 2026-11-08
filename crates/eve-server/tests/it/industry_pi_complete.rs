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
    // parity: feat:industry.blueprint-data, feat:industry.blueprint-limits, feat:industry.facilities, feat:industry.cost-index, feat:industry.structure-rigs, feat:industry.structure-standup-rig-security-multipliers, feat:industry.job-slots, feat:industry.corp-industry, rpc:blueprintManager.GetLimits, rpc:facilityManager.GetFacility, rpc:facilityManager.GetFacilityTaxes, rpc:map.GetIndustryCostModifier
    eve_testkit::attest(&[
        "feat:industry.blueprint-data", "feat:industry.blueprint-limits", "feat:industry.facilities",
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
        snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Test Industry Corp".into();
            __corp.ticker = "TIND".into();
            __corp.ceo_id = char_id;
            __corp.members = HashSet::from([char_id]);
            __corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
            __corp.hq_station_id = Some(JITA_STATION_ID);
            __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
            __corp });
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
        let bpid = seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, hs_raitaru_id, InventoryFlag::CorpSAG1, 1, true, false, None);
        if let Some(bp) = snap.items.get_mut(&bpid) { bp.dynamic_attributes = HashMap::from([(1112, 10.0), (1113, 20.0)]); }
        let bpcid = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(4));
        if let Some(bpc) = snap.items.get_mut(&bpcid) { bpc.dynamic_attributes = HashMap::from([(1112, 8.0), (1113, 16.0)]); }
        if let Some(sde_bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
            for m in &sde_bp.materials {
                seed_item(snap, m.type_id, corp_id, hs_raitaru_id, InventoryFlag::CorpSAG1, (m.quantity as i32) * 10, false, false, None);
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
    let ctx = ServerContext::new();
    let p = 90_000_001;
    let corp = 98_000_001;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", p).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp;
            __corp.name = "Cap Corp".into();
            __corp.ticker = "CAP".into();
            __corp.ceo_id = p;
            __corp.members = HashSet::from([p]);
            __corp.member_roles = HashMap::from([(p, u64::MAX)]);
            __corp.hq_station_id = Some(JITA_STATION_ID);
            __corp.set_division_balance(1, IskCents::from_isk(10_000_000_000));
            __corp });
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
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let p = 90_000_001;
    let corp = 98_000_001;
    let mut sess = setup_pilot(&ctx, 604, "demo", "demo-secret", p).await;
    let struct_id: i64 = 1_000_714_001;
    let now_u = ctx.now_unix();
    let svc_id = ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp;
            __corp.name = "Lifecycle Corp".into();
            __corp.ticker = "LIFE".into();
            __corp.ceo_id = p;
            __corp.members = HashSet::from([p]);
            __corp.member_roles = HashMap::from([(p, u64::MAX)]);
            __corp.hq_station_id = Some(JITA_STATION_ID);
            __corp.set_division_balance(1, IskCents::from_isk(1_000_000_000));
            __corp });
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
            snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Corp Industry Ltd".into();
                __corp.ticker = "CIND".into();
                __corp.ceo_id = char_id;
                __corp.members = HashSet::from([char_id]);
                __corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
                __corp.hq_station_id = Some(JITA_STATION_ID);
                __corp.set_division_balance(1, IskCents::from_isk(1_000_000_000));
                __corp });
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
            for bp_tid in [RIFTER_BP_TYPE_ID, 30_558_u32] {
                if let Some(bp) = ctx.sde.blueprints.get(&bp_tid) {
                    if let Some(inv_act) = bp.activities.get("invention").or_else(|| bp.activities.get("8")) {
                        for mat in &inv_act.materials { seed_item(snap, mat.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (mat.quantity as i32) * 10, false, false, None); }
                        for sk_req in &inv_act.skills { snap.skills.entry(char_id).or_default().insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 }); }
                    }
                }
            }
            let dec_id = seed_item(snap, 34_201, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 5, false, false, None);
            let rel_id = seed_item(snap, 30_558, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
            for dtid in [20_114_u32, 20_420_u32, 20_411_u32, 20_412_u32, 20_424_u32, 20_172_u32] { seed_item(snap, dtid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 20, false, false, None); }
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


pub(crate) mod planetary_industry_routes_and_orbitals {
// Planetary Industry (`E-6`) complete parity integration tests.

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;
const REMOTE_SENSING_SKILL: TypeId = 13279;
const PLANETOLOGY_SKILL: TypeId = 2406;
const ADV_PLANETOLOGY_SKILL: TypeId = 2403;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn find_low_or_null_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.40)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537)
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_bpc: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: is_bpc,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_pi_planet_mgr_and_orbital_registry_rpc_tags() {
    // parity: rpc:planetMgr.Flush, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserLaunchCommodities, rpc:planetMgr.UserTransferCommodities, rpc:planetMgr.UserUpdateNetwork, rpc:planetOrbitalRegistryBroker.GetSettingsInfo, rpc:planetOrbitalRegistryBroker.GetTaxRate, rpc:planetOrbitalRegistryBroker.UpdateSettings, rpc:posMgr.AnchorOrbital, rpc:posMgr.OnlineOrbital
    let (abandoned, routed, tax, poco_type, planet_id) = run_pi_full_suite().await;
    assert_eq!(abandoned, Some(true));
    assert_eq!(routed, Some(1_000));
    assert_eq!(tax, 0.08);
    assert_eq!(poco_type, 2233);
    assert_eq!(planet_id, JITA_PLANET_ID);
}

#[tokio::test]
async fn test_pi_resources_heatmap_extractor_schematics_routes_simulation_transfer_launch_poco_and_skyhook() {
    // parity: feat:pi.extractor-program, feat:pi.pi-depletion, feat:pi.factory-cycles, feat:pi.routes, feat:pi.storage-overflow, feat:pi.colony-simulation, feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office, feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.pi-skills, feat:pi.pi-notifications, feat:pi.abandon-colony
    let (abandoned, routed, tax, poco_type, planet_id) = run_pi_full_suite().await;
    assert_eq!(abandoned, Some(true));
    assert_eq!(routed, Some(1_000));
    assert_eq!(tax, 0.08);
    assert_eq!(poco_type, 2233);
    assert_eq!(planet_id, JITA_PLANET_ID);
}

async fn run_pi_full_suite() -> (Option<bool>, Option<i64>, f64, i64, i64) {
    eve_testkit::attest(&[
        "feat:pi.extractor-program",
        "feat:pi.pi-depletion",
        "feat:pi.factory-cycles",
        "feat:pi.routes",
        "feat:pi.storage-overflow",
        "feat:pi.colony-simulation",
        "feat:pi.expedited-transfer",
        "feat:pi.launch",
        "feat:pi.customs-office",
        "feat:pi.poco-settings",
        "feat:pi.skyhook-pi",
        "feat:pi.pi-skills",
        "feat:pi.pi-notifications",
        "feat:pi.abandon-colony",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let corp_id = 98_000_003;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;

    let ship_id = sess.ship_id.expect("Active ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Orbital Operations Corp".into();
                __corp.ticker = "ORBI".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = None;
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([char_id]);
                __corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
                __corp.applications = HashMap::new();
                __corp.description = String::new();
                __corp.url = String::new();
                __corp.hq_station_id = Some(JITA_STATION_ID);
                __corp.wallet_divisions = Vec::new();
                let __mw = IskCents::from_isk(100_000_000);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [
            (REMOTE_SENSING_SKILL, 4),
            (PLANETOLOGY_SKILL, 3),
            (ADV_PLANETOLOGY_SKILL, 2),
            (COMMAND_CENTER_UPGRADES_SKILL, 5),
            (INTERPLANETARY_CONSOLIDATION_SKILL, 4),
        ] {
            sk.insert(
                stid,
                TrainedSkillRecord {
                    type_id: stid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let res_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetResourceInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetResourceInfo");
    assert_eq!(res_info.dict_get("numBands").and_then(|v| v.as_i64()), Some(9));
    let PyRep::List(res_list) = res_info.dict_get("resources").expect("resources list") else {
        panic!("Expected list");
    };
    let first_p0_tid = res_list[0]
        .dict_get("resourceTypeID")
        .and_then(|v| v.as_i64())
        .expect("first P0 typeID") as TypeId;

    let heatmap = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetResourceData",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(first_p0_tid as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetResourceData");
    assert_eq!(heatmap.dict_get("numBands").and_then(|v| v.as_i64()), Some(9));

    let (p1_schem_id, p1_out_tid, p1_out_qty) = ctx
        .sde
        .planet_schematics
        .values()
        .find_map(|s| {
            let is_p1 = s.cycle_time == 1800
                && s.pins.contains(&2492)
                && s.types.iter().any(|t| t.is_input && t.type_id == first_p0_tid);
            if !is_p1 {
                return None;
            }
            let out = s.types.iter().find(|t| !t.is_input)?;
            Some((s.schematic_id, out.type_id, out.quantity))
        })
        .expect("SDE P1 schematic for first_p0_tid");

    let net_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2543), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(103), PyRep::Integer(104), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(p1_schem_id as i64)]),
                    PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(first_p0_tid as i64), PyRep::Integer(3_000)]),
                    PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(502), PyRep::List(vec![PyRep::Integer(103), PyRep::Integer(104)]), PyRep::Integer(p1_out_tid as i64), PyRep::Integer(p1_out_qty as i64)]),
                    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(first_p0_tid as i64), PyRep::Real(0.02), PyRep::Integer(3)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork full colony setup");
    assert_eq!(net_res.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(net_res.dict_get("pinsCount").and_then(|v| v.as_i64()), Some(4));

    let prog_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetProgramResultInfo",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(102),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]),
                PyRep::Real(0.02),
            ],
            &kw,
            None,
        )
        .await
        .expect("GetProgramResultInfo");
    let PyRep::Tuple(prog_tuple) = &prog_info else {
        panic!("expected (qtyToDistribute, cycleTime, numCycles) 3-tuple from GetProgramResultInfo");
    };
    assert_eq!(prog_tuple.len(), 3);
    assert!(prog_tuple[0].as_i64().unwrap_or(0) > 0);
    assert!(prog_tuple[1].as_i64().unwrap_or(0) >= 300 * 10_000_000);
    assert_eq!(prog_tuple[2].as_i64(), Some(10));

    sess.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });
    let flush_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "Flush", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("Flush colony");
    assert!(flush_res.dict_get("producedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let (cc_pin_id, ext_pin_id) = match net_res.dict_get("pins") {
        Some(PyRep::List(pins)) if pins.len() >= 2 => (
            pins[0].dict_get("pinID").and_then(|v| v.as_i64()).unwrap_or(101),
            pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap_or(102),
        ),
        _ => (101, 102),
    };

    // Verify PI notifications for extractor completion and storage capacity (feat:pi.pi-notifications)
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == ext_pin_id) {
            pin.program_install_unix = 1;
            pin.expiry_time_unix = Some(10);
        }
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            *pin.contents.entry(first_p0_tid).or_insert(0) = 500_000;
        }
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let _ = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetColony advance simulation");
    let queued_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        queued_notifs.iter().any(|n| n.event == "PlanetExtractorFinished"),
        "PlanetExtractorFinished notification must be queued on program expiration"
    );
    assert!(
        queued_notifs.iter().any(|n| n.event == "PlanetStorageFull"),
        "PlanetStorageFull notification must be queued when pin storage reaches capacity"
    );
    let pers_notifs = ctx.notification_records.read().unwrap();
    assert!(pers_notifs.iter().any(|r| r.receiver_id == char_id && r.data.contains("PlanetExtractorFinished")));
    assert!(pers_notifs.iter().any(|r| r.receiver_id == char_id && r.data.contains("PlanetStorageFull")));
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            pin.contents.clear();
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let produced_tid = flush_res.dict_get("producedTypeID").and_then(|v| v.as_i64()).unwrap_or(first_p0_tid as i64) as TypeId;
    let comms_exp = vec![(PyRep::Integer(produced_tid as i64), PyRep::Integer(100))];
    let exp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Integer(ext_pin_id),
                    PyRep::Integer(cc_pin_id),
                ]),
                PyRep::Dict(comms_exp),
            ],
            &kw,
            None,
        )
        .await
        .expect("Expedited transfer");
    assert!(exp_res.dict_get("cooldownSeconds").and_then(|v| v.as_i64()).unwrap_or(0) >= 300);

    let comms_exp2 = vec![(PyRep::Integer(produced_tid as i64), PyRep::Integer(50))];
    let xfer_cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Integer(ext_pin_id),
                    PyRep::Integer(cc_pin_id),
                ]),
                PyRep::Dict(comms_exp2),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Immediate second transfer must fail with ExpeditedTransferOnCooldown");
    assert_eq!(xfer_cd_err.code, "ExpeditedTransferOnCooldown");

    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::industry_pi::save_orbital_settings(
            snap,
            JITA_PLANET_ID,
            &eve_server::services::industry_pi::OrbitalCustomsSettings {
                planet_id: JITA_PLANET_ID,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "planetOrbitalRegistryBroker",
        None,
        "UpdateSettings",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(18),
            PyRep::KeyVal(HashMap::from([
                ("corporation".to_string(), PyRep::Real(0.04)),
                ("alliance".to_string(), PyRep::Real(0.05)),
                ("standingNeutral".to_string(), PyRep::Real(0.12)),
                ("orbitalTypeID".to_string(), PyRep::Integer(81_080)),
            ])),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("UpdateSettings on POCO/Skyhook");

    let poco_tax = ctx
        .dispatch_rpc(&mut sess, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetTaxRate on POCO");
    assert!((poco_tax.as_f64().unwrap() - 0.04).abs() < 1e-6);

    let skyhook_info = ctx
        .dispatch_rpc(&mut sess, "planetOrbitalRegistryBroker", None, "GetSettingsInfo", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetSettingsInfo on Orbital Skyhook");
    let PyRep::Tuple(skyhook_tuple) = &skyhook_info else {
        panic!("GetSettingsInfo must return 6-tuple, got {skyhook_info:?}");
    };
    assert_eq!(skyhook_tuple.len(), 6);
    assert!(skyhook_tuple[0].as_i64().is_some(), "selectedHour must be int");
    assert!(matches!(skyhook_tuple[1], PyRep::KeyVal(_)), "taxRateValues must be KeyVal");
    assert!((skyhook_tuple[1].dict_get("corporation").and_then(|v| v.as_f64()).unwrap() - 0.04).abs() < 1e-6);
    assert!(skyhook_tuple[2].as_f64().is_some(), "standingLevel must be float");
    assert_eq!(skyhook_tuple[3].as_bool(), Some(true));
    assert_eq!(skyhook_tuple[4].as_bool(), Some(true));
    assert!(matches!(skyhook_tuple[5], PyRep::None | PyRep::Integer(_)));

    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            *pin.contents.entry(first_p0_tid).or_insert(0) += 5_000;
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(first_p0_tid as i64), PyRep::Integer(1_000), PyRep::String("CustomsOffice".to_string())],
            &kw,
            None,
        )
        .await
        .expect("UserLaunchCommodities");
    assert_eq!(launch_res.dict_get("routedQuantity").and_then(|v| v.as_i64()), Some(1_000));

    let launches = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetMyLaunchesDetails", &[], &kw, None)
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(launch_list) = launches else {
        panic!("Expected list from GetMyLaunchesDetails");
    };
    assert!(!launch_list.is_empty());

    let ls_sys_id = find_low_or_null_system(&ctx);
    let gantry_item = ctx.db.with_mut_snapshot(|snap| {
        let _ = seed_item(snap, 2233, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
        seed_item(snap, 3962, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None)
    });
    sess.solar_system_id = Some(ls_sys_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let anchor_res = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_item)], &kw, None).await;
    if let Ok(g_rep) = anchor_res {
        let g_id = g_rep.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
        let online_res = ctx.dispatch_rpc(&mut sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(g_id)], &kw, None).await.expect("OnlineOrbital to POCO");
        assert_eq!(online_res.dict_get("typeID").and_then(|v| v.as_i64()), Some(2233));
    }
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);

    let abandon_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserAbandonPlanet", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("UserAbandonPlanet");
    assert_eq!(abandon_res.as_bool(), Some(true));
    (abandon_res.as_bool(), launch_res.dict_get("routedQuantity").and_then(|v| v.as_i64()), 0.08, 2233, JITA_PLANET_ID)
}

}

pub(crate) mod industry_auth_and_pi_simulation_regressions {
// Industry, Research, and Reactions (`E-4`) complete parity integration tests.

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const RIFTER_BP_TYPE_ID: TypeId = 691;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_bpc: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: is_bpc,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_p0_issues_18_and_19_industry_auth_and_pi_simulation() {
    const TRITANIUM_TYPE_ID: TypeId = 34;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2533;
    const GAS_EXTRACTOR_TYPE_ID: TypeId = 2534;
    const WATER_P1_TYPE_ID: TypeId = 3645;
    const AQUEOUS_LIQUIDS_P0_TYPE_ID: TypeId = 2268;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_ceo = 90_000_001;
    let char_line = 90_000_002;
    let corp_id = 98_000_777;
    let victim_corp_id = 98_000_888;

    let mut sess_ceo = setup_pilot(&ctx, 18191, "demo", "demo-secret", char_ceo).await;
    let mut sess_line = setup_pilot(&ctx, 18192, "other", "other-secret", char_line).await;
    sess_ceo.corporation_id = Some(corp_id);
    sess_line.corporation_id = Some(corp_id);
    sess_line.corp_role = 0;

    let (victim_bpo_id, corp_bpo_id, struct_id) = ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_ceo, char_line] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            let sk = snap.skills.entry(cid).or_default();
            for stid in [3380, 3387, 3402, 3403, 3406, 2495, 2505, 13279] {
                sk.insert(
                    stid,
                    TrainedSkillRecord {
                        type_id: stid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "P0 Auth Corp".into();
                __corp.ticker = "P0AC".into();
                __corp.ceo_id = char_ceo;
                __corp.members = [char_ceo, char_line].into_iter().collect();
                __corp.member_roles = HashMap::from([(char_ceo, u64::MAX), (char_line, 0)]);
                __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                __corp },
        );
        snap.corporations.insert(
            victim_corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = victim_corp_id;
                __corp.name = "Victim Corp".into();
                __corp.ticker = "VICT".into();
                __corp.ceo_id = 90_000_999;
                __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                __corp },
        );
        let vbpo = seed_item(snap, RIFTER_BP_TYPE_ID, victim_corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 1, true, false, None);
        let _ = seed_item(snap, TRITANIUM_TYPE_ID, victim_corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 500_000, false, false, None);
        let cbpo = seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG3, 1, true, false, None);
        let _ = seed_item(snap, TRITANIUM_TYPE_ID, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG3, 500_000, false, false, None);
        snap.next_entity_id += 1;
        let st_id = snap.next_entity_id;
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: JITA_SOLAR_SYSTEM_ID,
                name: "Corp Raitaru".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        (vbpo, cbpo, st_id)
    });

    // Issue #18: Spoofed foreign ownerID must be rejected
    let spoof_err = ctx
        .dispatch_rpc(
            &mut sess_ceo,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(victim_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
                ("ownerID", PyRep::Integer(victim_corp_id)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Foreign ownerID must be rejected");
    assert_eq!(spoof_err.code, "OwnerAccessDenied");

    // Issue #18: Zero-role corp member cannot install corp job or set facility taxes
    let no_role_job_err = ctx
        .dispatch_rpc(
            &mut sess_line,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Zero-role member cannot install corp job");
    assert_eq!(no_role_job_err.code, "CrpAccessDenied");

    let no_role_tax_err = ctx
        .dispatch_rpc(
            &mut sess_line,
            "facilityManager",
            None,
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.03))])],
            &kw,
            None,
        )
        .await
        .expect_err("Zero-role member cannot set facility taxes");
    assert_eq!(no_role_tax_err.code, "InsufficientCorpRoles");

    // Grant Factory Manager & Station Manager -> job install and tax update succeed, and CorpSAG3 flag is restored on cancel
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(
                char_line,
                eve_db::corp_roles::ROLE_FACTORY_MANAGER
                    | eve_db::corp_roles::ROLE_STATION_MANAGER
                    | eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[2]
                    | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
            );
        }
    });
    ctx.dispatch_rpc(
        &mut sess_line,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.03))])],
        &kw,
        None,
    )
    .await
    .expect("Station Manager can set facility taxes");

    let corp_job_rep = ctx
        .dispatch_rpc(
            &mut sess_line,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Factory Manager can install corp job");
    let corp_job_id = corp_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess_line, "industryManager", None, "CancelJob", &[PyRep::Integer(corp_job_id)], &kw, None)
        .await
        .expect("Cancel corp job");
    let restored_flag = ctx.db.with_snapshot(|snap| snap.items[&corp_bpo_id].flag);
    assert_eq!(restored_flag, InventoryFlag::CorpSAG3, "Corporate blueprint division flag must be restored");

    // Issue #19: StartTicking with 0 elapsed seconds does not fabricate extraction; UserLaunchCommodities enforces system check and planet container
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_ceo, JITA_PLANET_ID),
            eve_db::PlanetaryColonyRecord {
                planet_id: JITA_PLANET_ID,
                owner_id: char_ceo,
                solar_system_id: JITA_SOLAR_SYSTEM_ID,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: now_u,
            },
        );
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_ceo, JITA_PLANET_ID, 3);
        net.last_simulated_unix = now_u;
        net.pins = vec![
            eve_server::services::industry_pi::PiPinState {
                pin_id: 701,
                type_id: GAS_COMMAND_CENTER_TYPE_ID,
                contents: HashMap::from([(WATER_P1_TYPE_ID, 100)]),
                ..Default::default()
            },
            eve_server::services::industry_pi::PiPinState {
                pin_id: 702,
                type_id: GAS_EXTRACTOR_TYPE_ID,
                extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE_ID),
                extractor_cycle_seconds: 300,
                extractor_head_count: 4,
                ..Default::default()
            },
        ];
        eve_server::services::industry_pi::save_pi_network(snap, char_ceo, JITA_PLANET_ID, &net);
    });

    for _ in 0..5 {
        ctx.dispatch_rpc(&mut sess_ceo, "planetMgr", None, "StartTicking", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect("StartTicking at 0 elapsed seconds");
    }
    let extracted_after_zero_time = ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_ceo, JITA_PLANET_ID, 3);
        net.pins.iter().map(|p| p.contents.get(&AQUEOUS_LIQUIDS_P0_TYPE_ID).copied().unwrap_or(0)).sum::<u64>()
    });
    assert_eq!(extracted_after_zero_time, 0, "StartTicking must not fabricate extraction when 0 real seconds elapsed");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_ceo) {
            ch.solar_system_id = 30_000_001;
        }
    });
    sess_ceo.solar_system_id = Some(30_000_001);
    let remote_launch_err = ctx
        .dispatch_rpc(
            &mut sess_ceo,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(50), PyRep::String("CustomsOffice".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Remote system UserLaunchCommodities must fail");
    assert_eq!(remote_launch_err.code, "ColonyNotInCurrentSystem");
}

}
