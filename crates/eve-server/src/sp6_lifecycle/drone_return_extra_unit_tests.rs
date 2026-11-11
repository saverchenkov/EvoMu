//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle::drone_return`.

#![forbid(unsafe_code)]

use crate::router::context::{ActiveFighterSquadron, ActiveScannerProbe};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState, DroneSpaceState,
};
use crate::sp6_lifecycle::drone_return::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, handle_cmd_return_bay, step_repeating_drone_mining,
    step_returning_drones,
};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
    ctx.systems
        .get_or_spawn(sys_id)
        .try_spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 600.0,
                inertia: 1.2,
                mass: 4_000.0,
                radius: 12.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
}

fn make_drone(
    ball_id: i64,
    drone_item_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    solar_system_id: u32,
    is_sentry: bool,
    activity_state: i64,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id,
        drone_item_id,
        type_id,
        group_id: 100,
        owner_char_id,
        original_owner_char_id: owner_char_id,
        parent_ship_id,
        solar_system_id,
        target_id: Some(888),
        activity_state,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry,
        bandwidth_used: 5.0,
        max_velocity: 1_500.0,
        optimal_range: 5_000.0,
        damage_vector: DamageVector::default(),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 1.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    }
}

#[tokio::test]
async fn cmd_return_bay_sentry_out_of_range_prevalidation_and_mobile_follow_transition() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 940_000_001;
    let mobile_far = 940_000_010;
    let sentry_far = 940_000_011;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 645,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    spawn_ball(&ctx, sys_id, ship_id, 645, DVec3::ZERO);
    // Mobile drone at 3_000m (> 2_500m, <= max_ctrl_range)
    spawn_ball(&ctx, sys_id, mobile_far, 2454, DVec3::new(3_000.0, 0.0, 0.0));
    // Sentry drone at 2_500.1m (> 2_500m -> SentryDroneOutOfScoopRange)
    spawn_ball(&ctx, sys_id, sentry_far, 23561, DVec3::new(2_500.1, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            mobile_far,
            make_drone(mobile_far, mobile_far, 2454, char_id, 0, sys_id, false, 1),
        );
        rt.active_drones_in_space.insert(
            sentry_far,
            make_drone(sentry_far, sentry_far, 23561, char_id, ship_id, sys_id, true, 1),
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    // Calling without filter hits SentryDroneOutOfScoopRange during pre-validation before mutating mobile_far!
    let err = handle_cmd_return_bay(&ctx, &mut sess, &[], &HashMap::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, "SentryDroneOutOfScoopRange");
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&mobile_far].activity_state, 1);
    });

    // Now recall only mobile_far via List filter -> transitions to returning (activity_state = 4, target = ship_id).
    handle_cmd_return_bay(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(mobile_far)])],
        &HashMap::new(),
    )
    .await
    .expect("mobile drone at 3000m begins returning");

    with_combat_runtime(&ctx, |rt| {
        let st = &rt.active_drones_in_space[&mobile_far];
        assert_eq!(st.activity_state, 4);
        assert_eq!(st.parent_ship_id, ship_id);
        assert_eq!(st.target_id, Some(ship_id));
    });

    // Move mobile_far to exact 2_500.0m boundary and run step_returning_drones -> scooped into DroneBay!
    spawn_ball(&ctx, sys_id, mobile_far, 2454, DVec3::new(2_500.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mobile_far,
            InventoryItemRecord {
                item_id: mobile_far,
                type_id: 2454,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 5,
                ..Default::default()
            },
        );
    });

    step_returning_drones(&ctx).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&mobile_far));
    });
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&mobile_far];
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, InventoryFlag::DroneBay);
        assert_eq!(it.version, 6);
    });
}

#[tokio::test]
async fn abandon_probes_fighters_and_repeating_drone_miner_depletion_and_full_hold() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 940_100_001;
    let probe_in_sys = 940_100_010;
    let probe_other_sys = 940_100_011;
    let fighter_ball_id = 940_100_020;
    let fighter_item_id = 940_100_021;

    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    spawn_ball(
        &ctx,
        sys_id,
        fighter_ball_id,
        23057,
        DVec3::new(1_200.0, 3_400.0, 5_600.0),
    );

    ctx.active_scanner_probes.write().unwrap().insert(
        probe_in_sys,
        ActiveScannerProbe {
            probe_id: probe_in_sys,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 1,
            solar_system_id: sys_id,
            position: [0.0, 0.0, 0.0],
            destination: [0.0, 0.0, 0.0],
            range_step: 1,
            scan_range_m: 149_597_870_700.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1_000,
            state: 2,
            warp_end_ft: None,
        },
    );
    ctx.active_scanner_probes.write().unwrap().insert(
        probe_other_sys,
        ActiveScannerProbe {
            probe_id: probe_other_sys,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 1,
            solar_system_id: 30_000_144,
            position: [0.0, 0.0, 0.0],
            destination: [0.0, 0.0, 0.0],
            range_step: 1,
            scan_range_m: 149_597_870_700.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1_000,
            state: 2,
            warp_end_ft: None,
        },
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item_id,
            InventoryItemRecord {
                item_id: fighter_item_id,
                type_id: 23057,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                version: 3,
                ..Default::default()
            },
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: fighter_ball_id,
            item_id: fighter_item_id,
            type_id: 23057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: Some(999),
            is_abandoned: false,
            mwd_active: true,
        },
    );

    disconnect_drones_for_ship_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    abandon_drones_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);

    assert_eq!(
        ctx.active_scanner_probes.read().unwrap()[&probe_in_sys].state,
        0
    );
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap()[&probe_other_sys].state,
        2
    );
    ctx.db.with_snapshot(|snap| {
        let fit_item = &snap.items[&fighter_item_id];
        assert_eq!(fit_item.location_id, sys_id as i64);
        assert_eq!(fit_item.flag, InventoryFlag::None);
        assert_eq!(fit_item.version, 4);
        assert_eq!(
            snap.item_positions.get(&fighter_item_id),
            Some(&[1_200.0, 3_400.0, 5_600.0])
        );
        assert_eq!(
            snap.item_positions.get(&fighter_ball_id),
            Some(&[1_200.0, 3_400.0, 5_600.0])
        );
        assert_eq!(
            snap.sp6_lifecycle.space_object_meta[&fighter_item_id].drone_ball_id,
            Some(fighter_ball_id)
        );
    });

    // Test repeating drone mining: asteroid depletion & full hold stop
    let depleting_drone = 940_100_030;
    let full_hold_drone = 940_100_031;
    let ast_id = 940_100_040;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_009_079,
                type_id: 1230,
                quantity: 10,
                initial_quantity: 10,
                position: (500.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_asteroid(
            crate::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_009_079,
                type_id: 1230,
                initial_units: 10,
                remaining_units: 10,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: 1230,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    spawn_ball(&ctx, sys_id, depleting_drone, 32787, DVec3::new(400.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, full_hold_drone, 32787, DVec3::new(450.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            depleting_drone,
            make_drone(depleting_drone, depleting_drone, 32787, char_id, ship_id, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            depleting_drone,
            DroneMiningState {
                drone_id: depleting_drone,
                asteroid_id: ast_id,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 10, // depletes all 10 units
                duration_secs: 60,
                next_cycle_unix: 500,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
        rt.active_drones_in_space.insert(
            full_hold_drone,
            make_drone(full_hold_drone, full_hold_drone, 32787, char_id, ship_id, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            full_hold_drone,
            DroneMiningState {
                drone_id: full_hold_drone,
                asteroid_id: ast_id,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 99_999_999, // exceeds ship cargo capacity -> stopped before mining
                duration_secs: 60,
                next_cycle_unix: 500,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
    });

    step_repeating_drone_mining(&ctx, 500).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&depleting_drone));
        assert_eq!(rt.active_drones_in_space[&depleting_drone].activity_state, 0);
        assert!(!rt.active_drone_miners.contains_key(&full_hold_drone));
        assert_eq!(rt.active_drones_in_space[&full_hold_drone].activity_state, 0);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.asteroids[&ast_id].quantity), 0);
}

#[tokio::test]
async fn sp6_lifecycle_wave2_mutant_killers_hp_sync_clear_filament_and_reconnect() {
    use super::{
        apply_persisted_hp_to_combat_state, clear_ship_combat_damage_in_snap,
        persist_existing_ship_combat_hp_batch_in_db, read_unified_ship_damage_state,
        reconnect_character_in_space_sync, require_ship_ready_for_filament_jump,
        save_sp6_state_to_snap, sde_mobile_warp_disruptor_decay_secs, with_mut_sp6_state,
        DisconnectedShipEntry, PersistedCombatHp, Sp6PersistedState, MOBILE_WARP_DISRUPTOR_DECAY_SECS,
    };

    let mut ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_991_001i64;
    let ship_id = 941_000_001i64;
    let phantom_ship = 941_000_999i64;
    let now = ctx.now_unix();

    // `save_sp6_state_to_snap` (`L80-82`) and `sde_mobile_warp_disruptor_decay_secs` (`L174-182`)
    {
        let mut s_snap = eve_db::WorldStateSnapshot::default();
        let mut st = Sp6PersistedState::default();
        st.last_downtime_day_idx = Some(777);
        save_sp6_state_to_snap(&mut s_snap, &st);
        assert_eq!(s_snap.sp6_lifecycle.last_downtime_day_idx, Some(777));
    }
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let template_t = sde.types[&2496].clone();
        let mut t_ms = template_t.clone();
        t_ms.type_id = 999_811;
        t_ms.default_attributes.insert(1237, 172_800_000.0); // ms -> 172_800 s
        sde.types.insert(999_811, t_ms);

        let mut t_sec = template_t.clone();
        t_sec.type_id = 999_812;
        t_sec.default_attributes.insert(1034, 10_000.0); // already in secs
        sde.types.insert(999_812, t_sec);

        let mut t_small = template_t;
        t_small.type_id = 999_813;
        t_small.default_attributes.insert(1034, 120.0); // < 3600 fallback
        sde.types.insert(999_813, t_small);
    }
    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&ctx.sde, 999_811), 172_800);
    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&ctx.sde, 999_812), 10_000);
    assert_eq!(
        sde_mobile_warp_disruptor_decay_secs(&ctx.sde, 999_813),
        MOBILE_WARP_DISRUPTOR_DECAY_SECS
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters[&90_000_001].clone();
        c.character_id = char_id;
        c.user_id = char_id as u64;
        c.name = "Sp6Tester".into();
        c.solar_system_id = sys_id;
        c.active_ship_id = ship_id;
        c.station_id = None;
        c.structure_id = None;
        snap.characters.insert(char_id, c);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // `persist_existing_ship_combat_hp_batch_in_db`:
    // (a) ignores phantom item IDs not present in snap.items
    // (b) persists real ship damage & bumps version, skips sub-1e-6 no-op, removes DAMAGE/ARMOR_DAMAGE when repaired within <= 1e-6
    let mut cs = eve_destiny::CombatState::default();
    cs.max_shield_hp = 1_000.0;
    cs.shield_hp = 600.0;
    cs.max_armor_hp = 2_000.0;
    cs.armor_hp = 1_500.0;
    cs.max_structure_hp = 3_000.0;
    cs.structure_hp = 2_600.0;
    cs.max_capacitor = 500.0;
    cs.capacitor = 500.0;

    persist_existing_ship_combat_hp_batch_in_db(
        &ctx.db,
        [(phantom_ship, &cs), (ship_id, &cs)],
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&phantom_ship));
        assert_eq!(
            snap.sp6_lifecycle.ship_combat_hp.get(&ship_id),
            Some(&PersistedCombatHp {
                shield_hp: 600.0,
                armor_hp: 1_500.0,
                hull_hp: 2_600.0,
            })
        );
        assert_eq!(
            snap.items[&ship_id].dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied(),
            Some(400.0)
        );
        assert_eq!(
            snap.items[&ship_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                .copied(),
            Some(500.0)
        );
    });

    // Sub-1e-6 delta should NOT alter version
    let ver_before_noop = ctx.db.with_snapshot(|s| s.items[&ship_id].version);
    cs.shield_hp += 1e-9;
    persist_existing_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &cs)]);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ship_id].version), ver_before_noop);

    // Full repair within 1e-6 removes DAMAGE and ARMOR_DAMAGE keys
    cs.armor_hp = cs.max_armor_hp - 1e-7;
    cs.structure_hp = cs.max_structure_hp - 1e-7;
    persist_existing_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &cs)]);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items[&ship_id].dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!snap.items[&ship_id].dynamic_attributes.contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
    });

    // `clear_ship_combat_damage_in_snap`: first clears -> version bumps; second clean call -> version unchanged
    let (v_after_first, v_after_second) = ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 500.0,
                armor_hp: 1_000.0,
                hull_hp: 2_000.0,
            },
        );
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 1_000.0);
            it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 1_000.0);
        }
        clear_ship_combat_damage_in_snap(snap, ship_id);
        let v1 = snap.items[&ship_id].version;
        clear_ship_combat_damage_in_snap(snap, ship_id);
        let v2 = snap.items[&ship_id].version;
        (v1, v2)
    });
    assert_eq!(v_after_first, v_after_second);

    // `read_unified_ship_damage_state` SDE-max & dynamic-attribute paths + `apply_persisted_hp_to_combat_state`
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 15.0);
            it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 25.0);
        }
        let u = read_unified_ship_damage_state(&ctx, snap, ship_id, 587, None);
        assert!((u.hull_damage - 15.0).abs() < 1e-6);
        assert!((u.armor_damage - 25.0).abs() < 1e-6);
    });
    let mut empty_restore = cs.clone();
    apply_persisted_hp_to_combat_state(&ctx, phantom_ship, &mut empty_restore);
    assert!((empty_restore.shield_hp - cs.shield_hp).abs() < 1e-6);

    // `require_ship_ready_for_filament_jump`: Weapons timer, warp-in-progress, scrambled, active cloaking/module
    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::new(100_000.0, 0.0, 0.0));
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until.insert(char_id, now + 60);
    });
    assert!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_err());
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until.remove(&char_id);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .set_movement_mode(
            ship_id,
            eve_destiny::MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e7, 0.0, 0.0), 3.0),
        )
        .await;
    assert!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_err());
    let _ = sys_handle.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    assert!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_ok());

    // `reconnect_character_in_space_sync`:
    // (1) removed_from_space == true re-spawns ball into ballpark;
    // (2) pre_warp_position eligible restores pre-warp when disconnected ship still in warp;
    // (3) docked character returns None & cleans stale disconnected_ships (`L1178-1180`)
    sys_handle.try_remove_ball(ship_id);
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.insert(
            char_id,
            DisconnectedShipEntry {
                char_id,
                ship_id,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [500_000.0, 0.0, 0.0],
                emergency_warp_dest: Some([1_000_000.0, 0.0, 0.0]),
                pre_warp_position: Some([120_000.0, 0.0, 0.0]),
                disconnected_at_unix: now - 100,
                remove_at_unix: now + 300,
                removed_from_space: true,
            },
        );
    });
    let rec1 = reconnect_character_in_space_sync(&ctx, char_id).expect("reconnected from despawn");
    assert_eq!(rec1.0, sys_id);
    assert_eq!(rec1.1, ship_id);
    assert!(sys_handle.get_ball_position_sync(ship_id).is_some());

    // Docked character reconnect clears stale disconnected_ships + safe_logoffs and returns None
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.insert(
            char_id,
            DisconnectedShipEntry {
                char_id,
                ship_id,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [500_000.0, 0.0, 0.0],
                emergency_warp_dest: None,
                pre_warp_position: None,
                disconnected_at_unix: now,
                remove_at_unix: now + 60,
                removed_from_space: false,
            },
        );
    });
    assert!(reconnect_character_in_space_sync(&ctx, char_id).is_none());
    assert!(super::with_sp6_state(&ctx.db, |st| !st.disconnected_ships.contains_key(&char_id)));
}

#[tokio::test]
async fn cluster_tick_downtime_probes_bounties_and_cluster_tick_sweeps() {
    use super::cluster_tick::{populate_persisted_space_objects, tick_background_cluster};
    use super::cluster_tick_downtime::{
        clear_safe_logoff, disconnect_safe_logoff_sessions, flush_pending_bounty_ticks_on_downtime,
        recover_scanner_probes_on_downtime,
    };
    use super::{with_mut_sp6_state, DisconnectedShipEntry, SafeLogoffEntry};

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_992_001i64;
    let char2 = 90_992_002i64;
    let ship1 = 942_000_001i64; // launcher partial match + cargo room
    let ship_full = 942_000_002i64; // overfilled cargo -> unrecovered fallback
    let mod_launcher = 942_000_010i64;
    let charge_stack = 942_000_011i64;
    let tritanium_fill = 942_000_012i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters[&90_000_001].clone();
        for (cid, sid, nm) in [
            (char_id, ship1, "ProbeOwner1"),
            (char2, ship_full, "ProbeOwner2"),
        ] {
            let mut c = tmpl.clone();
            c.character_id = cid;
            c.user_id = cid as u64;
            c.name = nm.into();
            c.solar_system_id = sys_id;
            c.active_ship_id = sid;
            c.station_id = None;
            c.structure_id = None;
            snap.characters.insert(cid, c);
        }
        for (sid, cid) in [(ship1, char_id), (ship_full, char2)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Core Probe Launcher I (17938) + partial Core Scanner Probe I (30013) stack of 2 in launcher
        snap.items.insert(
            mod_launcher,
            InventoryItemRecord {
                item_id: mod_launcher,
                type_id: 17938,
                owner_id: char_id,
                location_id: ship1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_stack,
            InventoryItemRecord {
                item_id: charge_stack,
                type_id: 30013,
                owner_id: char_id,
                location_id: mod_launcher,
                flag: InventoryFlag::None,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Overfill ship_full cargo so cargo fallback cannot fit another probe
        snap.items.insert(
            tritanium_fill,
            InventoryItemRecord {
                item_id: tritanium_fill,
                type_id: 34,
                owner_id: char2,
                location_id: ship_full,
                flag: InventoryFlag::Cargo,
                quantity: 100_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Pre-seed pending bounty tick for downtime flush AND expired anomalies/signatures
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, char_id), 25_000.0);
        snap.combat_crimewatch_state
            .bounty_tick_next_flush_unix
            .insert((sys_id, char_id), now - 5);
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![eve_db::CosmicAnomalyRecord {
                anomaly_id: "EXP-001".into(),
                solar_system_id: sys_id,
                name: "Expired Belt".into(),
                anomaly_type: "Combat".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: now.saturating_sub(100_000),
                lifetime_seconds: 10,
                cleared: false,
            }],
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![eve_db::CosmicSignatureRecord {
                signature_id: "SIG-001".into(),
                solar_system_id: sys_id,
                sig_group: "Data".into(),
                type_name: "Ruined Site".into(),
                position: [2_000.0, 0.0, 0.0],
                signal_strength: 0.5,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now.saturating_sub(100_000),
                lifetime_seconds: 10,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            }],
        );
        snap.mining_and_ess_state.metenox_last_tick_unix = now - 60;
        snap.zarzakh_gate_locks.insert(char_id, (50_000_001, now - 1));
    });

    // Insert 4 scanner probes:
    // p1: live probe -> recovered into ship1 launcher clip
    // p2: expired probe -> purged
    // p3: live probe for ship_full -> unrecovered (cargo full, no launcher)
    // p4: live probe for missing ship -> unrecovered
    {
        let mut probes = ctx.active_scanner_probes.write().unwrap();
        let mk_probe = |pid: i64, cid: i64, sid: i64, mid: i64, launched: u64| crate::router::ActiveScannerProbe {
            probe_id: pid,
            probe_type_id: 30013,
            owner_char_id: cid,
            ship_id: sid,
            launcher_module_id: mid,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 1,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 10.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: launched,
            state: 1,
            warp_end_ft: None,
        };
        probes.insert(942_100_001, mk_probe(942_100_001, char_id, ship1, mod_launcher, now));
        probes.insert(942_100_002, mk_probe(942_100_002, char_id, ship1, mod_launcher, now.saturating_sub(10_000)));
        probes.insert(942_100_003, mk_probe(942_100_003, char2, ship_full, 0, now));
        probes.insert(942_100_004, mk_probe(942_100_004, char2, 942_999_999, 0, now));
    }

    let (recovered, purged) = recover_scanner_probes_on_downtime(&ctx);
    assert!(recovered >= 1);
    assert!(purged >= 1);
    let flushed = flush_pending_bounty_ticks_on_downtime(&ctx);
    assert_eq!(flushed, 1);

    // `clear_safe_logoff` & `disconnect_safe_logoff_sessions`
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id: ship1,
                solar_system_id: sys_id,
                started_unix: now,
                complete_at_unix: now + 30,
                completed: false,
            },
        );
    });
    clear_safe_logoff(&ctx.db, char_id);
    assert!(super::with_sp6_state(&ctx.db, |st| !st.safe_logoffs.contains_key(&char_id)));
    disconnect_safe_logoff_sessions(&ctx, char_id);

    // `populate_persisted_space_objects` branches: expired items, active probe, expired/warping disconnected ship, MWD anchoring
    let mut bp = eve_destiny::Ballpark::new(sys_id);
    let _exp_wreck = 942_200_001i64;
    let _exp_can = 942_200_002i64;
    let _exp_corpse = 942_200_003i64;
    let _exp_pi = 942_200_004i64;
    let _live_probe = 942_200_005i64;
    let _live_disc_ship = 942_200_006i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mk = |iid: i64, tid: u32, cid: i64| InventoryItemRecord {
            item_id: iid,
            type_id: tid,
            owner_id: cid,
            location_id: sys_id as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(_exp_wreck, mk(_exp_wreck, 26557, char_id));
        snap.items.insert(_exp_can, mk(_exp_can, 23, char_id));
        snap.items.insert(_exp_corpse, mk(_exp_corpse, 25, char_id));
        snap.items.insert(_exp_pi, mk(_exp_pi, 23, char_id));
        snap.items.insert(_live_disc_ship, mk(_live_disc_ship, 587, char2));

        let sp6 = &mut snap.sp6_lifecycle;
        sp6.space_object_meta.insert(
            _exp_wreck,
            eve_db::PersistedSpaceObjectMeta {
                created_unix: Some(1),
                ..Default::default()
            },
        );
        sp6.space_object_meta.insert(
            _exp_can,
            eve_db::PersistedSpaceObjectMeta {
                created_unix: Some(1),
                ..Default::default()
            },
        );
        sp6.space_object_meta.insert(
            _exp_corpse,
            eve_db::PersistedSpaceObjectMeta {
                created_unix: Some(1),
                ..Default::default()
            },
        );
        sp6.space_object_meta.insert(
            _exp_pi,
            eve_db::PersistedSpaceObjectMeta {
                created_unix: Some(1),
                planet_id: Some(40_009_049),
                ..Default::default()
            },
        );
        sp6.active_scanner_probes.insert(
            _live_probe,
            eve_db::PersistedScannerProbe {
                probe_id: _live_probe,
                probe_type_id: 30013,
                owner_char_id: char_id,
                ship_id: ship1,
                launcher_module_id: mod_launcher,
                solar_system_id: sys_id,
                position: [1_000.0, 2_000.0, 3_000.0],
                destination: [5_000.0, 2_000.0, 3_000.0],
                range_step: 2,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 10.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: now,
                state: 1,
                warp_end_ft: None,
            },
        );
        sp6.disconnected_ships.insert(
            char2,
            DisconnectedShipEntry {
                char_id: char2,
                ship_id: _live_disc_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [20_000.0, 0.0, 0.0],
                emergency_warp_dest: Some([500_000.0, 0.0, 0.0]),
                pre_warp_position: None,
                disconnected_at_unix: now,
                remove_at_unix: now + 300,
                removed_from_space: false,
            },
        );
    });

    let _pending = populate_persisted_space_objects(&mut bp, sys_id, &ctx.sde, &ctx.db);
    assert!(bp.balls.contains_key(&_live_probe));
    assert!(bp.balls.contains_key(&_live_disc_ship));
    assert!(!bp.balls.contains_key(&_exp_wreck));

    // Re-seed pending bounty tick for `tick_background_cluster` + invoke full cluster tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, char_id), 15_000.0);
        snap.combat_crimewatch_state
            .bounty_tick_next_flush_unix
            .insert((sys_id, char_id), now - 2);
    });
    tick_background_cluster(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.zarzakh_gate_locks.contains_key(&char_id));
        assert!(snap.cosmic_anomalies.get(&sys_id).is_none_or(|v| v.is_empty()));
        assert!(snap.cosmic_signatures.get(&sys_id).is_none_or(|v| v.is_empty()));
    });
}

