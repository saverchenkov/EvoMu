//! Unit and mutation-killing tests for `services::industry_facilities_bps`.

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, WorldRepository, corp_roles,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn char_session(char_id: i64, st_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

#[test]
fn structure_service_online_job_pause_and_taxes_cover_all_branches() {
    let ctx = test_ctx();
    let struct_id = 1_000_000_990_001_i64;
    let svc_mod_id = 889_001_i64;
    let now = ctx.now_unix();

    assert_eq!(facility_tax_note_id(60_003_760), -1_000_060_003_760);
    assert_eq!(job_cost_note_id(500), -2_000_000_000_500);

    ctx.db.with_mut_snapshot(|snap| {
        // Non-structure facility is always considered online
        assert!(is_structure_industry_service_online(snap, 60_003_760, 1));
        assert!(!has_custom_facility_taxes(snap, 60_003_760));
        let npc_taxes = load_facility_taxes(snap, 60_003_760);
        assert!((npc_taxes[&1] - NPC_STATION_INDUSTRY_TAX_RATE).abs() < 1e-9);

        // Insert Upwell Raitaru (35825)
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Test Raitaru".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        assert!(has_custom_facility_taxes(snap, struct_id));
        assert!((load_facility_taxes(snap, struct_id)[&1] - 0.05).abs() < 1e-9);

        // Without service module installed -> offline
        assert!(!is_structure_industry_service_online(snap, struct_id, 1));

        // Install Standup Manufacturing Plant I (35878) in ServiceSlot0 (flag 164)
        snap.items.insert(
            svc_mod_id,
            InventoryItemRecord {
                item_id: svc_mod_id,
                type_id: 35878,
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_mod_id, true);
        assert!(is_structure_industry_service_online(snap, struct_id, 1));
        assert!(!is_structure_industry_service_online(snap, struct_id, 8)); // Invention needs lab

        // Active job 101 (not yet ready) and job 102 (already ready before service goes offline)
        snap.industry_jobs.insert(
            101,
            IndustryJobRecord {
                job_id: 101,
                installer_id: 90_000_001,
                facility_id: struct_id,
                blueprint_item_id: 901,
                blueprint_type_id: 683,
                activity_id: 1,
                runs: 1,
                product_type_id: 582,
                status: "Active".into(),
                duration_seconds: 1_000,
                start_unix: now.saturating_sub(200),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            102,
            IndustryJobRecord {
                job_id: 102,
                installer_id: 90_000_001,
                facility_id: struct_id,
                blueprint_item_id: 902,
                blueprint_type_id: 683,
                activity_id: 1,
                runs: 1,
                product_type_id: 582,
                status: "Active".into(),
                duration_seconds: 100,
                start_unix: now.saturating_sub(500),
                ..Default::default()
            },
        );

        // Offline service module and run sync_industry_job_pause_states_on_service_change
        snap.module_online_states.insert(svc_mod_id, false);
        sync_industry_job_pause_states_on_service_change(snap, now);
        assert_eq!(snap.industry_jobs[&101].status, "Paused");
        assert_eq!(snap.industry_jobs[&102].status, "Active");
        assert!(load_job_cost_meta(snap, 102).unwrap().ready_before_offline);

        // Bring service back online 300s later -> job 101 resumes and duration increases by 300s
        snap.module_online_states.insert(svc_mod_id, true);
        sync_industry_job_pause_states(snap, now + 300);
        assert_eq!(snap.industry_jobs[&101].status, "Active");
        assert_eq!(snap.industry_jobs[&101].duration_seconds, 1_300);
    });
}

#[test]
fn sci_eiv_and_job_cost_computations_cover_alpha_fulcrum_and_fw_tiers() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let bp_tid = *ctx
        .sde
        .blueprints
        .iter()
        .find(|(_, b)| !b.materials.is_empty())
        .map(|(k, _)| k)
        .unwrap_or(&683);

    ctx.db.with_mut_snapshot(|snap| {
        // Base SCI with no jobs is 0.01
        assert!((compute_canonical_system_cost_index(&ctx, snap, sys_id, 1) - 0.01).abs() < 1e-9);

        // Unknown blueprint returns UnknownBlueprint error
        assert_eq!(
            compute_industry_job_costs(&ctx, snap, 999_999_999, 1, 1, st_id)
                .unwrap_err()
                .code,
            "UnknownBlueprint"
        );

        let omega_cost =
            compute_industry_job_costs_ext(&ctx, snap, bp_tid, 1, 2, st_id, false).unwrap();
        let alpha_cost =
            compute_industry_job_costs_ext(&ctx, snap, bp_tid, 1, 2, st_id, true).unwrap();
        assert!(omega_cost.eiv_isk > 0.0);
        assert_eq!(omega_cost.alpha_clone_tax_isk, 0.0);
        assert!(alpha_cost.alpha_clone_tax_isk > 0.0);
        assert!(alpha_cost.total_cost_isk > omega_cost.total_cost_isk);

        // Add active jobs in Jita and another system to exercise SCI sqrt(sys_work / univ_work)
        let other_st = *ctx
            .sde
            .stations
            .iter()
            .find(|(_, s)| s.solar_system_id != sys_id)
            .map(|(k, _)| k)
            .unwrap_or(&60_008_494);
        snap.industry_jobs.insert(
            201,
            IndustryJobRecord {
                job_id: 201,
                installer_id: char_id,
                facility_id: st_id,
                blueprint_type_id: bp_tid,
                activity_id: 1,
                runs: 1,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix: ctx.now_unix(),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            202,
            IndustryJobRecord {
                job_id: 202,
                installer_id: char_id,
                facility_id: other_st,
                blueprint_type_id: bp_tid,
                activity_id: 1,
                runs: 1,
                status: "Active".into(),
                duration_seconds: 10_800,
                start_unix: ctx.now_unix(),
                ..Default::default()
            },
        );
        let sci = compute_canonical_system_cost_index(&ctx, snap, sys_id, 1);
        assert!((sci - 0.5).abs() < 1e-6); // sqrt(3600 / 14400) = 0.5
    });
}

#[test]
fn blueprint_and_facility_manager_rpcs_work_end_to_end() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let struct_id = 1_000_000_990_099_i64;
    let corp_id = 98_000_099_i64;
    let mut sess = char_session(char_id, st_id);
    let kw = HashMap::new();

    let bp_tid = *ctx.sde.blueprints.keys().next().unwrap_or(&683);
    let bp_item_id = 889_500_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Indy Corp".into(),
                ticker: "INDY".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Corp Raitaru".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        let mut bp_it = InventoryItemRecord {
            item_id: bp_item_id,
            type_id: bp_tid,
            owner_id: char_id,
            location_id: st_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        bp_it.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 8.0);
        bp_it.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 16.0);
        snap.items.insert(bp_item_id, bp_it);
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;

    // GetLimits
    let limits = handle_industry_facilities_bps(&ctx, &mut sess, "blueprintManager", "GetLimits", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(limits.dict_get("maxBlueprintResults"), Some(&PyRep::Integer(25_000)));

    // GetBlueprintData by item_id and by type_id
    let bpd = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bp_item_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bpd.dict_get("materialEfficiency"), Some(&PyRep::Integer(8)));
    assert_eq!(bpd.dict_get("timeEfficiency"), Some(&PyRep::Integer(16)));

    // GetBlueprintDataByOwner (1-arg list and 2-arg v24 tuple + OwnerAccessDenied)
    let by_owner = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(char_id), PyRep::None],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(by_owner, PyRep::Tuple(_)));

    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );

    // Facility queries & SetFacilityTaxes
    let fac = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacility",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(fac.dict_get("facilityID"), Some(&PyRep::Integer(struct_id)));

    let facs = handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetFacilities", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(facs, PyRep::List(ref l) if !l.is_empty()));

    let locs = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityLocations",
        &[PyRep::Integer(st_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(locs, PyRep::List(ref l) if !l.is_empty()));

    // SetFacilityTaxes: 3.5% succeeds, 25% (>10%) fails with InvalidFacilityTaxRate
    let set_tax = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(3.5)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_tax.dict_get("tax"), Some(&PyRep::Real(0.035)));

    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(25.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );

    let taxes_rep = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityTaxes",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(taxes_rep.dict_get("tax"), Some(&PyRep::Real(0.035)));

    // GetMaxActivityModifiers, map.GetIndustryCostModifier, map.GetIndustryJobsOverLast24Hours
    assert!(
        handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetMaxActivityModifiers", &[], &kw)
            .unwrap()
            .is_ok()
    );
    assert!(
        handle_industry_facilities_bps(&ctx, &mut sess, "map", "GetIndustryCostModifier", &[], &kw)
            .unwrap()
            .is_ok()
    );
    assert!(
        handle_industry_facilities_bps(&ctx, &mut sess, "map", "GetIndustryJobsOverLast24Hours", &[], &kw)
            .unwrap()
            .is_ok()
    );
}
