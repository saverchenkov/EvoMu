//! Deep branch-coverage and mutation-killing unit tests for moon extraction
//! auto-fracture & decay, Metenox automatic moon drills, ore/gas compression &
//! decompression rig yields, in-space compression bursts, and cynosural beacons.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_cyno_portal::{
    active_cyno_jammer, handle_cyno_generator_activation, handle_cyno_portal,
    is_covert_beacon_type, is_ship_cyno_active, live_beacon, on_cyno_ship_destroyed,
};
use crate::services::mining_advanced::mining_compression::{
    caller_can_take_corp_hangar_item, compute_gas_decompression_efficiency,
    compute_structure_reprocessing_base_yield, compute_structure_rigged_base_yield,
    handle_in_space_compression_mgr, handle_structure_compression_mgr,
    has_active_compressor_burst, resolve_compressor_hull_limits,
    structure_has_online_reprocessing_service,
};
use crate::services::mining_advanced::{
    derive_moon_ores_from_sde, resolve_structure_moon_id, tick_all_metenox_moon_drills,
    tick_all_moon_extractions,
};
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use eve_db::{
    corp_roles, AutoMoonMinerRecord, FleetRecord, InventoryFlag, InventoryItemRecord,
    MoonExtractionRecord, PersistedSpaceObjectMeta, TrainedSkillRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeListInfo};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(ServerContext::new().sde.as_ref().clone()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
    if let PyRep::KeyVal(map) = rep {
        map.get(key).and_then(|v| v.as_i64())
    } else {
        None
    }
}

fn mk_upwell(id: i64, type_id: u32, owner_corp_id: i64, solar_system_id: u32, name: &str, position: [f64; 3], fuel_blocks: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        position,
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks,
        shield_hp: 100_000.0,
        armor_hp: 100_000.0,
        hull_hp: 100_000.0,
        ..Default::default()
    }
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
}

fn mk_deployable(item_id: i64, type_id: u32, group_id: u32, owner_id: i64, solar_system_id: u32, anchor_start_unix: u64, anchor_duration_secs: u64, now: u64, position: [f64; 3], fleet_id: Option<i64>) -> MobileDeployableRecord {
    MobileDeployableRecord {
        item_id,
        type_id,
        group_id,
        owner_id,
        solar_system_id,
        anchor_start_unix,
        anchor_duration_secs,
        anchor_completed: true,
        is_unanchored: false,
        expires_at_unix: now + 3_600,
        last_interaction_unix: now,
        reinforced_until_unix: None,
        last_ping_unix: None,
        position,
        deployed_fleet_id: fleet_id,
    }
}

#[tokio::test]
async fn deep_moon_extraction_auto_fracture_decay_and_metenox_drill_tick() {
    let ctx = test_ctx();
    let (lowsec_sys, moon_id, moon_pos) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.group_id == 8)
                .map(|m| (s.solar_system_id, m.celestial_id, [m.position.x, m.position.y, m.position.z]))
        })
        .expect("lowsec moon in SDE");

    assert_eq!(resolve_structure_moon_id(&ctx.sde, lowsec_sys, [moon_pos[0] + 120_000.0, moon_pos[1], moon_pos[2]]), Some(moon_id));
    assert!(resolve_structure_moon_id(&ctx.sde, 39_999_999, [0.0, 0.0, 0.0]).is_none());
    assert!(!derive_moon_ores_from_sde(&ctx.sde, lowsec_sys, moon_id).is_empty());

    let (corp_id, athanor_sid, metenox_sid) = (98_000_001i64, 1_040_000_001i64, 1_040_000_002i64);
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(athanor_sid, mk_upwell(athanor_sid, 35835, corp_id, lowsec_sys, "MoonRefinery", moon_pos, 500));
        snap.moon_extractions.insert(
            athanor_sid,
            MoonExtractionRecord {
                structure_id: athanor_sid,
                moon_id,
                extraction_start_unix: now_unix.saturating_sub(600_000),
                chunk_arrival_unix: now_unix.saturating_sub(20_000),
                natural_decay_unix: now_unix + 80_000,
                status: "Ready".into(),
                auto_fracture: false,
                event_id: 0,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: Vec::new(),
            },
        );

        let metenox_pos = [moon_pos[0] + 50_000.0, moon_pos[1], moon_pos[2]];
        snap.structures.insert(metenox_sid, mk_upwell(metenox_sid, 81826, corp_id, lowsec_sys, "MetenoxDrill", metenox_pos, 0));
        snap.items.insert(941_000_001, mk_item(941_000_001, 4247, corp_id, metenox_sid, InventoryFlag::StructureFuel, 25, false));
        snap.items.insert(941_000_002, mk_item(941_000_002, 81143, corp_id, metenox_sid, InventoryFlag::StructureFuel, 450, false));
    });

    // Tick 1: Athanor auto-fractures ready chunk into asteroids; Metenox harvests 2 hourly cycles
    tick_all_moon_extractions(&ctx).await;
    let first_spawned_ids = ctx.db.with_snapshot(|snap| {
        let ext = &snap.moon_extractions[&athanor_sid];
        assert_eq!(ext.status, "Fractured");
        assert!(ext.auto_fracture);
        assert!(!ext.spawned_asteroid_ids.is_empty());
        for aid in &ext.spawned_asteroid_ids {
            assert!(snap.asteroids.contains_key(aid));
        }
        ext.spawned_asteroid_ids.clone()
    });

    tick_all_metenox_moon_drills(&ctx, now_unix.saturating_sub(7_200), now_unix);
    let (mid_item_fuel, mid_hold_items) = ctx.db.with_snapshot(|snap| {
        let hold_cnt = snap.items.values().filter(|i| i.location_id == metenox_sid && i.flag == InventoryFlag::SpecializedMineralHold).count();
        let item_fuel = snap.items.get(&941_000_001).map(|i| i.quantity).unwrap_or(0);
        (item_fuel, hold_cnt)
    });
    assert!(mid_item_fuel < 25);
    assert!(mid_hold_items > 0);

    // Tick 2: Drain remaining fuel so Metenox transitions to OutOfFuel status
    tick_all_metenox_moon_drills(&ctx, now_unix, now_unix + 20_000);
    ctx.db.with_snapshot(|snap| {
        let am = snap.auto_moon_miners.get(&metenox_sid).expect("metenox record");
        assert_eq!(am.status, "OutOfFuel");
    });

    // Advance extraction natural_decay_unix past now_unix -> Decayed and asteroids cleaned up
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ext) = snap.moon_extractions.get_mut(&athanor_sid) {
            ext.natural_decay_unix = now_unix.saturating_sub(1);
        }
        let _: &mut HashMap<i64, AutoMoonMinerRecord> = &mut snap.auto_moon_miners;
    });
    tick_all_moon_extractions(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        let ext = &snap.moon_extractions[&athanor_sid];
        assert_eq!(ext.status, "Decayed");
        assert!(ext.spawned_asteroid_ids.is_empty());
        for aid in &first_spawned_ids {
            assert!(!snap.asteroids.contains_key(aid));
        }
    });
}

#[tokio::test]
async fn deep_compression_decompression_rig_yields_and_compressor_burst() {
    let mut ctx = test_ctx();
    let (char_id, corp_id) = (90_000_001i64, 98_000_001i64);
    let (highsec_sys, nullsec_sys) = (30_000_142u32, 30_004_759u32);
    let (sid_athanor, sid_tatara_null, ship_id) = (1_040_000_101i64, 1_040_000_102i64, 100_000_001i64);

    let (&raw_ore_tid, &comp_ore_tid) = ctx
        .sde
        .compressible_types
        .iter()
        .find(|(&raw_id, _)| {
            ctx.sde.types.get(&raw_id).is_some_and(|t| {
                t.category_id == 25 && !matches!(t.group_id, 711 | 465 | 4033 | 1884 | 1920..=1923)
            })
        })
        .expect("compressible ore in SDE");

    let (custom_raw_gas, custom_comp_gas, custom_ast_rig, custom_compressor, custom_type_list) =
        (999_711u32, 999_712u32, 999_941u32, 999_417u32, 999_325u32);
    let athanor_rig_size = ctx
        .sde
        .types
        .get(&35835)
        .and_then(|t| t.default_attributes.get(&1547).copied())
        .unwrap_or(2.0);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let base_item: InvType = sde.types.values().next().unwrap().clone();
        sde.types.insert(custom_raw_gas, InvType { type_id: custom_raw_gas, group_id: 711, category_id: 25, name: "Test Mykoserocin".into(), volume: 1.0, ..base_item.clone() });
        sde.types.insert(custom_comp_gas, InvType { type_id: custom_comp_gas, group_id: 711, category_id: 25, name: "Compressed Test Mykoserocin".into(), volume: 0.2, ..base_item.clone() });
        sde.compressible_types.insert(custom_raw_gas, custom_comp_gas);
        sde.types.insert(
            custom_ast_rig,
            InvType {
                type_id: custom_ast_rig,
                group_id: 1941,
                category_id: 66,
                name: "Test Medium Ore Rig".into(),
                default_attributes: HashMap::from([(1547, athanor_rig_size), (717, 0.56), (2355, 1.0), (2356, 1.06), (2357, 1.12)]),
                ..base_item.clone()
            },
        );
        sde.type_lists.insert(
            custom_type_list,
            TypeListInfo {
                type_list_id: custom_type_list,
                name: "CompressibleTestOre".into(),
                included_category_ids: Vec::new(),
                included_group_ids: Vec::new(),
                included_type_ids: vec![raw_ore_tid],
                excluded_category_ids: Vec::new(),
                excluded_group_ids: Vec::new(),
                excluded_type_ids: Vec::new(),
            },
        );
        sde.types.insert(
            custom_compressor,
            InvType {
                type_id: custom_compressor,
                group_id: 4174,
                category_id: 7,
                name: "Test Medium Asteroid Ore Compressor".into(),
                default_attributes: HashMap::from([(1302, 28606.0), (1298, 543.0), (54, 44_000.0), (3255, custom_type_list as f64)]),
                ..base_item
            },
        );
    }

    let (lim_types, lim_groups) = resolve_compressor_hull_limits(&ctx.sde.types[&custom_compressor]);
    assert_eq!(lim_types, vec![28606]);
    assert_eq!(lim_groups, vec![543]);

    let athanor_base = compute_structure_reprocessing_base_yield(&ctx.sde, 35835);
    let tatara_base = compute_structure_reprocessing_base_yield(&ctx.sde, 35836);
    assert!(tatara_base > athanor_base);
    assert_eq!(compute_gas_decompression_efficiency(&ctx, char_id, None), 0.80);

    let (ore_hangar_id, gas_hangar_id, rig_fitted_id, unfitted_reproc_id, space_ore_id, space_comp_mod_id) =
        (941_100_001i64, 941_100_002i64, 941_100_003i64, 941_100_004i64, 941_100_005i64, 941_100_006i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.skills.entry(char_id).or_default().insert(62452, TrainedSkillRecord { type_id: 62452, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(char_id).or_default().insert(62453, TrainedSkillRecord { type_id: 62453, level: 4, skill_points: 181_020.0 });
        snap.structures.insert(sid_athanor, mk_upwell(sid_athanor, 35835, corp_id, highsec_sys, "HighsecAthanor", [0.0; 3], 200));
        snap.structures.insert(sid_tatara_null, mk_upwell(sid_tatara_null, 35836, corp_id, nullsec_sys, "NullsecTatara", [0.0; 3], 200));
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            1,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                corp_only: true,
                ..Default::default()
            },
        );
        ext.structure_profile_id.insert(sid_athanor, 1);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.items.insert(rig_fitted_id, mk_item(rig_fitted_id, custom_ast_rig, corp_id, sid_athanor, InventoryFlag::RigSlot0, 1, true));

        assert!(structure_has_online_reprocessing_service(&ctx.sde, snap, sid_athanor, 35835));
        assert!(!structure_has_online_reprocessing_service(&ctx.sde, snap, sid_athanor, 35832));
        snap.items.insert(unfitted_reproc_id, mk_item(unfitted_reproc_id, 35899, corp_id, sid_athanor, InventoryFlag::Hangar, 1, false));
        assert!(!structure_has_online_reprocessing_service(&ctx.sde, snap, sid_athanor, 35835));

        snap.items.insert(941_100_010, mk_item(941_100_010, 35899, corp_id, sid_athanor, InventoryFlag::ServiceSlot0, 1, true));
        snap.module_online_states.insert(941_100_010, true);
        assert!(structure_has_online_reprocessing_service(&ctx.sde, snap, sid_athanor, 35835));

        snap.items.insert(ore_hangar_id, mk_item(ore_hangar_id, raw_ore_tid, char_id, sid_athanor, InventoryFlag::Hangar, 250, false));
        snap.items.insert(gas_hangar_id, mk_item(gas_hangar_id, custom_comp_gas, char_id, sid_athanor, InventoryFlag::Hangar, 100, false));
        assert!(caller_can_take_corp_hangar_item(snap, char_id, corp_id, InventoryFlag::CorpSAG1, corp_roles::ROLE_DIRECTOR));
    });

    ctx.db.with_snapshot(|snap| {
        let rigged_ast = compute_structure_rigged_base_yield(&ctx.sde, snap, sid_athanor, ctx.sde.types.get(&raw_ore_tid));
        let ice_type = ctx.sde.types.values().find(|t| t.group_id == 465);
        let rigged_ice = compute_structure_rigged_base_yield(&ctx.sde, snap, sid_athanor, ice_type);
        assert!(rigged_ast > athanor_base);
        assert!((rigged_ice - athanor_base).abs() < 1e-6);
    });

    let eff_athanor = compute_gas_decompression_efficiency(&ctx, char_id, Some(sid_athanor));
    assert!((eff_athanor - 0.89).abs() < 1e-4);

    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(highsec_sys);
    sess.structure_id = Some(sid_athanor);
    sess.ship_id = Some(ship_id);

    assert_eq!(handle_structure_compression_mgr(&ctx, &mut sess, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(sid_tatara_null)]).unwrap().unwrap_err().code, "InvalidStructure");
    let eff_rep = handle_structure_compression_mgr(&ctx, &mut sess, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(sid_athanor)]).unwrap().unwrap();
    assert!(matches!(eff_rep, PyRep::Real(r) if (r - eff_athanor).abs() < 1e-4));

    let comp_res = handle_structure_compression_mgr(&ctx, &mut sess, "CompressItemInStructure", &[PyRep::Integer(sid_athanor), PyRep::Integer(ore_hangar_id)]).unwrap().unwrap();
    assert_eq!(kv_i64(&comp_res, "compressedTypeID"), Some(comp_ore_tid as i64));

    let decomp_res = handle_structure_compression_mgr(&ctx, &mut sess, "DecompressGasInStructure", &[PyRep::Integer(gas_hangar_id)]).unwrap().unwrap();
    assert_eq!(kv_i64(&decomp_res, "decompressedTypeID"), Some(custom_raw_gas as i64));
    assert_eq!(kv_i64(&decomp_res, "decompressedUnits"), Some((100.0 * eff_athanor).floor() as i64));

    sess.structure_id = None;
    sess.solar_system_id = Some(nullsec_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = None;
            ch.station_id = None;
            ch.solar_system_id = nullsec_sys;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 28606;
            sh.location_id = nullsec_sys as i64;
            sh.flag = InventoryFlag::None;
        }
        snap.item_positions.insert(ship_id, [1_000.0, 0.0, 0.0]);
        snap.items.insert(space_comp_mod_id, mk_item(space_comp_mod_id, custom_compressor, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_active_states.insert(space_comp_mod_id);
        snap.items.insert(space_ore_id, mk_item(space_ore_id, raw_ore_tid, char_id, ship_id, InventoryFlag::SpecializedOreHold, 200, false));
        let burst = has_active_compressor_burst(&ctx.sde, snap, &HashMap::new(), char_id, nullsec_sys, raw_ore_tid, 10.0);
        assert!(burst.is_some_and(|(sid_b, rng)| sid_b == ship_id && (rng - 61_600.0).abs() < 1.0));
    });
    let in_space_rep = handle_in_space_compression_mgr(&ctx, &mut sess, &[PyRep::Integer(space_ore_id)]).await.unwrap();
    assert_eq!(kv_i64(&in_space_rep, "compressedTypeID"), Some(comp_ore_tid as i64));
}

#[tokio::test]
async fn deep_cyno_generator_beacon_expiry_inhibit_and_fleet_resolve() {
    let ctx = test_ctx();
    let (cyno_pilot, jumper_pilot, corp_id) = (90_000_001i64, 90_000_002i64, 98_000_001i64);
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.solar_system_id != 30_100_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let (cyno_ship, cyno_mod_id, jammer_item_id, inhibitor_id, deploy_beacon_id) =
        (100_000_001i64, 942_000_001i64, 942_000_009i64, 942_000_010i64, 942_000_020i64);

    assert!(is_covert_beacon_type(28650));
    assert!(is_covert_beacon_type(59631));
    assert!(!is_covert_beacon_type(21094));

    let cyno_tid = ctx.sde.types.get(&21096).or_else(|| ctx.sde.types.get(&28646)).map(|t| t.type_id).unwrap_or(21096);
    let mod_type = ctx.sde.types[&cyno_tid].clone();
    let allowed_hull_tid = mod_type
        .default_attributes
        .iter()
        .find_map(|(aid, &val)| {
            let name = ctx.sde.attributes.get(aid).map(|a| a.name.as_str())?;
            if val > 0.0 && name.starts_with("canFitShipType") {
                Some(val as u32)
            } else if val > 0.0 && name.starts_with("canFitShipGroup") {
                let grp = val as u32;
                ctx.sde.types.values().find(|t| t.group_id == grp && t.published).map(|t| t.type_id)
            } else {
                None
            }
        })
        .unwrap_or(11393);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cyno_pilot) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = cyno_ship;
        }
        if let Some(sh) = snap.items.get_mut(&cyno_ship) {
            sh.type_id = allowed_hull_tid;
        }
        if let Some(ch2) = snap.characters.get_mut(&jumper_pilot) {
            ch2.corporation_id = corp_id;
            ch2.solar_system_id = lowsec_sys;
        }
        snap.skills.entry(cyno_pilot).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        snap.item_positions.insert(cyno_ship, [5_000.0, 0.0, 0.0]);
        snap.items.insert(942_000_005, mk_item(942_000_005, 16273, cyno_pilot, cyno_ship, InventoryFlag::Cargo, 5_000, false));
        snap.items.insert(jammer_item_id, mk_item(jammer_item_id, 33700, jumper_pilot, lowsec_sys as i64, InventoryFlag::None, 1, true));
    });

    assert!(active_cyno_jammer(&ctx, lowsec_sys).is_some());

    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys);

    assert_eq!(handle_cyno_generator_activation(&ctx, &sys_handle, cyno_pilot, cyno_ship, 30_100_000, cyno_mod_id, &mod_type).await.unwrap().unwrap_err().code, "CynoDisallowedInZarzakh");
    assert_eq!(handle_cyno_generator_activation(&ctx, &sys_handle, cyno_pilot, cyno_ship, lowsec_sys, cyno_mod_id, &mod_type).await.unwrap().unwrap_err().code, "MustBeInFleet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(1, FleetRecord { fleet_id: 1, boss_id: jumper_pilot, members: vec![jumper_pilot, cyno_pilot], ..Default::default() });
    });
    assert_eq!(handle_cyno_generator_activation(&ctx, &sys_handle, cyno_pilot, cyno_ship, lowsec_sys, cyno_mod_id, &mod_type).await.unwrap().unwrap_err().code, "CynoJammed");

    // Clear system jammer, add nearby Mobile Cyno Inhibitor (type 33476, group 1249) within 100km -> CynoInhibited
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&jammer_item_id);
        snap.items.insert(inhibitor_id, mk_item(inhibitor_id, 33476, jumper_pilot, lowsec_sys as i64, InventoryFlag::None, 1, true));
        snap.item_positions.insert(inhibitor_id, [10_000.0, 0.0, 0.0]);
        snap.sp6_lifecycle.space_object_meta.insert(inhibitor_id, PersistedSpaceObjectMeta { anchor_completed: true, created_unix: Some(now), ..Default::default() });
        write_deployable_state_snap(snap, |dst| {
            dst.deployables.insert(inhibitor_id, mk_deployable(inhibitor_id, 33476, 1249, jumper_pilot, lowsec_sys, now.saturating_sub(120), 60, now, [10_000.0, 0.0, 0.0], Some(1)));
        });
    });
    assert_eq!(active_cyno_jammer(&ctx, lowsec_sys), None);
    assert_eq!(handle_cyno_generator_activation(&ctx, &sys_handle, cyno_pilot, cyno_ship, lowsec_sys, cyno_mod_id, &mod_type).await.unwrap().unwrap_err().code, "CynoInhibited");

    // Move inhibitor > 150km away, spawn ship ball, light cyno & resolve fleet beacon
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball { id: cyno_ship, type_id: 621, position: DVec3::new(5_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            Default::default(),
            Default::default(),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(inhibitor_id, [500_000.0, 0.0, 0.0]);
    });
    let lit = handle_cyno_generator_activation(&ctx, &sys_handle, cyno_pilot, cyno_ship, lowsec_sys, cyno_mod_id, &mod_type).await.unwrap().unwrap();
    let beacon_id = kv_i64(&lit, "beaconID").unwrap();
    assert!(is_ship_cyno_active(&ctx, cyno_ship));

    let mut jump_sess = SessionState::new(7);
    jump_sess.user_id = Some(7);
    jump_sess.character_id = Some(jumper_pilot);
    jump_sess.corporation_id = Some(corp_id);
    jump_sess.solar_system_id = Some(lowsec_sys);
    jump_sess.ship_id = Some(100_000_002);
    assert!(handle_cyno_portal(&ctx, &mut jump_sess, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(0), PyRep::Integer(beacon_id)]).await.is_some());

    // Ship destruction clears active cyno; deployable Mobile Cyno Beacon resolves for fleetmate when spooled
    on_cyno_ship_destroyed(&ctx, cyno_ship);
    assert!(!is_ship_cyno_active(&ctx, cyno_ship));
    assert!(live_beacon(&ctx, cyno_pilot, beacon_id, lowsec_sys).is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(deploy_beacon_id, mk_item(deploy_beacon_id, 57319, cyno_pilot, lowsec_sys as i64, InventoryFlag::None, 1, true));
        snap.item_positions.insert(deploy_beacon_id, [750_000.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |dst| {
            dst.deployables.insert(deploy_beacon_id, mk_deployable(deploy_beacon_id, 57319, 4093, cyno_pilot, lowsec_sys, now.saturating_sub(180), 120, now, [750_000.0, 0.0, 0.0], Some(1)));
        });
    });
    assert!(handle_cyno_portal(&ctx, &mut jump_sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &[PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(deploy_beacon_id)]).await.is_some());
}
