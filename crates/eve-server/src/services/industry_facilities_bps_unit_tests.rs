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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Indy Corp".into();
                __corp.ticker = "INDY".into();
                __corp.ceo_id = char_id;
                __corp },
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
    let max_mods_base =
        handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetMaxActivityModifiers", &[], &kw)
            .unwrap()
            .unwrap();
    let base_mat = max_mods_base.dict_get("materialMultiplier").and_then(|v| v.as_f64()).unwrap();
    assert!(base_mat > 0.0 && base_mat <= 0.99 + 1e-9);
    ctx.db.with_mut_snapshot(|snap| {
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(2594_u16, 4.0_f64); // 4% rig material bonus
        snap.items.insert(
            999_881,
            InventoryItemRecord {
                item_id: 999_881,
                type_id: 37178,
                owner_id: 1_000_009,
                location_id: struct_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: dyn_attrs,
                ..Default::default()
            },
        );
    });
    let max_mods_rigged =
        handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetMaxActivityModifiers", &[], &kw)
            .unwrap()
            .unwrap();
    let rigged_mat = max_mods_rigged.dict_get("materialMultiplier").and_then(|v| v.as_f64()).unwrap();
    assert!(rigged_mat < base_mat, "fitted rig must improve GetMaxActivityModifiers materialMultiplier: {rigged_mat} < {base_mat}");
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

fn assert_canonical_facility_rep(fac: &PyRep) {
    assert!(matches!(fac.dict_get("rigModifiers"), Some(PyRep::Dict(_))));
    assert!(matches!(fac.dict_get("serviceAccess"), Some(PyRep::Dict(_))));
    assert!(matches!(fac.dict_get("sccTaxModifier"), Some(PyRep::Real(_))));
    let Some(PyRep::Dict(global_mods)) = fac.dict_get("globalModifiers") else {
        panic!("expected globalModifiers Dict on facility: {fac:?}");
    };
    assert!(global_mods.iter().any(|(k, _)| k.as_str() == Some("faction")));
    assert!(global_mods.iter().any(|(k, _)| k.as_str() == Some("system")));

    let Some(PyRep::Dict(acts)) = fac.dict_get("activities") else {
        panic!("expected activities Dict directly from handler: {fac:?}");
    };
    assert!(!acts.is_empty(), "facility activities must be non-empty");
    for (k, v) in acts {
        assert!(k.as_i64().is_some(), "activity key must be Integer: {k:?}");
        let PyRep::Tuple(six) = v else {
            panic!("expected 6-tuple per activity, got {v:?}");
        };
        assert_eq!(six.len(), 6);
        let PyRep::List(cats) = &six[3] else {
            panic!("expected category list at index 3, got {:?}", six[3]);
        };
        assert!(!cats.is_empty(), "activity categories at index 3 must be non-empty");
    }
}

fn assert_canonical_blueprint_rep(bp: &PyRep, expect_details: bool) {
    for key in [
        "itemID",
        "blueprintID",
        "typeID",
        "blueprintTypeID",
        "productTypeID",
        "ownerID",
        "facilityID",
        "locationID",
        "locationTypeID",
        "locationFlagID",
        "flagID",
        "solarSystemID",
        "materialEfficiency",
        "timeEfficiency",
        "original",
        "copy",
        "runs",
        "runsRemaining",
        "quantity",
        "jobID",
        "isImpounded",
        "materials",
        "skills",
        "estimatedItemValueISK",
    ] {
        assert!(bp.dict_get(key).is_some(), "BlueprintInstance missing canonical key `{key}`: {bp:?}");
    }
    if expect_details {
        for key in [
            "maxProductionLimit",
            "manufacturingTime",
            "researchMaterialTime",
            "researchTimeTime",
            "copyingTime",
            "inventionTime",
            "reactionTime",
            "baseJobCost",
            "facilityTaxISK",
            "sccSurchargeISK",
            "totalJobCostISK",
        ] {
            assert!(bp.dict_get(key).is_some(), "GetBlueprintData missing detail key `{key}`: {bp:?}");
        }
    }
}

fn call_ind_wire_roundtrip(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> PyRep {
    let raw = handle_industry_facilities_bps(ctx, sess, service, method, args, kwargs)
        .unwrap_or_else(|| panic!("{service}.{method} should be handled"))
        .unwrap_or_else(|e| panic!("{service}.{method} failed unexpectedly: {e:?}"));

    // client_compat must be a no-op identity for C-9 migrated services
    let compat_out = crate::router::client_compat::normalize_macho_rpc_reply(
        service,
        method,
        Ok(raw.clone()),
    )
    .expect("client_compat passthrough");
    assert_eq!(compat_out, raw, "{service}.{method} should not be rewritten by client_compat");

    // Socket-loop dispatch + blue.marshal round-trip must produce the identical wire PyRep
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio test runtime");
    let mut wire_sess = sess.clone();
    let outcome = rt.block_on(crate::router::client_wire::dispatch_call_on_wire(
        ctx,
        &mut wire_sess,
        service,
        None,
        method,
        args,
        kwargs,
    ));
    assert!(
        outcome.marshal_errors.is_empty(),
        "{service}.{method} produced blue.marshal errors: {:?}",
        outcome.marshal_errors
    );
    let wire_rep = outcome
        .rpc_result
        .unwrap_or_else(|e| panic!("{service}.{method} wire call failed: {e:?}"));
    assert_eq!(wire_rep, raw, "{service}.{method} wire reply must match handler reply");
    raw
}

#[test]
fn facility_manager_c9_typed_replies_and_wire_passthrough() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let mut sess = char_session(char_id, st_id);
    let kw = HashMap::new();

    // 1. GetFacilities & GetFacilitiesByID emit dicts of 6-tuples and all modifier dicts directly
    let facs = call_ind_wire_roundtrip(&ctx, &mut sess, "facilityManager", "GetFacilities", &[], &kw);
    let PyRep::List(fac_list) = facs else {
        panic!("expected List from GetFacilities");
    };
    assert!(!fac_list.is_empty());
    assert_canonical_facility_rep(&fac_list[0]);

    let by_id = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilitiesByID",
        &[PyRep::List(vec![PyRep::Integer(st_id)])],
        &kw,
    );
    let PyRep::List(by_id_list) = by_id else {
        panic!("expected List from GetFacilitiesByID");
    };
    assert_eq!(by_id_list.len(), 1);
    assert_canonical_facility_rep(&by_id_list[0]);

    // 2. GetFacility returns canonical facility KeyVal or FacilityNotFound error for missing ID
    let single = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacility",
        &[PyRep::Integer(st_id)],
        &kw,
    );
    assert_canonical_facility_rep(&single);
    let missing_err = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacility",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .expect("handled")
    .expect_err("missing facility should return FacilityNotFound");
    assert_eq!(missing_err.code, "FacilityNotFound");

    let expected_st_type = ctx
        .sde
        .stations
        .get(&st_id)
        .map(|s| i64::from(s.station_type_id))
        .unwrap_or(1529);

    // 3. GetFacilityLocations directly emits ObjectEx type 2 (`industry.job.Location`)
    for args in [Vec::new(), vec![PyRep::List(vec![PyRep::Integer(st_id)])]] {
        let locs = call_ind_wire_roundtrip(
            &ctx,
            &mut sess,
            "facilityManager",
            "GetFacilityLocations",
            &args,
            &kw,
        );
        let PyRep::List(loc_items) = locs else {
            panic!("expected List from GetFacilityLocations");
        };
        assert!(!loc_items.is_empty());
        let PyRep::ObjectEx { is_type_2: true, header, .. } = &loc_items[0] else {
            panic!("expected ObjectEx type 2 industry.job.Location, got {:?}", loc_items[0]);
        };
        let PyRep::Tuple(hdr) = header.as_ref() else {
            panic!("expected Tuple header on industry.job.Location");
        };
        assert_eq!(hdr[0], PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]));
        assert_eq!(hdr[1].dict_get("itemID"), Some(&PyRep::Integer(st_id)));
        assert_eq!(hdr[1].dict_get("facilityID"), Some(&PyRep::Integer(st_id)));
        assert_eq!(hdr[1].dict_get("typeID"), Some(&PyRep::Integer(expected_st_type)));
        assert_eq!(hdr[1].dict_get("ownerID"), Some(&PyRep::Integer(char_id)));
        assert_eq!(hdr[1].dict_get("flagID"), Some(&PyRep::Integer(4)));
        assert_eq!(hdr[1].dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
        assert_eq!(hdr[1].dict_get("canView"), Some(&PyRep::Bool(true)));
        assert_eq!(hdr[1].dict_get("canTake"), Some(&PyRep::Bool(true)));
    }
}

#[test]
fn blueprint_manager_c9_typed_replies_and_wire_passthrough() {
    let ctx = test_ctx();
    let empty_char_id = 90_000_077_i64;
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let bpo_id = 981_001_i64;
    let bpc_id = 981_002_i64;
    let kw = HashMap::new();

    // 1. Owner with zero blueprints -> canonical (List([]), Dict([])) 2-tuple even for 1-arg calls
    let mut empty_sess = char_session(empty_char_id, st_id);
    let empty_rep = call_ind_wire_roundtrip(
        &ctx,
        &mut empty_sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(empty_char_id)],
        &kw,
    );
    let PyRep::Tuple(empty_parts) = empty_rep else {
        panic!("expected (bps, counts) Tuple for empty owner");
    };
    assert_eq!(empty_parts.len(), 2);
    assert_eq!(empty_parts[0], PyRep::List(Vec::new()));
    assert_eq!(empty_parts[1], PyRep::Dict(Vec::new()));

    // 2. Seed one BPO (type 683) and one BPC (type 684) for char_id at st_id
    ctx.db.with_mut_snapshot(|snap| {
        let mut bpo = InventoryItemRecord {
            item_id: bpo_id,
            type_id: 683,
            owner_id: char_id,
            location_id: st_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        bpo.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 5.0);
        bpo.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 10.0);
        snap.items.insert(bpo_id, bpo);

        let bpc = InventoryItemRecord {
            item_id: bpc_id,
            type_id: 684,
            owner_id: char_id,
            location_id: st_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            is_blueprint_copy: true,
            blueprint_runs_remaining: Some(10),
            ..Default::default()
        };
        snap.items.insert(bpc_id, bpc);
    });

    let mut sess = char_session(char_id, st_id);
    // Both 1-arg and 2-arg signatures emit the identical canonical 2-tuple
    let one_arg = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(char_id)],
        &kw,
    );
    let two_arg = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(char_id), PyRep::None],
        &kw,
    );
    assert_eq!(one_arg, two_arg);

    let PyRep::Tuple(owner_pair) = one_arg else {
        panic!("expected 2-tuple from GetBlueprintDataByOwner");
    };
    assert_eq!(owner_pair.len(), 2);
    let PyRep::List(ref bps) = owner_pair[0] else {
        panic!("expected blueprint List at tuple[0]");
    };
    let PyRep::Dict(ref counts) = owner_pair[1] else {
        panic!("expected facility count Dict at tuple[1]");
    };
    assert!(!counts.is_empty());

    let expected_st_type = ctx
        .sde
        .stations
        .get(&st_id)
        .map(|s| i64::from(s.station_type_id))
        .unwrap_or(1529);
    let bpo_entry = bps
        .iter()
        .find(|b| b.dict_get("itemID") == Some(&PyRep::Integer(bpo_id)))
        .expect("seeded BPO present");
    assert_canonical_blueprint_rep(bpo_entry, false);
    assert_eq!(bpo_entry.dict_get("typeID"), Some(&PyRep::Integer(683)));
    assert_eq!(bpo_entry.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(bpo_entry.dict_get("original"), Some(&PyRep::Bool(true)));
    assert_eq!(bpo_entry.dict_get("copy"), Some(&PyRep::Bool(false)));
    assert_eq!(bpo_entry.dict_get("runs"), Some(&PyRep::Integer(-1)));
    assert_eq!(bpo_entry.dict_get("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(bpo_entry.dict_get("locationTypeID"), Some(&PyRep::Integer(expected_st_type)));
    assert_eq!(bpo_entry.dict_get("jobID"), Some(&PyRep::None));

    let bpc_entry = bps
        .iter()
        .find(|b| b.dict_get("itemID") == Some(&PyRep::Integer(bpc_id)))
        .expect("seeded BPC present");
    assert_canonical_blueprint_rep(bpc_entry, false);
    assert_eq!(bpc_entry.dict_get("typeID"), Some(&PyRep::Integer(684)));
    assert_eq!(bpc_entry.dict_get("original"), Some(&PyRep::Bool(false)));
    assert_eq!(bpc_entry.dict_get("copy"), Some(&PyRep::Bool(true)));
    assert_eq!(bpc_entry.dict_get("runs"), Some(&PyRep::Integer(10)));
    assert_eq!(bpc_entry.dict_get("runsRemaining"), Some(&PyRep::Integer(10)));
    assert_eq!(bpc_entry.dict_get("quantity"), Some(&PyRep::Integer(-2)));

    // 3. GetBlueprintData on BPO & BPC returns canonical BlueprintInstance + job details directly
    let bpo_detail = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bpo_id)],
        &kw,
    );
    assert_canonical_blueprint_rep(&bpo_detail, true);
    assert_eq!(bpo_detail.dict_get("typeID"), Some(&PyRep::Integer(683)));
    assert_eq!(bpo_detail.dict_get("materialEfficiency"), Some(&PyRep::Integer(5)));
    assert_eq!(bpo_detail.dict_get("timeEfficiency"), Some(&PyRep::Integer(10)));
    assert_eq!(bpo_detail.dict_get("copy"), Some(&PyRep::Bool(false)));

    let bpc_detail = call_ind_wire_roundtrip(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bpc_id)],
        &kw,
    );
    assert_canonical_blueprint_rep(&bpc_detail, true);
    assert_eq!(bpc_detail.dict_get("typeID"), Some(&PyRep::Integer(684)));
    assert_eq!(bpc_detail.dict_get("copy"), Some(&PyRep::Bool(true)));
    assert_eq!(bpc_detail.dict_get("runs"), Some(&PyRep::Integer(10)));
    assert_eq!(bpc_detail.dict_get("quantity"), Some(&PyRep::Integer(-2)));
}

#[test]
fn get_blueprint_data_fails_closed_and_owner_omits_when_solar_system_unresolvable() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let unknown_loc = 9_999_999_999_i64;
    let unresolved_bp_id = 892_901_i64;
    let resolved_bp_id = 892_902_i64;
    let valid_st = ctx
        .sde
        .stations
        .keys()
        .copied()
        .min()
        .expect("canonical seed station");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 0;
            ch.station_id = None;
            ch.structure_id = None;
            ch.home_station_id = 0;
        }
        snap.items.retain(|_, it| !(it.owner_id == char_id && ctx.sde.blueprints.contains_key(&it.type_id)));
        snap.items.insert(
            unresolved_bp_id,
            eve_db::InventoryItemRecord {
                item_id: unresolved_bp_id,
                type_id: 683,
                owner_id: char_id,
                location_id: unknown_loc,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            resolved_bp_id,
            eve_db::InventoryItemRecord {
                item_id: resolved_bp_id,
                type_id: 684,
                owner_id: char_id,
                location_id: valid_st,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut unlocated_sess = SessionState::new(1);
    unlocated_sess.user_id = Some(1);
    unlocated_sess.character_id = Some(char_id);
    unlocated_sess.corporation_id = Some(98_000_001);

    let kw = HashMap::new();
    let owner_rep = call_ind_wire_roundtrip(
        &ctx,
        &mut unlocated_sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(char_id)],
        &kw,
    );
    let PyRep::Tuple(owner_pair) = owner_rep else {
        panic!("expected 2-tuple from GetBlueprintDataByOwner");
    };
    let PyRep::List(ref bps) = owner_pair[0] else {
        panic!("expected blueprint list");
    };
    let PyRep::Dict(ref counts) = owner_pair[1] else {
        panic!("expected facility counts dict");
    };
    assert!(
        bps.iter()
            .all(|b| b.dict_get("itemID") != Some(&PyRep::Integer(unresolved_bp_id))),
        "unresolvable solar system blueprint must be omitted rather than emitting solarSystemID=0"
    );
    assert!(
        bps.iter()
            .any(|b| b.dict_get("itemID") == Some(&PyRep::Integer(resolved_bp_id))),
        "resolvable station blueprint must remain present"
    );
    assert_eq!(counts.len(), 1, "omitted unresolvable blueprint must not inflate facility_counts");

    let by_item_err = handle_industry_facilities_bps(
        &ctx,
        &mut unlocated_sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(unresolved_bp_id)],
        &kw,
    )
    .expect("handled")
    .expect_err("unresolvable item GetBlueprintData must fail closed");
    assert_eq!(by_item_err.code, "CustomInfo");
    assert!(by_item_err.message.contains("Cannot resolve solar system"));

    let by_type_err = handle_industry_facilities_bps(
        &ctx,
        &mut unlocated_sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(683)],
        &kw,
    )
    .expect("handled")
    .expect_err("unresolvable type GetBlueprintData must fail closed");
    assert_eq!(by_type_err.code, "CustomInfo");
    assert!(by_type_err.message.contains("Cannot resolve solar system"));
}

#[test]
fn get_blueprint_data_enforces_corp_bp_roles_and_rejects_npc_corp() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let st_id = 60_003_760;
    let bp_type_id = *ctx.sde.blueprints.keys().min().expect("at least one blueprint in SDE");
    let corp_id = 98_000_821;
    let foreign_corp_id = 98_000_822;
    let npc_corp_id = 1_000_009;
    let ceo_id = 2_000_821;
    let factory_id = 2_000_822;
    let rent_fac_id = 2_000_823;
    let rent_res_id = 2_000_824;
    let station_mgr_id = 2_000_825;
    let plain_id = 2_000_826;
    let foreign_id = 2_000_827;
    let corp_bp_id = 9_200_821;
    let npc_bp_id = 9_200_822;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "BpRbacCorp".into();
        corp.ticker = "BPRB".into();
        corp.ceo_id = ceo_id;
        corp.members.extend([
            ceo_id,
            factory_id,
            rent_fac_id,
            rent_res_id,
            station_mgr_id,
            plain_id,
        ]);
        corp.member_roles.insert(factory_id, corp_roles::ROLE_FACTORY_MANAGER);
        corp.member_roles.insert(rent_fac_id, corp_roles::ROLE_RENT_FACTORY_FACILITY);
        corp.member_roles.insert(rent_res_id, corp_roles::ROLE_RENT_RESEARCH_FACILITY);
        corp.member_roles.insert(station_mgr_id, corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            corp_bp_id,
            InventoryItemRecord {
                item_id: corp_bp_id,
                type_id: bp_type_id,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            npc_bp_id,
            InventoryItemRecord {
                item_id: npc_bp_id,
                type_id: bp_type_id,
                owner_id: npc_corp_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    for allowed_char in [ceo_id, factory_id, rent_fac_id, rent_res_id] {
        let mut sess = char_session(allowed_char, st_id);
        sess.corporation_id = Some(corp_id);
        let rep = handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(corp_bp_id)],
            &kw,
        )
        .unwrap()
        .expect("authorized corp blueprint query must succeed");
        assert_eq!(rep.dict_get("itemID"), Some(&PyRep::Integer(corp_bp_id)));
    }

    for (denied_char, cid) in [
        (plain_id, corp_id),
        (station_mgr_id, corp_id),
        (foreign_id, foreign_corp_id),
    ] {
        let mut sess = char_session(denied_char, st_id);
        sess.corporation_id = Some(cid);
        let err = handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(corp_bp_id)],
            &kw,
        )
        .unwrap()
        .expect_err("member without CORP_BP_ROLES or foreign pilot must be denied");
        assert_eq!(err.code, "CrpAccessDenied");
    }

    let mut npc_sess = char_session(plain_id, st_id);
    npc_sess.corporation_id = Some(npc_corp_id);
    let npc_err = handle_industry_facilities_bps(
        &ctx,
        &mut npc_sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(npc_bp_id)],
        &kw,
    )
    .unwrap()
    .expect_err("NPC corp blueprint must not be exposed as corp-owned");
    assert_eq!(npc_err.code, "CrpAccessDenied");
}

#[test]
fn corp_blueprints_and_taxes_use_db_first_corp_and_enforce_corp_members() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let st_id = 60_003_760_i64;
    let struct_id = 1_040_999_821_i64;
    let bp_type_id = *ctx.sde.blueprints.keys().min().expect("blueprint in SDE");
    let corp_id = 98_900_821_i64;
    let other_corp_id = 98_900_822_i64;
    let ceo_id = 2_900_821_i64;
    let omitted_id = 2_900_822_i64;
    let stale_id = 2_900_823_i64;
    let corp_bp_id = 9_900_821_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "BpGuardCorp".into();
        corp.ticker = "BPGD".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id]);
        corp.member_roles.insert(
            omitted_id,
            corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_FACTORY_MANAGER
                | corp_roles::ROLE_STATION_MANAGER,
        );
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        snap.characters.insert(
            omitted_id,
            eve_db::CharacterRecord {
                character_id: omitted_id,
                corporation_id: corp_id,
                ..Default::default()
            },
        );
        snap.characters.insert(
            stale_id,
            eve_db::CharacterRecord {
                character_id: stale_id,
                corporation_id: other_corp_id,
                ..Default::default()
            },
        );

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Tax Guard Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );

        snap.items.insert(
            corp_bp_id,
            InventoryItemRecord {
                item_id: corp_bp_id,
                type_id: bp_type_id,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Stale session.corporation_id pointing to corp_id cannot bypass DB other_corp_id.
    let mut stale_sess = char_session(stale_id, st_id);
    stale_sess.corporation_id = Some(corp_id);
    stale_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_FACTORY_MANAGER;
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut stale_sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(corp_bp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut stale_sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );

    // Character omitted from non-empty corp.members is denied blueprint & structure tax operations even with DB/session roles.
    let mut omitted_sess = char_session(omitted_id, st_id);
    omitted_sess.corporation_id = Some(corp_id);
    omitted_sess.corp_role = u64::MAX;
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut omitted_sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(corp_bp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut omitted_sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut omitted_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(2.5)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
}

#[test]
fn industry_facilities_bps_db_first_roles_and_corp_facility_locations() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let st_id = 60_003_760_i64;
    let struct_id = 1_040_999_921_i64;
    let unreg_struct_id = 1_040_999_922_i64;
    let bp_type_id = *ctx.sde.blueprints.keys().min().expect("blueprint in SDE");
    let corp_id = 98_900_921_i64;
    let unreg_corp_id = 98_900_999_i64;
    let ceo_id = 2_900_921_i64;
    let plain_id = 2_900_922_i64;
    let div2_id = 2_900_923_i64;
    let unreg_id = 2_900_924_i64;
    let corp_bp_id = 9_900_921_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "DbRoleCorp".into();
        corp.ticker = "DRCP".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id, plain_id, div2_id]);
        corp.member_roles
            .insert(div2_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        for (cid, coid) in [
            (ceo_id, corp_id),
            (plain_id, corp_id),
            (div2_id, corp_id),
            (unreg_id, unreg_corp_id),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: coid,
                    ..Default::default()
                },
            );
        }

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "DB Role Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            unreg_struct_id,
            UpwellStructureRecord {
                structure_id: unreg_struct_id,
                type_id: 35832,
                owner_corp_id: unreg_corp_id,
                solar_system_id: 30_000_142,
                name: "Unregistered Corp Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_bp_id,
            InventoryItemRecord {
                item_id: corp_bp_id,
                type_id: bp_type_id,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. Registered corp member without DB roles is denied SetFacilityTaxes, GetBlueprintData, and corp facility locations even with stale session.corp_role.
    let mut plain_sess = char_session(plain_id, st_id);
    plain_sess.corporation_id = Some(corp_id);
    plain_sess.corp_role = u64::MAX;

    let plain_locs = handle_industry_facilities_bps(
        &ctx,
        &mut plain_sess,
        "facilityManager",
        "GetFacilityLocations",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(plain_rows) = plain_locs else {
        panic!("expected list from GetFacilityLocations");
    };
    assert!(
        !plain_rows
            .iter()
            .any(|row| row.dict_get("ownerID").and_then(PyRep::as_i64) == Some(corp_id)),
        "stale session.corp_role must not expose corp facility locations"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut plain_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.04)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut plain_sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(corp_bp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // 2. DB division take role grants only that division's corp facility location (flagID 116) even with session.corp_role == 0.
    let mut div2_sess = char_session(div2_id, st_id);
    div2_sess.corporation_id = Some(corp_id);
    div2_sess.corp_role = 0;
    let div2_locs = handle_industry_facilities_bps(
        &ctx,
        &mut div2_sess,
        "facilityManager",
        "GetFacilityLocations",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(div2_rows) = div2_locs else {
        panic!("expected list from GetFacilityLocations");
    };
    let corp_flags: Vec<i64> = div2_rows
        .iter()
        .filter(|row| row.dict_get("ownerID").and_then(PyRep::as_i64) == Some(corp_id))
        .filter_map(|row| row.dict_get("flagID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(corp_flags, vec![116]);

    // 3. Unregistered corp (>= 98M without DB CorporationRecord) is denied SetFacilityTaxes and GetBlueprintDataByOwner
    // even with ROLE_DIRECTOR in session and an orphaned corp_role_assignments row.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert(
            (unreg_corp_id, unreg_id),
            eve_db::snapshot_ext::CorpMemberRoleAssignment {
                character_id: unreg_id,
                roles: corp_roles::ROLE_DIRECTOR,
                ..Default::default()
            },
        );
    });
    let mut unreg_sess = char_session(unreg_id, st_id);
    unreg_sess.corporation_id = Some(unreg_corp_id);
    unreg_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut unreg_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(unreg_struct_id), PyRep::Real(0.04)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut unreg_sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(unreg_corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );

    // 4. NPC corp (even if CorporationRecord is seeded with CEO/director roles) emits zero corp facility locations
    // and is denied GetBlueprintDataByOwner.
    let npc_corp_id = 1_000_009_i64;
    let npc_char_id = 2_900_925_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            npc_char_id,
            eve_db::CharacterRecord {
                character_id: npc_char_id,
                corporation_id: npc_corp_id,
                ..Default::default()
            },
        );
        let mut npc_corp = CorporationRecord::default();
        npc_corp.corporation_id = npc_corp_id;
        npc_corp.ceo_id = npc_char_id;
        npc_corp.members.insert(npc_char_id);
        npc_corp.member_roles.insert(npc_char_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(npc_corp_id, npc_corp);
    });
    let mut npc_sess = char_session(npc_char_id, st_id);
    npc_sess.corporation_id = Some(npc_corp_id);
    npc_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    let npc_locs = handle_industry_facilities_bps(
        &ctx,
        &mut npc_sess,
        "facilityManager",
        "GetFacilityLocations",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(npc_rows) = npc_locs else {
        panic!("expected list from GetFacilityLocations");
    };
    assert!(
        !npc_rows
            .iter()
            .any(|row| row.dict_get("ownerID").and_then(PyRep::as_i64) == Some(npc_corp_id)),
        "NPC corp must not receive corp facility hangar locations"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut npc_sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(npc_corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );
}

#[test]
fn set_facility_taxes_rejects_seeded_npc_corp_with_ceo_and_director() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let st_id = 60_003_760_i64;
    let npc_corp_id = 1_000_009_i64;
    let npc_char_id = 2_900_991_i64;
    let npc_struct_id = 1_040_999_991_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            npc_char_id,
            eve_db::CharacterRecord {
                character_id: npc_char_id,
                corporation_id: npc_corp_id,
                ..Default::default()
            },
        );
        let mut npc_corp = CorporationRecord::default();
        npc_corp.corporation_id = npc_corp_id;
        npc_corp.name = "Seeded NPC Tax Corp".into();
        npc_corp.ticker = "SNTC".into();
        npc_corp.ceo_id = npc_char_id;
        npc_corp.members.insert(npc_char_id);
        npc_corp.member_roles.insert(
            npc_char_id,
            corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER,
        );
        snap.corporations.insert(npc_corp_id, npc_corp);
        snap.structures.insert(
            npc_struct_id,
            UpwellStructureRecord {
                structure_id: npc_struct_id,
                type_id: 35832,
                owner_corp_id: npc_corp_id,
                solar_system_id: 30_000_142,
                name: "NPC Seeded Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );
    });

    let mut npc_sess = char_session(npc_char_id, st_id);
    npc_sess.corporation_id = Some(npc_corp_id);
    npc_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

    let err = handle_industry_facilities_bps(
        &ctx,
        &mut npc_sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(npc_struct_id), PyRep::Real(0.01)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "FacilityPermissionDenied");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.owner_notes.contains_key(&facility_tax_note_id(npc_struct_id)));
    });
}

fn setup_hangar_rbac_corp(
    ctx: &ServerContext,
    corp_id: i64,
    ceo_id: i64,
    member_id: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = format!("HangarRbacCorp{corp_id}");
        corp.ticker = "HRBC".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id, member_id]);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        for cid in [ceo_id, member_id] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: corp_id,
                    ..Default::default()
                },
            );
        }
    });
}

fn set_member_roles_and_fetch_facility_flags(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    st_id: i64,
    role_bits: u64,
) -> (Vec<i64>, Vec<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap
            .corporations
            .get_mut(&corp_id)
            .expect("test corporation present");
        corp.member_roles.insert(char_id, role_bits);
    });
    let kw = HashMap::new();
    let mut sess = char_session(char_id, st_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;
    let res = handle_industry_facilities_bps(
        ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityLocations",
        &[],
        &kw,
    )
    .expect("GetFacilityLocations route handled")
    .expect("GetFacilityLocations succeeds");
    let PyRep::List(rows) = res else {
        panic!("expected list from GetFacilityLocations");
    };
    let personal_flags = rows
        .iter()
        .filter(|row| row.dict_get("ownerID").and_then(PyRep::as_i64) == Some(char_id))
        .filter_map(|row| row.dict_get("flagID").and_then(PyRep::as_i64))
        .collect();
    let corp_flags = rows
        .iter()
        .filter(|row| row.dict_get("ownerID").and_then(PyRep::as_i64) == Some(corp_id))
        .filter_map(|row| row.dict_get("flagID").and_then(PyRep::as_i64))
        .collect();
    (personal_flags, corp_flags)
}

#[test]
fn get_facility_locations_denies_unrelated_corp_roles_without_hangar_access() {
    let ctx = test_ctx();
    let st_id = 60_003_760_i64;
    let corp_id = 98_901_492_i64;
    let ceo_id = 2_901_491_i64;
    let member_id = 2_901_492_i64;
    setup_hangar_rbac_corp(&ctx, corp_id, ceo_id, member_id);

    let non_hangar_roles: [(&str, u64); 19] = [
        ("none", 0),
        ("ROLE_PERSONNEL_MANAGER", corp_roles::ROLE_PERSONNEL_MANAGER),
        ("ROLE_ACCOUNTANT", corp_roles::ROLE_ACCOUNTANT),
        ("ROLE_SECURITY_OFFICER", corp_roles::ROLE_SECURITY_OFFICER),
        ("ROLE_STATION_MANAGER", corp_roles::ROLE_STATION_MANAGER),
        ("ROLE_AUDITOR", corp_roles::ROLE_AUDITOR),
        ("ROLE_EQUIPMENT_CONFIG", corp_roles::ROLE_EQUIPMENT_CONFIG),
        ("ROLE_CHAT_MANAGER", corp_roles::ROLE_CHAT_MANAGER),
        ("ROLE_JUNIOR_ACCOUNTANT", corp_roles::ROLE_JUNIOR_ACCOUNTANT),
        ("ROLE_DELIVERIES_HANGAR_TAKE", corp_roles::ROLE_DELIVERIES_HANGAR_TAKE),
        ("ROLE_DELIVERIES_HANGAR_QUERY", corp_roles::ROLE_DELIVERIES_HANGAR_QUERY),
        (
            "ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE",
            corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE,
        ),
        ("ROLE_ACCOUNT_CAN_TAKE[0]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]),
        ("ROLE_ACCOUNT_CAN_TAKE[1]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]),
        ("ROLE_ACCOUNT_CAN_TAKE[2]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]),
        ("ROLE_ACCOUNT_CAN_TAKE[3]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[3]),
        ("ROLE_ACCOUNT_CAN_TAKE[4]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[4]),
        ("ROLE_ACCOUNT_CAN_TAKE[5]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[5]),
        ("ROLE_ACCOUNT_CAN_TAKE[6]", corp_roles::ROLE_ACCOUNT_CAN_TAKE[6]),
    ];

    let mut combined_non_hangar = 0_u64;
    for (name, role_bits) in non_hangar_roles {
        combined_non_hangar |= role_bits;
        let (personal_flags, corp_flags) =
            set_member_roles_and_fetch_facility_flags(&ctx, corp_id, member_id, st_id, role_bits);
        assert_eq!(personal_flags, vec![4], "personal flag missing for {name}");
        assert!(
            corp_flags.is_empty(),
            "role {name} unexpectedly granted corp hangar access: {corp_flags:?}"
        );
    }

    let (personal_flags, corp_flags) = set_member_roles_and_fetch_facility_flags(
        &ctx,
        corp_id,
        member_id,
        st_id,
        combined_non_hangar,
    );
    assert_eq!(personal_flags, vec![4]);
    assert!(
        corp_flags.is_empty(),
        "combined non-hangar roles unexpectedly granted access: {corp_flags:?}"
    );
}

#[test]
fn get_facility_locations_allows_exact_corp_hangar_divisions_one_through_seven() {
    let ctx = test_ctx();
    let st_id = 60_003_760_i64;
    let corp_id = 98_901_493_i64;
    let ceo_id = 2_901_493_i64;
    let member_id = 2_901_494_i64;
    setup_hangar_rbac_corp(&ctx, corp_id, ceo_id, member_id);

    for (div_idx, expected_flag) in (115_i64..=121).enumerate() {
        for (kind, role_bits) in [
            ("take", corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]),
            ("query", corp_roles::ROLE_HANGAR_CAN_QUERY[div_idx]),
            ("container_take", corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx]),
        ] {
            let (personal_flags, corp_flags) = set_member_roles_and_fetch_facility_flags(
                &ctx,
                corp_id,
                member_id,
                st_id,
                role_bits,
            );
            assert_eq!(
                personal_flags,
                vec![4],
                "personal flag missing for div {} ({kind})",
                div_idx + 1
            );
            assert_eq!(
                corp_flags,
                vec![expected_flag],
                "div {} ({kind}) must allow only flagID {expected_flag}",
                div_idx + 1
            );
        }
    }

    let all_div_flags: Vec<i64> = (115_i64..=121).collect();
    for (label, cid, role_bits) in [
        ("ROLE_DIRECTOR", member_id, corp_roles::ROLE_DIRECTOR),
        ("ROLE_FACTORY_MANAGER", member_id, corp_roles::ROLE_FACTORY_MANAGER),
        ("CEO", ceo_id, 0),
    ] {
        let (personal_flags, corp_flags) =
            set_member_roles_and_fetch_facility_flags(&ctx, corp_id, cid, st_id, role_bits);
        assert_eq!(personal_flags, vec![4], "personal flag missing for {label}");
        assert_eq!(
            corp_flags, all_div_flags,
            "{label} must retain access to all 7 corp hangar divisions"
        );
    }
}

#[cfg(test)]
#[path = "industry_facilities_bps_extra_unit_tests.rs"]
mod extra_unit_tests;
