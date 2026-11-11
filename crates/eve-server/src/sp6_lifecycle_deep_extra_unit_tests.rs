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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[test]
fn persist_existing_batch_handles_npc_structures_epsilon_and_zero_max_clamps() {
    let ctx = test_ctx();
    let ship_id = 100_910_001_i64;
    let st_id = 1_055_910_001_i64;
    let npc_id = 999_888_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, 90_000_001, 30_000_142, InventoryFlag::None, 1),
        );
        snap.structures.insert(
            st_id,
            eve_db::UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Persist Test Keepstar".into(),
                position: [15_000_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let mut ship_cs = ctx.build_ship_combat_state(90_000_001, ship_id, 606);
    ship_cs.max_shield_hp = 300.0;
    ship_cs.shield_hp = 200.0;
    ship_cs.max_armor_hp = 500.0;
    ship_cs.armor_hp = 350.0;
    ship_cs.max_structure_hp = 400.0;
    ship_cs.structure_hp = 300.0;

    let mut st_cs = ctx.build_ship_combat_state(90_000_001, st_id, 606);
    st_cs.shield_hp = 80_000.0;
    st_cs.armor_hp = 70_000.0;
    st_cs.structure_hp = 90_000.0;

    let mut npc_cs = ctx.build_ship_combat_state(90_000_001, npc_id, 606);
    npc_cs.shield_hp = 10.0;

    persist_existing_ship_combat_hp_batch_in_db(
        &ctx.db,
        [(npc_id, &npc_cs), (ship_id, &ship_cs), (st_id, &st_cs)],
    );

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&npc_id));
        assert!(snap.sp6_lifecycle.ship_combat_hp.contains_key(&st_id));
        let shp = snap.sp6_lifecycle.ship_combat_hp[&ship_id];
        assert!((shp.shield_hp - 200.0).abs() < 1e-9);
        assert!((shp.armor_hp - 350.0).abs() < 1e-9);
        assert!((shp.hull_hp - 300.0).abs() < 1e-9);
        let it = &snap.items[&ship_id];
        assert_eq!(it.version, 2);
        assert!((it.dynamic_attributes[&eve_sde::attr_ids::ARMOR_DAMAGE] - 150.0).abs() < 1e-9);
        assert!((it.dynamic_attributes[&eve_sde::attr_ids::DAMAGE] - 100.0).abs() < 1e-9);
    });

    // Sub-epsilon perturbation (<= 1e-6) must NOT overwrite dynamic damage attrs or bump version.
    let mut eps_cs = ship_cs.clone();
    eps_cs.armor_hp -= 1e-7;
    eps_cs.structure_hp += 1e-7;
    persist_existing_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &eps_cs)]);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&ship_id];
        assert_eq!(it.version, 2);
        assert!((it.dynamic_attributes[&eve_sde::attr_ids::ARMOR_DAMAGE] - 150.0).abs() < 1e-9);
        assert!((it.dynamic_attributes[&eve_sde::attr_ids::DAMAGE] - 100.0).abs() < 1e-9);
    });

    // Repairing back to full removes DAMAGE/ARMOR_DAMAGE and bumps version.
    let mut repaired_cs = ship_cs.clone();
    repaired_cs.armor_hp = 500.0;
    repaired_cs.structure_hp = 400.0;
    persist_existing_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &repaired_cs)]);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&ship_id];
        assert_eq!(it.version, 3);
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
    });

    // Zero max HP skips damage attributes even when HP is negative, and clamps persisted HP to 0.0.
    let mut zero_max_cs = ship_cs;
    zero_max_cs.max_armor_hp = 0.0;
    zero_max_cs.max_structure_hp = 0.0;
    zero_max_cs.shield_hp = -15.0;
    zero_max_cs.armor_hp = -5.0;
    zero_max_cs.structure_hp = -3.0;
    persist_existing_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &zero_max_cs)]);
    ctx.db.with_snapshot(|snap| {
        let shp = snap.sp6_lifecycle.ship_combat_hp[&ship_id];
        assert_eq!(shp.shield_hp, 0.0);
        assert_eq!(shp.armor_hp, 0.0);
        assert_eq!(shp.hull_hp, 0.0);
        let it = &snap.items[&ship_id];
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_unified_and_apply_persisted_hp_cover_defaults_max_clamp_and_zero_hull() {
    let ctx = test_ctx();
    let ship_id = 100_910_002_i64;
    let char_id = 90_000_001_i64;

    // Unknown SDE type fallback defaults (hull 0.0, armor 0.0, shield 400.0).
    let def_state = ctx.db.with_snapshot(|snap| {
        read_unified_ship_damage_state(&ctx, snap, 888_777_666, 999_999_999, None)
    });
    assert_eq!(def_state.max_hull, 0.0);
    assert_eq!(def_state.max_armor, 0.0);
    assert_eq!(def_state.max_shield, 400.0);
    assert_eq!(def_state.shield_hp, 400.0);
    assert_eq!(def_state.shield_damage, 0.0);
    assert_eq!(def_state.armor_damage, 0.0);
    assert_eq!(def_state.hull_damage, 0.0);

    // Spawn live ball with larger maxes than fallback; reconcile max(live, persisted, dyn) + clamp.
    let sys_id = 30_000_142_u32;
    let pos = DVec3::new(15_000_000_000.0, 0.0, 0.0);
    undock_ship(&ctx, char_id, ship_id, sys_id, pos);
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_handle.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.max_shield_hp = 600.0;
        cs.shield_hp = 500.0;
        cs.max_armor_hp = 700.0;
        cs.armor_hp = 650.0;
        cs.max_structure_hp = 800.0;
        cs.structure_hp = 780.0;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: -50.0,
                armor_hp: 500.0,
                hull_hp: 750.0,
            },
        );
        let it = snap.items.get_mut(&ship_id).unwrap();
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 300.0);
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 40.0);
    });

    let unified = ctx.db.with_snapshot(|snap| {
        read_unified_ship_damage_state(&ctx, snap, ship_id, 606, Some((100.0, 200.0, 300.0)))
    });
    assert_eq!(unified.max_shield, 600.0);
    assert_eq!(unified.max_armor, 700.0);
    assert_eq!(unified.max_hull, 800.0);
    assert_eq!(unified.shield_damage, 650.0);
    assert_eq!(unified.shield_hp, 0.0);
    assert_eq!(unified.armor_damage, 200.0);
    assert_eq!(unified.hull_damage, 300.0);

    // apply_persisted_hp_to_combat_state: persisted takes precedence; hull_hp == 0.0 leaves structure_hp intact.
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 606);
    cs.max_shield_hp = 200.0;
    cs.max_armor_hp = 300.0;
    cs.max_structure_hp = 400.0;
    cs.structure_hp = 375.0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 999.0,
                armor_hp: -10.0,
                hull_hp: 0.0,
            },
        );
    });
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut cs);
    assert_eq!(cs.shield_hp, 200.0);
    assert_eq!(cs.armor_hp, 0.0);
    assert_eq!(cs.structure_hp, 375.0);

    // Non-zero persisted hull_hp updates and clamps structure_hp.
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 50.0,
                armor_hp: 60.0,
                hull_hp: 999.0,
            },
        );
    });
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut cs);
    assert_eq!(cs.structure_hp, 400.0);

    // Fallback to dynamic_attributes when ship_combat_hp is absent; non-positive attrs ignored.
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.remove(&ship_id);
        let it = snap.items.get_mut(&ship_id).unwrap();
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 30.0);
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 40.0);
    });
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut cs);
    assert_eq!(cs.armor_hp, 270.0);
    assert_eq!(cs.structure_hp, 360.0);

    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&ship_id).unwrap();
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 0.0);
        it.dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, -5.0);
    });
    cs.armor_hp = 250.0;
    cs.structure_hp = 350.0;
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut cs);
    assert_eq!(cs.armor_hp, 250.0);
    assert_eq!(cs.structure_hp, 350.0);
}

#[test]
fn clear_ship_combat_damage_version_bump_and_persisted_cleanup() {
    let ctx = test_ctx();
    let ship_id = 100_910_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, 90_000_001, 30_000_142, InventoryFlag::None, 1),
        );
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 10.0,
                armor_hp: 20.0,
                hull_hp: 30.0,
            },
        );
    });

    // Clean item: removes ship_combat_hp without bumping item version.
    ctx.db
        .with_mut_snapshot(|snap| clear_ship_combat_damage_in_snap(snap, ship_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].version, 1);
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&ship_id));
    });

    // Only ARMOR_DAMAGE bumps version once.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 12.0);
        clear_ship_combat_damage_in_snap(snap, ship_id);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].version, 2);
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
    });

    // Only DAMAGE bumps version once.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 25.0);
        clear_ship_combat_damage_in_snap(snap, ship_id);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].version, 3);
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filament_jump_locks_and_reconnect_in_space_sync_pre_warp_and_distance() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_910_004_i64;
    let sys_id = 30_000_142_u32;
    let base_pos = DVec3::new(15_000_000_000.0, 0.0, 0.0);
    undock_ship(&ctx, char_id, ship_id, sys_id, base_pos);

    // Cloaked and tethered still allow Abyssal/Void filament readiness (reject_when_cloaked/tethered = false).
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_handle.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = true;
        cs.is_tethered = true;
    }
    assert!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_ok(),
        "cloaked/tethered ship must still be allowed for filament jump readiness"
    );

    // Lance disruption blocks filament jump.
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_handle.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = false;
        cs.is_tethered = false;
        cs.lance_disrupted_remaining_s = 12.0;
    }
    let lance_err = require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
        .expect_err("lance disruption must block filament");
    assert_eq!(lance_err.code, "DisruptiveLanceDebuffActive");

    // Clear lance debuff, add nav lock -> still Err.
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().lance_disrupted_remaining_s = 0.0;
    }
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((ctx.instance_id as usize, ship_id), ctx.now_unix() + 300);
    assert!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_err(),
        "navigation lock must block filament"
    );

    let insert_disc = |last: DVec3, pre_warp: Option<DVec3>| {
        with_mut_sp6_state(&ctx.db, |st, _snap| {
            st.disconnected_ships.insert(
                char_id,
                DisconnectedShipEntry {
                    char_id,
                    ship_id,
                    ship_type_id: 606,
                    solar_system_id: sys_id,
                    disconnected_at_unix: 100,
                    remove_at_unix: 160,
                    emergency_warp_dest: None,
                    last_position: [last.x, last.y, last.z],
                    removed_from_space: false,
                    pre_warp_position: pre_warp.map(|p| [p.x, p.y, p.z]),
                },
            );
        });
    };

    // Case A: live ball with pre_warp_eligible == false does not initiate return warp even when > 10 km.
    insert_disc(base_pos, Some(base_pos + DVec3::new(25_000.0, 0.0, 0.0)));
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect live nav-locked");
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(matches!(bp.balls[&ship_id].mode, MovementMode::Stop));
    }

    // Case B: respawned ball (not in ballpark) with pre_warp_eligible == false stays Stop.
    ctx.systems.get_or_spawn(sys_id).try_remove_ball(ship_id);
    insert_disc(base_pos, Some(base_pos + DVec3::new(25_000.0, 0.0, 0.0)));
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect respawn nav-locked");
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(matches!(bp.balls[&ship_id].mode, MovementMode::Stop));
    }

    // Clear nav lock so pre_warp_eligible == true.
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .clear();

    // Case C: clean respawn with distance <= 10_000.0 stays Stop.
    ctx.systems.get_or_spawn(sys_id).try_remove_ball(ship_id);
    insert_disc(base_pos, Some(base_pos + DVec3::new(9_999.0, 0.0, 0.0)));
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect within 10km");
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(matches!(bp.balls[&ship_id].mode, MovementMode::Stop));
    }

    // Case D: clean respawn with distance > 10_000.0 initiates return Warp.
    ctx.systems.get_or_spawn(sys_id).try_remove_ball(ship_id);
    insert_disc(base_pos, Some(base_pos + DVec3::new(10_001.0, 0.0, 0.0)));
    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect > 10km");
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(matches!(bp.balls[&ship_id].mode, MovementMode::Warp { .. }));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filament_gates_safe_logoff_conditions_abandon_all_and_reconnect_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    undock_ship(&ctx, char_id, ship_id, sys_id, DVec3::new(100_000.0, 0.0, 0.0));
    let mut sess = SessionState::new(77);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. require_ship_ready_for_filament_jump: scrambled, mid-warp, warp-exit invuln
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_warp_scramble_strength(ship_id, 2).await;
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    let _ = sys_handle.set_warp_scramble_strength(ship_id, 0).await;
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        let _ = bp.set_mode(
            ship_id,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        );
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    // In warp triggers SAFE_LOGOFF_IN_WARP
    let conds_warp = safe_logoff_blocking_space_conditions(&ctx, char_id, ship_id, sys_id);
    assert!(conds_warp.contains(&"SAFE_LOGOFF_IN_WARP"));

    if let Ok(mut bp) = sys_handle.ballpark.write() {
        let _ = bp.set_mode(ship_id, MovementMode::Stop);
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_exit_invulnerable_remaining_ticks = 10;
        }
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "ShipInvulnerable"
    );

    // 2. persisted active module blocks safe logoff
    let mod_id = 988_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.module_active_states.insert(mod_id);
    });
    let conds_mod = safe_logoff_blocking_space_conditions(&ctx, char_id, ship_id, sys_id);
    assert!(conds_mod.contains(&"SAFE_LOGOFF_ACTIVE_MODULES"));

    // 3. handle_cmd_abandon_drone with scalar PyRep::Integer + handle_cmd_reconnect_to_drones
    //    out-of-range (> 250km) and insufficient bandwidth
    let far_drone = 988_010_i64;
    let heavy_drone = 988_011_i64;
    for (did, pos, bw) in [
        (far_drone, DVec3::new(400_000.0, 0.0, 0.0), 5.0),
        (heavy_drone, DVec3::new(100_100.0, 0.0, 0.0), 999.0),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: did,
                type_id: 2454,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 1000.0,
                radius: 5.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                did,
                DroneSpaceState {
                    ball_id: did,
                    drone_item_id: did,
                    type_id: 2454,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: bw,
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
                },
            );
        });
    }
    handle_cmd_abandon_drone(&ctx, &mut sess, &[PyRep::Integer(far_drone)], &HashMap::new())
        .await
        .unwrap();
    handle_cmd_abandon_drone(&ctx, &mut sess, &[PyRep::Integer(heavy_drone)], &HashMap::new())
        .await
        .unwrap();
    let rec_res = handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new())
        .await
        .unwrap();
    let PyRep::Dict(err_map) = rec_res else { panic!() };
    assert!(err_map.iter().any(|(k, _)| k == &PyRep::String("OutOfRange".into())));
    assert!(err_map.iter().any(|(k, _)| k == &PyRep::String("InsufficientBandwidth".into())));

    // 4. reconnect_character_in_space when ship is already in ballpark far from pre_warp_position -> initiates return warp
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.disconnected_ships.insert(
            char_id,
            DisconnectedShipEntry {
                char_id,
                ship_id,
                ship_type_id: 606,
                solar_system_id: sys_id,
                disconnected_at_unix: ctx.now_unix(),
                remove_at_unix: ctx.now_unix() + 60,
                emergency_warp_dest: Some([1_000_000_000.0, 0.0, 0.0]),
                last_position: [1_000_000_000.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: Some([100_000.0, 0.0, 0.0]),
            },
        );
    });
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(1_000_000_000.0, 0.0, 0.0))
        .await;
    assert!(reconnect_character_in_space(&ctx, char_id).await.is_some());
    assert!(sys_handle.ballpark.read().unwrap().balls.get(&ship_id).is_some_and(|b| matches!(b.mode, MovementMode::Warp { .. })));

    // 5. NoActiveShip / NoSolarSystem guards on reconnect_to_drones
    sess.ship_id = None;
    assert_eq!(
        handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    assert_eq!(
        handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
}

#[tokio::test]
async fn mutants_cov_hp_batch_item_sync_unified_dmg_dominance_and_scramble_reconnect() {
    use eve_db::{DisconnectedShipEntry, InventoryFlag, PersistedCombatHp};
    use eve_destiny::{Ball, CombatState, MovementMode, WarpBubble};
    use glam::DVec3;

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_base = 91_770_000i64;
    let s_hp = 91_770_101i64;
    let s_live = 91_770_102i64;
    let s_pers = 91_770_103i64;
    let s_zero = 91_770_104i64;
    let char_docked = char_base + 1;
    let ship_docked = 91_770_105i64;
    let char_scr_live = char_base + 2;
    let ship_scr_live = 91_770_106i64;
    let char_scr_respawn = char_base + 3;
    let ship_scr_respawn = 91_770_107i64;
    let char_no_pre = char_base + 4;
    let ship_no_pre = 91_770_108i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for &(cid, sid, docked) in &[
            (char_docked, ship_docked, true),
            (char_scr_live, ship_scr_live, false),
            (char_scr_respawn, ship_scr_respawn, false),
            (char_no_pre, ship_no_pre, false),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = format!("Sp6MutChar-{cid}");
            ch.solar_system_id = sys_id;
            ch.station_id = if docked { Some(60_003_760) } else { None };
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                make_item(
                    sid,
                    587,
                    cid,
                    if docked { 60_003_760 } else { sys_id as i64 },
                    if docked { InventoryFlag::Hangar } else { InventoryFlag::None },
                    1,
                ),
            );
        }
        snap.items.insert(s_hp, make_item(s_hp, 587, char_docked, 60_003_760, InventoryFlag::Hangar, 1));
        snap.items.insert(s_live, make_item(s_live, 587, char_docked, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(s_pers, make_item(s_pers, 587, char_docked, 60_003_760, InventoryFlag::Hangar, 1));
        snap.items.insert(s_zero, make_item(s_zero, 587, char_docked, sys_id as i64, InventoryFlag::None, 1));
    });

    // 1. persist_ship_combat_hp_batch_inner with hp_changed == false (item_changed decides alone) & 1e-6 boundaries
    let mk_cs = |sh: f64, msh: f64, ar: f64, mar: f64, hu: f64, mhu: f64| CombatState {
        shield_hp: sh,
        max_shield_hp: msh,
        armor_hp: ar,
        max_armor_hp: mar,
        structure_hp: hu,
        max_structure_hp: mhu,
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..CombatState::default()
    };

    // 1a. hp_changed=false, cur_a == want_a (800.0), cur_h = None vs want_h = Some(500.0) -> bumps version & writes DAMAGE
    let cs_1a = mk_cs(400.0, 1_000.0, 1_200.0, 2_000.0, 2_500.0, 3_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            s_hp,
            PersistedCombatHp {
                shield_hp: 400.0,
                armor_hp: 1_200.0,
                hull_hp: 2_500.0,
            },
        );
        let it = snap.items.get_mut(&s_hp).unwrap();
        it.dynamic_attributes.clear();
        it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 800.0);
        it.version = 10;
    });
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(s_hp, &cs_1a)], true);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&s_hp];
        assert_eq!(it.version, 11);
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::ARMOR_DAMAGE).copied(), Some(800.0));
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied(), Some(500.0));
    });

    // 1b. hp_changed=false, cur_a = Some(0.0) vs want_a = Some(2.0), cur_h = Some(0.0) vs want_h = Some(2.0) -> 0/2 <= 1e-6 vs |0-2| > 1e-6
    let cs_1b = mk_cs(400.0, 1_000.0, 1_998.0, 2_000.0, 2_998.0, 3_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            s_hp,
            PersistedCombatHp {
                shield_hp: 400.0,
                armor_hp: 1_998.0,
                hull_hp: 2_998.0,
            },
        );
        let it = snap.items.get_mut(&s_hp).unwrap();
        it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 0.0);
        it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.0);
        it.version = 20;
    });
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(s_hp, &cs_1b)], true);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&s_hp];
        assert_eq!(it.version, 21);
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::ARMOR_DAMAGE).copied(), Some(2.0));
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied(), Some(2.0));
    });

    // 1c. Exact <= 1e-6 threshold: armor_dmg = 2_000.0 - (2_000.0 - 9.999998e-7) <= 1e-6 and hull_dmg = 3_000.0 - (3_000.0 - 9.999998e-7) <= 1e-6 yield want = None, removing both dyn attrs and bumping version
    let cs_1c = mk_cs(400.0, 1_000.0, 2_000.0 - 9.999998e-7, 2_000.0, 3_000.0 - 9.999998e-7, 3_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            s_hp,
            PersistedCombatHp {
                shield_hp: 400.0,
                armor_hp: 2_000.0 - 9.999998e-7,
                hull_hp: 3_000.0 - 9.999998e-7,
            },
        );
        let it = snap.items.get_mut(&s_hp).unwrap();
        it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 9.999998e-7);
        it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 9.999998e-7);
        it.version = 30;
    });
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(s_hp, &cs_1c)], true);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&s_hp];
        assert_eq!(it.version, 31);
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!it.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
    });

    // 1d. Sub-epsilon dyn delta (10.0 vs 10.0 + 5e-7) even with hp_changed=true (shield changed) must NOT overwrite 10.0 or bump version
    let cs_1d = mk_cs(410.0, 1_000.0, 1_990.0 - 5e-7, 2_000.0, 2_990.0 - 5e-7, 3_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&s_hp).unwrap();
        it.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 10.0);
        it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        it.version = 40;
    });
    super::persist_ship_combat_hp_batch_inner(&ctx.db, [(s_hp, &cs_1d)], true);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&s_hp];
        assert_eq!(it.version, 40);
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::ARMOR_DAMAGE).copied(), Some(10.0));
        assert_eq!(it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied(), Some(10.0));
    });

    // 2. read_unified_ship_damage_state: live-dominant, persisted-dominant, and zero-max clamp
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: s_live,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            mk_cs(450.0, 600.0, 600.0, 800.0, 700.0, 1_000.0),
        )
        .await
        .unwrap();
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: s_zero,
                type_id: 587,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            mk_cs(-10.0, 0.0, -10.0, 0.0, -10.0, 0.0),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            s_pers,
            PersistedCombatHp {
                shield_hp: 450.0,
                armor_hp: 600.0,
                hull_hp: 700.0,
            },
        );
        snap.sp6_lifecycle.ship_combat_hp.insert(
            s_zero,
            PersistedCombatHp {
                shield_hp: -10.0,
                armor_hp: -10.0,
                hull_hp: -10.0,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let u_live = read_unified_ship_damage_state(&ctx, snap, s_live, 587, Some((1_000.0, 800.0, 600.0)));
        assert!((u_live.shield_damage - 150.0).abs() < 1e-6);
        assert!((u_live.shield_hp - 450.0).abs() < 1e-6);
        assert!((u_live.armor_damage - 200.0).abs() < 1e-6);
        assert!((u_live.hull_damage - 300.0).abs() < 1e-6);

        let u_pers = read_unified_ship_damage_state(&ctx, snap, s_pers, 587, Some((1_000.0, 800.0, 600.0)));
        assert!((u_pers.shield_damage - 150.0).abs() < 1e-6);
        assert!((u_pers.shield_hp - 450.0).abs() < 1e-6);
        assert!((u_pers.armor_damage - 200.0).abs() < 1e-6);
        assert!((u_pers.hull_damage - 300.0).abs() < 1e-6);

        let u_zero = read_unified_ship_damage_state(&ctx, snap, s_zero, 587, Some((0.0, 0.0, 0.0)));
        assert_eq!(u_zero.shield_damage, 0.0);
        assert_eq!(u_zero.armor_damage, 0.0);
        assert_eq!(u_zero.hull_damage, 0.0);
        assert_eq!(u_zero.shield_hp, 0.0);
    });

    // 3. Docked character returns None on logoff & reconnect_character_in_space_sync
    assert!(handle_character_logoff_or_disconnect(&ctx, char_docked).await.is_none());
    assert!(reconnect_character_in_space_sync(&ctx, char_docked).is_none());

    // 4. reconnect_character_in_space_sync: pre_warp_eligible=true + warp-scrambled stays Stop on BOTH live ball (L1307) & respawned ball (L1358)
    let now = ctx.now_unix();
    let far_pos = [500_000.0, 0.0, 0.0];
    let no_pre_pos = [900_000.0, 0.0, 0.0];
    let pre_pos = [0.0, 0.0, 0.0];
    let dest_pos = [1_000_000.0, 0.0, 0.0];

    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_scr_live,
                type_id: 587,
                position: DVec3::from_array(far_pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                warp_scramble_strength: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sys_h
        .spawn_ball(Ball {
            id: ship_no_pre,
            type_id: 587,
            position: DVec3::from_array(no_pre_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.warp_bubble_mgr
            .add_bubble(WarpBubble::new(919_901, DVec3::from_array(far_pos), 20_000.0));
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.disconnected_ships.insert(
            char_scr_live,
            DisconnectedShipEntry {
                char_id: char_scr_live,
                ship_id: ship_scr_live,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 30,
                remove_at_unix: now + 600,
                emergency_warp_dest: Some(dest_pos),
                last_position: far_pos,
                removed_from_space: false,
                pre_warp_position: Some(pre_pos),
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            char_scr_respawn,
            DisconnectedShipEntry {
                char_id: char_scr_respawn,
                ship_id: ship_scr_respawn,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 120,
                remove_at_unix: now - 10,
                emergency_warp_dest: Some(dest_pos),
                last_position: far_pos,
                removed_from_space: true,
                pre_warp_position: Some(pre_pos),
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            char_no_pre,
            DisconnectedShipEntry {
                char_id: char_no_pre,
                ship_id: ship_no_pre,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 30,
                remove_at_unix: now + 600,
                emergency_warp_dest: None,
                last_position: no_pre_pos,
                removed_from_space: false,
                pre_warp_position: Some(no_pre_pos),
            },
        );
    });

    assert!(reconnect_character_in_space_sync(&ctx, char_scr_live).is_some());
    assert!(reconnect_character_in_space_sync(&ctx, char_scr_respawn).is_some());
    assert!(reconnect_character_in_space_sync(&ctx, char_no_pre).is_some());
    {
        let bp = sys_h.ballpark.read().unwrap();
        assert_eq!(bp.balls.get(&ship_scr_live).unwrap().mode, MovementMode::Stop);
        assert_eq!(bp.balls.get(&ship_scr_respawn).unwrap().mode, MovementMode::Stop);
        assert_eq!(bp.balls.get(&ship_no_pre).unwrap().mode, MovementMode::Stop);
    }
}
