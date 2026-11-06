//! Unit tests covering `industry_facilities_bps_helpers`
//! (`is_structure_industry_service_online`, `is_structure_industry_service_online_for_bp`,
//! `sync_industry_job_pause_states`, `sync_industry_job_pause_states_on_service_change`,
//! `is_fulcrum_pirate_ship_bonus_applicable`, `facility_tax_note_id`).

use super::*;
use crate::router::ServerContext;
use eve_db::{
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_sde::{BlueprintInfo, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn industry_structure_service_online_pause_resume_and_fulcrum_checks() {
    let mut sde = (*shared_sde()).clone();
    let base_inv: InvType = sde.types.get(&587).expect("Rifter in seed SDE").clone();
    let custom_bp = 99_911u32;
    let custom_ship = 99_912u32;
    let react_bp = 99_913u32;
    sde.types.insert(
        react_bp,
        InvType {
            type_id: react_bp,
            name: "Composite Reaction Formula".into(),
            group_id: 1888,
            category_id: 9,
            ..base_inv.clone()
        },
    );
    sde.types.insert(
        custom_ship,
        InvType {
            type_id: custom_ship,
            name: "Mamba".into(),
            group_id: 25,
            category_id: 6,
            faction_id: Some(500_010),
            ..base_inv
        },
    );
    sde.blueprints.insert(
        custom_bp,
        BlueprintInfo {
            blueprint_type_id: custom_bp,
            product_type_id: custom_ship,
            product_quantity: 1,
            manufacturing_time_sec: 3600,
            research_te_time_sec: 600,
            research_me_time_sec: 600,
            copy_time_sec: 600,
            invention_time_sec: 1200,
            invention_base_probability: 0.25,
            max_production_limit: 10,
            materials: Vec::new(),
            activities: HashMap::new(),
        },
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );

    let st_id = 1_000_000_099_001i64;
    let svc_mfg_id = 99_200_101i64;
    let svc_react_id = 99_200_102i64;
    let job_id = 99_200_201i64;

    ctx.db.with_mut_snapshot(|snap| {
        // NPC station (not in snap.structures) is always online
        assert!(is_structure_industry_service_online(snap, 60_003_760, 1));

        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 1_000_009,
                solar_system_id: 30_000_142,
                name: "Industry Astrahus".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        // Without service modules fitted -> offline
        assert!(!is_structure_industry_service_online(snap, st_id, 1));

        // Add Standup Manufacturing Plant I (35878) and Composite Reactor I (45537)
        snap.items.insert(
            svc_mfg_id,
            InventoryItemRecord {
                item_id: svc_mfg_id,
                type_id: 35878,
                owner_id: 1_000_009,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            svc_react_id,
            InventoryItemRecord {
                item_id: svc_react_id,
                type_id: 45537,
                owner_id: 1_000_009,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        assert!(is_structure_industry_service_online(snap, st_id, 1));
        assert!(is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 9, react_bp));

        // Insert Active job, drop fuel_blocks to 0 -> sync pauses job; restore fuel -> resumes job with extended duration
        snap.industry_jobs.insert(
            job_id,
            IndustryJobRecord {
                job_id,
                installer_id: 90_000_001,
                facility_id: st_id,
                blueprint_item_id: 99_200_301,
                blueprint_type_id: custom_bp,
                activity_id: 1,
                runs: 1,
                product_type_id: custom_ship,
                status: "Active".into(),
                duration_seconds: 1_000,
                start_unix: 1_000,
                ..Default::default()
            },
        );

        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.fuel_blocks = 0;
        }
        sync_industry_job_pause_states_on_service_change(snap, 1_200);
        assert_eq!(snap.industry_jobs.get(&job_id).unwrap().status, "Paused");

        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.fuel_blocks = 50;
        }
        sync_industry_job_pause_states(snap, 1_500);
        let resumed = snap.industry_jobs.get(&job_id).unwrap();
        assert_eq!(resumed.status, "Active");
        assert_eq!(resumed.duration_seconds, 1_300);

        // Fulcrum pirate ship bonus check
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.war_faction_id = Some(500_010);
        }
        assert!(is_fulcrum_pirate_ship_bonus_applicable(
            &ctx,
            snap,
            Some(90_000_001),
            custom_bp,
            1,
            60_015_187,
        ));
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(
            &ctx,
            snap,
            Some(90_000_001),
            custom_bp,
            8,
            60_015_187,
        ));
    });

    assert_eq!(facility_tax_note_id(60_003_760), -1_000_060_003_760);
}
