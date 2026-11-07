//! Semantic unit tests split from `industry_deep_extra_unit_tests`.

use super::industry_deep_extra_unit_tests::*;
use crate::services::industry_pi::{
    ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE, handle, sync_industry_job_pause_states,
    sync_industry_job_pause_states_on_service_change,
};
use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_proto::PyRep;
use std::collections::HashMap;

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
