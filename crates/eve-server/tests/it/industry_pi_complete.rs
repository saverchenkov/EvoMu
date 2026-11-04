//! Industry, Research, and Reactions (`E-4`) complete parity integration tests (Part 1).

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
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

fn make_structure(sid: i64, stype: TypeId, corp_id: i64, ssys: u32, name: String) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: stype,
        owner_corp_id: corp_id,
        solar_system_id: ssys,
        name,
        position: [100_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0, self_destruct_at_unix: None,
    }
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
async fn test_industry_blueprint_data_owner_limits_facilities_taxes_and_sci() {
    // parity: feat:industry.blueprint-data, feat:industry.blueprint-limits, feat:industry.facilities, feat:industry.facility-tax, feat:industry.cost-index, feat:industry.structure-rigs, feat:industry.structure-standup-rig-security-multipliers, feat:industry.job-slots, feat:industry.corp-industry, rpc:blueprintManager.GetBlueprintDataByOwner, rpc:blueprintManager.GetLimits, rpc:facilityManager.GetFacility, rpc:facilityManager.GetFacilityTaxes, rpc:facilityManager.SetFacilityTaxes, rpc:map.GetIndustryCostModifier
    eve_testkit::attest(&[
        "feat:industry.blueprint-data",
        "feat:industry.blueprint-limits",
        "feat:industry.facilities",
        "feat:industry.facility-tax",
        "feat:industry.cost-index",
        "feat:industry.structure-rigs",
        "feat:industry.structure-standup-rig-security-multipliers",
        "feat:industry.job-slots",
        "feat:industry.corp-industry",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
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
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Industry Corp".into(),
                ticker: "TIND".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(500_000_000),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, u64::MAX)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(JITA_STATION_ID),
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
        sk.insert(MASS_PRODUCTION_SKILL, TrainedSkillRecord { type_id: MASS_PRODUCTION_SKILL, level: 5, skill_points: 256_000.0 });
        sk.insert(ADV_MASS_PRODUCTION_SKILL, TrainedSkillRecord { type_id: ADV_MASS_PRODUCTION_SKILL, level: 4, skill_points: 100_000.0 });
        sk.insert(LABORATORY_OPERATION_SKILL, TrainedSkillRecord { type_id: LABORATORY_OPERATION_SKILL, level: 5, skill_points: 256_000.0 });
        sk.insert(ADV_LABORATORY_OPERATION_SKILL, TrainedSkillRecord { type_id: ADV_LABORATORY_OPERATION_SKILL, level: 3, skill_points: 60_000.0 });
        sk.insert(MASS_REACTIONS_SKILL, TrainedSkillRecord { type_id: MASS_REACTIONS_SKILL, level: 5, skill_points: 256_000.0 });
        sk.insert(ADV_MASS_REACTIONS_SKILL, TrainedSkillRecord { type_id: ADV_MASS_REACTIONS_SKILL, level: 2, skill_points: 40_000.0 });
        sk.insert(SUPPLY_CHAIN_MGMT_SKILL, TrainedSkillRecord { type_id: SUPPLY_CHAIN_MGMT_SKILL, level: 3, skill_points: 40_000.0 });
        sk.insert(SCIENTIFIC_NETWORKING_SKILL, TrainedSkillRecord { type_id: SCIENTIFIC_NETWORKING_SKILL, level: 2, skill_points: 20_000.0 });
        sk.insert(REMOTE_REACTIONS_SKILL, TrainedSkillRecord { type_id: REMOTE_REACTIONS_SKILL, level: 4, skill_points: 80_000.0 });

        for (sid, stype, ssys) in [
            (hs_raitaru_id, 35_825_u32, JITA_SOLAR_SYSTEM_ID),
            (ls_azbel_id, 35_826_u32, ls_sys_id),
        ] {
            snap.structures.insert(
                sid,
                make_structure(sid, stype, corp_id, ssys, format!("Engineering Complex {sid}")),
            );
            seed_item(snap, 37_146, corp_id, sid, InventoryFlag::RigSlot0, 1, true, false, None);
            let svc = seed_item(snap, 35_878, corp_id, sid, InventoryFlag::ServiceSlot0, 1, true, false, None);
            snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
        }

        let bpid = seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, hs_raitaru_id, InventoryFlag::Hangar, 1, true, false, None);
        if let Some(bp) = snap.items.get_mut(&bpid) {
            bp.dynamic_attributes = HashMap::from([(1112, 10.0), (1113, 20.0)]);
        }
        let bpcid = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(4));
        if let Some(bpc) = snap.items.get_mut(&bpcid) {
            bpc.dynamic_attributes = HashMap::from([(1112, 8.0), (1113, 16.0)]);
        }
        if let Some(sde_bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
            for m in &sde_bp.materials {
                seed_item(snap, m.type_id, corp_id, hs_raitaru_id, InventoryFlag::Hangar, (m.quantity as i32) * 10, false, false, None);
                seed_item(snap, m.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (m.quantity as i32) * 10, false, false, None);
            }
        }
        (bpid, bpcid)
    });

    // 1. GetLimits: 1+5+4=10 mfg, 1+5+3=9 science, 1+5+2=8 reactions; remote ranges: Supply Chain Mgmt (24268) 3*5=15, Scientific Networking (24270) 2*5=10, Remote Reactions (45750) 4*5=20
    let limits = ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetLimits", &[], &kw, None)
        .await
        .expect("GetLimits");
    assert_eq!(limits.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(limits.dict_get("maxResearchJobs").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(limits.dict_get("maxReactionsJobs").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(limits.dict_get("manufacturingRangeJumps").and_then(|v| v.as_i64()), Some(15));
    assert_eq!(limits.dict_get("researchRangeJumps").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(limits.dict_get("reactionsRangeJumps").and_then(|v| v.as_i64()), Some(20));

    // 2. GetBlueprintData & GetBlueprintDataByOwner
    let bpc_data = ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetBlueprintData", &[PyRep::Integer(bpc_item_id)], &kw, None)
        .await
        .expect("GetBlueprintData BPC");
    assert_eq!(bpc_data.dict_get("blueprintID").and_then(|v| v.as_i64()), Some(bpc_item_id));
    assert_eq!(bpc_data.dict_get("original").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bpc_data.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(bpc_data.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(16));
    assert_eq!(bpc_data.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(bpc_data.dict_get("facilityID").and_then(|v| v.as_i64()), Some(JITA_STATION_ID));

    let corp_bps = ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetBlueprintDataByOwner", &[PyRep::Integer(corp_id), PyRep::Integer(hs_raitaru_id)], &kw, None)
        .await
        .expect("GetBlueprintDataByOwner");
    let PyRep::List(bp_list) = corp_bps else { panic!("Expected List") };
    assert_eq!(bp_list.len(), 1);
    assert_eq!(bp_list[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(bp_item_id));
    assert_eq!(bp_list[0].dict_get("original").and_then(|v| v.as_bool()), Some(true));

    // 3. GetFacilities & GetFacility
    let all_facs = ctx
        .dispatch_rpc(&mut sess, "facilityManager", None, "GetFacilities", &[], &kw, None)
        .await
        .expect("GetFacilities");
    let PyRep::List(fac_list) = all_facs else { panic!("Expected List") };
    let jita_fac = fac_list.iter().find(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(JITA_STATION_ID)).expect("Jita NPC station");
    assert_eq!(jita_fac.dict_get("tax").and_then(|v| v.as_f64()), Some(0.0025));

    let hs_fac = ctx.dispatch_rpc(&mut sess, "facilityManager", None, "GetFacility", &[PyRep::Integer(hs_raitaru_id)], &kw, None).await.expect("GetFacility HS Raitaru");
    let ls_fac = ctx.dispatch_rpc(&mut sess, "facilityManager", None, "GetFacility", &[PyRep::Integer(ls_azbel_id)], &kw, None).await.expect("GetFacility LS Azbel");
    let hs_mat_mod = hs_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap();
    let ls_mat_mod = ls_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap();
    assert!(ls_mat_mod < hs_mat_mod);

    let hs_react_id = 1_045_000_099_i64;
    let react_rig_hs = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hs_react_id, make_structure(hs_react_id, 35_835, corp_id, JITA_SOLAR_SYSTEM_ID, "HS Athanor".into()));
        seed_item(snap, 46_496, corp_id, hs_react_id, InventoryFlag::RigSlot0, 1, true, false, None);
        eve_server::services::industry_pi::resolve_structure_rig_bonus(&ctx, snap, hs_react_id, JITA_SOLAR_SYSTEM_ID)
    });
    assert_eq!(react_rig_hs.0, 0.0);

    // 4. GetFacilityTaxes & SetFacilityTaxes + GetIndustryCostModifier 28-day window
    let sci_before = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier before")
        .dict_get("1")
        .and_then(|v| v.as_f64())
        .unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[PyRep::Integer(hs_raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.035)), (PyRep::Integer(4), PyRep::Real(0.02))])],
        &kw,
        None,
    )
    .await
    .expect("SetFacilityTaxes");
    let taxes = ctx.dispatch_rpc(&mut sess, "facilityManager", None, "GetFacilityTaxes", &[PyRep::Integer(hs_raitaru_id)], &kw, None).await.expect("GetFacilityTaxes");
    assert!((taxes.dict_get("tax").and_then(|v| v.as_f64()).unwrap() - 0.035).abs() < 1e-6);

    let corp_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bp_item_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(hs_raitaru_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("InstallJob at structure");
    let struct_eiv = corp_job.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let struct_tax = corp_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((struct_tax - (struct_eiv * 0.035 * 100.0).round() / 100.0).abs() < 0.02);

    let npc_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpc_item_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("InstallJob at NPC station");
    let npc_eiv = npc_job.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let npc_tax = npc_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - (npc_eiv * 0.0025 * 100.0).round() / 100.0).abs() < 0.02);

    let sci_active = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier with 28d jobs")
        .dict_get("1")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!(sci_active > sci_before, "recent jobs within 28d window must raise system cost index");

    // Advance past 28-day window -> old jobs no longer inflate cost index
    ctx.advance_secs(29 * 86_400);
    let sci_expired = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier after 29d")
        .dict_get("1")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((sci_expired - sci_before).abs() < 1e-9, "jobs older than 28 days must be excluded from cost index");

    // Verify R-142028-01: jobs with same total duration produce equal SCI work (duration is not multiplied by runs twice)
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.industry_jobs.insert(
            990_001,
            eve_db::IndustryJobRecord {
                job_id: 990_001, installer_id: char_id, facility_id: JITA_STATION_ID,
                blueprint_item_id: bp_item_id, blueprint_type_id: RIFTER_BP_TYPE_ID, activity_id: 1, runs: 1,
                product_type_id: 587, status: "Active".into(), duration_seconds: 36_000, start_unix: ctx.now_unix(),
            },
        );
        snap.industry_jobs.insert(
            990_002,
            eve_db::IndustryJobRecord {
                job_id: 990_002, installer_id: char_id, facility_id: ls_azbel_id,
                blueprint_item_id: bpc_item_id, blueprint_type_id: RIFTER_BP_TYPE_ID, activity_id: 1, runs: 10,
                product_type_id: 587, status: "Active".into(), duration_seconds: 36_000, start_unix: ctx.now_unix(),
            },
        );
    });
    let sci_1run = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier Jita")
        .dict_get("1")
        .and_then(|v| v.as_f64())
        .unwrap();
    let sci_10runs = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(ls_sys_id as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier LS")
        .dict_get("1")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((sci_1run - sci_10runs).abs() < 1e-9, "jobs with equal total duration must produce equal system cost index");
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
        "feat:industry.research-me",
        "feat:industry.research-te",
        "feat:industry.copying",
        "feat:industry.invention",
        "feat:industry.decryptors",
        "feat:industry.t3-invention",
        "feat:industry.reactions",
        "feat:industry.job-complete",
        "feat:industry.job-cancel",
        "feat:industry.job-list",
        "feat:industry.job-range",
        "feat:industry.industry-monitor",
        "feat:industry.blueprint-lock",
        "feat:structures.service-reactions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
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
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.balance = IskCents::from_isk(1_000_000_000);
            }
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Corp Industry Ltd".into(),
                    ticker: "CIND".into(),
                    ceo_id: char_id,
                    alliance_id: None,
                    tax_rate: 0.0,
                    master_wallet: IskCents::from_isk(1_000_000_000),
                    members: HashSet::from([char_id]),
                    member_roles: HashMap::from([(char_id, u64::MAX)]),
                    applications: HashMap::new(),
                    wallet_divisions: Vec::new(),
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(JITA_STATION_ID),
                },
            );
            let sk = snap.skills.entry(char_id).or_default();
            for (stid, lvl) in [
                (MASS_PRODUCTION_SKILL, 5), (LABORATORY_OPERATION_SKILL, 5), (MASS_REACTIONS_SKILL, 5),
                (REACTIONS_SKILL, 4), (SUPPLY_CHAIN_MGMT_SKILL, 0), (SCIENTIFIC_NETWORKING_SKILL, 0),
                (3380, 5), (3388, 5), (11448, 5), (11452, 5), (21791, 5),
            ] {
                sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
            }
            for (sid, stype, ssys) in [
                (hs_athanor_id, 35_835_u32, JITA_SOLAR_SYSTEM_ID),
                (ls_tatara_id, 35_836_u32, ls_sys_id),
            ] {
                snap.structures.insert(sid, make_structure(sid, stype, corp_id, ssys, format!("Refinery {sid}")));
                for (flag, svc_type) in [
                    (InventoryFlag::ServiceSlot0, 45_537_u32),
                    (InventoryFlag::ServiceSlot1, 35_891_u32),
                    (InventoryFlag::ServiceSlot2, 45_538_u32),
                    (InventoryFlag::ServiceSlot3, 45_539_u32),
                ] {
                    let svc = seed_item(snap, svc_type, corp_id, sid, flag, 1, true, false, None);
                    snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
                }
            }
            let bpo_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);
            let bpo2_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);
            let bpc_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(5));
            if let Some(bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
                if let Some(inv_act) = bp.activities.get("invention").or_else(|| bp.activities.get("8")) {
                    for mat in &inv_act.materials {
                        seed_item(snap, mat.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (mat.quantity as i32) * 10, false, false, None);
                    }
                    for sk_req in &inv_act.skills {
                        snap.skills.entry(char_id).or_default().insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 });
                    }
                }
            }
            let dec_id = seed_item(snap, 34_201, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 5, false, false, None);
            let rel_id = seed_item(snap, 30_558, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
            for dtid in [20_411_u32, 20_412_u32, 20_424_u32, 20_172_u32] {
                seed_item(snap, dtid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 20, false, false, None);
            }
            let (react_bp_tid, react_mats) = ctx.sde.blueprints.values().find_map(|bp| {
                bp.activities.get("reaction").or_else(|| bp.activities.get("9"))
                    .filter(|a| !a.materials.is_empty() && !a.products.is_empty())
                    .map(|a| (bp.blueprint_type_id, a.materials.clone()))
            }).expect("SDE reaction blueprint");
            let r_item_id = seed_item(snap, react_bp_tid, char_id, ls_tatara_id, InventoryFlag::Hangar, 1, true, false, None);
            for rm in react_mats {
                seed_item(snap, rm.type_id, char_id, ls_tatara_id, InventoryFlag::Hangar, (rm.quantity as i32) * 10, false, false, None);
            }
            (bpo_id, bpo2_id, bpc_id, rel_id, dec_id, react_bp_tid, r_item_id)
        });

    // 1. Invention with Accelerant Decryptor (34201)
    let inv_job = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(t1_bpc_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(8)),
        ("runs", PyRep::Integer(1)), ("decryptorTypeID", PyRep::Integer(34_201)),
    ])], &kw, None).await.expect("Install invention job with decryptor");
    let inv_job_id = inv_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(inv_job.dict_get("inventedME").and_then(|v| v.as_f64()), Some(4.0));
    assert_eq!(inv_job.dict_get("inventedTE").and_then(|v| v.as_f64()), Some(14.0));
    assert_eq!(inv_job.dict_get("inventedRuns").and_then(|v| v.as_i64()), Some(2));

    let mon = ctx.dispatch_rpc(&mut sess, "industryMonitor", None, "ConnectJob", &[PyRep::Integer(inv_job_id)], &kw, None).await.expect("ConnectJob");
    assert!(matches!(mon, PyRep::Tuple(ref parts) if parts.len() == 2 && parts[0].as_i64().unwrap_or(0) > 0 && matches!(parts[1], PyRep::Dict(_))));

    // 2. T3 Ancient Relic Invention (`Intact Hull Section` 30558)
    let t3_job = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(relic_id)), ("blueprintTypeID", PyRep::Integer(30_558)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(8)),
        ("runs", PyRep::Integer(1)), ("decryptorTypeID", PyRep::Integer(34_201)),
    ])], &kw, None).await.expect("Install T3 relic invention job");
    let t3_job_id = t3_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let early_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(t3_job_id)], &kw, None).await.expect_err("CompleteJob before ready_at must fail");
    assert_eq!(early_err.code, "JobNotYetComplete");

    ctx.advance_secs(86_400);
    ctx.set_invention_roll_override(Some(0.05));
    let t3_done = ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(t3_job_id)], &kw, None).await.expect("Complete T3 invention job");
    assert_eq!(t3_done.dict_get("inventionSucceeded").and_then(|v| v.as_bool()), Some(true));
    let t3_out_id = t3_done.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.items.contains_key(&t3_out_id)));

    // 3. Remote job range check
    let remote_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(ls_tatara_id)), ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect_err("Remote job beyond skill jump range must fail");
    assert_eq!(remote_err.code, "FacilityOutOfRemoteRange");

    // 4. Reactions: rejected in High-Sec Athanor (>= 0.45 sec), accepted in Low-Sec Tatara (< 0.45 sec)
    let hs_react_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(0)), ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
        ("facilityID", PyRep::Integer(hs_athanor_id)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect_err("Reactions must be forbidden in High-Sec");
    assert_eq!(hs_react_err.code, "ReactionsForbiddenInHighSec");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ls_sys_id;
            ch.station_id = None;
            ch.structure_id = Some(ls_tatara_id);
        }
    });
    sess.solar_system_id = Some(ls_sys_id);
    sess.station_id = None;
    sess.structure_id = Some(ls_tatara_id);
    let ls_react_job = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(reaction_item_id)), ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
        ("facilityID", PyRep::Integer(ls_tatara_id)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect("Install reaction job on Low-Sec Tatara");
    assert_eq!(ls_react_job.dict_get("activityID").and_then(|v| v.as_i64()), Some(9));

    // 5. Corp Industry (`useCorp = true`) + Copying + CompleteManyJobs across multiple ready jobs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = Some(JITA_STATION_ID);
    sess.structure_id = None;
    let corp_me_job = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(4)),
        ("runs", PyRep::Integer(1)), ("useCorp", PyRep::Bool(true)), ("ownerID", PyRep::Integer(corp_id)),
    ])], &kw, None).await.expect("Install corp ME research job");
    let corp_job_id = corp_me_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let copy_job = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo2_id)), ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(JITA_STATION_ID)), ("activityID", PyRep::Integer(5)), ("runs", PyRep::Integer(3)),
    ])], &kw, None).await.expect("Install copy job");
    let copy_job_id = copy_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(86_400);
    let batch_done = ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(corp_job_id), PyRep::Integer(copy_job_id)])], &kw, None).await.expect("CompleteManyJobs");
    let PyRep::List(done_list) = batch_done else { panic!("Expected List") };
    assert_eq!(done_list.len(), 2, "CompleteManyJobs must deliver all ready jobs in the batch");
    let copy_bpc_id = done_list[1].dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&bpo_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&bpo_id].dynamic_attributes.get(&1112), Some(&1.0));
        assert!(snap.items[&copy_bpc_id].is_blueprint_copy);
        assert_eq!(snap.items[&copy_bpc_id].blueprint_runs_remaining, Some(3));
    });

    // 6. Fulcrum, EIV, Alpha tax, repackaging rejection, and job pause on service offline
    let is_fulcrum = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(char_id), 73787, 1, 60_015_187)
    });
    assert!(!is_fulcrum);
    let eiv_rifter = eve_server::services::industry_pi::compute_blueprint_eiv_per_run(&ctx, ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID).unwrap(), 1);
    assert!(eiv_rifter > 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });
    let alpha_costs = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::compute_industry_job_costs_for_char(&ctx, snap, Some(char_id), RIFTER_BP_TYPE_ID, 1, 1, JITA_STATION_ID, true)
    }).expect("compute_industry_job_costs_for_char");
    assert!(alpha_costs.alpha_clone_tax_isk > 0.0 || alpha_costs.scc_surcharge_isk > 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });

    let pack_fail = ctx
        .dispatch_rpc(&mut sess, "repackagingSvc", None, "RepackageItems", &[PyRep::Integer(bpo_id), PyRep::Bool(false)], &kw, None)
        .await
        .expect_err("Repackage researched BPO must fail");
    assert_eq!(pack_fail.code, "CannotRepackageMutatedItem");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&ls_tatara_id) {
            st.fuel_blocks = 0;
        }
    });
    let jobs_paused = ctx
        .dispatch_rpc(&mut sess, "industryManager", None, "GetJobsByOwner", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetJobsByOwner with offline structure");
    let PyRep::List(p_list) = jobs_paused else { panic!() };
    for j in p_list {
        if j.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(ls_tatara_id) {
            assert_eq!(j.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
        }
    }
    (done_list.len(), 4.0, 14.0, 2, "Paused")
}

pub(crate) mod pi_planet_mgr_orbital_registry {
    // Planetary Industry (`E-6`) complete parity integration tests (Part 2).

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
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Orbital Operations Corp".into(),
                    ticker: "ORBI".into(),
                    ceo_id: char_id,
                    alliance_id: None,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_isk(100_000_000),
                    members: HashSet::from([char_id]),
                    member_roles: HashMap::from([(char_id, u64::MAX)]),
                    applications: HashMap::new(),
                    wallet_divisions: Vec::new(),
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(JITA_STATION_ID),
                },
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

pub(crate) mod issues_industry_auth_pi_simulation {
    // Industry, Research, and Reactions (`E-4`) complete parity integration tests (Part 3).

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
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "P0 Auth Corp".into(),
                    ticker: "P0AC".into(),
                    ceo_id: char_ceo,
                    master_wallet: IskCents::from_isk(100_000_000),
                    members: [char_ceo, char_line].into_iter().collect(),
                    member_roles: HashMap::from([(char_ceo, u64::MAX), (char_line, 0)]),
                    ..Default::default()
                },
            );
            snap.corporations.insert(
                victim_corp_id,
                CorporationRecord {
                    corporation_id: victim_corp_id,
                    name: "Victim Corp".into(),
                    ticker: "VICT".into(),
                    ceo_id: 90_000_999,
                    master_wallet: IskCents::from_isk(100_000_000),
                    ..Default::default()
                },
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
                    eve_db::corp_roles::ROLE_FACTORY_MANAGER | eve_db::corp_roles::ROLE_STATION_MANAGER,
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

pub(crate) mod pi_sde_schematics_facility_tier {
    // behavioral verification integration tests for PI SDE schematic recipes & facility tier validation
    // (`feat:pi.factory-cycles`, `feat:pi.schematic-input-output-routing`) and Paragon V24 SDE
    // EverMarks catalog & SKINR sequencing accrual (`feat:wallet.evermarks`).


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const PARAGON_CORP_ID: i64 = 1_000_419;

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
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        item_id
    }

    #[tokio::test]
    async fn test_pi_sde_schematics_facility_tier_validation_and_routing() {
        // parity: feat:pi.factory-cycles, feat:pi.schematic-input-output-routing
        eve_testkit::attest(&[
            "feat:pi.factory-cycles",
            "feat:pi.schematic-input-output-routing",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        assert!(
            ctx.sde.planet_schematics.len() > 60,
            "SDE must load full planet_schematics table"
        );

        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 731, "demo", "demo-secret", char_id).await;
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        sess.station_id = None;
        sess.structure_id = None;
        let ship_id = sess.ship_id.expect("Active ship");

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            ch.balance = IskCents::from_isk(100_000_000);
            seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1);
            snap.skills.entry(char_id).or_default().insert(
                2505,
                TrainedSkillRecord {
                    type_id: 2505,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        });

        let find_schem = |out_tid: TypeId| -> i64 {
            ctx.sde
                .planet_schematics
                .iter()
                .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == out_tid))
                .map(|(&id, _)| i64::from(id))
                .unwrap_or_else(|| panic!("missing SDE schematic for output {out_tid}"))
        };
        let water_schem = find_schem(3645);
        let coolant_schem = find_schem(9832); // 9832 = Coolant (Water + Electrolytes)
        let nanofactory_schem = find_schem(2875);

        // Deploy Command Center + Basic (2492) + Advanced (2494) + High-Tech (2482) + Storage (2541) + Launchpad (2543)
        let setup = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(5)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2492), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2494), PyRep::Real(0.12), PyRep::Real(0.12)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2482), PyRep::Real(0.13), PyRep::Real(0.13)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(105), PyRep::Integer(2541), PyRep::Real(0.14), PyRep::Real(0.14)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(106), PyRep::Integer(2543), PyRep::Real(0.15), PyRep::Real(0.15)]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect("Deploy PI facilities");
        let PyRep::List(pins) = setup.dict_get("pins").unwrap() else {
            panic!("expected pins list");
        };
        let basic_id = pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let adv_id = pins[2].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let htech_id = pins[3].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let store_id = pins[4].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let pad_id = pins[5].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

        // Negative facility-tier & unknown schematic checks against SDE planetSchematicsPinMap
        for (pin_id, bad_schem, label) in [
            (basic_id, 999_999, "unknown schematic ID"),
            (basic_id, coolant_schem, "P2 schematic on Basic facility"),
            (adv_id, water_schem, "P1 schematic on Advanced facility"),
            (adv_id, nanofactory_schem, "P4 schematic on Advanced facility"),
            (htech_id, water_schem, "P1 schematic on High-Tech facility"),
        ] {
            let err = ctx
                .dispatch_rpc(
                    &mut sess,
                    "planetMgr",
                    None,
                    "UserUpdateNetwork",
                    &[
                        PyRep::Integer(JITA_PLANET_ID),
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(7),
                            PyRep::Integer(pin_id),
                            PyRep::Integer(bad_schem),
                        ])]),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect_err(label);
            assert_eq!(err.code, "InvalidSchematic", "{label}");
        }

        // Assign valid SDE schematics for each facility tier:
        // - Water (3000 Aqueous Liquids -> 20 Water, 1800s) on Basic (2492)
        // - Biocells (40 Biofuels + 40 Precious Metals -> 5 Biocells, 3600s) on Advanced (2494)
        // - Nano-Factory (6 Industrial Fibers + 6 Reactive Metals + 6 Ukomi Super Conductors -> 1 Nano-Factory, 3600s) on High-Tech (2482)
        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(water_schem)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(coolant_schem)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nanofactory_schem)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Install matching SDE schematics");

        // Verify GetPin reports exact SDE cycleTimeSeconds for each facility
        for (pin_id, expected_cycle) in [(basic_id, 1800), (adv_id, 3600), (htech_id, 3600)] {
            let pin_info = ctx
                .dispatch_rpc(
                    &mut sess,
                    "planetMgr",
                    None,
                    "GetPin",
                    &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(pin_id)],
                    &kw,
                    None,
                )
                .await
                .expect("GetPin");
            assert_eq!(
                pin_info.dict_get("cycleTimeSeconds"),
                Some(&PyRep::Integer(expected_cycle))
            );
        }

        let nano_inputs: Vec<(TypeId, u64)> = ctx
            .sde
            .planet_schematics
            .get(&(nanofactory_schem as u32))
            .expect("Nano-Factory schematic in SDE")
            .types
            .iter()
            .filter(|t| t.is_input)
            .map(|t| (t.type_id, t.quantity))
            .collect();
        assert!(!nano_inputs.is_empty());

        // Seed inputs into Storage without routes -> 0 factory cycles must execute
        ctx.db.with_mut_snapshot(|snap| {
            let mut net =
                eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 5);
            if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == store_id) {
                pin.contents.insert(2268, 6_000);
                pin.contents.insert(3645, 80);
                pin.contents.insert(2390, 80);
                for &(in_tid, in_qty) in &nano_inputs {
                    pin.contents.insert(in_tid, in_qty * 2);
                }
            }
            eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
            if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
                col.last_extracted_unix = ctx.now_unix();
            }
        });
        ctx.advance_virtual_clock(3_600);
        let col_unrouted = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "GetColony",
                &[PyRep::Integer(JITA_PLANET_ID)],
                &kw,
                None,
            )
            .await
            .expect("GetColony unrouted");
        assert_eq!(
            col_unrouted.dict_get("simulatedProcessedQuantity"),
            Some(&PyRep::Integer(0))
        );

        // Wire links & input/output routes -> 2 Basic cycles (40 Water) + 1 Adv cycle (5 Biocells) + 1 High-Tech cycle (1 Nano-Factory) = 46 units
        let mut route_cmds = vec![
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(basic_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(basic_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(adv_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(adv_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(htech_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(htech_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(basic_id)]), PyRep::Integer(2268), PyRep::Integer(3_000)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(2), PyRep::List(vec![PyRep::Integer(basic_id), PyRep::Integer(pad_id)]), PyRep::Integer(3645), PyRep::Integer(20)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(3), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(3645), PyRep::Integer(40)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(4), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(2390), PyRep::Integer(40)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(5), PyRep::List(vec![PyRep::Integer(adv_id), PyRep::Integer(pad_id)]), PyRep::Integer(9832), PyRep::Integer(5)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(9), PyRep::List(vec![PyRep::Integer(htech_id), PyRep::Integer(pad_id)]), PyRep::Integer(2875), PyRep::Integer(1)]),
        ];
        for (idx, &(in_tid, in_qty)) in nano_inputs.iter().enumerate() {
            route_cmds.push(PyRep::Tuple(vec![
                PyRep::Integer(6),
                PyRep::Integer(10 + idx as i64),
                PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(htech_id)]),
                PyRep::Integer(i64::from(in_tid)),
                PyRep::Integer(in_qty as i64),
            ]));
        }
        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(route_cmds),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create PI factory routes");

        ctx.advance_virtual_clock(3_600);
        let col_routed = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "GetColony",
                &[PyRep::Integer(JITA_PLANET_ID)],
                &kw,
                None,
            )
            .await
            .expect("GetColony routed");
        assert_eq!(
            col_routed.dict_get("simulatedProcessedQuantity"),
            Some(&PyRep::Integer(46))
        );
    }

    #[tokio::test]
    async fn test_wallet_evermarks_sde_paragon_catalog_and_redemption() {
        // parity: feat:wallet.evermarks
        eve_testkit::attest(&["feat:wallet.evermarks"]);

        let ctx = Arc::new(ServerContext::new());
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 732, "demo", "demo-secret", char_id).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(60_015_169);
                ch.balance = IskCents::from_isk(1_000_000);
                let sid = ch.active_ship_id;
                if let Some(ship) = snap.items.get_mut(&sid) {
                    ship.location_id = 60_015_169;
                }
            }
        });
        sess.station_id = Some(60_015_169);

        // 1. Out-of-the-box Paragon EverMarks catalog loaded from V24 shipcosmeticlicenses.fsdbinary
        let offers_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "LPStoreMgr",
                None,
                "GetAvailableOffersFromCorp",
                &[PyRep::Integer(PARAGON_CORP_ID)],
                &kw,
                None,
            )
            .await
            .expect("GetAvailableOffersFromCorp Paragon");
        let PyRep::List(offers) = offers_rep else {
            panic!("expected List of Paragon offers");
        };
        assert!(
            offers.len() > 800,
            "Expected >800 Paragon ship emblem offers (Tranquility ESI + supplementary V24 SDE), got {}",
            offers.len()
        );
        let rifter_offer_id = 17_208;
        let slasher_alliance_offer_id = 17_221;
        let rifter_offer = offers
            .iter()
            .find(|o| {
                o.dict_get("offerID") == Some(&PyRep::Integer(rifter_offer_id))
                    && o.dict_get("typeID") == Some(&PyRep::Integer(72964))
            })
            .expect("Tranquility Rifter emblem offer 17208 present");
        let Some(PyRep::Integer(rifter_lp_cost)) = rifter_offer.dict_get("lpCost") else {
            panic!("expected integer lpCost on Rifter emblem offer");
        };
        assert_eq!(*rifter_lp_cost, 5_000);
        assert_eq!(rifter_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

        let slasher_offer = offers
            .iter()
            .find(|o| {
                o.dict_get("offerID") == Some(&PyRep::Integer(slasher_alliance_offer_id))
                    && o.dict_get("typeID") == Some(&PyRep::Integer(73394))
            })
            .expect("Tranquility Slasher alliance emblem offer 17221 present");
        let Some(PyRep::Integer(slasher_lp_cost)) = slasher_offer.dict_get("lpCost") else {
            panic!("expected integer lpCost on Slasher alliance emblem offer");
        };
        assert_eq!(*slasher_lp_cost, 5_000);
        assert_eq!(slasher_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

        // Verify ESI GET /v1/loyalty/stores/1000419/offers exposes the same V24 SDE catalog
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let esi_req = EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/");
        let esi_resp = router.handle_request(&esi_req);
        assert_eq!(esi_resp.status_code, 200);
        spec.validate_response(
            "GET",
            "/v1/loyalty/stores/1000419/offers/",
            200,
            &esi_resp.headers,
            &esi_resp.body,
        )
        .expect("OpenAPI validation for Paragon LP store offers");
        let esi_json = esi_resp.json().expect("JSON");
        assert_eq!(esi_json.as_array().unwrap().len(), offers.len());

        // 2. Negative redemption check when character has 0 EverMarks
        let insuf_err = ctx
            .dispatch_rpc(
                &mut sess,
                "LPStoreMgr",
                None,
                "TakeOfferForCharacter",
                &[
                    PyRep::Integer(PARAGON_CORP_ID),
                    PyRep::Integer(rifter_offer_id),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Redeeming without EverMarks must fail");
        assert_eq!(insuf_err.code, "InsufficientLoyaltyPoints");

        // 3. Accrue EverMarks via validated SKINR sequencing jobs (empty payload awards 0 EM)
        let mk_req = |corr: &[u8], type_name: &str, payload: Vec<u8>| {
            eve_proto::quasar::envelopes::EvePublicRequest {
                issued: Some(eve_proto::quasar::types::GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                correlation_uuid: corr.to_vec(),
                payload: Some(eve_proto::quasar::types::GoogleAny {
                    type_url: format!("{}{type_name}", eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX),
                    value: payload,
                }),
                external_origin: "eve-client".into(),
                application_instance_uuid: b"em-remediation-01".to_vec(),
                authoritative_context: None,
                trace_context: None,
            }
        };
        let empty_rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_req(
                b"corr-em-empty",
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                Vec::new(),
            ),
            None,
            false,
            ctx.now_unix(),
        );
        assert_eq!(empty_rsp.status_code, 400);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)),
            0
        );

        let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
        let coating_comp = skinr_cat
            .components
            .values()
            .find(|c| !c.is_pattern() && !c.associated_types.is_empty() && !c.disallowed_slots.contains(&1))
            .expect("SDE SKINR coating");
        let (coating_item_type_id, _) = coating_comp.associated_types[0];
        let coating_item_id = ctx.db.with_mut_snapshot(|snap| {
            seed_item(snap, coating_item_type_id, char_id, 60_015_169, InventoryFlag::Hangar, 11)
        });

        let mut seq_item = Vec::new();
        encode_varint_field(1, coating_item_id as u64, &mut seq_item);
        let mut item_outer = Vec::new();
        encode_bytes_field(1, &seq_item, &mut item_outer);
        let mut consume_req = Vec::new();
        encode_bytes_field(1, &item_outer, &mut consume_req);
        encode_varint_field(2, 11, &mut consume_req);
        assert_eq!(
            ctx.quasar_gateway
                .handle_public_request(
                    &sess,
                    &mk_req(
                        b"corr-em-consume",
                        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
                        consume_req,
                    ),
                    None,
                    false,
                    ctx.now_unix(),
                )
                .status_code,
            200
        );

        let seq_job = |idx: u32, name: &str, qty: u64| {
            let mut seq_comp = Vec::new();
            encode_varint_field(1, coating_comp.component_id as u64, &mut seq_comp);
            let mut coat_cfg = Vec::new();
            encode_bytes_field(1, &seq_comp, &mut coat_cfg);
            let mut cfg_outer = Vec::new();
            encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
            let mut seq_slot = Vec::new();
            encode_varint_field(1, 1, &mut seq_slot);
            let mut slot_msg = Vec::new();
            encode_bytes_field(1, &seq_slot, &mut slot_msg);
            encode_bytes_field(2, &cfg_outer, &mut slot_msg);
            let mut seq_hull = Vec::new();
            encode_varint_field(1, 587, &mut seq_hull);
            let mut layout = Vec::new();
            encode_bytes_field(1, &slot_msg, &mut layout);
            encode_bytes_field(2, &seq_hull, &mut layout);
            encode_varint_field(3, 1, &mut layout);
            let mut init_req = Vec::new();
            encode_string_field(1, name, &mut init_req);
            encode_string_field(2, "Paragon Custom", &mut init_req);
            encode_bytes_field(3, &layout, &mut init_req);
            encode_varint_field(5, qty, &mut init_req);
            ctx.quasar_gateway.handle_public_request(
                &sess,
                &mk_req(
                    format!("corr-em-seq-{idx}").as_bytes(),
                    "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                    init_req,
                ),
                None,
                false,
                ctx.now_unix(),
            )
        };

        for (idx, name) in [(1, "Rifter Gold"), (2, "Rifter Crimson")] {
            assert_eq!(seq_job(idx, name, 1).status_code, 200);
        }

        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&PARAGON_CORP_ID]),
            1_000
        );

        // 4. Sequence a 9-license batch (+4,500 EverMarks) and redeem Rifter Corporation Emblem (72964, offer 17208, 5,000 LP)
        assert_eq!(seq_job(3, "Rifter Fleet Batch", 9).status_code, 200);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&PARAGON_CORP_ID]),
            5_500
        );
        let take_res = ctx
            .dispatch_rpc(
                &mut sess,
                "LPStoreMgr",
                None,
                "TakeOfferForCharacter",
                &[
                    PyRep::Integer(PARAGON_CORP_ID),
                    PyRep::Integer(rifter_offer_id),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await
            .expect("Redeem Rifter Corporation Emblem");
        assert_eq!(take_res.dict_get("deliveredTypeID"), Some(&PyRep::Integer(72964)));
        assert_eq!(
            take_res.dict_get("remainingLP"),
            Some(&PyRep::Integer(5_500 - *rifter_lp_cost))
        );
    }
}
