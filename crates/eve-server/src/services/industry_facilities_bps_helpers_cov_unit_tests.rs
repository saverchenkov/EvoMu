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
    assert_eq!(facility_tax_note_id(0), -1_000_000_000_000);
}

#[test]
fn industry_helpers_boundary_mutations_slots_activities_pause_and_fulcrum() {
    let mut sde = (*shared_sde()).clone();
    let base_inv = sde.types.get(&587).expect("Rifter in seed SDE").clone();
    let custom_bp = 99_941_u32;
    let custom_ship = 99_942_u32;
    let freighter_bp = 99_943_u32;
    let freighter_ship = 99_944_u32;
    let nonship_bp = 99_945_u32;
    let nonship_prod = 99_946_u32;
    let react_bp_1888 = 99_947_u32;
    let react_bp_1889 = 99_948_u32;
    let react_bp_1890 = 99_949_u32;

    for (tid, group_id, category_id, name, faction_id) in [
        (custom_ship, 25_u32, 6_u32, "Pirate Frigate", Some(500_011_u32)),
        (freighter_ship, 513_u32, 6_u32, "Freighter Hull", Some(500_010_u32)),
        (nonship_prod, 18_u32, 4_u32, "Mineral", Some(500_010_u32)),
        (react_bp_1888, 1888_u32, 9_u32, "Composite Reaction", None),
        (react_bp_1889, 1889_u32, 9_u32, "Biochemical Reaction", None),
        (react_bp_1890, 1890_u32, 9_u32, "Hybrid Reaction", None),
    ] {
        sde.types.insert(
            tid,
            InvType {
                type_id: tid,
                name: name.into(),
                group_id,
                category_id,
                faction_id,
                ..base_inv.clone()
            },
        );
    }
    let bp_tmpl = sde.blueprints.values().next().cloned().expect("seed blueprint");
    for (bp_id, prod_id) in [
        (custom_bp, custom_ship),
        (freighter_bp, freighter_ship),
        (nonship_bp, nonship_prod),
    ] {
        let mut bp = bp_tmpl.clone();
        bp.blueprint_type_id = bp_id;
        bp.product_type_id = prod_id;
        sde.blueprints.insert(bp_id, bp);
    }

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let st_id = 1_000_888_000_001_i64;
    let fulcrum_id = 1_000_888_000_002_i64;
    let mod_id = 99_300_101_i64;
    let job_ready_eq = 77_300_201_i64;
    let job_pause_plus1 = 77_300_202_i64;
    let job_delivered = 77_300_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                name: "Boundary Engineering".into(),
                type_id: 35_827,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                hull_hp: 1.0,
                armor_hp: 1.0,
                shield_hp: 1.0,
                reinforced_until_unix: 0,
                fuel_blocks: 1, // exact boundary > 0
                quantum_core_installed: true,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 35_878, // Standup Manufacturing Plant -> activity 1 only
                owner_id: 98_000_001,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0, // 164 (lowest service slot)
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        // Flag & service state boundaries
        assert!(is_structure_industry_service_online(snap, st_id, 1));
        assert!(!is_structure_industry_service_online(snap, st_id, 3));
        assert!(!is_structure_industry_service_online(snap, st_id, 8));
        assert!(!is_structure_industry_service_online(snap, st_id, 9));

        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::ServiceSlot7; // 171 (highest service slot)
        assert!(is_structure_industry_service_online(snap, st_id, 1));
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::FighterBay; // 158 < 164
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::StructureFuel; // 172 > 171
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::ServiceSlot0;

        // Module online map & dynamic_attributes[2] guards
        snap.module_online_states.insert(mod_id, false);
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.module_online_states.insert(mod_id, true);
        snap.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(2, 0.0);
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(2, 1.0);

        // Activity type mapping across Research Lab (35891), Invention Lab (35886), Reactors (45537/45538/45539), unknown (999_999)
        snap.items.get_mut(&mod_id).unwrap().type_id = 35_891;
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        assert!(is_structure_industry_service_online(snap, st_id, 3));
        assert!(is_structure_industry_service_online(snap, st_id, 4));
        assert!(is_structure_industry_service_online(snap, st_id, 5));

        snap.items.get_mut(&mod_id).unwrap().type_id = 35_886;
        assert!(is_structure_industry_service_online(snap, st_id, 8));
        assert!(!is_structure_industry_service_online(snap, st_id, 1));

        // Reactions: group 1888 -> 45537, 1889 -> 45538, 1890 -> 45539 on activity 9/11
        assert!(!is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 9, react_bp_1888));
        snap.items.get_mut(&mod_id).unwrap().type_id = 45_537;
        assert!(is_structure_industry_service_online(snap, st_id, 9));
        assert!(is_structure_industry_service_online(snap, st_id, 11));
        assert!(is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 9, react_bp_1888));
        assert!(!is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 9, react_bp_1889));
        snap.items.get_mut(&mod_id).unwrap().type_id = 45_538;
        assert!(is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 11, react_bp_1889));
        snap.items.get_mut(&mod_id).unwrap().type_id = 45_539;
        assert!(is_structure_industry_service_online_for_bp(&ctx.sde, snap, st_id, 9, react_bp_1890));

        snap.items.get_mut(&mod_id).unwrap().type_id = 999_999;
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.items.get_mut(&mod_id).unwrap().type_id = 35_878;

        // Quantum core & Destroyed state & fuel == 0
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false;
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true;
        snap.structures.get_mut(&st_id).unwrap().state = "Destroyed".into();
        assert!(!is_structure_industry_service_online(snap, st_id, 1));
        assert_eq!(resolve_facility_offline_moment_opt(snap, st_id), None);
        snap.structures.get_mut(&st_id).unwrap().state = "ShieldVulnerable".into();

        // Pause threshold boundary: ready_at == offline_moment stays Active + sets ready_before_offline; ready_at == offline_moment + 1 pauses!
        snap.industry_jobs.insert(
            job_ready_eq,
            IndustryJobRecord {
                job_id: job_ready_eq,
                installer_id: 90_000_001,
                facility_id: st_id,
                blueprint_item_id: 99_300_301,
                blueprint_type_id: custom_bp,
                activity_id: 1,
                runs: 1,
                product_type_id: custom_ship,
                status: "Active".into(),
                duration_seconds: 500,
                start_unix: 1_000, // ready_at = 1_500 == offline_moment
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            job_pause_plus1,
            IndustryJobRecord {
                job_id: job_pause_plus1,
                installer_id: 90_000_001,
                facility_id: st_id,
                blueprint_item_id: 99_300_302,
                blueprint_type_id: custom_bp,
                activity_id: 1,
                runs: 1,
                product_type_id: custom_ship,
                status: "Active".into(),
                duration_seconds: 501,
                start_unix: 1_000, // ready_at = 1_501 == offline_moment + 1
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            job_delivered,
            IndustryJobRecord {
                job_id: job_delivered,
                installer_id: 90_000_001,
                facility_id: st_id,
                blueprint_item_id: 99_300_303,
                blueprint_type_id: custom_bp,
                activity_id: 1,
                runs: 1,
                product_type_id: custom_ship,
                status: "Delivered".into(),
                duration_seconds: 2_000,
                start_unix: 1_000,
                ..Default::default()
            },
        );

        snap.structures.get_mut(&st_id).unwrap().fuel_blocks = 0;
        {
            use crate::services::station_structure::structure_upwell_ops::{
                load_ext_state, save_ext_state, StructureLifecycleState,
            };
            let mut ext = load_ext_state(snap);
            ext.lifecycle.insert(
                st_id,
                StructureLifecycleState {
                    repair_until_unix: None,
                    fight_started_unix: None,
                    reinforce_jitter_secs: 0,
                    last_fuel_tick_unix: None,
                    low_power_since_unix: Some(1_500),
                    online_modules: Vec::new(),
                    unjittered_reinforce_exit_unix: None,
                    shield_fight_started_unix: None,
                    armor_reinforce_secs_applied: None,
                    anchoring_duration_secs: None,
                },
            );
            save_ext_state(snap, &ext);
        }
        assert_eq!(resolve_facility_offline_moment_opt(snap, st_id), Some(1_500));

        // offline_is_now = false leaves ready_before_offline = false, still pauses job_pause_plus1 using low_power_since_unix = 1500
        use crate::services::industry_pi::{load_job_cost_meta, save_job_cost_meta};
        sync_industry_job_pause_states_at(snap, 1_900, false);
        assert_eq!(snap.industry_jobs[&job_ready_eq].status, "Active");
        assert!(load_job_cost_meta(snap, job_ready_eq).unwrap_or_default().ready_before_offline);
        assert_eq!(snap.industry_jobs[&job_pause_plus1].status, "Paused");
        assert_eq!(load_job_cost_meta(snap, job_pause_plus1).unwrap().paused_at_unix, Some(1_500));
        assert_eq!(snap.industry_jobs[&job_delivered].status, "Delivered");

        // offline_is_now = true marks ready_before_offline = true on job_ready_eq, and falls back to now_unix when low_power_since_unix is cleared
        snap.industry_jobs.get_mut(&job_pause_plus1).unwrap().status = "Active".into();
        {
            let mut m = load_job_cost_meta(snap, job_pause_plus1).unwrap_or_default();
            m.paused_at_unix = None;
            save_job_cost_meta(snap, job_pause_plus1, &m);
            let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.lifecycle.remove(&st_id);
            crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        }
        sync_industry_job_pause_states_at(snap, 1_500, true);
        assert!(load_job_cost_meta(snap, job_ready_eq).unwrap().ready_before_offline);
        assert_eq!(load_job_cost_meta(snap, job_pause_plus1).unwrap().paused_at_unix, Some(1_500));

        // Fulcrum pirate ship bonus boundaries: None char, 500_011 faction, restricted groups, non-ship, Upwell Fulcrum in Zarzakh (30_100_000 / 78_334)
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, None, custom_bp, 1, 60_015_187));
        snap.characters.get_mut(&90_000_001).unwrap().war_faction_id = Some(500_011);
        assert!(is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), custom_bp, 1, 60_015_187));
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), freighter_bp, 1, 60_015_187));
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), nonship_bp, 1, 60_015_187));

        snap.structures.insert(
            fulcrum_id,
            UpwellStructureRecord {
                structure_id: fulcrum_id,
                name: "The Fulcrum".into(),
                type_id: 78_334,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_100_000,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                hull_hp: 1.0,
                armor_hp: 1.0,
                shield_hp: 1.0,
                reinforced_until_unix: 0,
                fuel_blocks: 100,
                quantum_core_installed: true,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.characters.get_mut(&90_000_001).unwrap().war_faction_id = Some(500_001);
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), custom_bp, 1, fulcrum_id));
        snap.characters.get_mut(&90_000_001).unwrap().war_faction_id = Some(500_010);
        assert!(is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), custom_bp, 1, fulcrum_id));
        assert!(!is_fulcrum_pirate_ship_bonus_applicable(&ctx, snap, Some(90_000_001), custom_bp, 1, st_id));
    });
}
