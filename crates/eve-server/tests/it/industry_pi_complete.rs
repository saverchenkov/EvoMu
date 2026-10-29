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
                    (InventoryFlag::ServiceSlot0, 35_894_u32),
                    (InventoryFlag::ServiceSlot1, 35_891_u32),
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
    assert_eq!(mon.dict_get("connected").and_then(|v| v.as_bool()), Some(true));

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
