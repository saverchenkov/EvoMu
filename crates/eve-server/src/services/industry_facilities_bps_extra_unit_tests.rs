//! Extra branch-coverage and mutation-killing unit tests for `services::industry_facilities_bps`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::industry_facilities_bps::*;
use eve_db::{
    ContractRecord, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
    corp_roles,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
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

fn char_session(char_id: i64, st_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: st_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn call_ind_bps(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_industry_facilities_bps(ctx, session, service, method, args, kwargs)
        .expect("industry_facilities_bps method must be dispatched")
}

#[test]
fn structure_reaction_reactor_matching_fulcrum_pirate_bonus_and_fw_tier_tax_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let struct_id = 1_000_000_991_001_i64;
    let mod_id = 890_001_i64;

    // Find a reaction formula blueprint in group 1888 (Composite), 1889 (Hybrid), or 1890 (Biochemical)
    let comp_bp = ctx
        .sde
        .blueprints
        .keys()
        .copied()
        .find(|&tid| ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 1888))
        .unwrap_or(46166);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835, // Athanor refinery
                owner_corp_id: 98_000_001,
                solar_system_id: 30_002_537, // lowsec/nullsec system
                name: "Refinery".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );

        // Install Standup Hybrid Reactor I (45538) in ServiceSlot0 (164)
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 45538,
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);

        // Generic reaction activity 9 is online, but Composite formula (1888) requires 45537!
        assert!(is_structure_industry_service_online(snap, struct_id, 9));
        if ctx.sde.types.get(&comp_bp).is_some_and(|t| t.group_id == 1888) {
            assert!(!is_structure_industry_service_online_for_bp(
                &ctx.sde, snap, struct_id, 9, comp_bp
            ));
            snap.items.get_mut(&mod_id).unwrap().type_id = 45537;
            assert!(is_structure_industry_service_online_for_bp(
                &ctx.sde, snap, struct_id, 9, comp_bp
            ));
        }

        // Dynamic attribute 2 == 0.0 marks module offline
        snap.items
            .get_mut(&mod_id)
            .unwrap()
            .dynamic_attributes
            .insert(2, 0.0);
        assert!(!is_structure_industry_service_online(snap, struct_id, 9));
        snap.items.get_mut(&mod_id).unwrap().dynamic_attributes.clear();

        // No fuel or no quantum core -> offline
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 0;
        assert!(!is_structure_industry_service_online(snap, struct_id, 9));
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 100;
        snap.structures
            .get_mut(&struct_id)
            .unwrap()
            .quantum_core_installed = false;
        assert!(!is_structure_industry_service_online(snap, struct_id, 9));
        snap.structures
            .get_mut(&struct_id)
            .unwrap()
            .quantum_core_installed = true;

        // Fulcrum pirate subcap bonus (facility 60_015_187, war_faction_id 500_010 or 500_011)
        if let Some((&pirate_bp_tid, _)) = ctx.sde.blueprints.iter().find(|(_, bp)| {
            ctx.sde.types.get(&bp.product_type_id).is_some_and(|t| {
                t.category_id == 6
                    && matches!(t.faction_id, Some(500_010 | 500_011))
                    && !matches!(
                        t.group_id,
                        30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594 | 5120
                    )
            })
        }) {
            snap.characters.get_mut(&char_id).unwrap().war_faction_id = None;
            assert!(!is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(char_id),
                pirate_bp_tid,
                1,
                60_015_187
            ));
            snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_010);
            assert!(is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(char_id),
                pirate_bp_tid,
                1,
                60_015_187
            ));
            // Activity 3 (TE research) does not get Fulcrum bonus
            assert!(!is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(char_id),
                pirate_bp_tid,
                3,
                60_015_187
            ));

            let normal_cost = compute_industry_job_costs_for_char(
                &ctx,
                snap,
                Some(char_id),
                pirate_bp_tid,
                1,
                1,
                60_003_760,
                false,
            )
            .unwrap();
            let fulcrum_cost = compute_industry_job_costs_for_char(
                &ctx,
                snap,
                Some(char_id),
                pirate_bp_tid,
                1,
                1,
                60_015_187,
                false,
            )
            .unwrap();
            assert!(fulcrum_cost.scc_surcharge_isk < normal_cost.scc_surcharge_isk);
        }

        // FW tier facility tax discount vs enemy militia exemption
        let bp_tid = *ctx
            .sde
            .blueprints
            .iter()
            .find(|(_, b)| !b.materials.is_empty())
            .map(|(k, _)| k)
            .unwrap_or(&683);
        let jita_st = 60_003_760_i64;
        let jita_sys = 30_000_142_u32;
        snap.fw_runtime.ihub_tiers.insert(jita_sys, 3);
        snap.fw_system_occupations.insert(
            jita_sys,
            FwSystemOccupationRecord {
                solar_system_id: jita_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001, // Caldari
                victory_points: 0,
                victory_points_threshold: 3000,
            },
        );
        // Friendly Caldari pilot gets tier 3 discount
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
        let friendly_cost = compute_industry_job_costs_for_char(
            &ctx,
            snap,
            Some(char_id),
            bp_tid,
            1,
            10,
            jita_st,
            false,
        )
        .unwrap();
        // Enemy Gallente pilot (500_004) in Caldari system gets fw_tier = 0 (no discount)
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_004);
        let enemy_cost = compute_industry_job_costs_for_char(
            &ctx,
            snap,
            Some(char_id),
            bp_tid,
            1,
            10,
            jita_st,
            false,
        )
        .unwrap();
        assert!(friendly_cost.facility_tax_isk <= enemy_cost.facility_tax_isk);
    });
}

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
    assert!(matches!(by_id, PyRep::List(ref l) if l.len() == 2));

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
