//! Unit and mutation-killing tests for `sp6_lifecycle`.

use super::*;
use crate::probe_test_helpers;
use crate::router::{ActiveMoonSurveyProbe, ActiveScannerProbe, ServerContext};
use crate::services::combat_crimewatch::DroneSpaceState;
use crate::services::drone_test_helpers;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedCombatHp, SafeLogoffEntry, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: hull,
        max_structure_hp: hull,
        capacitor: cap,
        max_capacitor: cap,
        ..CombatState::default()
    }
}

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

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn undock_char_in_space(ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 606);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 606,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[test]
fn filetime_meta_and_sde_helpers_cover_exact_boundaries() {
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
    assert_eq!(
        unix_to_filetime(1_700_000_000),
        (1_700_000_000_i64 + 11_644_473_600) * 10_000_000
    );

    let ctx = test_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        record_wreck_or_container_meta_in_snap(snap, 5001, 1_000, false);
        record_wreck_or_container_meta_in_snap(snap, 5001, 2_000, true);
        record_drone_ball_meta_in_snap(snap, 5002, 9902);
        record_anchor_meta_in_snap(snap, 5003, 3_000, 60.0, Some(20_000.0));
        record_anchor_meta_in_snap(snap, 5003, 3_500, 45.0, None);
    });

    ctx.db.with_snapshot(|snap| {
        let st = load_sp6_state_from_snap(snap);
        let m1 = st.space_object_meta.get(&5001).unwrap();
        assert_eq!(m1.created_unix, Some(1_000));
        assert!(m1.is_npc_wreck);

        let m2 = st.space_object_meta.get(&5002).unwrap();
        assert_eq!(m2.drone_ball_id, Some(9902));

        let m3 = st.space_object_meta.get(&5003).unwrap();
        assert_eq!(m3.anchor_start_unix, Some(3_500));
        assert!((m3.anchoring_delay_secs - 45.0).abs() < 1e-9);
        assert_eq!(m3.bubble_radius_m, Some(20_000.0));
    });

    let mut sde = (*ctx.sde).clone();
    assert_eq!(
        sde_mobile_warp_disruptor_decay_secs(&sde, 999_999),
        MOBILE_WARP_DISRUPTOR_DECAY_SECS
    );
    let mut t_ms: InvType = sde.types.get(&606).unwrap().clone();
    t_ms.type_id = 999_001;
    t_ms.default_attributes.clear();
    t_ms.default_attributes.insert(1237, 7_200_000.0); // ms -> 7200s
    sde.types.insert(999_001, t_ms.clone());
    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&sde, 999_001), 7_200);

    let mut t_sec = t_ms.clone();
    t_sec.type_id = 999_002;
    t_sec.default_attributes.clear();
    t_sec.default_attributes.insert(1034, 14_400.0);
    sde.types.insert(999_002, t_sec);
    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&sde, 999_002), 14_400);

    let mut t_short = t_ms;
    t_short.type_id = 999_003;
    t_short.default_attributes.clear();
    t_short.default_attributes.insert(1237, 1_800.0); // < 3600s -> default
    sde.types.insert(999_003, t_short);
    assert_eq!(
        sde_mobile_warp_disruptor_decay_secs(&sde, 999_003),
        MOBILE_WARP_DISRUPTOR_DECAY_SECS
    );

    let fallback_phys = sde_ship_physics(&sde, 888_888);
    assert_eq!(fallback_phys, (25, 6, 1_200_000.0, 40.0, 300.0, 2.5, 3.0));
}

#[test]
fn combat_hp_persistence_and_restoration_clamp_correctly() {
    let ctx = test_ctx();
    let ship_id = 777_001_i64;
    let mut cs = make_combat_state(1_000.0, 2_000.0, 3_000.0, 500.0);
    cs.shield_hp = 450.0;
    cs.armor_hp = 1_200.0;
    cs.structure_hp = 2_500.0;

    persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);
    // Second call with identical values hits the early-return no-op branch
    persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);

    let mut restored = make_combat_state(1_000.0, 2_000.0, 3_000.0, 500.0);
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored);
    assert!((restored.shield_hp - 450.0).abs() < 1e-6);
    assert!((restored.armor_hp - 1_200.0).abs() < 1e-6);
    assert!((restored.structure_hp - 2_500.0).abs() < 1e-6);

    // Persisted hull_hp == 0.0 should NOT overwrite structure_hp to 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 9_999.0,
                armor_hp: -50.0,
                hull_hp: 0.0,
            },
        );
    });
    let mut restored2 = make_combat_state(1_000.0, 2_000.0, 3_000.0, 500.0);
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored2);
    assert!((restored2.shield_hp - 1_000.0).abs() < 1e-6);
    assert!((restored2.armor_hp - 0.0).abs() < 1e-6);
    assert!((restored2.structure_hp - 3_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn timers_aggression_and_filament_preconditions_enforce_all_guards() {
    let ctx = test_ctx();
    let att = 90_000_001_i64;
    let vic = 90_000_002_i64;
    let ship_id = 880_001_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();

    undock_char_in_space(&ctx, att, ship_id, sys_id, DVec3::new(100_000.0, 0.0, 0.0));
    assert_eq!(active_timers_for_char(&ctx, att, now), (0, 0, 0, 0));
    assert!(!has_active_pvp_timer(&ctx, att, now));
    assert!(require_no_pvp_timer_for_filament(&ctx, att).is_ok());
    assert!(require_ship_ready_for_filament_jump(&ctx, att, ship_id, sys_id).is_ok());

    // Safe logoff blocks filament
    with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            att,
            SafeLogoffEntry {
                char_id: att,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now,
                complete_at_unix: now + 30,
                completed: false,
            },
        );
    });
    assert_eq!(
        require_no_pvp_timer_for_filament(&ctx, att).unwrap_err().code,
        "LogoffTimerActive"
    );

    // PvP aggression aborts safe logoff on both attacker & victim and sets Weapons (60s) + PvP (900s)
    with_mut_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.insert(
            vic,
            DisconnectedShipEntry {
                char_id: vic,
                ship_id: 880_002,
                ship_type_id: 606,
                solar_system_id: sys_id,
                disconnected_at_unix: now,
                remove_at_unix: now + 60,
                emergency_warp_dest: None,
                last_position: [200_000.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });
    record_pvp_aggression_and_renew_logoff_timer(&ctx, att, vic, now);
    let (weap, _npc, pvp, _crim) = active_timers_for_char(&ctx, att, now);
    assert_eq!(weap, now + WEAPONS_TIMER_SECS);
    assert_eq!(pvp, now + PVP_TIMER_SECS);
    assert!(has_active_pvp_timer(&ctx, att, now));
    assert_eq!(
        require_no_pvp_timer_for_filament(&ctx, att).unwrap_err().code,
        "PvpTimerActive"
    );
    // Disconnected victim's remove_at_unix was renewed to now + 900s
    let vic_remove = with_sp6_state(&ctx.db, |st| st.disconnected_ships[&vic].remove_at_unix);
    assert_eq!(vic_remove, now + PVP_TIMER_SECS);

    // NPC aggression does NOT apply to a disconnected character (vic), but DOES apply to online char (att)
    record_npc_aggression_timer(&ctx, vic, now);
    assert_eq!(active_timers_for_char(&ctx, vic, now).1, 0);
    record_npc_aggression_timer(&ctx, att, now);
    assert_eq!(active_timers_for_char(&ctx, att, now).1, now + NPC_TIMER_SECS);

    // Clear PvP timer on att, keep Weapons timer -> WeaponsTimerActive
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        if let Some(st) = cw.states.get_mut(&att) {
            st.pvp_timer_until_unix = 0;
        }
    }
    with_mut_sp6_state(&ctx.db, |st| {
        st.pvp_timers_until.remove(&att);
    });
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, att, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );

    // Clear Weapons timer, test WarpScrambled, ShipInWarp, and ShipInvulnerable
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        if let Some(st) = cw.states.get_mut(&att) {
            st.weapon_timer_until_unix = 0;
        }
    }
    with_mut_sp6_state(&ctx.db, |st| {
        st.weapon_timers_until.remove(&att);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .test_with_ballpark_mut_async(move |bp| {
            bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 1;
        })
        .await;
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, att, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys_handle
        .test_with_ballpark_mut_async(move |bp| {
            bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 0;
            bp.combat_states.get_mut(&ship_id).unwrap().gate_cloak_remaining_ticks = 10;
        })
        .await;
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, att, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInvulnerable"
    );
    sys_handle
        .test_with_ballpark_mut_async(move |bp| {
            bp.combat_states.get_mut(&ship_id).unwrap().gate_cloak_remaining_ticks = 0;
            bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::new_warp(
                DVec3::new(100_000.0, 0.0, 0.0),
                DVec3::new(900_000_000.0, 0.0, 0.0),
                3.0,
            );
        })
        .await;
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, att, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
}

#[tokio::test]
async fn safe_logoff_disconnect_reconnect_and_drone_lifecycle_work_end_to_end() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 890_001_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();

    // 1. Docked safe logoff fails with SAFE_LOGOFF_DOCKED
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let docked_res = handle_safe_logoff(&ctx, &mut sess).await.unwrap();
    assert_eq!(
        docked_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])
    );

    // 2. Undock into space, add probe + drone + self-destruct -> safe logoff reports blocking conditions
    sess.station_id = None;
    let start_pos = DVec3::new(500_000.0, 100_000.0, -200_000.0);
    undock_char_in_space(&ctx, char_id, ship_id, sys_id, start_pos);
    register_self_destruct_countdown(&ctx, ship_id, char_id, sys_id, now, now + 120);
    probe_test_helpers::seed_probe(
        &ctx,
        ActiveScannerProbe {
            probe_id: 991_001,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: sys_id,
            position: [0.0, 0.0, 0.0],
            destination: [0.0, 0.0, 0.0],
            range_step: 1,
            scan_range_m: 4.0 * eve_destiny::AU_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: now,
            state: 1,
            warp_end_ft: Some(0),
        },
    );
    ctx.active_moon_probes.write().unwrap().push(ActiveMoonSurveyProbe {
        probe_id: 991_002,
        probe_type_id: 47562,
        owner_char_id: char_id,
        solar_system_id: sys_id,
        hit_moon_id: Some(40_009_082),
        arrival_unix_secs: now + 60,
    });
    let conds = safe_logoff_blocking_space_conditions(&ctx, char_id, ship_id, sys_id);
    assert!(conds.contains(&"SAFE_LOGOFF_PROBES_IN_SPACE"));
    let failed_res = handle_safe_logoff(&ctx, &mut sess).await.unwrap();
    let PyRep::List(failed_items) = failed_res else {
        panic!("expected list");
    };
    assert!(failed_items.contains(&PyRep::String("SAFE_LOGOFF_PROBES_IN_SPACE".into())));
    assert!(failed_items.contains(&PyRep::String("SAFE_LOGOFF_SELF_DESTRUCT_ACTIVE".into())));

    // Clean up blocking state and verify safe logoff succeeds + abort works
    probe_test_helpers::clear_probes(&ctx);
    ctx.active_moon_probes.write().unwrap().clear();
    assert!(abort_self_destruct_countdown(&ctx, ship_id));
    assert!(!abort_self_destruct_countdown(&ctx, ship_id));

    let ok_res = handle_safe_logoff(&ctx, &mut sess).await.unwrap();
    assert_eq!(ok_res, PyRep::List(vec![]));
    assert!(maybe_abort_safe_logoff_on_action(
        &ctx,
        char_id,
        "SAFE_LOGOFF_ABORT_MOVEMENT"
    ));
    assert!(!maybe_abort_safe_logoff_on_action(
        &ctx,
        char_id,
        "SAFE_LOGOFF_ABORT_MOVEMENT"
    ));

    // Start again and abort via handle_abort_safe_logoff
    let _ = handle_safe_logoff(&ctx, &mut sess).await.unwrap();
    assert_eq!(
        handle_abort_safe_logoff(&ctx, &mut sess, "USER_ABORT").unwrap(),
        PyRep::Bool(true)
    );

    // 3. Emergency warp destination is deterministic and 1,000,000 km away
    let dest = compute_emergency_warp_destination(start_pos, char_id, ship_id);
    assert!((dest.distance(start_pos) - EMERGENCY_WARP_DISTANCE_M).abs() < 1.0);

    // 4. Ungraceful disconnect in space initiates emergency warp and records DisconnectedShipEntry
    let before_disc = ctx.now_unix();
    let disc = handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("should record disconnected ship");
    let after_disc = ctx.now_unix();
    assert_eq!(disc.ship_id, ship_id);
    assert!(
        disc.remove_at_unix >= before_disc + UNAGGRESSED_LOGOFF_REMOVE_SECS
            && disc.remove_at_unix <= after_disc + UNAGGRESSED_LOGOFF_REMOVE_SECS,
        "expected remove_at_unix ({}) within [{}, {}]",
        disc.remove_at_unix,
        before_disc + UNAGGRESSED_LOGOFF_REMOVE_SECS,
        after_disc + UNAGGRESSED_LOGOFF_REMOVE_SECS
    );
    assert!(disc.emergency_warp_dest.is_some());

    // Reconnecting in space restores character and clears disconnected entry
    let (rec_sys, rec_ship, rec_pos) =
        reconnect_character_in_space(&ctx, char_id).await.unwrap();
    assert_eq!(rec_sys, sys_id);
    assert_eq!(rec_ship, ship_id);
    assert!((rec_pos - start_pos).length() < 1.0);

    // 5. Abyssal session registration & clearing
    register_abyssal_session(&ctx, char_id, ship_id, 32_000_001, sys_id, now, now + 1200);
    assert!(with_sp6_state(&ctx.db, |st| st.abyssal_sessions.contains_key(&char_id)));
    clear_abyssal_session(&ctx, char_id);
    assert!(!with_sp6_state(&ctx.db, |st| st.abyssal_sessions.contains_key(&char_id)));

    // 6. Drone abandon and reconnect (within range vs OutOfRange)
    let drone_ball_ok = 995_001_i64;
    let drone_item_ok = 995_101_i64;
    let drone_ball_far = 995_002_i64;
    let drone_item_far = 995_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_item_ok,
            make_item(drone_item_ok, 2454, char_id, sys_id as i64, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            drone_item_far,
            make_item(drone_item_far, 2454, char_id, sys_id as i64, InventoryFlag::DroneBay, 1),
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: drone_ball_ok,
            type_id: 2454,
            position: start_pos + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 5000.0,
            radius: 5.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        make_combat_state(100.0, 100.0, 100.0, 0.0),
    );
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: drone_ball_far,
            type_id: 2454,
            position: start_pos + DVec3::new(300_000.0, 0.0, 0.0), // > 250km
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 1.0,
            mass: 5000.0,
            radius: 5.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        make_combat_state(100.0, 100.0, 100.0, 0.0),
    );
    drone_test_helpers::seed_active_drones(
        &ctx,
        [(drone_ball_ok, drone_item_ok), (drone_ball_far, drone_item_far)]
            .into_iter()
            .map(|(bid, did)| DroneSpaceState {
                ball_id: bid,
                drone_item_id: did,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            }),
    );

    let _ = handle_cmd_abandon_drone(&ctx, &mut sess, &[], &HashMap::new())
        .await
        .unwrap();
    assert!(
        drone_test_helpers::active_drone(&ctx, drone_ball_ok)
            .unwrap()
            .is_abandoned
            && drone_test_helpers::active_drone(&ctx, drone_ball_far)
                .unwrap()
                .is_abandoned
    );

    let rec_res = handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new())
        .await
        .unwrap();
    let PyRep::Dict(err_entries) = rec_res else {
        panic!("expected dict");
    };
    let out_of_range = err_entries
        .iter()
        .find(|(k, _)| k.as_str() == Some("OutOfRange"))
        .map(|(_, v)| v.clone());
    assert_eq!(
        out_of_range,
        Some(PyRep::List(vec![PyRep::Integer(drone_ball_far)]))
    );
    assert!(
        !drone_test_helpers::active_drone(&ctx, drone_ball_ok)
            .unwrap()
            .is_abandoned
            && drone_test_helpers::active_drone(&ctx, drone_ball_far)
                .unwrap()
                .is_abandoned
    );
}

#[test]
fn arch_585_three_way_ship_hp_sync_and_unified_repair_clear() {
    let ctx = test_ctx();
    let char_id = 90_585_001_i64;
    let ship_id = 885_585_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut cs = make_combat_state(1_000.0, 2_000.0, 3_000.0, 500.0);
    cs.shield_hp = 400.0;
    cs.armor_hp = 1_200.0;
    cs.structure_hp = 2_500.0;
    persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);

    let (dyn_hull_dmg, dyn_armor_dmg, persisted_hp) = ctx.db.with_snapshot(|snap| {
        let item = &snap.items[&ship_id];
        (
            item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied(),
            item.dynamic_attributes.get(&eve_sde::attr_ids::ARMOR_DAMAGE).copied(),
            snap.sp6_lifecycle.ship_combat_hp.get(&ship_id).cloned(),
        )
    });
    assert_eq!(dyn_hull_dmg, Some(500.0));
    assert_eq!(dyn_armor_dmg, Some(800.0));
    assert_eq!(
        persisted_hp,
        Some(PersistedCombatHp {
            shield_hp: 400.0,
            armor_hp: 1_200.0,
            hull_hp: 2_500.0,
        })
    );

    let unified = ctx.db.with_snapshot(|snap| {
        read_unified_ship_damage_state(&ctx, snap, ship_id, 587, Some((3_000.0, 2_000.0, 1_000.0)))
    });
    assert!((unified.shield_hp - 400.0).abs() < 1e-6);
    assert!((unified.shield_damage - 600.0).abs() < 1e-6);
    assert!((unified.armor_damage - 800.0).abs() < 1e-6);
    assert!((unified.max_armor - 2_000.0).abs() < 1e-6);
    assert!((unified.hull_damage - 500.0).abs() < 1e-6);
    assert!((unified.max_hull - 3_000.0).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        clear_ship_combat_damage_in_snap(snap, ship_id);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&ship_id));
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
    });

    // Dynamic-attributes-only fallback (e.g. legacy docked repair / Damage state) restores cleanly
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 250.0);
            it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 400.0);
        }
    });
    let mut restored = make_combat_state(1_000.0, 2_000.0, 3_000.0, 500.0);
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored);
    assert!((restored.shield_hp - 1_000.0).abs() < 1e-6);
    assert!((restored.armor_hp - 1_600.0).abs() < 1e-6);
    assert!((restored.structure_hp - 2_750.0).abs() < 1e-6);

    // `persist_ship_combat_hp_batch_inner` skips unknown entities when `require_persisted_entity=true`
    // and records `ship_combat_hp` when `require_persisted_entity=false`.
    let unknown_ball = 999_888_777_i64;
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(unknown_ball, &cs)], true);
    assert!(ctx
        .db
        .with_snapshot(|s| !s.sp6_lifecycle.ship_combat_hp.contains_key(&unknown_ball)));
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(unknown_ball, &cs)], false);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&unknown_ball).cloned()),
        Some(PersistedCombatHp {
            shield_hp: 400.0,
            armor_hp: 1_200.0,
            hull_hp: 2_500.0,
        })
    );
    // Repeat identical batch call is a zero-write no-op.
    let epoch_before = ctx.db.state_epoch();
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(unknown_ball, &cs)], false);
    assert_eq!(ctx.db.state_epoch(), epoch_before);
}

#[test]
fn w5a_p2_0c_sp6_and_crimewatch_restore_guards_preserve_state_across_closure_panic() {
    let ctx = test_ctx();
    let char_id = 90_904_001_i64;
    let ship_id = 885_904_001_i64;

    with_mut_sp6_state(&ctx.db, |st| {
        st.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 500.0,
                armor_hp: 600.0,
                hull_hp: 700.0,
            },
        );
    });
    ctx.crimewatch.with_mut_crimewatch(|dir| {
        dir.trigger_suspect_flag(char_id, 10_000);
        dir.aggression_mgr
            .record_aggression(char_id, 90_904_002, 30_000_142, false, false, 10_000);
    });

    // 1. Panic inside try_with_mut_sp6_state rolls back working snapshot + SQLite tx without wiping
    //    pre-existing sp6_lifecycle state (#904 / #833 / #921).
    let sp6_err = try_with_mut_sp6_state(&ctx.db, |st| -> Result<(), String> {
        st.ship_combat_hp.clear();
        st.ship_combat_hp.insert(
            999_999,
            PersistedCombatHp {
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
            },
        );
        panic!("simulated sp6 closure panic");
    });
    assert!(sp6_err.is_err());
    assert_eq!(
        with_sp6_state(&ctx.db, |st| st.ship_combat_hp.get(&ship_id).cloned()),
        Some(PersistedCombatHp {
            shield_hp: 500.0,
            armor_hp: 600.0,
            hull_hp: 700.0,
        }),
        "try_with_mut_sp6_state + tx rollback must preserve pre-panic sp6_lifecycle state (#904 / #921)"
    );
    assert!(!with_sp6_state(&ctx.db, |st| st.ship_combat_hp.contains_key(&999_999)));

    // 2. Panic inside try_with_mut_crimewatch rolls back crimewatch_states and crimewatch_aggressions
    //    without wiping pre-existing state (#904 / #833 / #921).
    let cw_err: Result<(), String> = ctx.crimewatch.try_with_mut_crimewatch(|dir| {
        dir.states.clear();
        dir.aggression_mgr.records.clear();
        panic!("simulated crimewatch closure panic");
    });
    assert!(cw_err.is_err());
    ctx.crimewatch.with_crimewatch(|dir| {
        assert!(
            dir.is_suspect(char_id, 10_100),
            "try_with_mut_crimewatch + tx rollback must preserve pre-panic suspect state (#904 / #921)"
        );
        assert!(
            dir.get_aggression(char_id, 90_904_002, 10_100).is_some(),
            "try_with_mut_crimewatch + tx rollback must preserve pre-panic aggressions (#904 / #921)"
        );
    });
}

#[tokio::test]
async fn bf04_reconnect_stops_outbound_emergency_warp_and_delivers_set_state() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let orig_pos = DVec3::new(15_000_000_000.0, 0.0, 0.0);

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        snap.item_positions.insert(ch.active_ship_id, orig_pos.to_array());
        ch.active_ship_id
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: orig_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. Disconnecting starts an outbound ~1,000,000 km emergency warp.
    let disc = handle_character_logoff_or_disconnect_sync(&ctx, char_id)
        .expect("disconnected ship hold");
    assert!(disc.pre_warp_position.is_some());
    let (_, warp_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(warp_ball.mode, MovementMode::Warp { .. }));

    // Reconnecting while still <= 10,000 m from `orig_pos` (no return warp started) must
    // stop the outbound emergency warp and NEVER insert `ship_id` into `ctx.active_warps` (#843).
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect");
    assert!(
        !sys_handle.is_active_warp(ship_id),
        "emergency warp must not be tracked in active_warps (#843)"
    );
    let (_, stop_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(stop_ball.mode, MovementMode::Stop),
        "outbound emergency warp within 10km must stop on reconnect"
    );

    // 2. Reconnecting after moving > 10 km away initiates return warp without entering
    // `active_warps`, and `UpdateStateRequest` still delivers `SetState`.
    let _ = handle_character_logoff_or_disconnect_sync(&ctx, char_id).expect("logoff again");
    sys_handle
        .test_with_ballpark_mut_async(move |bp| {
            if let Some(b) = bp.balls.get_mut(&ship_id) {
                b.position = orig_pos + DVec3::new(50_000_000.0, 0.0, 0.0);
            }
        })
        .await;
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect return warp");
    assert!(!sys_handle.is_active_warp(ship_id));
    let _ = sys_handle.drain_observer_updates(ship_id).await;

    let mut sess = SessionState::new(991_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let bnd = ctx.bind_object(&sess, "beyonce", i64::from(sys_id)).unwrap();
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bnd),
            "UpdateStateRequest",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(res.unwrap(), PyRep::None);
    let (_, obs_evs, _) = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(
        obs_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "UpdateStateRequest on reconnect must deliver SetState (#843)"
    );
    let (_, post_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        sys_handle.is_return_warp(ship_id),
        "SP-6 reconnect return-warp must remain tracked in return_warps across UpdateStateRequest (#843)"
    );
    assert!(
        matches!(
            post_ball.mode,
            MovementMode::Warp { destination, .. } if destination.distance(orig_pos) < 1.0
        ),
        "UpdateStateRequest on reconnect must preserve return warp toward orig_pos (#843): {:?}",
        post_ball.mode
    );
}

fn decode_set_state_ship_ball(pyrep: &PyRep, ship_id: i64) -> eve_destiny::wire::DecodedBall {
    let PyRep::List(entries) = pyrep else {
        panic!("expected DoDestinyUpdate PyRep::List, got {pyrep:?}");
    };
    for entry in entries {
        let PyRep::Tuple(stamped) = entry else {
            continue;
        };
        let Some(PyRep::Tuple(action)) = stamped.get(1) else {
            continue;
        };
        if action.first() != Some(&PyRep::String("SetState".into())) {
            continue;
        }
        let Some(PyRep::Tuple(args)) = action.get(1) else {
            panic!("expected SetState args tuple, got {action:?}");
        };
        let Some(PyRep::KeyVal(kv)) = args.first() else {
            panic!("expected SetState KeyVal, got {args:?}");
        };
        let Some((_, PyRep::Bytes(state_bytes))) = kv.iter().find(|(k, _)| *k == "state") else {
            panic!("expected SetState state bytes");
        };
        let (_, _, decoded) =
            eve_destiny::wire::decode_add_balls(state_bytes).expect("decode SetState state bytes");
        return decoded
            .into_iter()
            .find(|b| b.id == ship_id)
            .expect("ship ball present in SetState bytes");
    }
    panic!("SetState action missing from DoDestinyUpdate: {pyrep:?}");
}

#[tokio::test]
async fn reconnect_mid_outbound_warp_redirects_cleanly_and_encodes_return_warp_set_state() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let orig_pos = DVec3::new(15_000_000_000.0, 0.0, 0.0);

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        snap.item_positions.insert(ch.active_ship_id, orig_pos.to_array());
        ch.active_ship_id
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: orig_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = handle_character_logoff_or_disconnect_sync(&ctx, char_id)
        .expect("disconnected ship hold");

    // Step until outbound emergency warp leaves WarpPhase::Aligning (> 10 km from orig_pos),
    // which previously caused set_mode() to fail with ShipAlreadyInWarp (#1454 / #1458).
    let mut left_aligning = false;
    for _ in 0..30 {
        let _ = sys_handle.step_manual(1.0).await;
        let (_, b) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(
            b.mode,
            MovementMode::Warp { phase, .. } if phase != eve_destiny::WarpPhase::Aligning
        ) && b.position.distance(orig_pos) > 10_000.0
        {
            left_aligning = true;
            break;
        }
    }
    assert!(left_aligning, "outbound emergency warp must advance past Aligning phase");

    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect mid-outbound emergency warp");

    let (_, obs_evs, pyrep) = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    assert_eq!(
        obs_evs.first(),
        Some(&eve_destiny::DestinyEvent::SetState {
            observer_ball_id: ship_id
        }),
        "reconnect must emit SetState at index 0 after clearing prior events and setting mode (#1458), got {obs_evs:?}"
    );
    assert_eq!(
        obs_evs
            .iter()
            .filter(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. }))
            .count(),
        1
    );
    assert!(
        !obs_evs
            .iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { .. })),
        "reconnect must not emit stale Stop before SetState (#1458), got {obs_evs:?}"
    );
    let wire_ball = decode_set_state_ship_ball(&pyrep, ship_id);
    assert_eq!(
        wire_ball.mode,
        eve_destiny::wire::DST_BALL_WARP,
        "SetState must encode DST_BALL_WARP return warp, never stale Stop (#1458)"
    );
    assert!(
        wire_ball
            .warp_destination
            .is_some_and(|dest| dest.distance(orig_pos) < 1.0),
        "SetState return warp must target orig_pos, got {:?}",
        wire_ball.warp_destination
    );
    assert!(sys_handle.is_return_warp(ship_id));
    assert_eq!(
        sys_handle.warp_provenance(ship_id),
        Some(crate::system_actor::WarpProvenance::ReconnectReturnWarp)
    );
    let (_, post_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(
        post_ball.mode,
        MovementMode::Warp {
            destination,
            phase: eve_destiny::WarpPhase::Aligning,
            ..
        } if destination.distance(orig_pos) < 1.0
    ));
}

#[tokio::test]
async fn reconnect_without_existing_ball_queues_set_state_after_return_warp_mode() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let orig_pos = DVec3::new(15_000_000_000.0, 0.0, 0.0);
    let displaced_pos = orig_pos + DVec3::new(250_000.0, 0.0, 0.0);
    let outbound_dest = orig_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let now_unix = ctx.now_unix();

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        let sid = ch.active_ship_id;
        snap.item_positions.insert(sid, displaced_pos.to_array());
        snap.sp6_lifecycle
            .as_inner_mut()
            .disconnected_ships
            .insert(
                char_id,
                DisconnectedShipEntry {
                    char_id,
                    ship_id: sid,
                    ship_type_id: 606,
                    solar_system_id: sys_id,
                    disconnected_at_unix: now_unix,
                    remove_at_unix: now_unix + 60,
                    emergency_warp_dest: Some(outbound_dest.to_array()),
                    last_position: displaced_pos.to_array(),
                    last_velocity: [0.0, 0.0, 0.0],
                    removed_from_space: false,
                    pre_warp_position: Some(orig_pos.to_array()),
                },
            );
        sid
    });

    reconnect_character_in_space_sync(&ctx, char_id)
        .expect("reconnect spawning missing ball must succeed");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, obs_evs, pyrep) = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    assert_eq!(
        obs_evs.first(),
        Some(&eve_destiny::DestinyEvent::SetState {
            observer_ball_id: ship_id
        }),
        "existing_ball == false reconnect must emit SetState at index 0 after return warp set_mode (#1458), got {obs_evs:?}"
    );
    assert_eq!(
        obs_evs
            .iter()
            .filter(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. }))
            .count(),
        1
    );
    assert!(
        !obs_evs
            .iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { .. })),
        "existing_ball == false reconnect must not emit stale Stop (#1458), got {obs_evs:?}"
    );
    let wire_ball = decode_set_state_ship_ball(&pyrep, ship_id);
    assert_eq!(wire_ball.mode, eve_destiny::wire::DST_BALL_WARP);
    assert!(wire_ball
        .warp_destination
        .is_some_and(|dest| dest.distance(orig_pos) < 1.0));
    assert!(sys_handle.is_return_warp(ship_id));
    assert_eq!(
        sys_handle.warp_provenance(ship_id),
        Some(crate::system_actor::WarpProvenance::ReconnectReturnWarp)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_hold_immobilize_module_blocks_emergency_warp_via_system_view_1342() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 890_777_i64;
    let sys_id = 30_000_142_u32;
    let start_pos = DVec3::new(350_000.0, 25_000.0, -50_000.0);

    undock_char_in_space(&ctx, char_id, ship_id, sys_id, start_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: 999_777,
            module_type_id: 33_400,
            effect_name: "bastion".to_string(),
            duration_s: 60.0,
            immobilize_self: true,
            ..crate::system_actor::ActiveModuleCycle::default()
        })
        .await
        .unwrap();

    assert!(
        sys_handle.view().has_immobilize_or_no_assist_module(ship_id),
        "SystemView must mirror immobilizing module cycles for lock-free disconnect hold checks"
    );
    let disc = handle_character_logoff_or_disconnect_sync(&ctx, char_id)
        .expect("should record disconnected ship");
    assert!(
        disc.emergency_warp_dest.is_none(),
        "immobilizing active module cycle on SystemView must suppress emergency warp"
    );

    let (rec_sys, rec_ship, rec_pos) = reconnect_character_in_space_sync(&ctx, char_id)
        .expect("reconnect after immobilized logoff must succeed");
    assert_eq!(rec_sys, sys_id);
    assert_eq!(rec_ship, ship_id);
    assert!((rec_pos - start_pos).length() < 1.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_system_filament_ready_and_safe_logoff_do_not_spawn_actor() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 890_999_i64;
    let cold_sys = 30_000_144_u32;
    let now = ctx.now_unix();

    assert!(ctx.systems.get_handle(cold_sys).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = cold_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, char_id, cold_sys as i64, InventoryFlag::None, 1),
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(1, 9001, Some(char_id), true, now);

    // 1. Weapons timer rejection runs BEFORE get_handle without waking cold_sys.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = now + 60;
    }
    let weapons_err = require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, cold_sys)
        .expect_err("weapons timer must fail before space check");
    assert_eq!(weapons_err.code, "WeaponsTimerActive");
    assert!(ctx.systems.get_handle(cold_sys).is_none());

    // 2. Clean character/ship on cold system fails closed with ShipNotInSpace.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = 0;
    }
    let cold_err = require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, cold_sys)
        .expect_err("cold system must fail closed with ShipNotInSpace");
    assert_eq!(cold_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(cold_sys).is_none());

    // 3. Safe logoff checks & countdown start stay cold without spawning an actor.
    assert!(safe_logoff_blocking_space_conditions(&ctx, char_id, ship_id, cold_sys).is_empty());
    assert!(ctx.systems.get_handle(cold_sys).is_none());

    let mut sess = SessionState::new(901_542);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(cold_sys);
    sess.ship_id = Some(ship_id);
    let safe_res = handle_safe_logoff(&ctx, &mut sess)
        .await
        .expect("clean safe logoff on cold system must succeed");
    assert_eq!(safe_res, PyRep::List(vec![]));
    assert!(with_sp6_state(&ctx.db, |st| st.safe_logoffs.contains_key(&char_id)));
    assert!(ctx.systems.get_handle(cold_sys).is_none());
}


