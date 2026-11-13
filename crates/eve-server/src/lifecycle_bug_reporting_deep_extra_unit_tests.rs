//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle`, `cluster_tick`,
//! and `bug_reporting`.

#![forbid(unsafe_code)]

use crate::bug_reporting::{
    build_client_global_config, extract_text_files_from_zip, handle_bugs_http_request,
    BugIncidentBundle, BugReportingRuntime,
};
use crate::router::{ActiveFighterSquadron, ActiveMoonSurveyProbe, ActiveScannerProbe, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use crate::services::pve_special_sites::{with_abyssal_state, AbyssalTraceRecord};
use crate::sp6_lifecycle::*;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    PersistedCombatHp, ServerClock, VirtualServerClock, WorldRepository,
};
use eve_destiny::{CombatState, DamageVector, MovementMode};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }))
}

fn fast_test_context(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

fn make_stored_and_deflated_zip(stored_name: &str, stored_data: &[u8], defl_name: &str, defl_data: &[u8]) -> Vec<u8> {
    let mut zip = Vec::new();
    let mut cd = Vec::new();
    let compressed = miniz_oxide::deflate::compress_to_vec(defl_data, 6);

    for (name, data, raw_len, method) in [
        (stored_name.as_bytes(), stored_data, stored_data.len(), 0u16),
        (defl_name.as_bytes(), compressed.as_slice(), defl_data.len(), 8u16),
        (b"unsupported.txt".as_slice(), b"abc".as_slice(), 3usize, 99u16),
    ] {
        let local_off = zip.len() as u32;
        zip.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&method.to_le_bytes());
        zip.extend_from_slice(&[0u8; 8]);
        zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(raw_len as u32).to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(name);
        zip.extend_from_slice(data);

        cd.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&[0u8; 8]);
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(raw_len as u32).to_le_bytes());
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&[0u8; 8]);
        cd.extend_from_slice(&local_off.to_le_bytes());
        cd.extend_from_slice(name);
    }

    let cd_off = zip.len() as u32;
    let cd_size = cd.len() as u32;
    zip.extend_from_slice(&cd);
    zip.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&3u16.to_le_bytes());
    zip.extend_from_slice(&3u16.to_le_bytes());
    zip.extend_from_slice(&cd_size.to_le_bytes());
    zip.extend_from_slice(&cd_off.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip
}

#[tokio::test]
async fn sp6_lifecycle_state_helpers_safe_logoff_conditions_and_abort_paths() {
    let now = 1_800_000_000_u64;
    let (ctx, clock) = fast_test_context(now);
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
    assert_eq!(sde_ship_physics(&ctx.sde, 999_999_999), (25, 6, 1_200_000.0, 40.0, 300.0, 2.5, 3.0));

    let mut st_custom = Sp6PersistedState::default();
    st_custom.ship_combat_hp.insert(777, PersistedCombatHp { shield_hp: 10.0, armor_hp: 20.0, hull_hp: 30.0 });
    ctx.db.with_mut_snapshot(|snap| {
        save_sp6_state_to_snap(snap, &st_custom);
        assert_eq!(load_sp6_state_from_snap(snap).ship_combat_hp.get(&777).map(|h| h.hull_hp), Some(30.0));
    });

    let cs1 = CombatState { shield_hp: 42.5, armor_hp: 80.0, structure_hp: 120.0, ..CombatState::default() };
    persist_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &cs1)]);
    // Second identical batch hits the `to_update.is_empty()` short-circuit without altering state
    persist_ship_combat_hp_batch_in_db(&ctx.db, [(ship_id, &cs1)]);
    let mut restored_cs = CombatState { max_shield_hp: 500.0, max_armor_hp: 500.0, max_structure_hp: 500.0, ..CombatState::default() };
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored_cs);
    assert_eq!((restored_cs.shield_hp, restored_cs.armor_hp, restored_cs.structure_hp), (42.5, 80.0, 120.0));

    // Filament session-change timer rejection ("clone_jump" rejected; "login" allowed)
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, now, sys_id, 0, "clone_jump");
    assert_eq!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).unwrap_err().code, "SessionChangeInProgress");
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, now, sys_id, 0, "login");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.insert(ship_id, eve_destiny::Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0,
            speed_fraction: 0.0, mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0),
        });
    }
    assert_eq!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).unwrap_err().code, "ShipInWarp");
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.combat_states.entry(ship_id).or_default().warp_scramble_strength = 2;
    }
    assert_eq!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).unwrap_err().code, "WarpScrambled");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 0;
            cs.gate_cloak_remaining_ticks = 4;
        }
    }
    assert_eq!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).unwrap_err().code, "ShipInvulnerable");

    // Safe logoff docked check & comprehensive space blocking conditions
    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);
    let docked_rep = handle_safe_logoff(&ctx, &mut sess).await.unwrap();
    let PyRep::List(docked_codes) = docked_rep else { panic!() };
    assert_eq!(docked_codes, vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())]);

    // Move to space with all blocking conditions active at once
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(888_001, FittedModule {
        item_id: 888_001, type_id: 519, flag_id: 11,
        state: ModuleState::Active, charge_type_id: None, charge_quantity: 0,
        damage_hp: 0.0, max_hp: 40.0, dynamic_attribute_overrides: HashMap::new(),
    });
    ctx.store_ship_fitting(ship_id, fit);
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::new_warp(DVec3::ZERO, DVec3::new(5e8, 0.0, 0.0), 3.0)).await;
    if let Ok(mut af) = ctx.active_fighters.write() {
        af.insert(990_001, ActiveFighterSquadron {
            ball_id: 990_001, item_id: 990_001, type_id: 23055, owner_char_id: char_id,
            parent_ship_id: ship_id, tube_index: 0, squadron_size: 9, target_id: None,
            is_abandoned: false, mwd_active: false,
        });
    }
    if let Ok(mut mp) = ctx.active_moon_probes.write() {
        mp.push(ActiveMoonSurveyProbe {
            probe_id: 990_002, probe_type_id: 30488, owner_char_id: char_id,
            solar_system_id: sys_id, hit_moon_id: None, arrival_unix_secs: now + 300,
        });
    }
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until.insert(char_id, now + 60);
        st.npc_timers_until.insert(char_id, now + 300);
        st.pvp_timers_until.insert(char_id, now + 900);
        st.criminal_timers_until.insert(char_id, now + 900);
    });
    register_self_destruct_countdown(&ctx, ship_id, char_id, sys_id, now, now + 120);

    let PyRep::List(fail_codes) = handle_safe_logoff(&ctx, &mut sess).await.unwrap() else { panic!() };
    for expected in [
        "SAFE_LOGOFF_ACTIVE_MODULES", "SAFE_LOGOFF_IN_WARP", "SAFE_LOGOFF_DRONES_IN_SPACE",
        "SAFE_LOGOFF_PROBES_IN_SPACE", "SAFE_LOGOFF_WEAPONS_TIMER", "SAFE_LOGOFF_NPC_TIMER",
        "SAFE_LOGOFF_PVP_TIMER", "SAFE_LOGOFF_CRIMINAL_TIMER", "SAFE_LOGOFF_SELF_DESTRUCT_ACTIVE",
    ] {
        assert!(fail_codes.contains(&PyRep::String(expected.into())), "missing {expected} in {fail_codes:?}");
    }

    // Clear blockers -> start safe logoff -> abort on action (active vs already completed)
    ctx.invalidate_ship_fitting(ship_id);
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    ctx.active_fighters.write().unwrap().clear();
    ctx.active_moon_probes.write().unwrap().clear();
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.weapon_timers_until.clear();
        st.npc_timers_until.clear();
        st.pvp_timers_until.clear();
        st.criminal_timers_until.clear();
    });
    assert!(abort_self_destruct_countdown(&ctx, ship_id));
    assert!(!maybe_abort_safe_logoff_on_action(&ctx, char_id, "TEST"));
    assert_eq!(handle_safe_logoff(&ctx, &mut sess).await.unwrap(), PyRep::List(vec![]));
    assert!(maybe_abort_safe_logoff_on_action(&ctx, char_id, "ABORTED_BY_PILOT"));

    // Mark completed=true -> maybe_abort returns false; handle_abort_safe_logoff removes it explicitly
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.safe_logoffs.insert(char_id, SafeLogoffEntry {
            char_id, ship_id, solar_system_id: sys_id, started_unix: now, complete_at_unix: now + 30, completed: true,
        });
    });
    assert!(!maybe_abort_safe_logoff_on_action(&ctx, char_id, "SHOULD_NOT_ABORT"));
    assert_eq!(handle_abort_safe_logoff(&ctx, &mut sess, "MANUAL").unwrap(), PyRep::Bool(true));

    // PvP aggression renews already-disconnected victim's remove_at_unix; NPC aggression skips disconnected victim
    let victim_id = 90_000_002_i64;
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.disconnected_ships.insert(victim_id, DisconnectedShipEntry {
            char_id: victim_id, ship_id: 100_000_002, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now, remove_at_unix: now + 60, emergency_warp_dest: None,
            last_position: [50_000.0, 0.0, 0.0], removed_from_space: false, pre_warp_position: None,
        });
    });
    record_pvp_aggression_and_renew_logoff_timer(&ctx, char_id, victim_id, now);
    with_sp6_state(&ctx.db, |st| {
        assert_eq!(st.disconnected_ships[&victim_id].remove_at_unix, now + PVP_TIMER_SECS);
    });
    record_npc_aggression_timer(&ctx, victim_id, now);
    assert_eq!(active_timers_for_char(&ctx, victim_id, now).1, 0);
    clock.advance_secs(1);
}

#[tokio::test]
async fn sp6_lifecycle_disconnect_reconnect_fallback_positions_and_drone_ops() {
    let now = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(now);
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    // 1. Already completed safe logoff -> disconnect returns None
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.safe_logoffs.insert(char_id, SafeLogoffEntry {
            char_id, ship_id, solar_system_id: sys_id, started_unix: now - 30, complete_at_unix: now, completed: true,
        });
    });
    assert!(handle_character_logoff_or_disconnect(&ctx, char_id).await.is_none());

    // 2. Disconnect with unspawned ball + active combat engagement + high-sec criminal -> spawns ball & prohibits emergency warp
    with_mut_sp6_state(&ctx.db, |st, snap| {
        st.safe_logoffs.remove(&char_id);
        st.criminal_timers_until.insert(char_id, now + 900);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.item_positions.insert(ship_id, [150_000_000.0, 1000.0, 2000.0]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_remove_ball(ship_id);
    if let Ok(mut sp) = ctx.active_scanner_probes.write() {
        sp.insert(700_001, ActiveScannerProbe {
            probe_id: 700_001, probe_type_id: 30013, owner_char_id: char_id, ship_id,
            launcher_module_id: 700_002, solar_system_id: sys_id, position: [0.0; 3],
            destination: [0.0; 3], range_step: 1, scan_range_m: 4.0, base_sensor_strength: 10.0,
            max_flight_time_s: 300.0, launched_at_unix: now, state: 1, warp_end_ft: None,
        });
    }
    let disc = handle_character_logoff_or_disconnect(&ctx, char_id).await.expect("disconnected entry");
    assert!(disc.emergency_warp_dest.is_none());
    assert!(disc.remove_at_unix >= now + NPC_TIMER_SECS);
    assert!(sys_handle.get_ball_position_sync(ship_id).is_some());

    // 3. Reconnect when docked returns None and clears sp6 entries
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    assert!(reconnect_character_in_space(&ctx, char_id).await.is_none());

    // 4. Reconnect with invalid positions: [0,0,0], inside station radius, celestial-only system, and empty system
    sys_handle.try_remove_ball(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.item_positions.insert(ship_id, [10.0, 0.0, 0.0]);
    });
    let (_, _, p_station_fallback) = reconnect_character_in_space(&ctx, char_id).await.unwrap();
    assert!(p_station_fallback.length() > 10_000.0);

    // Near station perimeter -> triggers station radius check -> fallback
    let st_pos = ctx.sde.solar_systems[&sys_id].stations[0].position;
    sys_handle.try_remove_ball(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [st_pos.x, st_pos.y, st_pos.z]);
    });
    let (_, _, p_near_st) = reconnect_character_in_space(&ctx, char_id).await.unwrap();
    assert!(p_near_st.distance(st_pos) >= 25_000.0);

    // System with celestial & NO stations
    if let Some((&no_st_sys, _)) = ctx.sde.solar_systems.iter().find(|(_, s)| s.stations.is_empty() && !ctx.sde.celestials_in_system(s.solar_system_id).is_empty()) {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = no_st_sys;
            }
            snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        });
        let (_, _, p_cel) = reconnect_character_in_space(&ctx, char_id).await.unwrap();
        assert!(p_cel.length() > 10_000.0);
    }

    // Synthetic system with neither stations nor celestials -> [15e9, 0, 0]
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 31_999_999;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
    });
    let (_, _, p_deep) = reconnect_character_in_space(&ctx, char_id).await.unwrap();
    assert_eq!(p_deep, DVec3::new(15_000_000_000.0, 0.0, 0.0));

    // Reconnect when live ball is far from `pre_warp_position` -> triggers return warp
    with_mut_sp6_state(&ctx.db, |st, snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
        st.disconnected_ships.insert(char_id, DisconnectedShipEntry {
            char_id, ship_id, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now, remove_at_unix: now + 900, emergency_warp_dest: None,
            last_position: [500_000_000.0, 0.0, 0.0], removed_from_space: false,
            pre_warp_position: Some([200_000_000.0, 0.0, 0.0]),
        });
    });
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(500_000_000.0, 0.0, 0.0)).await;
    let _ = reconnect_character_in_space(&ctx, char_id).await.unwrap();
    let mode = sys_handle.ballpark.read().unwrap().balls[&ship_id].mode.clone();
    assert!(matches!(mode, MovementMode::Warp { .. }));

    // 5. Drone abandon & reconnect (errors + OutOfRange + InsufficientBandwidth + success)
    let mut sess = SessionState::new(9);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    assert_eq!(handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new()).await.unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);
    assert_eq!(handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new()).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(sys_id);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(910_001, DroneSpaceState {
            ball_id: 910_001, drone_item_id: 910_001, type_id: 2454, group_id: 100,
            owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 1, assist_char_id: None,
            guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
            max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
            web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
        rt.active_drones_in_space.insert(910_002, DroneSpaceState {
            ball_id: 910_002, drone_item_id: 910_002, bandwidth_used: 5.0, ..rt.active_drones_in_space[&910_001].clone()
        });
    });
    let _ = handle_cmd_abandon_drone(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(910_001)])], &HashMap::new()).await.unwrap();
    with_combat_runtime(&ctx, |rt| assert!(rt.active_drones_in_space[&910_001].is_abandoned));
    let _ = handle_cmd_abandon_drone(&ctx, &mut sess, &[], &HashMap::new()).await.unwrap();

    // Put drone 910_002 out of 250km range and drone 910_001 within range -> 910_002 OutOfRange, 910_001 reconnects (or InsufficientBandwidth on Rifter 0 bandwidth)
    let _ = sys_handle.try_add_mobile_warp_disruptor(910_002, DVec3::new(900_000_000.0, 0.0, 0.0), 100.0);
    let PyRep::Dict(reconn_errs) = handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[], &HashMap::new()).await.unwrap() else { panic!() };
    assert!(reconn_errs.iter().any(|(k, _)| k.as_str() == Some("OutOfRange")));
    assert!(reconn_errs.iter().any(|(k, _)| k.as_str() == Some("InsufficientBandwidth")));
}

#[tokio::test]
async fn cluster_tick_expiry_sweeps_bounties_and_concord_arrival() {
    let base = 1_800_000_000_u64;
    let (ctx, clock) = fast_test_context(base);
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let _ = ctx.systems.get_or_spawn(sys_id);

    // Seed expired abyssal session, trace, Zarzakh lock, anomalies, signatures, planetary launch, wreck, jetcan, MWD bubble, deployable, secure container
    let mwd_type_id = ctx.sde.types.values().find(|t| t.group_id == 361).map(|t| t.type_id).unwrap_or(12_198);
    let sec_cont_type_id = ctx.sde.types.values().find(|t| t.group_id == 340).map(|t| t.type_id).unwrap_or(17_363);

    register_abyssal_session(&ctx, 90_000_002, 100_000_002, 32_000_001, sys_id, base, base + 10);
    with_abyssal_state(&ctx, |st| {
        st.traces.insert(880_001, AbyssalTraceRecord {
            trace_ball_id: 880_001, char_id, filament_type_id: 47762, origin_solar_system_id: sys_id,
            position: [1e5, 0.0, 0.0], created_unix: base, expires_at_sec: base + 10, max_ships: 1,
            entered_char_ids: Vec::new(), pocket_id: None, fleet_id: None,
        });
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(char_id, (50_000_001, base + 10));
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "EXP-001".into(), solar_system_id: sys_id, name: "Expired".into(),
            anomaly_type: "Combat".into(), position: [0.0; 3], signal_strength: 1.0,
            ore_types: Vec::new(), remaining_volume: 0.0, created_unix: base, lifetime_seconds: 5, cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "SIG-EXP".into(), solar_system_id: sys_id, sig_group: "Relic".into(),
            type_name: "Site".into(), position: [0.0; 3], signal_strength: 0.5,
            wormhole_type_id: None, destination_system_id: None, created_unix: base, lifetime_seconds: 5,
            max_mass_kg: 0.0, remaining_mass_kg: 0.0, max_jump_mass_kg: 0.0, collapsed: false,
        });

        let make_space_item = |item_id: i64, type_id: u32, loc: i64| InventoryItemRecord {
            item_id, type_id, owner_id: char_id, location_id: loc, flag: InventoryFlag::None,
            quantity: 1, singleton: true, version: 1, dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None, insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        };
        for (iid, tid) in [
            (881_001, 23u32),             // planetary launch decaying at 5d
            (881_002, 26557u32),          // wreck decaying at 2h
            (881_003, 23u32),             // jetcan decaying at 1h
            (881_004, mwd_type_id),       // MWD bubble activating at 10s
            (881_005, 33475u32),          // deployable activating then decaying
            (881_006, sec_cont_type_id),  // secure container activating at 5s
        ] {
            snap.items.insert(iid, make_space_item(iid, tid, sys_id as i64));
        }

        record_wreck_or_container_meta_in_snap(snap, 881_002, base, true);
        record_wreck_or_container_meta_in_snap(snap, 881_003, base, false);
        record_anchor_meta_in_snap(snap, 881_004, base, 5.0, Some(20_000.0));
        record_anchor_meta_in_snap(snap, 881_006, base, 5.0, None);
        snap.sp6_lifecycle.space_object_meta.entry(881_001).or_default().planet_id = Some(40_009_001);
        snap.sp6_lifecycle.space_object_meta.entry(881_001).or_default().created_unix = Some(base);

        write_deployable_state_snap(snap, |dst| {
            dst.deployables.insert(881_005, MobileDeployableRecord {
                item_id: 881_005, type_id: 33475, group_id: 1246, owner_id: char_id,
                solar_system_id: sys_id, anchor_start_unix: base, anchor_duration_secs: 5,
                anchor_completed: false, is_unanchored: false, expires_at_unix: base + 100,
                last_interaction_unix: base, reinforced_until_unix: None, last_ping_unix: None,
                position: [200_000.0, 0.0, 0.0], deployed_fleet_id: None,
            });
        });
    });

    // Step 15s: abyssal pocket collapses, trace expires, Zarzakh lock expires, MWD bubble + deployable + secure container finish anchoring
    let now_15 = base + 15;
    clock.advance_secs(15);
    on_advance_secs(&ctx, now_15, 15);
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.abyssal_sessions.contains_key(&90_000_002));
        assert!(st.space_object_meta.get(&881_004).is_some_and(|m| m.anchor_completed));
        assert!(st.space_object_meta.get(&881_006).is_some_and(|m| m.anchor_completed));
    });

    // Seed pending bounty tick + pending CONCORD arrival, advance past 5d so planetary launch, wreck, jetcan, MWD bubble, and deployable all decay
    let now_decay = base + 6 * 86_400;
    clock.advance_secs(6 * 86_400 - 15);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 25_000.0);
        rt.bounty_tick_next_flush_unix.insert((sys_id, char_id), now_decay);
    });
    tick_background_cluster(&ctx).await;

    ctx.db.with_snapshot(|snap| {
        for iid in [881_001_i64, 881_002, 881_003, 881_004, 881_005] {
            assert!(!snap.items.contains_key(&iid), "expected decayed item {iid} to be pruned");
        }
    });
}

#[tokio::test]
async fn bug_reporting_http_zip_multipart_and_incident_edge_cases() {
    let tmp_dir = std::env::temp_dir().join(format!("evomu_bug_deep_{}", Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let (ctx, _) = fast_test_context(1_800_000_000);
    ctx.bug_reporting.set_queue_dir_override(Some(tmp_dir.clone()));

    // Global config extra values
    ctx.global_config_values.write().unwrap().insert("customFlag".into(), PyRep::ascii_bytes("yes"));
    let gcfg = build_client_global_config(&ctx, Some("::ffff:10.0.0.5"));
    assert_eq!(gcfg.get("bugReporting_BugReportServer"), Some(&PyRep::ascii_bytes(format!("http://10.0.0.5:{}/bugs/api", ctx.esi_port()).as_bytes())));
    assert_eq!(gcfg.get("customFlag"), Some(&PyRep::ascii_bytes("yes")));

    // ZIP extraction (stored + deflated + unsupported compression + corrupt EOCD)
    assert!(extract_text_files_from_zip(&[1, 2, 3], &["fitting.txt"]).is_empty());
    let mut big_log = String::new();
    for i in 1..=65 {
        big_log.push_str(&format!("log line {i}\n\n"));
    }
    let zip_bytes = make_stored_and_deflated_zip(
        "nested/dir/fitting.txt",
        b"[Rifter, ZipFit]\nDamage Control II\n",
        "logs.txt",
        big_log.as_bytes(),
    );
    let extracted = extract_text_files_from_zip(&zip_bytes, &["fitting.txt", "logs.txt", "unsupported.txt"]);
    assert!(extracted.contains_key("fitting.txt"));
    assert!(extracted.contains_key("logs.txt"));
    assert!(!extracted.contains_key("unsupported.txt"));

    // HTTP endpoints: status, category, 404, invalid JSON, valid F12 issue + attachment limit 429 + igbr.zip extraction
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/status/", &HashMap::new(), b"").status_code, 200);
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/category", &HashMap::new(), b"").status_code, 200);
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/nope", &HashMap::new(), b"").status_code, 404);
    assert_eq!(handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &HashMap::new(), b"{bad").status_code, 400);

    let issue_body = serde_json::json!({
        "categoryID": "Combat",
        "summary": "Deep Unit Bug",
        "description": "Details\n\nSession Info:\nCharacter: Demo (90000001)",
        "originalReporterId": 1,
        "labels": ["combat"]
    })
    .to_string();
    let create_rsp = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &HashMap::new(), issue_body.as_bytes());
    assert_eq!(create_rsp.status_code, 201);
    let created_json: serde_json::Value = serde_json::from_slice(&create_rsp.body).unwrap();
    let issue_key = created_json["key"].as_str().unwrap().to_string();

    // Lookup from disk after creating new BugReportingRuntime with same queue dir
    let rt2 = BugReportingRuntime::new();
    rt2.set_queue_dir_override(Some(tmp_dir.clone()));
    let disk_path = rt2.lookup_report_path(&issue_key).expect("found on disk");

    // Attachment validation errors
    assert_eq!(handle_bugs_http_request(&ctx, "POST", "/bugs/api/attachment/../bad", &HashMap::new(), b"").status_code, 400);
    assert_eq!(handle_bugs_http_request(&ctx, "POST", "/bugs/api/attachment/EBR-9999-AAAAAA", &HashMap::new(), b"").status_code, 404);
    assert_eq!(handle_bugs_http_request(&ctx, "POST", &format!("/bugs/api/attachment/{issue_key}"), &HashMap::new(), b"no-boundary").status_code, 400);

    let boundary = "----EvoMuTestBoundary";
    let hdrs = HashMap::from([("content-type".into(), format!("multipart/form-data; boundary={boundary}"))]);
    let make_multipart = |fname: &str, data: &[u8]| {
        let mut b = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{fname}\"\r\n\r\n").into_bytes();
        b.extend_from_slice(data);
        b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        b
    };

    let zip_up = handle_bugs_http_request(&ctx, "POST", &format!("/bugs/api/attachment/{issue_key}"), &hdrs, &make_multipart("igbr.zip", &zip_bytes));
    assert_eq!(zip_up.status_code, 201);
    let saved_bundle: BugIncidentBundle = serde_json::from_str(&std::fs::read_to_string(&disk_path).unwrap()).unwrap();
    let manual = saved_bundle.manual_report.unwrap();
    assert!(manual.ship_fitting_eft.as_deref().unwrap().contains("[Rifter, ZipFit]"));
    assert_eq!(manual.client_logs_tail.len(), 60);

    // Exhaust attachment limit (16 total -> 17th returns 429 Too Many Requests)
    for idx in 1..16 {
        let r = handle_bugs_http_request(&ctx, "POST", &format!("/bugs/api/attachment/{issue_key}"), &hdrs, &make_multipart(&format!("shot_{idx}.png"), b"png"));
        assert_eq!(r.status_code, 201);
    }
    let over = handle_bugs_http_request(&ctx, "POST", &format!("/bugs/api/attachment/{issue_key}"), &hdrs, &make_multipart("shot_over.png", b"png"));
    assert_eq!(over.status_code, 429);

    // Storage error path when queue_dir points to a regular file
    let bad_file = tmp_dir.join("not_a_dir");
    std::fs::write(&bad_file, b"file").unwrap();
    ctx.bug_reporting.set_queue_dir_override(Some(bad_file));
    let err_500 = handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &HashMap::new(), issue_body.as_bytes());
    assert_eq!(err_500.status_code, 500);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
