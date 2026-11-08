//! Semantic unit tests split from `industry_facilities_bps_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::industry_pi::industry_facilities_bps::*;
use eve_db::{
    ContractRecord, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
    corp_roles,
};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[test]
fn blueprint_data_corp_permissions_contract_visibility_and_nested_container_resolution() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    let bp_tid = *ctx.sde.blueprints.keys().next().unwrap_or(&683);
    let container_id = 890_100_i64;
    let corp_bp_id = 890_101_i64;
    let other_bp_id = 890_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "BP Corp".into(),
                ticker: "BPC".into(),
                ceo_id: other_id, // char_id is NOT CEO initially
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;

        // Container in station, BPC inside container (impounded flag test + BPC runs/quantity codes)
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 3293, // Medium Standard Container
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_bp_id,
            InventoryItemRecord {
                item_id: corp_bp_id,
                type_id: bp_tid,
                owner_id: corp_id,
                location_id: container_id,
                flag: InventoryFlag::Impounded,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(7),
                ..Default::default()
            },
        );
        snap.items.insert(
            other_bp_id,
            InventoryItemRecord {
                item_id: other_bp_id,
                type_id: bp_tid,
                owner_id: other_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, Some(st_id));
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;

    // 1. Without factory/director roles, querying corp blueprints fails with OwnerAccessDenied
    let err_corp = call_ind_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(corp_id)],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_corp.code, "OwnerAccessDenied");

    // 2. Grant ROLE_FACTORY_MANAGER -> 1-arg call returns List with BPC resolved to st_id facility!
    sess.corp_role = corp_roles::ROLE_FACTORY_MANAGER;
    let corp_bps = call_ind_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(corp_id)],
        &kw,
    )
    .unwrap();
    let PyRep::List(list) = corp_bps else {
        panic!("expected 1-arg GetBlueprintDataByOwner to return List");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].dict_get("facilityID"), Some(&PyRep::Integer(st_id)));
    assert_eq!(list[0].dict_get("original"), Some(&PyRep::Bool(false)));
    assert_eq!(list[0].dict_get("runs"), Some(&PyRep::Integer(7)));
    assert_eq!(list[0].dict_get("quantity"), Some(&PyRep::Integer(-2)));
    assert_eq!(list[0].dict_get("isImpounded"), Some(&PyRep::Bool(true)));

    // 3. GetBlueprintData: missing arg -> InvalidBlueprintTypeID; other player's BP not in contract -> CrpAccessDenied
    assert_eq!(
        call_ind_bps(&ctx, &mut sess, "blueprintManager", "GetBlueprintData", &[], &kw)
            .unwrap_err()
            .code,
        "InvalidBlueprintTypeID"
    );
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(other_bp_id)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Put other_bp_id in a Public contract -> GetBlueprintData succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            701,
            ContractRecord {
                contract_id: 701,
                issuer_id: other_id,
                issuer_corp_id: 1_000_009,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: st_id,
                end_station_id: st_id,
                price: IskCents::from_isk(1_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                title: "Public BPO".into(),
                for_corp: false,
                item_ids: vec![other_bp_id],
                requested_items: Vec::new(),
                ..Default::default()
            },
        );
    });
    let pub_bp = call_ind_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(other_bp_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(pub_bp.dict_get("blueprintID"), Some(&PyRep::Integer(other_bp_id)));
}

#[test]
fn facility_manager_limits_locations_and_set_facility_taxes_dict_keyval_and_roles() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_600_002_i64;
    let st_id = 60_003_760_i64;
    let struct_id = 1_000_000_991_055_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(3387, TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 }); // Mass Production V (+5)
        smap.insert(24625, TrainedSkillRecord { type_id: 24625, level: 4, skill_points: 362_039.0 }); // Adv Mass Production IV (+4)
        smap.insert(24268, TrainedSkillRecord { type_id: 24268, level: 3, skill_points: 24_000.0 }); // Supply Chain Mgmt III (15 jumps)

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Tax Corp".into(),
                ticker: "TXC".into(),
                ceo_id: other_id,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Tax Athanor".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(char_id, Some(st_id));
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;

    let limits = call_ind_bps(&ctx, &mut sess, "blueprintManager", "GetLimits", &[], &kw).unwrap();
    assert_eq!(limits.dict_get("maxManufacturingJobs"), Some(&PyRep::Integer(10)));
    assert_eq!(limits.dict_get("manufacturingRangeJumps"), Some(&PyRep::Integer(15)));

    // GetFacilitiesByID with explicit list & GetFacilityLocations with list
    let by_id = call_ind_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilitiesByID",
        &[PyRep::List(vec![PyRep::Integer(st_id), PyRep::Integer(struct_id)])],
        &kw,
    )
    .unwrap();
    let PyRep::List(ref fac_items) = by_id else {
        panic!("expected List from GetFacilitiesByID");
    };
    assert_eq!(fac_items.len(), 2);
    for fac in fac_items {
        assert!(matches!(fac.dict_get("serviceAccess"), Some(PyRep::Dict(_))));
        assert_eq!(fac.dict_get("sccTaxModifier"), Some(&PyRep::Real(1.0)));
        assert!(matches!(fac.dict_get("rigModifiers"), Some(PyRep::Dict(_))));
        let Some(PyRep::Dict(gm)) = fac.dict_get("globalModifiers") else {
            panic!("expected globalModifiers dict");
        };
        assert!(gm.iter().any(|(k, _)| k.as_str() == Some("faction")));
        assert!(gm.iter().any(|(k, v)| k.as_str() == Some("system") && matches!(v, PyRep::Dict(_))));
    }

    let locs_list = call_ind_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityLocations",
        &[PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(struct_id)])],
        &kw,
    )
    .unwrap();
    assert!(matches!(locs_list, PyRep::List(ref l) if l.len() == 2));

    // Unknown facility -> FacilityNotFound on GetFacility and GetFacilityTaxes
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "GetFacility",
            &[PyRep::Integer(-999_999)],
            &kw,
        )
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "GetFacilityTaxes",
            &[PyRep::Integer(-999_999)],
            &kw,
        )
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );

    // SetFacilityTaxes permission & input validation branches
    assert_eq!(
        call_ind_bps(&ctx, &mut sess, "facilityManager", "SetFacilityTaxes", &[], &kw)
            .unwrap_err()
            .code,
        "InvalidFacilityID"
    );
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(-1.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );

    // Member of owning corp without Director or Station Manager role -> InsufficientCorpRoles
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.04)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    // Character in another corp -> FacilityPermissionDenied
    let mut outsider_sess = char_session(other_id, Some(st_id));
    outsider_sess.corporation_id = Some(1_000_009);
    assert_eq!(
        call_ind_bps(
            &ctx,
            &mut outsider_sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Real(0.04)],
            &kw,
        )
        .unwrap_err()
        .code,
        "FacilityPermissionDenied"
    );

    // Grant ROLE_STATION_MANAGER and test KeyVal and Dict tax updates (including ppm > 100.0 normalization)
    sess.corp_role = corp_roles::ROLE_STATION_MANAGER;
    let kv_taxes = PyRep::keyval([
        ("manufacturing", PyRep::Real(40_000.0)), // 40,000 ppm -> 0.04 (4%)
        ("reaction", PyRep::Real(0.06)),
    ]);
    let set_kv = call_ind_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), kv_taxes],
        &kw,
    )
    .unwrap();
    assert_eq!(set_kv.dict_get("1"), Some(&PyRep::Real(0.04)));
    assert_eq!(set_kv.dict_get("9"), Some(&PyRep::Real(0.06)));

    let dict_taxes = PyRep::Dict(vec![
        (PyRep::Integer(1), PyRep::Real(0.02)),
        (PyRep::String("invention".into()), PyRep::Real(0.08)),
    ]);
    let set_dict = call_ind_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Integer(corp_id), dict_taxes],
        &kw,
    )
    .unwrap();
    assert_eq!(set_dict.dict_get("1"), Some(&PyRep::Real(0.02)));
    assert_eq!(set_dict.dict_get("8"), Some(&PyRep::Real(0.08)));
}
