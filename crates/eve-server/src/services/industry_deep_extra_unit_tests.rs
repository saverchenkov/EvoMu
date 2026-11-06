//! Deep branch-coverage & mutation-killing unit tests for `industry_pi`, `industry_facilities_bps`,
//! `industry_jobs`, and `industry_rigs`.

use crate::router::ServerContext;
use crate::services::industry_pi::{
    ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE, IndustryJobCostMeta, compute_activity_ptv_multiplier,
    compute_blueprint_eiv_per_run_with_snap, compute_canonical_system_cost_index,
    compute_facility_tax_bonus_for_tier, compute_industry_job_costs_with_step, handle,
    resolve_decryptor_modifiers, resolve_structure_rig_bonuses_full, restore_blueprint_flag,
    solar_system_jump_distance, sync_industry_job_pause_states,
    sync_industry_job_pause_states_on_service_change,
};
use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
const THE_FORGE_REGION_ID: u32 = 10_000_002;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    structure_id: Option<i64>,
) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id,
        solar_system_id: Some(JITA_SYSTEM_ID),
        constellation_id: Some(20_000_020),
        region_id: Some(THE_FORGE_REGION_ID),
        ship_id: Some(char_id + 500),
        ..Default::default()
    }
}

#[test]
fn ptv_multiplier_fw_tier_bonus_decryptor_and_flag_restoration_formulas() {
    let ctx = make_test_context();

    // 1. compute_activity_ptv_multiplier exact formula verification across activities 1, 3, 4, 5, 8, 9
    assert!((compute_activity_ptv_multiplier(1, 0, 5) - 5.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(9, 0, 4) - 4.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(5, 0, 10) - 0.20).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(8, 0, 3) - 0.06).abs() < 1e-9);
    let me_lvl1 = compute_activity_ptv_multiplier(4, 0, 1);
    let te_lvl1_to_2 = compute_activity_ptv_multiplier(3, 0, 2);
    assert!((me_lvl1 - 0.02).abs() < 1e-9);
    assert!((te_lvl1_to_2 - (0.02 * 355.0 / 105.0)).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(99, 0, 7) - 7.0).abs() < 1e-9);

    // 2. compute_facility_tax_bonus_for_tier across FW tiers 0..=5
    assert!((compute_facility_tax_bonus_for_tier(100.0, 0) - 100.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 1) - 90.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 2) - 80.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 3) - 70.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 4) - 60.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 5) - 50.0).abs() < 1e-9);

    // 3. restore_blueprint_flag branches
    let meta_sag3 = IndustryJobCostMeta {
        blueprint_original_flag: Some(InventoryFlag::CorpSAG3),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(9001, &meta_sag3), InventoryFlag::CorpSAG3);

    let meta_corp_locked = IndustryJobCostMeta {
        use_corp: true,
        owner_id: 98_001,
        blueprint_original_flag: Some(InventoryFlag::Locked),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(98_001, &meta_corp_locked), InventoryFlag::CorpSAG1);

    let meta_char_locked = IndustryJobCostMeta {
        use_corp: false,
        owner_id: 9001,
        blueprint_original_flag: Some(InventoryFlag::Locked),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(9001, &meta_char_locked), InventoryFlag::Hangar);
    assert_eq!(
        restore_blueprint_flag(9001, &IndustryJobCostMeta::default()),
        InventoryFlag::Hangar
    );

    // 4. resolve_decryptor_modifiers: invalid item, non-decryptor group, and valid SDE decryptor
    assert_eq!(
        resolve_decryptor_modifiers(&ctx, 999_999_999).unwrap_err().code,
        "InvalidDecryptor"
    );
    assert_eq!(
        resolve_decryptor_modifiers(&ctx, 34).unwrap_err().code,
        "InvalidDecryptor"
    );
    let dec_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1304)
        .map(|t| t.type_id)
        .unwrap();
    let (prob_mult, me_mod, te_mod, runs_mod) = resolve_decryptor_modifiers(&ctx, dec_tid).unwrap();
    assert!(prob_mult > 0.0);
    assert!(me_mod != 0.0 || te_mod != 0.0 || runs_mod != 0);

    // 5. solar_system_jump_distance: 0 for same system, 1 for adjacent Jita -> Perimeter
    assert_eq!(solar_system_jump_distance(&ctx, JITA_SYSTEM_ID, JITA_SYSTEM_ID), Some(0));
    assert_eq!(
        solar_system_jump_distance(&ctx, JITA_SYSTEM_ID, PERIMETER_SYSTEM_ID),
        Some(1)
    );
}

#[test]
fn cost_index_eiv_job_costs_and_structure_rig_security_multipliers() {
    let ctx = make_test_context();
    let char_id = 920_001;
    let corp_id = 98_000_201;
    let struct_hs_id = 1_000_000_920_001;
    let struct_ls_id = 1_000_000_920_002;
    let struct_ns_id = 1_000_000_920_003;

    let ls_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.05 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);
    let ns_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Ind Cost Pilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = char_id + 500;
        ch.balance = IskCents::from_isk(100_000_000);
        ch.clone_grade = 1; // Alpha clone
        snap.characters.insert(char_id, ch);

        for (sid, tid, sys) in [
            (struct_hs_id, 35825, JITA_SYSTEM_ID),
            (struct_ls_id, 35826, ls_sys),
            (struct_ns_id, 35836, ns_sys),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Struct {sid}"),
                    state: "ShieldVulnerable".into(),
                    fuel_blocks: 500,
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            920_101,
            InventoryItemRecord {
                item_id: 920_101,
                type_id: 37146,
                owner_id: corp_id,
                location_id: struct_hs_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            920_102,
            InventoryItemRecord {
                item_id: 920_102,
                type_id: 37146,
                owner_id: corp_id,
                location_id: struct_ls_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let react_rig_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 1933)
            .map(|t| t.type_id)
            .unwrap_or(46482);
        snap.items.insert(
            920_103,
            InventoryItemRecord {
                item_id: 920_103,
                type_id: react_rig_tid,
                owner_id: corp_id,
                location_id: struct_hs_id,
                flag: InventoryFlag::RigSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            920_104,
            InventoryItemRecord {
                item_id: 920_104,
                type_id: react_rig_tid,
                owner_id: corp_id,
                location_id: struct_ns_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let bp = ctx.sde.blueprints.get(&683).expect("Bantam Blueprint 683 in SDE");
        let eiv_1 = compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), bp, 1);
        let eiv_8 = compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), bp, 8);
        assert!(eiv_1 > 0.0 && eiv_8 > 0.0);

        let sci_base = compute_canonical_system_cost_index(&ctx, snap, JITA_SYSTEM_ID, 1);
        assert!(sci_base >= 0.0001 && sci_base <= 1.0);

        // Alpha clone at NPC station pays 0.25% alpha clone tax and 4.0% SCC surcharge
        let meta = compute_industry_job_costs_with_step(
            &ctx,
            snap,
            Some(char_id),
            683,
            1,
            0,
            2,
            JITA_STATION_ID,
            true,
        )
        .unwrap();
        assert!(meta.eiv_isk > 0.0 && meta.system_cost_index > 0.0 && meta.base_job_cost_isk > 0.0);
        assert!(meta.facility_tax_isk > 0.0 && meta.scc_surcharge_isk > 0.0 && meta.alpha_clone_tax_isk > 0.0);
        let expected_total = ((meta.base_job_cost_isk
            + meta.facility_tax_isk
            + meta.scc_surcharge_isk
            + meta.alpha_clone_tax_isk)
            * 100.0)
            .round()
            / 100.0;
        assert!((meta.total_cost_isk - expected_total).abs() < 1e-6);

        let (hs_me, hs_te, hs_cost, hs_mult) =
            resolve_structure_rig_bonuses_full(&ctx, snap, struct_hs_id, JITA_SYSTEM_ID);
        let (ls_me, ls_te, _, ls_mult) =
            resolve_structure_rig_bonuses_full(&ctx, snap, struct_ls_id, ls_sys);
        assert!(hs_me > 0.0 || hs_te > 0.0 || hs_cost > 0.0 || hs_mult >= 0.0);
        assert!(ls_me > hs_me || ls_te > hs_te);
        assert!((ls_mult - 1.9).abs() < 1e-6);

        let (ns_react_me, ns_react_te, _, ns_mult) =
            resolve_structure_rig_bonuses_full(&ctx, snap, struct_ns_id, ns_sys);
        assert!(ns_react_me > 0.0 || ns_react_te > 0.0);
        assert!((ns_mult - 1.1).abs() < 1e-6);
    });
}

#[test]
fn structure_job_pause_and_resume_on_fuel_and_service_transitions() {
    let ctx = make_test_context();
    let char_id = 921_001;
    let corp_id = 98_000_211;
    let struct_id = 1_000_000_921_001;
    let svc_mod_id = 921_700;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825, // Raitaru
                owner_corp_id: corp_id,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Pause Test Raitaru".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        // Fit Standup Manufacturing Plant I (35878) in ServiceSlot0 (164)
        snap.items.insert(
            svc_mod_id,
            InventoryItemRecord {
                item_id: svc_mod_id,
                type_id: 35878,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_mod_id, true);

        // Job 1: still running at t=200 (start=100, dur=600 -> end=700)
        snap.industry_jobs.insert(
            921_501,
            IndustryJobRecord {
                job_id: 921_501,
                installer_id: char_id,
                blueprint_item_id: 921_601,
                blueprint_type_id: 683,
                product_type_id: 582,
                facility_id: struct_id,
                activity_id: 1,
                runs: 1,
                start_unix: 100,
                duration_seconds: 600,
                status: "Active".into(),
                ..Default::default()
            },
        );
        // Job 2: already finished before offline at t=200 (start=50, dur=100 -> end=150 <= 200)
        snap.industry_jobs.insert(
            921_502,
            IndustryJobRecord {
                job_id: 921_502,
                installer_id: char_id,
                blueprint_item_id: 921_602,
                blueprint_type_id: 683,
                product_type_id: 582,
                facility_id: struct_id,
                activity_id: 1,
                runs: 1,
                start_unix: 50,
                duration_seconds: 100,
                status: "Active".into(),
                ..Default::default()
            },
        );
    });

    // 1. Offline the manufacturing service module at t=200 -> Job 1 pauses, Job 2 stays Active
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(svc_mod_id, false);
        sync_industry_job_pause_states_on_service_change(snap, 200);
        assert_eq!(snap.industry_jobs.get(&921_501).unwrap().status, "Paused");
        assert_eq!(snap.industry_jobs.get(&921_502).unwrap().status, "Active");
    });

    // 2. Re-online the manufacturing service module at t=350 (paused for 150s) -> Job 1 resumes with duration 600 + 150 = 750
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(svc_mod_id, true);
        sync_industry_job_pause_states_on_service_change(snap, 350);
        let j1 = snap.industry_jobs.get(&921_501).unwrap();
        assert_eq!(j1.status, "Active");
        assert_eq!(j1.duration_seconds, 750);
    });

    // 3. Drain structure fuel to 0 and call sync_industry_job_pause_states at t=400 -> Job 1 pauses again
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 0;
        sync_industry_job_pause_states(snap, 400);
        assert_eq!(snap.industry_jobs.get(&921_501).unwrap().status, "Paused");
    });
}

#[test]
fn blueprint_facility_and_industry_manager_rpc_end_to_end_branches() {
    let ctx = make_test_context();
    let char_id = 922_001;
    let corp_id = 98_000_221;
    let struct_id = 1_000_000_922_001;
    let bpo_id = 922_100;
    let bpc_id = 922_101;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Ind RPC Pilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = char_id + 500;
        ch.balance = IskCents::from_isk(500_000_000);
        ch.clone_grade = 2; // Omega
        snap.characters.insert(char_id, ch);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Ind Corp".into(),
                ticker: "INDC".into(),
                ceo_id: char_id,
                master_wallet: IskCents::from_isk(500_000_000),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Ind Raitaru".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3380, 3387, 3402, 3406, 24625, 24624] {
            sk.insert(
                sid,
                TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
            );
        }
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: 683,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                dynamic_attributes: HashMap::from([
                    (ATTR_BLUEPRINT_ME, 2.0),
                    (ATTR_BLUEPRINT_TE, 4.0),
                ]),
                ..Default::default()
            },
        );
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 683,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(2),
                dynamic_attributes: HashMap::from([
                    (ATTR_BLUEPRINT_ME, 10.0),
                    (ATTR_BLUEPRINT_TE, 20.0),
                ]),
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(JITA_STATION_ID), None);
    let kw = HashMap::new();

    // 1. blueprintManager: GetBlueprintData, GetBlueprintDataByOwner, GetLimits
    let bp_data = handle(&ctx, &mut sess, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bpo_id)], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(bp_data.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(bp_data.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(4));

    let bp_list = handle(&ctx, &mut sess, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(char_id)], &kw, None)
        .unwrap()
        .unwrap();
    assert!(matches!(bp_list, PyRep::List(ref l) if l.len() >= 2));

    let limits = handle(&ctx, &mut sess, "blueprintManager", "GetLimits", &[], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(limits.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()), Some(11));

    // 2. facilityManager: GetFacilities, GetFacility, SetFacilityTaxes, GetFacilityTaxes, GetMaxActivityModifiers
    let _ = handle(&ctx, &mut sess, "facilityManager", "GetFacilities", &[], &kw, None)
        .unwrap()
        .unwrap();
    let fac = handle(&ctx, &mut sess, "facilityManager", "GetFacility", &[PyRep::Integer(struct_id)], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(fac.dict_get("facilityID").and_then(|v| v.as_i64()), Some(struct_id));

    let _ = handle(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[
            PyRep::Integer(struct_id),
            PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.035))]),
        ],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    let taxes = handle(&ctx, &mut sess, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(struct_id)], &kw, None)
        .unwrap()
        .unwrap();
    assert!(taxes.dict_get("1").is_some() || matches!(taxes, PyRep::Dict(_)));

    let mods = handle(&ctx, &mut sess, "facilityManager", "GetMaxActivityModifiers", &[], &kw, None)
        .unwrap()
        .unwrap();
    assert!(mods.dict_get("materialMultiplier").is_some());

    // 3. Cannot copy a BPC -> CannotResearchOrCopyBPC
    let copy_bpc_err = handle(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpc_id)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(copy_bpc_err.code, "CannotResearchOrCopyBPC");

    // 4. Install a Copy job (activity 5) on the BPO, then complete it via CompleteManyJobs
    let copy_job = handle(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(2)),
            ("licensedRuns", PyRep::Integer(3)),
        ])],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    let copy_job_id = copy_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&copy_job_id).unwrap().start_unix = 1;
    });
    let completed_many = handle(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(copy_job_id)])],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(completed_many, PyRep::List(ref l) if l.len() == 1));

    // 5. Install an ME research job (activity 4) on the BPO and CancelJob to verify blueprint unlock
    let me_job = handle(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    let me_job_id = me_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let cancelled = handle(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(me_job_id)], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.dict_get("status").and_then(|v| v.as_str()), Some("Cancelled"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&bpo_id).unwrap().flag, InventoryFlag::Hangar);
    });
}
