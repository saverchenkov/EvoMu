//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle`.

use super::*;
use crate::router::{ActiveMoonSurveyProbe, ActiveScannerProbe, ServerContext};
use crate::services::combat_crimewatch::DroneSpaceState;
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedCombatHp, WorldRepository};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
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

fn undock_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, pos: DVec3) {
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
fn deep_sde_helpers_hp_persistence_and_metadata_boundaries() {
    let mut sde = SdeRegistry::default();
    let mk_type = |tid: u32, attrs: Vec<(u16, f64)>| InvType {
        type_id: tid,
        group_id: 361,
        category_id: 22,
        name: "TestObj".into(),
        mass: 50.0,
        volume: 10.0,
        capacity: 0.0,
        radius: 5.0,
        base_price: 100.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: attrs.into_iter().collect(),
    };
    // Millisecond attribute (> 1_000_000.0) divides by 1000
    sde.types.insert(9001, mk_type(9001, vec![(1237, 7_200_000.0)]));
    // Fallback attribute 1034 in seconds (>= 3_600)
    sde.types.insert(9002, mk_type(9002, vec![(1034, 5_000.0)]));
    // Below minimum threshold (3_599 < 3_600) falls back to default 48h
    sde.types.insert(9003, mk_type(9003, vec![(1237, 3_599.0)]));
    // Sub-minimum and over-maximum ship physics clamping
    sde.types.insert(
        9004,
        mk_type(9004, vec![(37, 10.0), (70, 0.1), (1281, 0.1), (600, 0.2)]),
    );
    sde.types.insert(
        9005,
        mk_type(9005, vec![(37, 900.0), (70, 5.0), (1281, 10.0), (600, 3.0)]),
    );

    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&sde, 9001), 7_200);
    assert_eq!(sde_mobile_warp_disruptor_decay_secs(&sde, 9002), 5_000);
    assert_eq!(
        sde_mobile_warp_disruptor_decay_secs(&sde, 9003),
        MOBILE_WARP_DISRUPTOR_DECAY_SECS
    );
    assert_eq!(
        sde_mobile_warp_disruptor_decay_secs(&sde, u32::MAX),
        MOBILE_WARP_DISRUPTOR_DECAY_SECS
    );

    let (_g, _c, mass_lo, rad_lo, vel_lo, in_lo, warp_lo) = sde_ship_physics(&sde, 9004);
    assert_eq!(mass_lo, 1_000.0);
    assert_eq!(rad_lo, 20.0);
    assert_eq!(vel_lo, 50.0);
    assert_eq!(in_lo, 0.5);
    assert_eq!(warp_lo, 1.5);

    let (_g2, _c2, _m2, _r2, vel_hi, _i2, warp_hi) = sde_ship_physics(&sde, 9005);
    assert_eq!(vel_hi, 900.0);
    assert_eq!(warp_hi, 12.0);
    assert_eq!(
        sde_ship_physics(&sde, u32::MAX),
        (25, 6, 1_200_000.0, 40.0, 300.0, 2.5, 3.0)
    );

    // Emergency warp destination is deterministic and exactly 1e9 m away
    let origin = DVec3::new(100_000.0, -50_000.0, 250_000.0);
    let d1 = compute_emergency_warp_destination(origin, 90_000_001, 100_000_001);
    let d2 = compute_emergency_warp_destination(origin, 90_000_001, 100_000_001);
    let d3 = compute_emergency_warp_destination(origin, 90_000_002, 100_000_001);
    assert_eq!(d1, d2);
    assert_ne!(d1, d3);
    assert!((d1.distance(origin) - EMERGENCY_WARP_DISTANCE_M).abs() < 1e-2);

    // Persisted HP idempotency + clamping + zero hull preservation
    let ctx = test_ctx();
    let ship_id = 100_000_001;
    let cs = CombatState {
        shield_hp: 150.0,
        max_shield_hp: 200.0,
        armor_hp: 80.0,
        max_armor_hp: 100.0,
        structure_hp: 0.0,
        max_structure_hp: 300.0,
        ..CombatState::default()
    };
    persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);
    persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);
    let stored = with_sp6_state(&ctx.db, |st| st.ship_combat_hp.get(&ship_id).copied());
    assert_eq!(
        stored,
        Some(PersistedCombatHp {
            shield_hp: 150.0,
            armor_hp: 80.0,
            hull_hp: 0.0,
        })
    );
    let mut restored = CombatState {
        shield_hp: 10.0,
        max_shield_hp: 120.0,
        armor_hp: 10.0,
        max_armor_hp: 50.0,
        structure_hp: 222.0,
        max_structure_hp: 300.0,
        ..CombatState::default()
    };
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored);
    assert_eq!(restored.shield_hp, 120.0);
    assert_eq!(restored.armor_hp, 50.0);
    assert_eq!(restored.structure_hp, 222.0);
}

#[tokio::test]
async fn deep_aggression_timers_and_filament_preconditions() {
    let ctx = test_ctx();
    let attacker = 90_000_001;
    let victim = 90_000_002;
    let sys_id = 30_000_142;
    let ship_id = 100_000_001;
    let pos = DVec3::new(150_000.0, 0.0, 0.0);
    undock_ship(&ctx, attacker, ship_id, sys_id, pos);
    let now_unix = ctx.now_unix();

    // Put disconnected ship for victim and verify PvP renews its removal timer while NPC does not
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            attacker,
            SafeLogoffEntry {
                char_id: attacker,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + SAFE_LOGOFF_COUNTDOWN_SECS,
                completed: false,
            },
        );
        st.disconnected_ships.insert(
            victim,
            DisconnectedShipEntry {
                char_id: victim,
                ship_id: 100_000_002,
                ship_type_id: 606,
                solar_system_id: sys_id,
                disconnected_at_unix: now_unix,
                remove_at_unix: now_unix + 60,
                emergency_warp_dest: None,
                last_position: [200_000.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    record_npc_aggression_timer(&ctx, victim, now_unix);
    let victim_npc = with_sp6_state(&ctx.db, |st| st.npc_timers_until.get(&victim).copied());
    assert_eq!(victim_npc, None);

    record_pvp_aggression_and_renew_logoff_timer(&ctx, attacker, victim, now_unix);
    assert!(!with_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.contains_key(&attacker)
    }));
    let victim_disc_remove = with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.get(&victim).map(|d| d.remove_at_unix)
    });
    assert_eq!(victim_disc_remove, Some(now_unix + PVP_TIMER_SECS));
    assert!(has_active_pvp_timer(&ctx, attacker, now_unix));
    assert_eq!(
        require_no_pvp_timer_for_filament(&ctx, attacker)
            .unwrap_err()
            .code,
        "PvpTimerActive"
    );

    // Clear crimewatch/sp6 timers and test WeaponsTimerActive, LogoffTimerActive, ShipInWarp, WarpScrambled, ShipInvulnerable, and clean Ok
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.clear();
    }
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.pvp_timers_until.clear();
        st.criminal_timers_until.clear();
        st.weapon_timers_until.insert(attacker, now_unix + 30);
    });
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );

    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until.clear();
        st.safe_logoffs.insert(
            attacker,
            SafeLogoffEntry {
                char_id: attacker,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 30,
                completed: false,
            },
        );
    });
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id)
            .unwrap_err()
            .code,
        "LogoffTimerActive"
    );
    assert!(maybe_abort_safe_logoff_on_action(
        &ctx,
        attacker,
        "SAFE_LOGOFF_ABORT_MOVEMENT"
    ));
    assert!(!maybe_abort_safe_logoff_on_action(
        &ctx,
        attacker,
        "SAFE_LOGOFF_ABORT_MOVEMENT"
    ));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(ball) = bp.balls.get_mut(&ship_id) {
            ball.mode = MovementMode::new_warp(pos, pos + DVec3::X * 500_000.0, 3.0);
        }
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(ball) = bp.balls.get_mut(&ship_id) {
            ball.mode = MovementMode::Stop;
        }
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 2;
        }
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 0;
            cs.gate_cloak_remaining_ticks = 10;
        }
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInvulnerable"
    );

    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.gate_cloak_remaining_ticks = 0;
        }
    }
    assert!(require_ship_ready_for_filament_jump(&ctx, attacker, ship_id, sys_id).is_ok());
}

#[tokio::test]
async fn deep_safe_logoff_disconnect_reconnect_and_drones_paths() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;

    // 1. Unauthenticated & docked safe logoff refusal
    let mut docked_sess = SessionState::new(1);
    assert!(handle_safe_logoff(&ctx, &mut docked_sess).await.is_err());
    docked_sess.user_id = Some(1);
    docked_sess.character_id = Some(char_id);
    docked_sess.station_id = Some(60_003_760);
    docked_sess.ship_id = Some(ship_id);
    let docked_res = handle_safe_logoff(&ctx, &mut docked_sess)
        .await
        .expect("docked safe logoff");
    assert_eq!(
        docked_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])
    );

    // 2. Undocked with drones & probes -> refuses with SAFE_LOGOFF_DRONES_IN_SPACE + SAFE_LOGOFF_PROBES_IN_SPACE
    let pos = DVec3::new(250_000.0, 0.0, 0.0);
    undock_ship(&ctx, char_id, ship_id, sys_id, pos);
    let drone_id = 710_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            make_item(drone_id, 2454, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
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
                max_velocity: 1_000.0,
                optimal_range: 5_000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    ctx.active_scanner_probes.write().unwrap().insert(
        710_100,
        ActiveScannerProbe {
            probe_id: 710_100,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: sys_id,
            position: pos.into(),
            destination: pos.into(),
            range_step: 1,
            scan_range_m: 4.0 * eve_destiny::AU_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: ctx.now_unix(),
            state: 1,
            warp_end_ft: Some(0),
        },
    );
    ctx.active_moon_probes
        .write()
        .unwrap()
        .push(ActiveMoonSurveyProbe {
            probe_id: 710_101,
            probe_type_id: 47562,
            owner_char_id: char_id,
            solar_system_id: sys_id,
            hit_moon_id: Some(40_009_082),
            arrival_unix_secs: ctx.now_unix() + 60,
        });

    let mut space_sess = SessionState::new(2);
    space_sess.user_id = Some(1);
    space_sess.character_id = Some(char_id);
    space_sess.solar_system_id = Some(sys_id);
    space_sess.ship_id = Some(ship_id);
    let conds = safe_logoff_blocking_space_conditions(&ctx, char_id, ship_id, sys_id);
    assert!(conds.contains(&"SAFE_LOGOFF_DRONES_IN_SPACE"));
    assert!(conds.contains(&"SAFE_LOGOFF_PROBES_IN_SPACE"));

    // Abandon drone then reconnect to it
    let abandon_rep = handle_cmd_abandon_drone(
        &ctx,
        &mut space_sess,
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &HashMap::new(),
    )
    .await
    .expect("abandon drone");
    assert_eq!(abandon_rep, PyRep::Dict(vec![]));
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space[&drone_id].is_abandoned
    }));

    let reconnect_rep = handle_cmd_reconnect_to_drones(
        &ctx,
        &mut space_sess,
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &HashMap::new(),
    )
    .await
    .expect("reconnect drone");
    assert_eq!(reconnect_rep, PyRep::Dict(vec![]));
    assert!(!with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space[&drone_id].is_abandoned
    }));

    // Clean drones & probes -> start safe logoff -> abort safe logoff
    with_mut_combat_runtime(&ctx, |rt| rt.active_drones_in_space.clear());
    ctx.active_scanner_probes.write().unwrap().clear();
    ctx.active_moon_probes.write().unwrap().clear();
    let started = handle_safe_logoff(&ctx, &mut space_sess)
        .await
        .expect("start safe logoff");
    assert_eq!(started, PyRep::List(vec![]));
    let aborted =
        handle_abort_safe_logoff(&ctx, &mut space_sess, "SAFE_LOGOFF_ABORT_CLIENT")
            .expect("abort safe logoff");
    assert_eq!(aborted, PyRep::Bool(true));

    // Ungraceful disconnect in space initiates emergency warp & reconnect restores state
    let before_unix = ctx.now_unix();
    let disc_entry = handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnect entry");
    assert!(disc_entry.emergency_warp_dest.is_some());
    assert!(disc_entry.disconnected_at_unix >= before_unix);
    assert_eq!(
        disc_entry.remove_at_unix,
        disc_entry.disconnected_at_unix + UNAGGRESSED_LOGOFF_REMOVE_SECS
    );
    let (_re_sys, re_ship, _re_pos) = reconnect_character_in_space(&ctx, char_id)
        .await
        .expect("reconnect in space");
    assert_eq!(re_ship, ship_id);
}
