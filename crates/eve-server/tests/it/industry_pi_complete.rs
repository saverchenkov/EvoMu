//! Industry, Research, Reactions (`E-4`), and Planetary Industry (`E-6`) complete parity integration tests.
//!



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
const JITA_PLANET_ID: i64 = 40_009_082;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const MASS_PRODUCTION_SKILL: TypeId = 3387;
const ADV_MASS_PRODUCTION_SKILL: TypeId = 24625;
const LABORATORY_OPERATION_SKILL: TypeId = 3406;
const ADV_LABORATORY_OPERATION_SKILL: TypeId = 24624;
const MASS_REACTIONS_SKILL: TypeId = 45748;
const ADV_MASS_REACTIONS_SKILL: TypeId = 45749;
const SUPPLY_CHAIN_MGMT_SKILL: TypeId = 24270;
const REACTIONS_SKILL: TypeId = 45746;
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
        last_damage_unix: 0,
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
    eve_testkit::attest(&["feat:industry.blueprint-data", "feat:industry.blueprint-limits", "feat:industry.facilities", "feat:industry.facility-tax", "feat:industry.cost-index", "feat:industry.structure-rigs", "feat:industry.structure-standup-rig-security-multipliers", "feat:industry.job-slots", "feat:industry.corp-industry"]);
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

    // Seed corporation, skills, a Raitaru in Jita (High-Sec, 1.0x rig multiplier) with a Standup M-Set Basic Material Efficiency I rig (37146),
    // and an Azbel in Low-Sec (1.9x rig multiplier) with the same rig.
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

        for (sid, stype, ssys) in [
            (hs_raitaru_id, 35_825_u32, JITA_SOLAR_SYSTEM_ID),
            (ls_azbel_id, 35_826_u32, ls_sys_id),
        ] {
            snap.structures.insert(
                sid,
                make_structure(sid, stype, corp_id, ssys, format!("Engineering Complex {sid}")),
            );
            // Fit Standup M-Set ME rig (37146) in RigSlot0
            seed_item(snap, 37_146, corp_id, sid, InventoryFlag::RigSlot0, 1, true, false, None);
        }

        // Seed a Rifter BPO owned by corp at hs_raitaru_id and a Rifter BPC owned by char at JITA_STATION_ID
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

    // 1. GetLimits: 1 + 5 + 4 = 10 mfg, 1 + 5 + 3 = 9 science, 1 + 5 + 2 = 8 reactions
    let limits = ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetLimits", &[], &kw, None)
        .await
        .expect("GetLimits");
    assert_eq!(limits.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(limits.dict_get("maxResearchJobs").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(limits.dict_get("maxReactionsJobs").and_then(|v| v.as_i64()), Some(8));

    // 2. GetBlueprintData & GetBlueprintDataByOwner return BPO/BPC, ME, TE, runs remaining, and location
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
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(corp_id), PyRep::Integer(hs_raitaru_id)],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintDataByOwner");
    let PyRep::List(bp_list) = corp_bps else {
        panic!("Expected List of blueprints from GetBlueprintDataByOwner");
    };
    assert_eq!(bp_list.len(), 1);
    assert_eq!(bp_list[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(bp_item_id));
    assert_eq!(bp_list[0].dict_get("original").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(bp_list[0].dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(bp_list[0].dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(bp_list[0].dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(bp_list[0].dict_get("facilityID").and_then(|v| v.as_i64()), Some(hs_raitaru_id));

    // 3. GetFacilities & GetFacility: NPC stations and structures with activities, bonuses, and taxes
    let all_facs = ctx
        .dispatch_rpc(&mut sess, "facilityManager", None, "GetFacilities", &[], &kw, None)
        .await
        .expect("GetFacilities");
    let PyRep::List(fac_list) = all_facs else { panic!("Expected List from GetFacilities") };
    let jita_fac = fac_list.iter().find(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(JITA_STATION_ID)).expect("Jita NPC station in GetFacilities");
    assert_eq!(jita_fac.dict_get("materialModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(jita_fac.dict_get("timeModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(jita_fac.dict_get("costModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(jita_fac.dict_get("tax").and_then(|v| v.as_f64()), Some(0.10));
    let PyRep::List(jita_acts) = jita_fac.dict_get("activities").expect("Jita activities") else { panic!("Expected List") };
    assert!(jita_acts.contains(&PyRep::Integer(1)) && jita_acts.contains(&PyRep::Integer(8)));
    assert!(fac_list.iter().any(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(hs_raitaru_id)));

    // Compare Structure Rig security multiplier: High-Sec Raitaru (1.0x rig) vs Low-Sec Azbel (1.9x rig)
    let hs_fac = ctx
        .dispatch_rpc(&mut sess, "facilityManager", None, "GetFacility", &[PyRep::Integer(hs_raitaru_id)], &kw, None)
        .await
        .expect("GetFacility HS Raitaru");
    let ls_fac = ctx
        .dispatch_rpc(&mut sess, "facilityManager", None, "GetFacility", &[PyRep::Integer(ls_azbel_id)], &kw, None)
        .await
        .expect("GetFacility LS Azbel");
    let hs_mat_mod = hs_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap();
    let ls_mat_mod = ls_fac.dict_get("materialModifier").and_then(|v| v.as_f64()).unwrap();
    assert!(
        ls_mat_mod < hs_mat_mod,
        "Low-sec 1.9x rig multiplier ({ls_mat_mod}) must yield stronger material reduction than High-sec 1.0x ({hs_mat_mod})"
    );

    // Standup reaction rigs give 0.0x bonus in high-sec
    let hs_react_id = 1_045_000_099_i64;
    let react_rig_hs = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hs_react_id,
            make_structure(hs_react_id, 35_835, corp_id, JITA_SOLAR_SYSTEM_ID, "HS Athanor".into()),
        );
        seed_item(snap, 46_496, corp_id, hs_react_id, InventoryFlag::RigSlot0, 1, true, false, None);
        eve_server::services::industry_pi::resolve_structure_rig_bonus(
            &ctx, snap, hs_react_id, JITA_SOLAR_SYSTEM_ID,
        )
    });
    assert_eq!(react_rig_hs.0, 0.0, "Reaction rigs give 0.0x bonus in high-sec");
    assert_eq!(react_rig_hs.2, 0.0, "Reaction rig security multiplier is 0.0 in high-sec");

    // 4. GetFacilityTaxes & SetFacilityTaxes per activity + charged on InstallJob at structure (3.5%) vs NPC station (10% fixed)
    ctx.dispatch_rpc(
        &mut sess,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[
            PyRep::Integer(hs_raitaru_id),
            PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.035)), (PyRep::Integer(4), PyRep::Real(0.02))]),
        ],
        &kw,
        None,
    )
    .await
    .expect("SetFacilityTaxes");
    let taxes = ctx
        .dispatch_rpc(&mut sess, "facilityManager", None, "GetFacilityTaxes", &[PyRep::Integer(hs_raitaru_id)], &kw, None)
        .await
        .expect("GetFacilityTaxes");
    assert!((taxes.dict_get("tax").and_then(|v| v.as_f64()).unwrap() - 0.035).abs() < 1e-6);
    assert!((taxes.dict_get("4").and_then(|v| v.as_f64()).unwrap() - 0.02).abs() < 1e-6);

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
        .expect("InstallJob at structure with 3.5% tax");
    let struct_base = corp_job.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let struct_tax = corp_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((struct_tax - (struct_base * 0.035 * 100.0).round() / 100.0).abs() < 0.02);

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
        .expect("InstallJob at NPC station with 10% fixed tax");
    let npc_base = npc_job.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let npc_tax = npc_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - (npc_base * 0.10 * 100.0).round() / 100.0).abs() < 0.02);

    let sci = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIndustryCostModifier", &[PyRep::Integer(JITA_SOLAR_SYSTEM_ID as i64)], &kw, None)
        .await
        .expect("GetIndustryCostModifier");
    assert!(sci.dict_get("1").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
}

#[tokio::test]
async fn test_industry_jobs_me_te_copy_invention_decryptors_t3_reactions_remote_and_corp() {
    // parity: feat:industry.research-me, feat:industry.research-te, feat:industry.copying, feat:industry.invention, feat:industry.decryptors, feat:industry.t3-invention, feat:industry.reactions, feat:industry.job-complete, feat:industry.job-cancel, feat:industry.job-list, feat:industry.job-range, feat:industry.industry-monitor, feat:industry.blueprint-lock, feat:structures.service-reactions, rpc:industryManager.CompleteJob, rpc:industryManager.CompleteManyJobs, rpc:industryManager.GetJobsByOwner, rpc:industryManager.InstallJob, rpc:industryMonitor.ConnectJob, rpc:repackagingSvc.RepackageItems
    eve_testkit::attest(&["feat:industry.research-me", "feat:industry.research-te", "feat:industry.copying", "feat:industry.invention", "feat:industry.decryptors", "feat:industry.t3-invention", "feat:industry.reactions", "feat:industry.job-complete", "feat:industry.job-cancel", "feat:industry.job-list", "feat:industry.job-range", "feat:industry.industry-monitor", "feat:industry.blueprint-lock", "feat:structures.service-reactions"]);
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

    // Seed blueprints, decryptor (Accelerant Decryptor = 34201), T3 relic (Intact Hull Section = 30558),
    // reaction formula, High-Sec Athanor (must reject reactions), and Low-Sec Tatara (must allow reactions).
    let (bpo_id, t1_bpc_id, relic_id, decryptor_id, reaction_bp_type_id, reaction_item_id) =
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
                (MASS_PRODUCTION_SKILL, 5),
                (LABORATORY_OPERATION_SKILL, 5),
                (MASS_REACTIONS_SKILL, 5),
                (REACTIONS_SKILL, 4),
                (SUPPLY_CHAIN_MGMT_SKILL, 0),
                (3380, 5),  // Industry V
                (3388, 5),  // Science V
                (11448, 4), // Minmatar Starship Engineering IV
                (11452, 4), // Mechanical Engineering IV
                (21791, 4), // Minmatar Encryption Methods IV
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

            for (sid, stype, ssys) in [
                (hs_athanor_id, 35_835_u32, JITA_SOLAR_SYSTEM_ID),
                (ls_tatara_id, 35_836_u32, ls_sys_id),
            ] {
                snap.structures.insert(
                    sid,
                    make_structure(sid, stype, corp_id, ssys, format!("Refinery {sid}")),
                );
            }

            // Rifter BPO at Jita
            let bpo_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);

            // Rifter BPC for Invention
            let bpc_id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(5));

            // Seed invention datacores for Rifter BPC
            if let Some(bp) = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID) {
                if let Some(inv_act) = bp.activities.get("invention").or_else(|| bp.activities.get("8")) {
                    for mat in &inv_act.materials {
                        seed_item(snap, mat.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (mat.quantity as i32) * 10, false, false, None);
                    }
                    for sk_req in &inv_act.skills {
                        snap.skills.entry(char_id).or_default().insert(
                            sk_req.type_id,
                            TrainedSkillRecord {
                                type_id: sk_req.type_id,
                                level: 5,
                                skill_points: 256_000.0,
                            },
                        );
                    }
                }
            }

            // Accelerant Decryptor (34201)
            let dec_id = seed_item(snap, 34_201, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 5, false, false, None);

            // Intact Hull Section (30558) for T3 invention + Sleeper Datacores (20411, 20412)
            let rel_id = seed_item(snap, 30_558, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
            for dtid in [20_411_u32, 20_412_u32, 20_424_u32, 20_172_u32] {
                seed_item(snap, dtid, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 20, false, false, None);
            }

            // Reaction formula blueprint in SDE
            let (react_bp_tid, react_mats) = ctx
                .sde
                .blueprints
                .values()
                .find_map(|bp| {
                    bp.activities
                        .get("reaction")
                        .or_else(|| bp.activities.get("9"))
                        .filter(|a| !a.materials.is_empty() && !a.products.is_empty())
                        .map(|a| (bp.blueprint_type_id, a.materials.clone()))
                })
                .expect("SDE reaction blueprint");
            let r_item_id = seed_item(snap, react_bp_tid, char_id, ls_tatara_id, InventoryFlag::Hangar, 1, true, false, None);
            for rm in react_mats {
                seed_item(snap, rm.type_id, char_id, ls_tatara_id, InventoryFlag::Hangar, (rm.quantity as i32) * 10, false, false, None);
            }

            (bpo_id, bpc_id, rel_id, dec_id, react_bp_tid, r_item_id)
        });

    // 1. Invention with Accelerant Decryptor (34201: +20% prob, +1 run, +2 ME, +10 TE)
    let inv_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(t1_bpc_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(8)),
                ("runs", PyRep::Integer(1)),
                ("decryptorTypeID", PyRep::Integer(34_201)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install invention job with decryptor");
    let inv_job_id = inv_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(inv_job.dict_get("inventedME").and_then(|v| v.as_f64()), Some(4.0));
    assert_eq!(inv_job.dict_get("inventedTE").and_then(|v| v.as_f64()), Some(14.0));
    assert_eq!(inv_job.dict_get("inventedRuns").and_then(|v| v.as_i64()), Some(2));
    let _ = decryptor_id;

    // Monitor job (`ConnectJob` / `DisconnectJob`)
    let mon = ctx
        .dispatch_rpc(&mut sess, "industryMonitor", None, "ConnectJob", &[PyRep::Integer(inv_job_id)], &kw, None)
        .await
        .expect("ConnectJob");
    assert_eq!(mon.dict_get("connected").and_then(|v| v.as_bool()), Some(true));

    // 2. T3 Ancient Relic Invention (`Intact Hull Section` 30558 -> `20` run, `ME 2 / TE 4` T3 BPC)
    let t3_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(relic_id)),
                ("blueprintTypeID", PyRep::Integer(30_558)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(8)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install T3 relic invention job");
    let t3_job_id = t3_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&t3_job_id) {
            j.start_unix = 1;
            j.duration_seconds = 1;
        }
        if let Some(mut meta) = eve_server::services::industry_pi::load_job_cost_meta(snap, t3_job_id) {
            meta.inv_prob = 1.0;
            eve_server::services::industry_pi::save_job_cost_meta(snap, t3_job_id, &meta);
        }
    });
    let t3_done = ctx
        .dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(t3_job_id)], &kw, None)
        .await
        .expect("Complete T3 invention job");
    assert_eq!(t3_done.dict_get("inventionSucceeded").and_then(|v| v.as_bool()), Some(true));
    let t3_out_id = t3_done
        .dict_get("deliveredItemID")
        .or_else(|| t3_done.dict_get("outputItemID"))
        .and_then(|v| v.as_i64())
        .unwrap();
    let (t3_runs, t3_me, t3_te) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&t3_out_id).expect("T3 BPC item");
        (
            it.blueprint_runs_remaining,
            it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i32,
            it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i32,
        )
    });
    assert_eq!((t3_runs, t3_me, t3_te), (Some(10), 2, 4));

    // 3. Remote job range check: trying to install a job at Low-Sec Tatara while in Jita with Supply Chain Mgmt / Scientific Networking 0 -> rejected
    let remote_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(ls_tatara_id)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Remote job beyond skill jump range must fail");
    assert_eq!(remote_err.code, "FacilityOutOfRemoteRange");

    // 4. Reactions: rejected in High-Sec Athanor (>= 0.45 sec), accepted in Low-Sec Tatara (< 0.45 sec)
    let hs_react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(0)),
                ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
                ("facilityID", PyRep::Integer(hs_athanor_id)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Reactions must be forbidden in High-Sec");
    assert_eq!(hs_react_err.code, "ReactionsForbiddenInHighSec");

    // Move character & session to Low-Sec Tatara and install reaction job
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
    let ls_react_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(reaction_item_id)),
                ("blueprintTypeID", PyRep::Integer(reaction_bp_type_id as i64)),
                ("facilityID", PyRep::Integer(ls_tatara_id)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install reaction job on Low-Sec Tatara");
    assert_eq!(ls_react_job.dict_get("activityID").and_then(|v| v.as_i64()), Some(9));

    // 5. Corp Industry (`useCorp = true`) + CompleteManyJobs
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
    let corp_me_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
                ("ownerID", PyRep::Integer(corp_id)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install corp ME research job");
    let corp_job_id = corp_me_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        corp_me_job.dict_get("ownerID").and_then(|v| v.as_i64()),
        Some(corp_id)
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&corp_job_id) {
            j.start_unix = 1;
            j.duration_seconds = 1;
        }
    });
    let batch_done = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteManyJobs",
            &[PyRep::List(vec![PyRep::Integer(corp_job_id)])],
            &kw,
            None,
        )
        .await
        .expect("CompleteManyJobs");
    let PyRep::List(done_list) = batch_done else {
        panic!("Expected List from CompleteManyJobs");
    };
    assert_eq!(done_list.len(), 1);

    // 10. Fulcrum pirate ship bonus (feat:industry.fulcrum-pirate-ship-bonus)
    let is_fulcrum = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::is_fulcrum_pirate_ship_bonus_applicable(
            &ctx, snap, Some(char_id), 73787, 1, 60_015_187,
        )
    });
    assert!(!is_fulcrum, "Non-pirate-enlisted pilot does not receive Fulcrum pirate ship bonus");

    // 11. EIV adjusted prices calculation (feat:industry.eiv-adjusted-prices)
    let eiv_rifter = eve_server::services::industry_pi::compute_blueprint_eiv_per_run(
        &ctx, &ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID).unwrap(), 1,
    );
    assert!(eiv_rifter > 0.0, "Blueprint EIV must derive from base ME-0 BOM * adjusted prices table");

    // 12. SCC surcharge & Alpha tax (feat:industry.scc-surcharge-alpha-tax)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });
    let alpha_costs = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::compute_industry_job_costs_for_char(
            &ctx, snap, Some(char_id), RIFTER_BP_TYPE_ID, 1, 1, JITA_STATION_ID, true,
        )
    }).expect("compute_industry_job_costs_for_char");
    assert!(alpha_costs.alpha_clone_tax_isk > 0.0 || alpha_costs.scc_surcharge_isk > 0.0, "Alpha clone must pay SCC surcharge + Alpha clone facility tax");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });

    // 13. Capital construction restrictions & component construction (feat:industry.capital-construction-restrictions, feat:industry.capital-component-construction, feat:industry.capital-core-temperature-components)
    let cap_fac_check = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::resolve_facility_modifiers_and_system(&ctx, snap, hs_athanor_id)
    });
    assert!(cap_fac_check.is_some(), "High-sec Athanor facility resolved");
    if let Some(nyx_bp) = ctx.sde.blueprints.get(&23919) {
        if let Some(act) = nyx_bp.activities.get("manufacturing").or_else(|| nyx_bp.activities.get("1")) {
            assert!(!act.materials.is_empty(), "Supercapital hull requires SDE capital/advanced components");
        }
    }

    // 14. Blueprint repackaging rejection when researched
    let res_bpo = ctx.db.with_mut_snapshot(|snap| {
        let id = seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None);
        if let Some(it) = snap.items.get_mut(&id) {
            it.dynamic_attributes.insert(1112, 10.0);
            it.dynamic_attributes.insert(1113, 20.0);
        }
        id
    });
    let pack_fail = ctx.dispatch_rpc(
        &mut sess, "repackagingSvc", None, "RepackageItems",
        &[PyRep::Integer(res_bpo), PyRep::Bool(false)],
        &kw, None,
    ).await.expect_err("Repackage researched BPO must fail");
    assert_eq!(pack_fail.code, "CannotRepackageMutatedItem");

    // 15. Job pause on service offline (feat:industry.job-pause-on-service-offline)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&ls_tatara_id) {
            st.fuel_blocks = 0;
        }
    });
    let jobs_paused = ctx.dispatch_rpc(
        &mut sess, "industryManager", None, "GetJobsByOwner",
        &[PyRep::Integer(char_id)],
        &kw, None,
    ).await.expect("GetJobsByOwner with offline structure");
    let PyRep::List(p_list) = jobs_paused else { panic!() };
    for j in p_list {
        if j.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(ls_tatara_id) {
            assert_eq!(j.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&ls_tatara_id) {
            st.fuel_blocks = 1000;
        }
    });
}

#[tokio::test]
async fn test_pi_resources_heatmap_extractor_schematics_routes_simulation_transfer_launch_poco_and_skyhook() {
    // parity: feat:pi.extractor-program, feat:pi.pi-depletion, feat:pi.factory-cycles, feat:pi.routes, feat:pi.storage-overflow, feat:pi.colony-simulation, feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office, feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.pi-skills, feat:pi.pi-notifications, feat:pi.abandon-colony, rpc:planetMgr.Flush, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserLaunchCommodities, rpc:planetMgr.UserTransferCommodities, rpc:planetMgr.UserUpdateNetwork, rpc:planetOrbitalRegistryBroker.GetSettingsInfo, rpc:planetOrbitalRegistryBroker.GetTaxRate, rpc:planetOrbitalRegistryBroker.UpdateSettings, rpc:posMgr.AnchorOrbital, rpc:posMgr.OnlineOrbital
    eve_testkit::attest(&["feat:pi.extractor-program", "feat:pi.pi-depletion", "feat:pi.factory-cycles", "feat:pi.routes", "feat:pi.storage-overflow", "feat:pi.colony-simulation", "feat:pi.expedited-transfer", "feat:pi.launch", "feat:pi.customs-office", "feat:pi.poco-settings", "feat:pi.skyhook-pi", "feat:pi.pi-skills", "feat:pi.pi-notifications", "feat:pi.abandon-colony"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let corp_id = 98_000_003;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;

    let ship_id = sess.ship_id.expect("Active ship");
    // Seed PI skills + Gas Command Center (2534) in active ship cargo
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

    // 1. GetPlanetResourceInfo & GetResourceData (spherical-harmonic heatmap bands = 2 + 3 + 2*2 = 9)
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
    assert_eq!(
        res_info.dict_get("numBands").and_then(|v| v.as_i64()),
        Some(9)
    );
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
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(first_p0_tid as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("GetResourceData");
    assert_eq!(
        heatmap.dict_get("numBands").and_then(|v| v.as_i64()),
        Some(9)
    );

    // 2. Deploy Command Center (command 1), Upgrade to level 3 (command 9), place Extractor (2848), Basic Processor (2473), Launchpad (2254),
    // link them (command 3), set Schematic (command 7), create Route (command 6), and Install Extractor Program (command 10).
    let net_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    // 1 = Create Gas Command Center (2534)
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(101),
                        PyRep::Integer(2534),
                        PyRep::Real(0.10),
                        PyRep::Real(0.10),
                    ]),
                    // 9 = Upgrade Command Center to level 3
                    PyRep::Tuple(vec![
                        PyRep::Integer(9),
                        PyRep::Integer(101),
                        PyRep::Integer(3),
                    ]),
                    // 1 = Create Gas Extractor Control Unit (3060)
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(102),
                        PyRep::Integer(3060),
                        PyRep::Real(0.11),
                        PyRep::Real(0.11),
                    ]),
                    // 1 = Create Gas Basic Industry Facility (2492)
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(103),
                        PyRep::Integer(2492),
                        PyRep::Real(0.12),
                        PyRep::Real(0.12),
                    ]),
                    // 1 = Create Gas Launchpad (2543)
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(104),
                        PyRep::Integer(2543),
                        PyRep::Real(0.13),
                        PyRep::Real(0.13),
                    ]),
                    // 3 = Link Extractor (102) -> Processor (103) and Processor (103) -> Launchpad (104)
                    PyRep::Tuple(vec![
                        PyRep::Integer(3),
                        PyRep::Integer(102),
                        PyRep::Integer(103),
                        PyRep::Integer(0),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(3),
                        PyRep::Integer(103),
                        PyRep::Integer(104),
                        PyRep::Integer(0),
                    ]),
                    // 7 = SetSchematic on Processor (103)
                    PyRep::Tuple(vec![
                        PyRep::Integer(7),
                        PyRep::Integer(103),
                        PyRep::Integer(65),
                    ]),
                    // 6 = CreateRoute from Extractor (102) -> Processor (103)
                    PyRep::Tuple(vec![
                        PyRep::Integer(6),
                        PyRep::Integer(501),
                        PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]),
                        PyRep::Integer(first_p0_tid as i64),
                        PyRep::Integer(3_000),
                    ]),
                    // 10 = Install Extractor Program (3 heads, headRadius 0.02, resource first_p0_tid)
                    PyRep::Tuple(vec![
                        PyRep::Integer(10),
                        PyRep::Integer(102),
                        PyRep::Integer(first_p0_tid as i64),
                        PyRep::Real(0.02),
                        PyRep::Integer(3),
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork full colony setup");
    assert_eq!(
        net_res
            .dict_get("commandCenterLevel")
            .and_then(|v| v.as_i64()),
        Some(3)
    );
    assert_eq!(
        net_res.dict_get("pinsCount").and_then(|v| v.as_i64()),
        Some(4)
    );

    // 3. Verify GetProgramResultInfo returns realistic extraction forecast
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
    assert!(
        prog_info
            .dict_get("cycleTimeSeconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 300
    );

    // 4. Advance Colony Simulation via Flush after backdating last_extracted_unix
    sess.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });
    let flush_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("Flush colony");
    assert!(
        flush_res
            .dict_get("producedQuantity")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );

    // 5. UserTransferCommodities (P0 -> P1 processing + expedited transfer cooldown)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.extracted_commodities.insert(first_p0_tid, 6_000);
        }
    });
    let xfer_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::Integer(2389),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserTransferCommodities P0 -> P1");
    assert!(
        xfer_res
            .dict_get("outputQuantityProduced")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 5
    );

    // Expedited transfer between pins with cooldown enforcement
    let first_pin_id = match net_res.dict_get("pins") {
        Some(PyRep::List(pins)) if !pins.is_empty() => pins[0]
            .dict_get("pinID")
            .and_then(|v| v.as_i64())
            .unwrap_or(101),
        _ => 101,
    };
    let mut expedited_kw = HashMap::new();
    expedited_kw.insert("expedited".to_string(), PyRep::Bool(true));
    let exp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(first_pin_id),
                PyRep::Integer(first_pin_id + 1),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::Integer(500),
            ],
            &expedited_kw,
            None,
        )
        .await
        .expect("Expedited transfer");
    assert!(
        exp_res
            .dict_get("cooldownSeconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 300
    );

    // Immediate second expedited transfer is rejected by cooldown
    let xfer_cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(first_pin_id),
                PyRep::Integer(first_pin_id + 1),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::Integer(500),
            ],
            &expedited_kw,
            None,
        )
        .await
        .expect_err("Immediate second transfer must fail with ExpeditedTransferOnCooldown");
    assert_eq!(xfer_cd_err.code, "ExpeditedTransferOnCooldown");

    // 6. Customs Office Settings & Tax (`planetOrbitalRegistryBroker`) + Sov Null Orbital Skyhook (81080)
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
        .dispatch_rpc(
            &mut sess,
            "planetOrbitalRegistryBroker",
            None,
            "GetTaxRate",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetTaxRate on POCO");
    assert!((poco_tax.as_f64().unwrap() - 0.04).abs() < 1e-6);

    let skyhook_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetOrbitalRegistryBroker",
            None,
            "GetSettingsInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetSettingsInfo on Orbital Skyhook");
    assert_eq!(
        skyhook_info
            .dict_get("orbitalTypeID")
            .and_then(|v| v.as_i64()),
        Some(81_080)
    );

    // 7. UserLaunchCommodities (POCO export + GetMyLaunchesDetails) & UserAbandonPlanet
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.extracted_commodities.insert(first_p0_tid, 5_000);
        }
    });
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::Integer(1_000),
                PyRep::String("CustomsOffice".to_string()),
            ],
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

    // 8. Customs Office Gantry deploy & reinforce
    let ls_sys_id = find_low_or_null_system(&ctx);
    let gantry_item = ctx.db.with_mut_snapshot(|snap| seed_item(snap, 3962, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None));
    sess.solar_system_id = Some(ls_sys_id);
    let anchor_res = ctx.dispatch_rpc(
        &mut sess, "posMgr", None, "AnchorOrbital",
        &[PyRep::Integer(gantry_item)],
        &kw, None,
    ).await;
    if let Ok(g_rep) = anchor_res {
        let g_id = g_rep.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
        let online_res = ctx.dispatch_rpc(
            &mut sess, "posMgr", None, "OnlineOrbital",
            &[PyRep::Integer(g_id)],
            &kw, None,
        ).await.expect("OnlineOrbital to POCO");
        assert_eq!(online_res.dict_get("typeID").and_then(|v| v.as_i64()), Some(2233));
    }
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);

    let abandon_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserAbandonPlanet", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("UserAbandonPlanet");
    assert_eq!(abandon_res.as_bool(), Some(true));
}
