//! Comprehensive branch-coverage and mutation-killing tests for `pve_abyssal_pylons.rs`.

#![forbid(unsafe_code)]

use crate::router::{ActiveScannerProbe, ServerContext};
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_abyssal_hazards::{
    AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
    TACHYON_CLOUD_TYPE_ID,
};
use crate::services::pve_abyssal_pylons::{
    build_fitted_ship_space_ball, destroy_pod_and_respawn_faction_corvette_in_snap,
    is_suppressor_target_type, is_tracking_pylon_beneficiary, prune_out_of_bounds_abyssal_probes,
    sample_abyssal_room_hazards, step_abyssal_hazards_and_pylons_in_ballpark,
    suppressor_pulse_damage, suppressor_radius_m, tracking_pylon_radius_and_mult,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pve_special_sites_abyssal::{
    AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord, AbyssalWeatherKind,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dummy_inv_type(type_id: u32, category_id: u32, group_id: u32, max_vel: Option<f64>) -> InvType {
    let mut t = shared_sde().types.values().next().unwrap().clone();
    t.type_id = type_id;
    t.category_id = category_id;
    t.group_id = group_id;
    t.default_attributes.clear();
    if let Some(v) = max_vel {
        t.default_attributes.insert(37, v);
    }
    t
}

#[test]
fn pylon_parameter_fallbacks_target_classifications_and_room_hazard_sampling() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);

    // 1. build_fitted_ship_space_ball fallback when neither fitting nor SDE type exists
    let fallback_ball = build_fitted_ship_space_ball(
        &ctx,
        999_999,
        888_888,
        999_999_999,
        DVec3::new(1.0, 2.0, 3.0),
    );
    assert_eq!(fallback_ball.max_velocity, 250.0);
    assert_eq!(fallback_ball.inertia, 0.5);
    assert_eq!(fallback_ball.mass, 1_000_000.0);
    assert_eq!(fallback_ball.radius, 50.0);

    // 2. suppressor_radius_m & suppressor_pulse_damage & tracking_pylon_radius_and_mult when sde_opt is None
    assert_eq!(
        suppressor_radius_m(None, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 0.0),
        15_000.0
    );
    assert_eq!(
        suppressor_radius_m(None, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 0.0),
        25_000.0
    );
    assert_eq!(
        suppressor_radius_m(None, AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 0.0),
        40_000.0
    );
    assert_eq!(suppressor_radius_m(None, 999_999, 0.0), 15_000.0);
    assert_eq!(suppressor_radius_m(None, 999_999, 18_500.0), 18_500.0);

    let fallback_dmg = suppressor_pulse_damage(None, 999_999);
    assert_eq!(fallback_dmg.explosive, 90.0);
    assert_eq!(
        fallback_dmg.em + fallback_dmg.thermal + fallback_dmg.kinetic,
        0.0
    );

    assert_eq!(
        tracking_pylon_radius_and_mult(None, MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 0.0),
        (40_000.0, 1.6)
    );
    assert_eq!(
        tracking_pylon_radius_and_mult(None, MULTIBODY_TRACKING_PYLON_TYPE_ID, 0.0),
        (15_000.0, 1.8)
    );
    assert_eq!(
        tracking_pylon_radius_and_mult(None, 999_999, 22_000.0),
        (22_000.0, 1.8)
    );

    // 3. is_suppressor_target_type exclusions (Sentry drones, type 60481, Tessera cruisers 48262..=48265) and inclusions
    assert!(!is_suppressor_target_type(&dummy_inv_type(
        28276,
        18,
        100,
        Some(0.0)
    )));
    assert!(!is_suppressor_target_type(&dummy_inv_type(
        60_481,
        18,
        100,
        Some(100.0)
    )));
    for tessera_tid in 48_262..=48_265 {
        assert!(!is_suppressor_target_type(&dummy_inv_type(
            tessera_tid,
            11,
            1997,
            Some(300.0)
        )));
    }
    assert!(is_suppressor_target_type(&dummy_inv_type(
        2454,
        18,
        100,
        Some(300.0)
    )));
    assert!(is_suppressor_target_type(&dummy_inv_type(200, 8, 88, None)));
    assert!(is_suppressor_target_type(&dummy_inv_type(
        23055, 87, 1652, None
    )));
    assert!(is_suppressor_target_type(&dummy_inv_type(
        50000, 11, 4037, None
    )));
    assert!(!is_suppressor_target_type(&dummy_inv_type(
        587, 6, 25, None
    )));

    // 4. is_tracking_pylon_beneficiary inclusions & exclusions
    assert!(is_tracking_pylon_beneficiary(&dummy_inv_type(
        587, 6, 25, None
    )));
    assert!(is_tracking_pylon_beneficiary(&dummy_inv_type(
        28276, 18, 100, None
    )));
    assert!(is_tracking_pylon_beneficiary(&dummy_inv_type(
        50001, 11, 1997, None
    )));
    for excl_grp in [1971, 1981, 1991, 4081] {
        assert!(!is_tracking_pylon_beneficiary(&dummy_inv_type(
            50002, 11, excl_grp, None
        )));
    }
    for excl_tid in [47_685, 47_686, 47_951, 49_661, 49_662] {
        assert!(!is_tracking_pylon_beneficiary(&dummy_inv_type(
            excl_tid, 11, 1997, None
        )));
    }

    // 5. sample_abyssal_room_hazards invariants across rooms 1..=3
    for room in 1..=3 {
        let (clouds, pylons) = sample_abyssal_room_hazards(&ctx, 42, room);
        let c_cnt = clouds.iter().filter(|&&b| b).count();
        let p_cnt = pylons.iter().filter(|&&b| b).count();
        assert!((1..=2).contains(&c_cnt));
        assert!((1..=2).contains(&p_cnt));
    }
}

#[test]
fn pod_destruction_implant_loss_and_faction_corvette_race_mapping() {
    let db = Arc::new(WorldRepository::new_empty());
    // Test all 4 race_ids (1=Caldari->601, 2=Minmatar->588, 4=Amarr->596, 8=Gallente->606) with sde_opt = None
    // and plugged-in implants
    for (idx, (race_id, expected_corvette)) in
        [(1u32, 601u32), (2, 588), (4, 596), (8, 606)].into_iter().enumerate()
    {
        let cid = 90_060_001 + idx as i64;
        let imp_item_id = 900_001 + idx as i64;
        db.provision_gm_account(&format!("race_u_{idx}"), "pw", cid, "RacePilot");
        db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.race_id = race_id;
            ch.home_station_id = 60_003_760;
            ch.solar_system_id = 32_000_001;
            snap.active_implants.insert(cid, vec![9899]);
            snap.items.insert(
                imp_item_id,
                InventoryItemRecord {
                    item_id: imp_item_id,
                    type_id: 9941,
                    owner_id: cid,
                    location_id: cid,
                    flag: InventoryFlag::Implant,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let (st_id, sys_id, corv_id, corpse_id, pod_kid) =
                destroy_pod_and_respawn_faction_corvette_in_snap(
                    snap,
                    None,
                    cid,
                    32_000_001,
                    [10.0, 20.0, 30.0],
                    1_775_700_000,
                );
            assert_eq!(st_id, 60_003_760);
            assert_eq!(sys_id, 32_000_001);
            assert_eq!(snap.items[&corv_id].type_id, expected_corvette);
            assert_eq!(snap.items[&corpse_id].type_id, 25);
            assert!(!snap.items.contains_key(&imp_item_id));
            assert!(!snap.active_implants.contains_key(&cid));
            let dkm = &snap.combat_crimewatch_state.detailed_killmails[&pod_kid];
            assert_eq!(dkm.destroyed_items.len(), 2);
            assert_eq!(dkm.total_value_isk, 3_000.0); // 1000 pod + 2 * 1000 fallback implants
        });
    }
}

#[tokio::test]
async fn probe_pruning_cloud_transitions_and_pocket_trace_cleanup_in_ballpark() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let cid = 90_070_001i64;
    let ship_id = 1_070_000_001i64;
    let pod_id = 1_070_000_002i64;
    let probe_in_id = 1_070_000_010i64;
    let probe_out_id = 1_070_000_011i64;
    let ab_sys = 32_000_001u32;

    db.provision_gm_account("abyss_cloud_u", "pw", cid, "AbyssCloudPilot");
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());

    let make_room = |idx: u8| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 2100 + idx as u32,
        dungeon_name: "R".into(),
        npc_ball_ids: Vec::new(),
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.insert(
            cid,
            AbyssalPocketSession {
                pocket_id: 7,
                character_id: cid,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0, 0.0, 0.0],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2, // z offset = (2 - 1) * 50_000 = 50_000
                rooms: [make_room(0), make_room(1), make_room(2)],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                saved_ship_attrs: HashMap::new(),
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 1_897_000_007,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 2)]),
                max_ships: 1,
            },
        );
        reg.abyssal_traces.insert(
            1_897_000_007,
            AbyssalTraceRecord {
                trace_ball_id: 1_897_000_007,
                char_id: cid,
                filament_type_id: 56131,
                origin_solar_system_id: 30_000_142,
                position: [0.0; 3],
                created_unix: 1_000,
                expires_at_sec: 2_200,
                max_ships: 1,
                entered_char_ids: vec![cid],
                pocket_id: Some(7),
                fleet_id: None,
            },
        );
    });

    // 1. prune_out_of_bounds_abyssal_probes: non-abyssal system is no-op; abyssal system prunes only probe > 75 km from (0, 0, 50_000)
    {
        let mut probes = ctx.active_scanner_probes.write().unwrap();
        for (pid, pos) in [
            (probe_in_id, [0.0, 0.0, 60_000.0]),
            (probe_out_id, [0.0, 0.0, 150_000.0]),
        ] {
            probes.insert(
                pid,
                ActiveScannerProbe {
                    probe_id: pid,
                    probe_type_id: 30013,
                    owner_char_id: cid,
                    ship_id,
                    launcher_module_id: 1,
                    solar_system_id: ab_sys,
                    position: pos,
                    destination: [0.0; 3],
                    range_step: 1,
                    scan_range_m: 1_000_000.0,
                    base_sensor_strength: 40.0,
                    max_flight_time_s: 3600.0,
                    launched_at_unix: 1_000,
                    state: 1,
                    warp_end_ft: None,
                },
            );
        }
    }
    prune_out_of_bounds_abyssal_probes(&ctx, 30_000_142);
    assert_eq!(ctx.active_scanner_probes.read().unwrap().len(), 2);
    prune_out_of_bounds_abyssal_probes(&ctx, ab_sys);
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&probe_in_id));
    assert!(!ctx.active_scanner_probes.read().unwrap().contains_key(&probe_out_id));

    // 2. step_abyssal_hazards_and_pylons_in_ballpark: non-abyssal and VOID_SPACE_SYSTEM_ID return early
    let mut bp = Ballpark::new(ab_sys);
    assert!(
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&sde), Some(&db), 30_000_142)
            .is_empty()
    );
    assert!(step_abyssal_hazards_and_pylons_in_ballpark(
        &mut bp,
        Some(&sde),
        Some(&db),
        VOID_SPACE_SYSTEM_ID
    )
    .is_empty());

    // 3. Test cloud transitions (enter Bioluminescence cloud -> switch to Filament cloud -> exit all clouds)
    // and out-of-bounds pod destruction triggering trace & cache purge for pocket 7
    let cache_id = 1_896_000_071i64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cache_id,
            InventoryItemRecord {
                item_id: cache_id,
                type_id: 47951,
                owner_id: 0,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: cid,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(0.0, 0.0, 50_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_id,
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 500.0,
            max_armor_hp: 500.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            signature_radius: 40.0,
            capacitor_recharge_time_ms: 10_000.0,
            ..Default::default()
        },
    );
    let cloud_bid = 1_898_000_721i64;
    bp.add_ball(Ball {
        id: cloud_bid,
        type_id: BIOLUMINESCENT_CLOUD_TYPE_ID,
        position: DVec3::new(0.0, 0.0, 50_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 15_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // Tick 1: enters Bioluminescence cloud (signature_radius * 4.0 = 160.0)
    let _ = step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&sde), Some(&db), ab_sys);
    assert!((bp.combat_states[&ship_id].signature_radius - 160.0).abs() < 1e-6);

    // Tick 2: stays in same Bioluminescence cloud -> signature_radius stays 160.0
    let _ = step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&sde), Some(&db), ab_sys);
    assert!((bp.combat_states[&ship_id].signature_radius - 160.0).abs() < 1e-6);

    // Tick 3: switch cloud to FILAMENT_CLOUD_TYPE_ID -> reverts 4.0x sig and applies Filament cloud shield boost penalty
    bp.balls.get_mut(&cloud_bid).unwrap().type_id = FILAMENT_CLOUD_TYPE_ID;
    let _ = step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&sde), Some(&db), ab_sys);
    assert!((bp.combat_states[&ship_id].signature_radius - 40.0).abs() < 1e-6);

    // Tick 4: remove cloud and add out-of-bounds pod -> reverts Filament cloud and collapses pocket 7 + purges cache_id & trace!
    bp.remove_ball(cloud_bid);
    bp.add_ball(Ball {
        id: pod_id,
        type_id: 670,
        position: DVec3::new(0.0, 0.0, 200_000.0), // > 75 km from (0, 0, 50_000)
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 15.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        pod_id,
        CombatState {
            structure_hp: 10.0,
            max_structure_hp: 100.0,
            structure_resonance: [1.0; 4],
            ..Default::default()
        },
    );
    let _ = step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&sde), Some(&db), ab_sys);
    assert_eq!(bp.combat_states[&ship_id].shield_boost_multiplier, 1.0);
    db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cache_id));
    });
    let _ = TACHYON_CLOUD_TYPE_ID;
}
