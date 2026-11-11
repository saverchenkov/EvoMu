//! Additional edge-case and branch coverage tests for `services::industry_facilities_bps`.

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, FwSystemOccupationRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, UpwellStructureRecord, WorldRepository, corp_roles,
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

fn char_session(char_id: i64, corp_id: i64, st_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: st_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

#[test]
fn tax_note_job_cost_meta_eiv_and_cost_index_edges() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;

    // 1. resolve_caller_facility_id fallback order
    let mut sess = SessionState {
        station_id: Some(60_003_760),
        structure_id: Some(1_000_000_990_001),
        ..Default::default()
    };
    assert_eq!(resolve_caller_facility_id(&ctx, &sess, char_id, Some(60_003_761)), 60_003_761);
    assert_eq!(resolve_caller_facility_id(&ctx, &sess, char_id, None), 60_003_760);
    sess.station_id = None;
    assert_eq!(resolve_caller_facility_id(&ctx, &sess, char_id, None), 1_000_000_990_001);
    sess.structure_id = None;
    // Unknown char_id returns 0
    assert_eq!(resolve_caller_facility_id(&ctx, &sess, 999_888_777, None), 0);

    // 2. job_cost_note_id, save_job_cost_meta, load_job_cost_meta (missing / corrupt / roundtrip)
    assert_eq!(job_cost_note_id(42), -2_000_000_000_042);
    ctx.db.with_mut_snapshot(|snap| {
        assert!(load_job_cost_meta(snap, 42).is_none());
        let meta = IndustryJobCostMeta {
            eiv_isk: 99_000.0,
            system_cost_index: 0.025,
            structure_cost_modifier: 1.0,
            base_job_cost_isk: 1_234.5,
            facility_tax_rate: 0.01,
            facility_tax_isk: 56.0,
            scc_surcharge_isk: 40.0,
            alpha_clone_tax_isk: 25.0,
            total_cost_isk: 1_355.5,
            decryptor_type_id: None,
            product_type_id: 587,
            ..Default::default()
        };
        save_job_cost_meta(snap, 42, &meta);
        let loaded = load_job_cost_meta(snap, 42).expect("saved job cost meta");
        assert!((loaded.base_job_cost_isk - 1_234.5).abs() < 1e-6);
        assert!((loaded.alpha_clone_tax_isk - 25.0).abs() < 1e-6);

        // Corrupt JSON note yields None
        snap.owner_notes.insert(
            job_cost_note_id(43),
            eve_db::OwnerNoteRecord {
                note_id: job_cost_note_id(43),
                character_id: -3,
                item_id: 43,
                label: "bad".into(),
                note: "{not valid json".into(),
                timestamp_unix: now,
            },
        );
        assert!(load_job_cost_meta(snap, 43).is_none());

        // has_custom_facility_taxes & save_facility_taxes
        assert!(!has_custom_facility_taxes(snap, 60_003_760));
        let mut custom = HashMap::new();
        custom.insert(1_u8, 0.035_f64);
        save_facility_taxes(snap, 60_003_760, &custom);
        assert!(has_custom_facility_taxes(snap, 60_003_760));
        let reloaded = load_facility_taxes(snap, 60_003_760);
        assert!((reloaded[&1] - 0.035).abs() < 1e-9);
    });

    // 3. compute_blueprint_eiv_per_run & canonical cost index edges
    let bp_type_id = *ctx.sde.blueprints.keys().next().unwrap();
    let bp_info = ctx.sde.blueprints.get(&bp_type_id).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let eiv = compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), bp_info, 1);
        assert!(eiv >= 0.0);
        let eiv_rxn = compute_blueprint_eiv_per_run(&ctx, bp_info, 9);
        assert!(eiv_rxn >= 0.0);

        // compute_canonical_system_cost_index: empty -> 0.01, Cancelled/Reverted/>28d excluded, floor 0.001 & ceiling 0.50
        let sys_id = 30_000_144_u32;
        let struct_144 = 1_000_000_770_001_i64;
        snap.structures.insert(
            struct_144,
            UpwellStructureRecord {
                structure_id: struct_144,
                type_id: 35825,
                owner_corp_id: 98_000_144,
                solar_system_id: sys_id,
                name: "SCI Test Raitaru".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let base = compute_canonical_system_cost_index(&ctx, snap, sys_id, 1);
        assert!((base - 0.01).abs() < 1e-9);

        for (idx, (status, start_off)) in [
            ("Cancelled", 100_u64),
            ("Reverted", 100),
            ("Active", 29 * 86_400),
        ]
        .into_iter()
        .enumerate()
        {
            snap.industry_jobs.insert(
                770_000 + idx as i64,
                IndustryJobRecord {
                    job_id: 770_000 + idx as i64,
                    installer_id: char_id,
                    facility_id: struct_144,
                    blueprint_item_id: 900_001 + idx as i64,
                    blueprint_type_id: bp_type_id,
                    activity_id: 1,
                    runs: 500,
                    product_type_id: bp_info.product_type_id,
                    status: status.into(),
                    duration_seconds: 3_600,
                    start_unix: now.saturating_sub(start_off),
                    ..Default::default()
                },
            );
        }
        let still_empty = compute_canonical_system_cost_index(&ctx, snap, sys_id, 1);
        assert!((still_empty - 0.01).abs() < 1e-9);

        // Tiny work in sys_id vs huge work in 30_000_142 hits 0.001 floor
        snap.industry_jobs.insert(
            770_090,
            IndustryJobRecord {
                job_id: 770_090,
                installer_id: char_id,
                facility_id: struct_144,
                blueprint_item_id: 900_090,
                blueprint_type_id: bp_type_id,
                activity_id: 1,
                runs: 1,
                product_type_id: bp_info.product_type_id,
                status: "Active".into(),
                duration_seconds: 1,
                start_unix: now.saturating_sub(60),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            770_091,
            IndustryJobRecord {
                job_id: 770_091,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 900_091,
                blueprint_type_id: bp_type_id,
                activity_id: 1,
                runs: 1,
                product_type_id: bp_info.product_type_id,
                status: "Active".into(),
                duration_seconds: 1_000_000_000,
                start_unix: now.saturating_sub(60),
                ..Default::default()
            },
        );
        let floored = compute_canonical_system_cost_index(&ctx, snap, sys_id, 1);
        assert!((floored - 0.001).abs() < 1e-9);

        // Massive recent active job in sys_id hits 0.50 ceiling
        snap.industry_jobs.insert(
            770_099,
            IndustryJobRecord {
                job_id: 770_099,
                installer_id: char_id,
                facility_id: struct_144,
                blueprint_item_id: 900_099,
                blueprint_type_id: bp_type_id,
                activity_id: 1,
                runs: 100_000_000,
                product_type_id: bp_info.product_type_id,
                status: "Active".into(),
                duration_seconds: 1_000_000_000_000,
                start_unix: now.saturating_sub(60),
                ..Default::default()
            },
        );
        let capped = compute_canonical_system_cost_index(&ctx, snap, sys_id, 1);
        assert!((capped - 0.50).abs() < 1e-9);
    });

    // 4. compute_industry_job_costs_with_step: enemy vs friendly militia & alpha surcharge & UnknownBlueprint
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.clone_grade = 1; // Alpha
        ch.war_faction_id = Some(500_001); // Caldari
        snap.characters.insert(char_id, ch);

        snap.fw_system_occupations.insert(
            30_000_142,
            FwSystemOccupationRecord {
                solar_system_id: 30_000_142,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004, // Gallente enemy
                victory_points: 10_000,
                victory_points_threshold: 20_000,
            },
        );

        let cost_enemy = compute_industry_job_costs_with_step(
            &ctx,
            snap,
            Some(char_id),
            bp_type_id,
            1,
            0,
            2,
            60_003_760,
            true,
        )
        .expect("cost calc enemy");
        assert!(cost_enemy.alpha_clone_tax_isk >= 0.0);

        snap.fw_system_occupations.insert(
            30_000_142,
            FwSystemOccupationRecord {
                solar_system_id: 30_000_142,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001, // Friendly Caldari
                victory_points: 10_000,
                victory_points_threshold: 20_000,
            },
        );
        let cost_friendly = compute_industry_job_costs_with_step(
            &ctx,
            snap,
            Some(char_id),
            bp_type_id,
            1,
            0,
            2,
            60_003_760,
            true,
        )
        .expect("cost calc friendly");
        assert!(cost_friendly.facility_tax_isk <= cost_enemy.facility_tax_isk);

        let err = compute_industry_job_costs_with_step(
            &ctx,
            snap,
            None,
            999_999_777,
            1,
            0,
            1,
            60_003_760,
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, "UnknownBlueprint");
    });
}

#[test]
fn blueprint_data_by_owner_limits_and_locations_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let bp_type_id = *ctx.sde.blueprints.keys().next().unwrap();
    let prod_type_id = ctx.sde.blueprints.get(&bp_type_id).unwrap().product_type_id;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Test Ind Corp".into(); __corp.ceo_id = other_char_id; __corp.ensure_wallet_divisions(); __corp };
        corp.member_roles
            .insert(char_id, corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.corporation_id = corp_id;
        snap.characters.insert(char_id, ch);

        // Corp contract BPC accessible vs inaccessible personal BP
        snap.items.insert(
            810_001,
            InventoryItemRecord {
                item_id: 810_001,
                type_id: bp_type_id,
                owner_id: other_char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(15),
                dynamic_attributes: HashMap::from([
                    (ATTR_BLUEPRINT_ME, 4.0),
                    (ATTR_BLUEPRINT_TE, 8.0),
                ]),
                ..Default::default()
            },
        );
        snap.contracts.insert(
            550_001,
            eve_db::ContractRecord {
                contract_id: 550_001,
                issuer_id: other_char_id,
                issuer_corp_id: corp_id,
                availability: "Public".into(),
                status: "Outstanding".into(),
                item_ids: vec![810_001],
                ..Default::default()
            },
        );

        // Private BP owned by other_char_id without contract -> CrpAccessDenied
        snap.items.insert(
            810_002,
            InventoryItemRecord {
                item_id: 810_002,
                type_id: bp_type_id,
                owner_id: other_char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        // Nested container BP owned by char_id inside station hangar
        snap.items.insert(
            810_003,
            InventoryItemRecord {
                item_id: 810_003,
                type_id: bp_type_id,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            810_004,
            InventoryItemRecord {
                item_id: 810_004,
                type_id: bp_type_id,
                owner_id: char_id,
                location_id: 810_003,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        // Corp-owned BP + active job + impounded BP
        snap.items.insert(
            810_005,
            InventoryItemRecord {
                item_id: 810_005,
                type_id: bp_type_id,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            810_006,
            InventoryItemRecord {
                item_id: 810_006,
                type_id: bp_type_id,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            770_500,
            IndustryJobRecord {
                job_id: 770_500,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: 810_005,
                blueprint_type_id: bp_type_id,
                activity_id: 1,
                runs: 1,
                product_type_id: prod_type_id,
                status: "Active".into(),
                duration_seconds: 600,
                start_unix: now.saturating_sub(60),
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, corp_id, Some(station_id));

    // GetBlueprintData error paths
    assert_eq!(
        handle_industry_facilities_bps(&ctx, &mut sess, "blueprintManager", "GetBlueprintData", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidBlueprintTypeID"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(999_999_000)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownBlueprint"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(810_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // GetBlueprintData via contract access -> ME = 4, TE = 8, EIV metadata
    let bp_data = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(810_001)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bp_data.dict_get("materialEfficiency"), Some(&PyRep::Integer(4)));
    assert_eq!(bp_data.dict_get("timeEfficiency"), Some(&PyRep::Integer(8)));
    assert!(bp_data.dict_get("estimatedItemValueISK").is_some());

    // GetBlueprintDataByOwner: unauthorized owner & NPC corp -> OwnerAccessDenied
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(other_char_id)],
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
            &mut sess,
            "blueprintManager",
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(1_000_009)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OwnerAccessDenied"
    );

    // GetBlueprintDataByOwner with facilityID filter returns (blueprints, facility_counts)
    let mut fac_kw = HashMap::new();
    fac_kw.insert("facilityID".into(), PyRep::Integer(station_id));
    let filtered = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(corp_id)],
        &fac_kw,
    )
    .unwrap()
    .unwrap();
    match filtered {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            match &items[0] {
                PyRep::List(bps) => {
                    assert!(bps.iter().any(|b| b.dict_get("itemID") == Some(&PyRep::Integer(810_005))
                        && b.dict_get("jobID") == Some(&PyRep::Integer(770_500))));
                    assert!(!bps.iter().any(|b| b.dict_get("itemID") == Some(&PyRep::Integer(810_006))));
                }
                other => panic!("expected list, got {other:?}"),
            }
        }
        other => panic!("expected tuple, got {other:?}"),
    }

    // GetLimits
    let limits = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetLimits",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(limits.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);

    // GetFacilityLocations with explicit facility_ids
    let mut sess_corp = sess.clone();
    sess_corp.corp_role = corp_roles::ROLE_DIRECTOR;
    let locs = handle_industry_facilities_bps(
        &ctx,
        &mut sess_corp,
        "facilityManager",
        "GetFacilityLocations",
        &[PyRep::List(vec![PyRep::Integer(station_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    match locs {
        PyRep::List(rows) => {
            assert!(rows.iter().any(|r| r.dict_get("facilityID") == Some(&PyRep::Integer(station_id))
                && r.dict_get("flagID") == Some(&PyRep::Integer(4))));
        }
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn facility_taxes_permissions_formats_and_activity_modifiers_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_002_i64;
    let struct_id = 1_000_000_991_001_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Tax Test Corp".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp };
        corp.member_roles
            .insert(char_id, corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.corporation_id = corp_id;
        snap.characters.insert(char_id, ch);

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Tax Raitaru".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, corp_id, Some(struct_id));

    // GetFacility / GetFacilityTaxes FacilityNotFound
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "GetFacility",
            &[PyRep::Integer(777_666_555)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "GetFacilityTaxes",
            &[PyRep::Integer(777_666_555)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );

    // GetFacilitiesByID filters unknown IDs
    let by_id = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilitiesByID",
        &[PyRep::List(vec![PyRep::Integer(struct_id), PyRep::Integer(777_666_555)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    match by_id {
        PyRep::List(rows) => assert_eq!(rows.len(), 1),
        other => panic!("expected list, got {other:?}"),
    }

    // SetFacilityTaxes validation error paths
    for (args, expected_code) in [
        (vec![PyRep::String("bad".into()), PyRep::Real(0.05)], "InvalidFacilityID"),
        (vec![PyRep::Integer(0), PyRep::Real(0.05)], "FacilityNotFound"),
        (vec![PyRep::Integer(struct_id), PyRep::Real(-0.01)], "InvalidFacilityTaxRate"),
        (vec![PyRep::Integer(struct_id), PyRep::Real(f64::NAN)], "InvalidFacilityTaxRate"),
        (vec![PyRep::Integer(struct_id), PyRep::List(vec![])], "InvalidFacilityTaxRate"),
        (vec![PyRep::Integer(777_666_555), PyRep::Real(0.05)], "FacilityNotFound"),
    ] {
        let err = handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &args,
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code, "args={args:?}");
    }

    // Non-owner corp permission denied
    let mut outsider_sess = char_session(other_char_id, 98_999_999, Some(struct_id));
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut outsider_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityPermissionDenied"
    );

    // Member without role -> InsufficientCorpRoles
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&other_char_id).unwrap().clone();
        ch.corporation_id = corp_id;
        snap.characters.insert(other_char_id, ch);
    });
    let mut no_role_sess = char_session(other_char_id, corp_id, Some(struct_id));
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut no_role_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    // PPM normalization (25_000.0 -> 0.025) & 3-arg form
    let ok_bp = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(25_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_bp.dict_get("facilityID").and_then(|v| v.as_i64()), Some(struct_id));
    assert_eq!(ok_bp.dict_get("tax").and_then(|v| v.as_f64()), Some(0.025));

    let taxes_dict = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityTaxes",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        taxes_dict.dict_get("manufacturing").and_then(|v| v.as_f64()),
        Some(0.025)
    );

    let ok_3arg = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Integer(4), PyRep::Real(5.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_3arg.dict_get("facilityID").and_then(|v| v.as_i64()), Some(struct_id));
    assert_eq!(ok_3arg.dict_get("4").and_then(|v| v.as_f64()), Some(0.05));

    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(1_100_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );

    // GetMaxActivityModifiers & GetIndustryCostModifier & GetIndustryJobsOverLast24Hours
    let max_mods = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetMaxActivityModifiers",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(max_mods
        .dict_get("materialMultiplier")
        .and_then(|v| v.as_f64())
        .is_some_and(|v| (0.01..=1.0).contains(&v)));
    assert!(max_mods
        .dict_get("timeMultiplier")
        .and_then(|v| v.as_f64())
        .is_some_and(|v| (0.01..=1.0).contains(&v)));
    assert!(max_mods
        .dict_get("costMultiplier")
        .and_then(|v| v.as_f64())
        .is_some_and(|v| (0.01..=1.0).contains(&v)));

    let cost_mod = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "map",
        "GetIndustryCostModifier",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(cost_mod.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(1));
    assert!(cost_mod.dict_get("1").and_then(|v| v.as_f64()).is_some_and(|v| v >= 0.0));
    assert!(cost_mod.dict_get("manufacturing").and_then(|v| v.as_f64()).is_some_and(|v| v >= 0.0));

    let jobs_24h = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "map",
        "GetIndustryJobsOverLast24Hours",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    match jobs_24h {
        PyRep::Rowset { columns, .. } => {
            assert_eq!(columns, vec!["solarSystemID".to_string(), "noOfJobs".to_string()]);
        }
        other => panic!("expected rowset, got {other:?}"),
    }
}
