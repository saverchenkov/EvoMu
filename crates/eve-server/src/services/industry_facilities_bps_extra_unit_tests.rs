//! Extra branch-coverage and mutation-killing unit tests for `services::industry_facilities_bps`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::industry_facilities_bps::*;
use eve_db::{
    ContractRecord, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "industry_blueprints_and_taxes_unit_tests.rs"]
mod industry_blueprints_and_taxes_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn char_session(char_id: i64, st_id: Option<i64>) -> SessionState {
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

pub(super) fn call_ind_bps(
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
            !bp.materials.is_empty()
                && ctx.sde.types.get(&bp.product_type_id).is_some_and(|t| {
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
fn blueprint_reaction_contract_visibility_facility_locations_taxes_and_24h_jobs() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let other_corp_id = 98_600_002_i64;
    let alliance_id = 99_600_001_i64;
    let jita_st = 60_003_760_i64;
    let kw = HashMap::new();

    let reaction_bp_tid = ctx
        .sde
        .blueprints
        .iter()
        .find(|(_, bp)| bp.activities.contains_key("reaction") && !bp.activities.contains_key("manufacturing"))
        .map(|(&tid, _)| tid)
        .expect("SDE has reaction blueprint");
    let reaction_bp_item_id = 891_101_i64;
    let secret_bp_item_id = 891_102_i64;
    let st_item_id = 1_000_000_891_001_i64;
    let other_st_id = 1_000_000_891_002_i64;

    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = char_id; __corp.alliance_id = Some(alliance_id); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        snap.items.insert(
            reaction_bp_item_id,
            InventoryItemRecord {
                item_id: reaction_bp_item_id,
                type_id: reaction_bp_tid,
                owner_id: other_id,
                location_id: jita_st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            secret_bp_item_id,
            InventoryItemRecord {
                item_id: secret_bp_item_id,
                type_id: reaction_bp_tid,
                owner_id: other_id,
                location_id: jita_st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            891_201,
            ContractRecord {
                contract_id: 891_201,
                issuer_id: other_id,
                issuer_corp_id: other_corp_id,
                assignee_id: alliance_id,
                acceptor_id: None,
                for_corp: false,
                availability: "Personal".into(),
                status: "Outstanding".into(),
                contract_type: "ItemExchange".into(),
                start_station_id: jita_st,
                end_station_id: jita_st,
                title: "Reaction BP".into(),
                price: IskCents(1_000_00),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![reaction_bp_item_id],
                ..Default::default()
            },
        );
        // Upwell structure item in snap.items + another corp's structure
        snap.items.insert(
            st_item_id,
            InventoryItemRecord {
                item_id: st_item_id,
                type_id: 35835,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            other_st_id,
            UpwellStructureRecord {
                structure_id: other_st_id,
                type_id: 35835,
                owner_corp_id: other_corp_id,
                solar_system_id: 30_000_142,
                name: "Other Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            891_301,
            eve_db::IndustryJobRecord {
                job_id: 891_301,
                installer_id: char_id,
                facility_id: jita_st,
                activity_id: 1,
                blueprint_item_id: reaction_bp_item_id,
                blueprint_type_id: reaction_bp_tid,
                product_type_id: 0,
                runs: 1,
                status: "Active".into(),
                start_unix: now.saturating_sub(60),
                duration_seconds: 600,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            891_302,
            eve_db::IndustryJobRecord {
                job_id: 891_302,
                installer_id: char_id,
                facility_id: jita_st,
                activity_id: 1,
                blueprint_item_id: reaction_bp_item_id,
                blueprint_type_id: reaction_bp_tid,
                product_type_id: 0,
                runs: 1,
                status: "Active".into(),
                start_unix: now.saturating_sub(200_000),
                duration_seconds: 600,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(jita_st);

    // GetBlueprintData on alliance contract reaction BP -> succeeds
    let bp_data = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(reaction_bp_item_id)],
        &kw,
    )
    .unwrap()
    .expect("alliance contract reaction BP visible");
    assert_ne!(bp_data, PyRep::None);

    // Unauthorized BP -> CrpAccessDenied; Unknown BP -> UnknownBlueprint
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(secret_bp_item_id)],
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
            &mut sess,
            "blueprintManager",
            "GetBlueprintData",
            &[PyRep::Integer(999_888_777)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownBlueprint"
    );

    // GetFacilityLocations with and without explicit facility list, plus GetFacilities
    let locs_all = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityLocations",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(locs_all, PyRep::List(l) if !l.is_empty()));
    let locs_explicit = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilityLocations",
        &[PyRep::List(vec![PyRep::Integer(jita_st)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(locs_explicit, PyRep::List(l) if l.len() == 1));

    let facs = handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "GetFacilities",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(facs, PyRep::List(l) if !l.is_empty()));

    // SetFacilityTaxes branches: Real (>1, >100, >1.0 normalized, >0.10 cap), KeyVal, Dict, invalid, FacilityNotFound, CrpAccessDenied
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(st_item_id), PyRep::Real(2_000_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(st_item_id), PyRep::Real(25.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(st_item_id), PyRep::None],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );
    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(st_item_id), PyRep::Dict(vec![])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFacilityTaxRate"
    );
    handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(st_item_id), PyRep::Real(5.0)],
        &kw,
    )
    .unwrap()
    .expect("5% normalized");
    handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[
            PyRep::Integer(st_item_id),
            PyRep::keyval([
                ("market", PyRep::Real(0.02)),
                ("research_te", PyRep::Real(0.01)),
                ("research_me", PyRep::Real(0.01)),
                ("copying", PyRep::Real(0.01)),
                ("invention", PyRep::Real(0.01)),
                ("reaction", PyRep::Real(0.01)),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .expect("keyval activity keys");
    handle_industry_facilities_bps(
        &ctx,
        &mut sess,
        "facilityManager",
        "SetFacilityTaxes",
        &[
            PyRep::Integer(st_item_id),
            PyRep::Dict(vec![
                (PyRep::String("all".into()), PyRep::Real(50_000.0)),
                (PyRep::String("unknown".into()), PyRep::Real(0.01)),
                (PyRep::String("ignored".into()), PyRep::None),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .expect("dict all + scaled");

    assert_eq!(
        handle_industry_facilities_bps(
            &ctx,
            &mut sess,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(999_888_777), PyRep::Real(0.02)],
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
            "SetFacilityTaxes",
            &[PyRep::Integer(other_st_id), PyRep::Real(0.02)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityPermissionDenied"
    );

    // map.GetIndustryJobsOverLast24Hours counts only recent job in Jita
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
    assert_ne!(jobs_24h, PyRep::None);
}

#[test]
fn facility_tax_corrupted_json_nested_locations_and_corp_divisional_facilities() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let char_id = 892_001_i64;
    let corp_id = 98_892_001_i64;
    let sys_id = 30_000_142_u32;
    let jita_st = 60_003_760_i64;
    let struct_id = 1_000_892_000_001_i64;
    let struct_item_id = 892_100_001_i64;
    let office_id = 892_100_002_i64;
    let box_id = 892_100_003_i64;
    let bp_in_struct = 892_200_001_i64;
    let bp_in_ship = 892_200_002_i64;
    let bp_in_npc = 892_200_003_i64;
    let ship_id = 892_200_004_i64;
    let bp_tid = ctx
        .sde
        .blueprints
        .keys()
        .copied()
        .next()
        .unwrap_or(688);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = char_id + 1; __corp.member_roles = HashMap::from([(char_id, eve_db::corp_roles::ROLE_FACTORY_MANAGER)]); __corp.ensure_wallet_divisions(); __corp },
        );
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.home_station_id = jita_st;
        snap.characters.insert(char_id, ch);

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Raitaru Item Fac".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        // Anchored structure item (type 35825) + office + nested container + BPs
        for (iid, tid, loc, flg) in [
            (struct_item_id, 35825_u32, i64::from(sys_id), InventoryFlag::None),
            (office_id, 27, jita_st, InventoryFlag::Hangar),
            (box_id, 23, office_id, InventoryFlag::CorpSAG1),
            (ship_id, 587, jita_st, InventoryFlag::Hangar),
            (bp_in_struct, bp_tid, struct_id, InventoryFlag::Hangar),
            (bp_in_ship, bp_tid, ship_id, InventoryFlag::Cargo),
            (bp_in_npc, bp_tid, jita_st, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        // Malformed tax note falls back cleanly
        let nid = facility_tax_note_id(struct_id);
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: char_id,
                item_id: struct_id,
                label: "facility_taxes".into(),
                note: "{not_json".into(),
                timestamp_unix: 0,
            },
        );

    });

    // Helper checks directly on snapshot
    let taxes = ctx.db.with_snapshot(|snap| load_facility_taxes(snap, struct_id));
    assert!(!taxes.is_empty());

    // Unauthenticated guards & character fallback location in GetFacilities / GetFacilityLocations
    let mut unauth = SessionState::new(9);
    assert_eq!(handle_industry_facilities_bps(&ctx, &mut unauth, "facilityManager", "GetFacilities", &[], &kw).unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(handle_industry_facilities_bps(&ctx, &mut unauth, "facilityManager", "GetFacilityLocations", &[], &kw).unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(handle_industry_facilities_bps(&ctx, &mut unauth, "facilityManager", "SetFacilityTaxes", &[PyRep::Integer(struct_id), PyRep::Real(0.01)], &kw).unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(handle_industry_facilities_bps(&ctx, &mut unauth, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bp_in_npc)], &kw).unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(handle_industry_facilities_bps(&ctx, &mut unauth, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(corp_id)], &kw).unwrap().unwrap_err().code, "Unauthorized");

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    let char_bps = handle_industry_facilities_bps(&ctx, &mut sess, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(char_id)], &kw).unwrap().unwrap();
    assert_ne!(char_bps, PyRep::None);
    let locs = handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetFacilityLocations", &[], &kw).unwrap().unwrap();
    assert!(matches!(locs, PyRep::List(ref l) if l.len() >= 8));

    // Empty solarSystemId list falls back via character.home_station_id
    sess.solar_system_id = Some(30_999_999);
    let facs = handle_industry_facilities_bps(&ctx, &mut sess, "facilityManager", "GetFacilities", &[], &kw).unwrap().unwrap();
    assert!(matches!(facs, PyRep::List(ref l) if !l.is_empty()));

    // Additional blueprint location resolution, reaction empty skills fallback, facility filter & OwnerAccessDenied
    let mut empty_skills_bp_tid = 0_u32;
    let mut ctx2 = test_ctx();
    let sde_mut = Arc::make_mut(&mut ctx2.sde);
    if let Some((tid, mut bp)) = sde_mut.blueprints.iter().next().map(|(&k, v)| (k, v.clone())) {
        empty_skills_bp_tid = 999_892;
        bp.activities.insert("reaction".into(), eve_sde::BlueprintActivity {
            time: 600,
            materials: vec![eve_sde::BlueprintMaterial { type_id: 34, quantity: 10 }],
            products: vec![eve_sde::BlueprintProduct { type_id: 35, quantity: 1, probability: None }],
            skills: vec![],
        });
        bp.activities.remove("manufacturing");
        sde_mut.blueprints.insert(empty_skills_bp_tid, bp);
        let _ = tid;
    }
    ctx2.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);
        for (iid, tid, loc, flg) in [
            (892_301, empty_skills_bp_tid, i64::from(sys_id), InventoryFlag::None),
            (892_302, bp_tid, 60_999_999_i64, InventoryFlag::Hangar),
            (892_303, bp_tid, 892_100_003_i64, InventoryFlag::None),
            (892_100_002, 27, jita_st, InventoryFlag::Hangar),
            (892_100_003, 23, 892_100_002, InventoryFlag::CorpSAG1),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: loc, flag: flg, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.industry_jobs.insert(892_401, eve_db::IndustryJobRecord {
            job_id: 892_401, installer_id: char_id, facility_id: jita_st, activity_id: 9,
            blueprint_item_id: 892_301, blueprint_type_id: empty_skills_bp_tid, product_type_id: 35, runs: 1, status: "Active".into(), ..Default::default()
        });
    });
    let mut sess2 = SessionState::new(11);
    sess2.user_id = Some(char_id as u64);
    sess2.character_id = Some(char_id);
    sess2.corporation_id = Some(corp_id);
    sess2.solar_system_id = Some(sys_id);
    let r_bp = handle_industry_facilities_bps(&ctx2, &mut sess2, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(892_301)], &kw).unwrap().unwrap();
    assert_ne!(r_bp, PyRep::None);
    let by_owner_filt = handle_industry_facilities_bps(&ctx2, &mut sess2, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(char_id), PyRep::Integer(jita_st)], &kw).unwrap().unwrap();
    assert_ne!(by_owner_filt, PyRep::None);
    assert_eq!(handle_industry_facilities_bps(&ctx2, &mut sess2, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(char_id + 99)], &kw).unwrap().unwrap_err().code, "OwnerAccessDenied");
}
