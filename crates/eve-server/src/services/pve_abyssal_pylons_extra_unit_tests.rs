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
    is_suppressor_target_type, is_tracking_pylon_beneficiary, on_abyssal_boundary_death_hook,
    prune_out_of_bounds_abyssal_probes, pulse_abyssal_automated_pylons,
    sample_abyssal_room_hazards, step_abyssal_hazards_and_pylons_in_ballpark,
    step_abyssal_hazards_and_pylons_in_ballpark_for_tick, suppressor_pulse_damage,
    suppressor_radius_m, tracking_pylon_radius_and_mult,
};
use crate::services::pve_special_sites::with_abyssal_state;
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
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
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
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 1_897_000_007,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 2)]),
                max_ships: 1,
            },
        );
        st.traces.insert(
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

#[tokio::test]
async fn fitted_ship_ball_sde_zero_filter_boundary_escalation_defer_and_death_hook() {
    use eve_sde::attr_ids;

    // 1. build_fitted_ship_space_ball: SDE >0.0 vs ==0.0 fallbacks and fitted-ship attribute precedence
    let mut sde = (*shared_sde()).clone();
    let custom_zero_tid = 998_101_u32;
    let custom_pos_tid = 998_102_u32;

    let mut zero_t = sde.types[&587].clone();
    zero_t.type_id = custom_zero_tid;
    zero_t.default_attributes.insert(attr_ids::MAX_VELOCITY, 0.0);
    zero_t.default_attributes.insert(attr_ids::AGILITY, 0.0);
    zero_t.mass = 0.0;
    zero_t.radius = 0.0;
    sde.types.insert(custom_zero_tid, zero_t);

    let mut pos_t = sde.types[&587].clone();
    pos_t.type_id = custom_pos_tid;
    pos_t.default_attributes.insert(attr_ids::MAX_VELOCITY, 412.5);
    pos_t.default_attributes.insert(attr_ids::AGILITY, 1.35);
    pos_t.default_attributes.remove(&attr_ids::MASS);
    pos_t.default_attributes.remove(&162);
    pos_t.mass = 2_345_000.0;
    pos_t.radius = 77.5;
    sde.types.insert(custom_pos_tid, pos_t);

    let db = Arc::new(WorldRepository::new_empty());
    let cid_a = 90_080_001_i64;
    let cid_b = 90_080_002_i64;
    let cid_c = 90_080_003_i64;
    db.provision_gm_account("ab_py_a", "pw", cid_a, "AbyssA");
    db.provision_gm_account("ab_py_b", "pw", cid_b, "AbyssB");
    db.provision_gm_account("ab_py_c", "pw", cid_c, "AbyssC");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::clone(&db));

    // Exact 0.0 in SDE -> falls through `.filter(|&v| v > 0.0)` to (250.0, 0.5, 1_000_000.0, 50.0)
    let b_zero = build_fitted_ship_space_ball(&ctx, cid_a, 8_080_001, custom_zero_tid, DVec3::ZERO);
    assert_eq!(b_zero.max_velocity, 250.0);
    assert_eq!(b_zero.inertia, 0.5);
    assert_eq!(b_zero.mass, 1_000_000.0);
    assert_eq!(b_zero.radius, 50.0);

    // Non-zero SDE defaults (without fitted item) -> uses SDE values
    let b_pos = build_fitted_ship_space_ball(&ctx, cid_a, 8_080_002, custom_pos_tid, DVec3::ZERO);
    assert_eq!(b_pos.max_velocity, 412.5);
    assert_eq!(b_pos.inertia, 1.35);
    assert_eq!(b_pos.mass, 2_345_000.0);
    assert_eq!(b_pos.radius, 77.5);

    // Fitted ship present -> fitted attrs (from 587 Rifter) override fallback; verify version/system_epoch increment on pod respawn
    let fit_ship_id = 8_080_003_i64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fit_ship_id,
            InventoryItemRecord {
                item_id: fit_ship_id,
                type_id: 587,
                owner_id: cid_a,
                location_id: 32_000_002,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&cid_c).unwrap();
        let ver_before = ch.version;
        let ep_before = ch.system_epoch;
        let _ = destroy_pod_and_respawn_faction_corvette_in_snap(
            snap,
            None,
            cid_c,
            32_000_002,
            [0.0; 3],
            1_775_700_100,
        );
        let ch_after = &snap.characters[&cid_c];
        assert_eq!(ch_after.version, ver_before + 1);
        assert_eq!(ch_after.system_epoch, ep_before + 1);
    });
    let b_fit = build_fitted_ship_space_ball(&ctx, cid_a, fit_ship_id, 587, DVec3::ZERO);
    assert!(b_fit.max_velocity > 0.0 && b_fit.mass > 0.0 && b_fit.radius > 0.0);

    // 2. Non-zero grid_offset, fleet ship `ship_room`, asymmetric resonance boundary damage escalation,
    // Tachyon cloud 4 multiplier fields, deferred ship death (`step_..._for_tick`), and co-op pod/death-hook cleanup.
    let ab_sys = 32_000_004_u32;
    let origin_sys = 30_000_142_u32;
    let pid = 12_i64;
    let grid_off = [10_000.0, -20_000.0, 5_000.0];
    let ship_a = 1_080_000_001_i64;
    let ship_b = 1_080_000_002_i64;
    let fleet_ship_room3 = 1_080_000_003_i64;
    let completed_ship = 1_080_000_004_i64;
    let bound_trace_only = 1_897_000_012_i64;
    let extra_pid_trace = 1_897_000_099_i64;
    let unrelated_trace = 1_897_000_555_i64;

    let make_room = |idx: u8, npcs: Vec<i64>| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 2100 + idx as u32,
        dungeon_name: "R".into(),
        npc_ball_ids: npcs,
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };

    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            cid_a,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: cid_a,
                ship_id: ship_a,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: grid_off,
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2, // center = grid_off + (0, 0, 50_000) = (10_000, -20_000, 55_000)
                rooms: [
                    make_room(0, vec![]),
                    make_room(1, vec![1_898_001_211]),
                    make_room(2, vec![]),
                ],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: bound_trace_only,
                fleet_ship_ids: vec![ship_a, ship_b, fleet_ship_room3],
                ship_room: HashMap::from([(ship_a, 2), (ship_b, 2), (fleet_ship_room3, 3)]),
                max_ships: 2,
            },
        );
        st.pockets.insert(
            cid_b,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: cid_b,
                ship_id: ship_b,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: grid_off,
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2,
                rooms: [make_room(0, vec![]), make_room(1, vec![]), make_room(2, vec![])],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: bound_trace_only,
                fleet_ship_ids: vec![ship_a, ship_b, fleet_ship_room3],
                ship_room: HashMap::from([(ship_a, 2), (ship_b, 2), (fleet_ship_room3, 3)]),
                max_ships: 2,
            },
        );
        // Completed pocket for cid_c must NOT be resolved as active pocket
        st.pockets.insert(
            cid_c,
            AbyssalPocketSession {
                pocket_id: 99,
                character_id: cid_c,
                ship_id: completed_ship,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 0.0,
                weather_bonus_pct: 0.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [make_room(0, vec![]), make_room(1, vec![]), make_room(2, vec![])],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                completed: true,
                collapsed_destroyed: false,
                trace_ball_id: unrelated_trace,
                fleet_ship_ids: vec![completed_ship],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
        // bound_trace_only has pocket_id == None (matched only via bound_tid); extra_pid_trace matched via pocket_id == Some(pid)
        st.traces.insert(
            bound_trace_only,
            AbyssalTraceRecord {
                trace_ball_id: bound_trace_only,
                char_id: cid_a,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: 1_000,
                expires_at_sec: 2_200,
                max_ships: 2,
                entered_char_ids: vec![cid_a, cid_b],
                pocket_id: None,
                fleet_id: None,
            },
        );
        st.traces.insert(
            extra_pid_trace,
            AbyssalTraceRecord {
                trace_ball_id: extra_pid_trace,
                char_id: cid_a,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: 1_000,
                expires_at_sec: 2_200,
                max_ships: 2,
                entered_char_ids: vec![cid_a, cid_b],
                pocket_id: Some(pid),
                fleet_id: None,
            },
        );
        st.traces.insert(
            unrelated_trace,
            AbyssalTraceRecord {
                trace_ball_id: unrelated_trace,
                char_id: cid_c,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: 1_000,
                expires_at_sec: 2_200,
                max_ships: 1,
                entered_char_ids: vec![cid_c],
                pocket_id: Some(99),
                fleet_id: None,
            },
        );
    });

    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid_a).unwrap().active_ship_id = ship_a;
        snap.characters.get_mut(&cid_b).unwrap().active_ship_id = ship_b;
        for (sid, oid, tid) in [
            (ship_a, cid_a, 587_u32),
            (ship_b, cid_b, 587_u32),
            (fleet_ship_room3, cid_a, 587_u32),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: oid,
                    location_id: ab_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let room2_center = DVec3::new(10_000.0, -20_000.0, 55_000.0);
    let room3_center = DVec3::new(10_000.0, -20_000.0, 105_000.0);
    let mut bp = Ballpark::new(ab_sys);
    let add_ship = |bp: &mut Ballpark, id: i64, pos: DVec3, hp: f64| {
        bp.add_ball(Ball {
            id,
            type_id: 587,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        bp.combat_states.insert(
            id,
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: hp,
                max_structure_hp: hp,
                structure_resonance: [0.2, 0.4, 0.6, 0.8], // sum = 2.0
                signature_radius: 40.0,
                capacitor_recharge_time_ms: 10_000.0,
                ..Default::default()
            },
        );
    };
    // ship_a at room2_center + (70_000, 0, 0) is inside boundary; moved out later
    add_ship(&mut bp, ship_a, room2_center + DVec3::new(10_000.0, 0.0, 0.0), 20_000.0);
    // fleet_ship_room3 in room 3 center (10_000, -20_000, 105_000) -> only valid if sr=3 z-offset applied!
    add_ship(&mut bp, fleet_ship_room3, room3_center, 5_000.0);
    // ship_b outside boundary -> escalates 500, 750, 1000
    add_ship(&mut bp, ship_b, room2_center + DVec3::new(80_000.0, 0.0, 0.0), 20_000.0);

    // Tachyon cloud on ship_a
    let tachyon_bid = 1_898_001_221_i64;
    bp.add_ball(Ball {
        id: tachyon_bid,
        type_id: TACHYON_CLOUD_TYPE_ID,
        position: room2_center + DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 15_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // Tick 1 (defer=true): ship_a gets Tachyon cloud multipliers; ship_b loses 500.0; fleet_ship_room3 keeps full 5000.0
    let sde_ref = ctx.sde.as_ref();
    let d1 = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(&mut bp, Some(sde_ref), Some(&db), ab_sys);
    assert!(d1.is_empty());
    let cs_a = &bp.combat_states[&ship_a];
    assert_eq!(cs_a.cloud_velocity_multiplier, Some(4.0));
    assert_eq!(cs_a.cloud_sig_multiplier, Some(4.0));
    assert_eq!(cs_a.cloud_inertia_multiplier, Some(0.5));
    assert_eq!(cs_a.cloud_cap_recharge_multiplier, None);
    assert!((bp.combat_states[&ship_b].structure_hp - 19_500.0).abs() < 1e-6);
    assert_eq!(bp.combat_states[&fleet_ship_room3].structure_hp, 5_000.0);

    // Tick 2: ship_b loses 750.0 (t_count=2) -> 18_750.0
    let _ = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(&mut bp, Some(sde_ref), Some(&db), ab_sys);
    assert!((bp.combat_states[&ship_b].structure_hp - 18_750.0).abs() < 1e-6);

    // Tick 3: remove Tachyon cloud (multipliers reset to None) while ship_a remains inside boundary
    bp.remove_ball(tachyon_bid);
    let d3_cloud = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(&mut bp, Some(sde_ref), Some(&db), ab_sys);
    assert!(d3_cloud.is_empty());
    assert!((bp.combat_states[&ship_b].structure_hp - 17_750.0).abs() < 1e-6);
    assert_eq!(bp.combat_states[&ship_a].cloud_velocity_multiplier, None);
    assert_eq!(bp.combat_states[&ship_a].cloud_sig_multiplier, None);
    assert_eq!(bp.combat_states[&ship_a].cloud_inertia_multiplier, None);

    // Tick 4: move ship_a outside with lethal HP and defer=true -> destroyed & removed from ballpark, -ship_a inserted in boundary_ticks
    bp.balls.get_mut(&ship_a).unwrap().position = room2_center + DVec3::new(90_000.0, 0.0, 0.0);
    bp.combat_states.get_mut(&ship_a).unwrap().structure_hp = 100.0;
    let d3 = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(&mut bp, Some(sde_ref), Some(&db), ab_sys);
    assert_eq!(d3, vec![ship_a]);
    assert!((bp.combat_states[&ship_b].structure_hp - 16_500.0).abs() < 1e-6);
    assert!(!bp.combat_states.contains_key(&ship_a));
    db.with_snapshot(|snap| {
        assert_eq!(snap.abyssal_state.boundary_ticks.get(&-ship_a), Some(&1));
    });

    // Tick 5 with defer=true retains negative boundary_ticks[&-ship_a] (*bid < 0)
    bp.remove_ball(ship_a);
    let _ = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(&mut bp, Some(sde_ref), Some(&db), ab_sys);
    db.with_snapshot(|snap| {
        assert_eq!(snap.abyssal_state.boundary_ticks.get(&-ship_a), Some(&1));
    });

    // 3. on_abyssal_boundary_death_hook:
    // victim_char_id <= 0 is no-op
    on_abyssal_boundary_death_hook(&ctx, ab_sys, 0, ship_a, None);

    // Ejected capsule carries -ship_a tick -> cap_a, pushes cap_a once, updates ship_room across pocket pid only
    let cap_a = 1_080_000_901_i64;
    on_abyssal_boundary_death_hook(&ctx, ab_sys, cid_a, ship_a, Some(cap_a));
    db.with_snapshot(|snap| {
        assert!(!snap.abyssal_state.boundary_ticks.contains_key(&-ship_a));
        assert_eq!(snap.abyssal_state.boundary_ticks.get(&cap_a), Some(&1));
        let pa = &snap.abyssal_state.pockets[&cid_a];
        let pb = &snap.abyssal_state.pockets[&cid_b];
        assert_eq!(pa.ship_id, cap_a);
        assert_eq!(pa.fleet_ship_ids.iter().filter(|&&id| id == cap_a).count(), 1);
        assert!(!pa.ship_room.contains_key(&ship_a));
        assert_eq!(pa.ship_room.get(&cap_a), Some(&2));
        assert_eq!(pb.ship_room.get(&cap_a), Some(&2));
    });

    // Seed caches & item inside sid_b for pocket pid=12 (cid_b = 1_896_000_121, sid_b = 1_896_500_121)
    let cid_b_cache = 1_896_000_121_i64;
    let sid_b_cache = cid_b_cache + 500_000;
    let cache_loot = 1_080_000_999_i64;
    db.with_mut_snapshot(|snap| {
        for (iid, loc, tid) in [
            (cid_b_cache, ab_sys as i64, 47951),
            (sid_b_cache, ab_sys as i64, 47952),
            (cache_loot, sid_b_cache, 34),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 0,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // First pilot collapse (podless) while cid_b alive -> traces & caches MUST REMAIN
    on_abyssal_boundary_death_hook(&ctx, ab_sys, cid_a, cap_a, None);
    db.with_snapshot(|snap| {
        assert!(snap.abyssal_state.pockets[&cid_a].collapsed_destroyed);
        assert!(snap.abyssal_state.traces.contains_key(&bound_trace_only));
        assert!(snap.abyssal_state.traces.contains_key(&extra_pid_trace));
        assert!(snap.items.contains_key(&cid_b_cache));
        assert!(snap.items.contains_key(&sid_b_cache));
        assert!(snap.items.contains_key(&cache_loot));
    });

    // Second pilot collapse (last alive) -> purges bound_trace_only & extra_pid_trace + caches & cache_loot, preserves unrelated_trace!
    on_abyssal_boundary_death_hook(&ctx, ab_sys, cid_b, ship_b, None);
    db.with_snapshot(|snap| {
        assert!(!snap.abyssal_state.traces.contains_key(&bound_trace_only));
        assert!(!snap.abyssal_state.traces.contains_key(&extra_pid_trace));
        assert!(snap.abyssal_state.traces.contains_key(&unrelated_trace));
        assert!(!snap.items.contains_key(&cid_b_cache));
        assert!(!snap.items.contains_key(&sid_b_cache));
        assert!(!snap.items.contains_key(&cache_loot));
    });

    // 4. pulse_abyssal_automated_pylons returning >1 for both suppressor and tracking pylon (kills (0,0),(0,1),(1,0),(1,1) FnValues)
    let pulse_sys = 32_000_003_u32;
    let pship1 = 1_081_000_001_i64;
    let pship2 = 1_081_000_002_i64;
    let pdrone1 = 1_081_000_011_i64;
    let pdrone2 = 1_081_000_012_i64;
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            cid_a,
            AbyssalPocketSession {
                pocket_id: 33,
                character_id: cid_a,
                ship_id: pship1,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: pulse_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 0.0,
                weather_bonus_pct: 0.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [make_room(0, vec![]), make_room(1, vec![]), make_room(2, vec![])],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![pship1, pship2],
                ship_room: HashMap::new(),
                max_ships: 2,
            },
        );
    });
    let sh = ctx.systems.get_or_spawn(pulse_sys);
    for (bid, tid, pos, hp) in [
        (pship1, 587_u32, DVec3::new(0.0, 0.0, 0.0), 10_000.0),
        (pship2, 587_u32, DVec3::new(100.0, 0.0, 0.0), 10_000.0),
        (pdrone1, 2454_u32, DVec3::new(200.0, 0.0, 0.0), 10.0),
        (pdrone2, 2454_u32, DVec3::new(300.0, 0.0, 0.0), 10.0),
        (1_899_003_311_i64, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, DVec3::new(250.0, 0.0, 0.0), 10_000.0),
        (1_899_003_312_i64, MULTIBODY_TRACKING_PYLON_TYPE_ID, DVec3::new(50.0, 0.0, 0.0), 10_000.0),
    ] {
        let _ = sh
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 0.0,
                    max_shield_hp: 0.0,
                    armor_hp: 0.0,
                    max_armor_hp: 0.0,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    ..Default::default()
                },
            )
            .await;
    }
    let (sup_hits, trk_hits) = pulse_abyssal_automated_pylons(&ctx, pulse_sys).await;
    assert_eq!(sup_hits, 2);
    assert_eq!(trk_hits, 2);
}
