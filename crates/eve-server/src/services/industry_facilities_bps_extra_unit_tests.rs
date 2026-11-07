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
