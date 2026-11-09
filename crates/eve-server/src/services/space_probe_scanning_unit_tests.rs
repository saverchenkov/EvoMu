//! Unit and mutation-killing tests for `services::space_probe_scanning`
//! (probe range/deviation math, difficulty levels, overdue probe expiry, launcher activation, and `scanMgr` RPCs).

use super::space_probe_scanning::*;
use crate::router::{ActiveScannerProbe, BoundObjectEntry, ServerContext};
use eve_db::types::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_scan_ctx_and_session() -> (Arc<ServerContext>, SessionState) {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let mut sess = SessionState::new(92_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = 100_000_001;
        }
        if let Some(ship) = snap.items.get_mut(&100_000_001) {
            ship.location_id = 30_000_142;
        }
    });
    (ctx, sess)
}

#[test]
fn probe_math_ranges_deviation_and_difficulty_thresholds() {
    let (ctx, _) = make_scan_ctx_and_session();
    let sde = &ctx.sde;

    // Core Scanner Probe I (30013) vs Combat Scanner Probe I (30028)
    assert!(!is_combat_scanner_probe(sde, 30_013));
    assert!(is_combat_scanner_probe(sde, 30_028));

    let base_range = probe_base_scan_range_au(sde, 30_013);
    assert!(base_range > 0.0);
    let ranges = probe_allowed_ranges_au(sde, 30_013);
    assert_eq!(ranges[0], base_range);
    assert!((ranges[1] - base_range * 2.0).abs() < 1e-9);
    assert!((ranges[7] - base_range * 128.0).abs() < 1e-9);

    let base_dev = probe_base_max_deviation_au(sde, 30_013);
    assert!(base_dev > 0.0);
    assert!(probe_base_sensor_strength(sde, 30_013) > 0.0);
    assert!(probe_max_flight_time_s(sde, 30_013) >= 1_000.0);

    // Deviation scales linearly with range_au / base_range_au and decreases 5%/level for astrometrics & pinpointing
    let dev_lvl0 = compute_probe_max_deviation_au(0.25, 1.0, 0.25, 0, 0);
    assert!((dev_lvl0 - 1.0).abs() < 1e-9);
    let dev_lvl5 = compute_probe_max_deviation_au(0.25, 1.0, 0.25, 5, 5);
    assert!((dev_lvl5 - 1.0 * 0.75 * 0.75).abs() < 1e-9);

    let sig = CosmicSignatureRecord {
        signature_id: "ABC-001".into(),
        solar_system_id: 30_000_142,
        sig_group: "Relic Site".into(),
        type_name: "Unknown Test Signature".into(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 1_000,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    let (lvl, min_str) = signature_difficulty_level_and_min_strength(sde, 30_000_142, &sig);
    assert_eq!(lvl, 1);
    assert_eq!(min_str, 42.0);
}

#[tokio::test]
async fn overdue_probe_expiry_returns_to_cargo_or_marks_lost_when_full() {
    let (ctx, _) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let now_unix = ctx.now_unix();

    // Insert one overdue probe with room in cargo
    ctx.active_scanner_probes.write().unwrap().insert(
        700_001,
        ActiveScannerProbe {
            probe_id: 700_001,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: 30_000_142,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 60.0,
            launched_at_unix: now_unix.saturating_sub(120),
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );

    let (returned, lost) = expire_overdue_probes_for_char(&ctx, char_id, now_unix).await;
    assert_eq!(returned, vec![700_001]);
    assert!(lost.is_empty());
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap().get(&700_001).unwrap().state,
        -1
    );

    // Now set ship capacity to 0.0 so next overdue probe is lost
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(38, 0.0);
        }
    });
    ctx.active_scanner_probes.write().unwrap().insert(
        700_002,
        ActiveScannerProbe {
            probe_id: 700_002,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: 30_000_142,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 60.0,
            launched_at_unix: now_unix.saturating_sub(120),
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );
    expire_all_overdue_probes(&ctx, now_unix).await;
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap().get(&700_002).unwrap().state,
        -2
    );
}

#[tokio::test]
async fn activate_scan_probe_launcher_and_scan_mgr_rpcs_end_to_end() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    // 1. No probes loaded -> NoProbesLoaded
    let launcher_id = 710_001;
    let charge_id = 710_002;
    let err_no_probes = activate_scan_probe_launcher(
        &ctx,
        &sess,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_938,
        27,
        "useMissiles",
        None,
        &kw,
    )
    .await
    .expect_err("no probes loaded");
    assert_eq!(err_no_probes.code, "NoProbesLoaded");

    // 2. Combat probes (30028) in Core Probe Launcher I (17938) -> InvalidProbeForLauncher
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 30_028,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let err_combat_in_core = activate_scan_probe_launcher(
        &ctx,
        &sess,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_938,
        InventoryFlag::HiSlot0 as u16,
        "useMissiles",
        None,
        &kw,
    )
    .await
    .expect_err("combat probe in core launcher");
    assert_eq!(err_combat_in_core.code, "InvalidProbeForLauncher");

    // 3. Switch to Core Scanner Probe I (30013) and launch 8 probes
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&charge_id) {
            c.type_id = 30_013;
        }
    });
    let launch_rep = activate_scan_probe_launcher(
        &ctx,
        &sess,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_938,
        InventoryFlag::HiSlot0 as u16,
        "useMissiles",
        None,
        &kw,
    )
    .await
    .expect("launch 8 core probes");
    assert_eq!(
        launch_rep.dict_get("launchedCount"),
        Some(&PyRep::Integer(8))
    );

    // 4. GetSystemScanMgr, GetAnomalies, GetFullState, SetActivityState
    let scan_mgr = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        None,
        "scanMgr",
        "GetSystemScanMgr",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetSystemScanMgr");
    assert_eq!(
        scan_mgr.dict_get("solarSystemID"),
        Some(&PyRep::Integer(sys_id as i64))
    );

    // Remote system denied without GM role
    let remote_err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        None,
        "scanMgr",
        "GetSystemScanMgr",
        &[PyRep::Integer(30_000_144)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(remote_err.code, "AccessDenied");

    let bound = BoundObjectEntry {
        bound_id: 10,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: sess.character_id.unwrap_or(140_000_001) as i64,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: "N=1:10".into(),
        ref_id: 10,
    };

    let anoms = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "GetAnomalies",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetAnomalies");
    assert!(matches!(anoms, PyRep::List(_)));

    let full_state = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "GetFullState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetFullState");
    let PyRep::Tuple(state_parts) = full_state else {
        panic!("expected 4-tuple from GetFullState");
    };
    assert_eq!(state_parts.len(), 4);
    let PyRep::Dict(anom_dict) = &state_parts[0] else {
        panic!("expected anomaly dict");
    };
    assert!(
        !anom_dict.is_empty(),
        "seeded system should include anomalies in GetFullState"
    );
    for (_, val) in anom_dict {
        assert!(matches!(val, PyRep::KeyVal(_)), "anomaly entry must be KeyVal");
        for req_key in [
            "instanceID",
            "position",
            "targetID",
            "difficulty",
            "dungeonID",
            "archetypeID",
            "dungeonNameID",
            "factionID",
            "scanStrengthAttribute",
            "allowedTypes",
        ] {
            assert!(
                val.dict_get(req_key).is_some(),
                "anomaly KeyVal missing required V24 field `{req_key}`"
            );
        }
        assert!(
            val.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
            "anomaly KeyVal dungeonID must be a positive integer for V24 GetDungeon (#747)"
        );
        assert!(
            val.dict_get("archetypeID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
            "anomaly KeyVal archetypeID must be a positive integer (#747)"
        );
        assert!(
            val.dict_get("factionID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
            "anomaly KeyVal factionID must be a positive integer (#747)"
        );
        assert!(
            val.dict_get("difficulty").and_then(|v| v.as_i64()).is_some_and(|d| d >= 1),
            "anomaly KeyVal difficulty must be >= 1 (#747)"
        );
        assert!(
            val.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
            "anomaly KeyVal scanStrengthAttribute must be a positive integer (#747)"
        );
    }
    let PyRep::Dict(sig_dict) = &state_parts[1] else {
        panic!("expected signature dict");
    };
    assert!(
        !sig_dict.is_empty(),
        "seeded system should include signatures in GetFullState"
    );
    for (_, val) in sig_dict {
        assert!(matches!(val, PyRep::KeyVal(_)), "signature entry must be KeyVal");
        for req_key in [
            "position",
            "targetID",
            "difficulty",
            "dungeonID",
            "archetypeID",
            "deviation",
        ] {
            assert!(
                val.dict_get(req_key).is_some(),
                "signature KeyVal missing required V24 field `{req_key}`"
            );
        }
    }

    let act_state = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("SetActivityState");
    assert_eq!(act_state, PyRep::None);

    // 5. RequestScans with coordinate list, then tick_probe_warp_end
    let coord_list = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Real(5_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::Integer(2),
    ])]);
    let scan_res = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[coord_list],
        &kw,
    )
    .await
    .unwrap()
    .expect("RequestScans");
    assert!(matches!(scan_res, PyRep::List(_)));

    ctx.advance_secs(30);
    tick_probe_warp_end(&ctx);

    // 6. QA methods require GM role, then succeed with GM role
    let qa_denied = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "QAScanSites",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(qa_denied.code, "GMRoleRequired");

    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "QAOverrideProbeExpiry",
        &[PyRep::Real(300.0)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "QAScanSites",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // 7. ReconnectToLostProbes, DestroyProbe (one), and RecoverProbes (remaining)
    let first_pid = *ctx.active_scanner_probes.read().unwrap().keys().next().unwrap();
    ctx.active_scanner_probes
        .write()
        .unwrap()
        .get_mut(&first_pid)
        .unwrap()
        .state = PROBE_STATE_INACTIVE;

    let recon = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "ReconnectToLostProbes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("ReconnectToLostProbes");
    assert_eq!(
        recon.dict_get("reconnectedCount"),
        Some(&PyRep::Integer(1))
    );

    let destroy_one = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "DestroyProbe",
        &[PyRep::Integer(first_pid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("DestroyProbe");
    assert_eq!(
        destroy_one.dict_get("destroyedCount"),
        Some(&PyRep::Integer(1))
    );

    let recover = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RecoverProbes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("RecoverProbes");
    assert_eq!(
        recover.dict_get("recoveredCount"),
        Some(&PyRep::Integer(7))
    );
    let _ = DVec3::ZERO;
}

#[tokio::test]
async fn set_activity_state_updates_probe_state_and_rejects_invalid_state() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let bound = BoundObjectEntry {
        bound_id: 11,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: "N=1:11".into(),
        ref_id: 11,
    };
    ctx.active_scanner_probes.write().unwrap().insert(
        700_901,
        ActiveScannerProbe {
            probe_id: 700_901,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id: 100_000_001,
            launcher_module_id: 0,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1_700_000_000,
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );

    let ok = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[
            PyRep::List(vec![PyRep::Integer(700_901)]),
            PyRep::Integer(i64::from(PROBE_STATE_INACTIVE)),
        ],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(ok, Ok(PyRep::None));
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap().get(&700_901).unwrap().state,
        PROBE_STATE_INACTIVE
    );

    let err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(99)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidProbeState");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toctou_concurrent_activate_scan_probe_launcher_caps_at_8_probes() {
    let (ctx, sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let launcher_id = 710_801;
    let charge_id = 710_802;

    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 30_013, // Core Scanner Probe I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 16,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut kw = HashMap::new();
    kw.insert("probeCount".into(), PyRep::Integer(8));
    let (r1, r2) = tokio::join!(
        activate_scan_probe_launcher(
            &ctx, &sess, &sys_handle, char_id, ship_id, sys_id,
            launcher_id, 17_938, InventoryFlag::HiSlot0 as u16, "useMissiles", None, &kw,
        ),
        activate_scan_probe_launcher(
            &ctx, &sess, &sys_handle, char_id, ship_id, sys_id,
            launcher_id, 17_938, InventoryFlag::HiSlot0 as u16, "useMissiles", None, &kw,
        ),
    );
    assert!(r1.is_ok() ^ r2.is_ok(), "expected exactly 1 launch of 8 probes, got r1={r1:?} r2={r2:?}");
    let active_count = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id)
        .count();
    assert_eq!(active_count, 8);
    let rem_qty = ctx.db.with_snapshot(|s| s.items.get(&charge_id).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(8), "probes in launcher must only be consumed once");
}

#[tokio::test]
async fn request_scans_evaluates_probe_strength_and_deviation_via_dogma_and_expedition_burst() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let launcher_id = 700_950;
    let rig_id = 700_951;
    let kw = HashMap::new();
    let bound = BoundObjectEntry {
        bound_id: 12,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: "N=1:12".into(),
        ref_id: 12,
    };

    let now_unix = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 587; // Rifter (small rig size = 1, calibration = 400)
        }
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 17_938, // Core Probe Launcher I (has 6118/6119 Expedition Burst effects)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            rig_id,
            InventoryItemRecord {
                item_id: rig_id,
                type_id: 31_213, // Small Gravity Capacitor Upgrade I (+10% probe sensor strength)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.module_online_states.insert(rig_id, true);
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "DOG-901".into(),
                solar_system_id: sys_id,
                sig_group: "Data Site".into(),
                type_name: "Central Guristas Sparking Transmitter".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 0.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            }],
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    for k in 0..4 {
        let pid = 700_960 + k;
        ctx.active_scanner_probes.write().unwrap().insert(
            pid,
            ActiveScannerProbe {
                probe_id: pid,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: launcher_id,
                solar_system_id: sys_id,
                position: [0.0; 3],
                destination: [0.0; 3],
                range_step: 4,
                scan_range_m: 2.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3600.0,
                launched_at_unix: ctx.now_unix_secs(),
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
    }

    let extract_sig = |rep: &PyRep| -> (f64, f64) {
        let PyRep::List(rows) = rep else { panic!("expected List") };
        let row = rows
            .iter()
            .find(|r| r.dict_get("id").and_then(|v| v.as_str()) == Some("DOG-901"))
            .expect("DOG-901 row");
        (
            row.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap(),
            row.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap(),
        )
    };

    // Baseline scan (rig +10% already active via ShipFittingEngine; Astrometrics/Rangefinding/Pinpointing = 0)
    let base_rep = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (s_base, dev_base) = extract_sig(&base_rep);

    // Train real SDE Rangefinding (25739) IV (+20% probe strength) & real SDE Pinpointing (25810) IV (-20% max deviation)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            25_739,
            eve_db::TrainedSkillRecord {
                type_id: 25_739,
                level: 4,
                skill_points: 362_039.0,
            },
        );
        sk.insert(
            25_810,
            eve_db::TrainedSkillRecord {
                type_id: 25_810,
                level: 4,
                skill_points: 226_275.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let skilled_rep = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (s_skilled, dev_skilled) = extract_sig(&skilled_rep);
    assert!(s_skilled > s_base * 1.15, "Rangefinding 25739 must boost probe strength via eve-dogma ({s_base} -> {s_skilled})");
    assert!((dev_skilled - dev_base * 0.80).abs() < 1e-6, "Pinpointing 25810 IV must reduce maxDeviationAU by 20% ({dev_base} -> {dev_skilled})");

    // Apply Expedition Command Burst (`dbuff 2464` +25% on Core Probe Launcher I `6118` -> probe `1371`)
    let now_u = ctx.now_unix_secs();
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs
            .entry((ctx.instance_id, ship_id))
            .or_default()
            .push(crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id: 2464,
                value: 25.0,
                expires_at_unix: now_u + 60,
                source_module_id: 888_001,
            });
    }
    let burst_rep = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (s_burst, _) = extract_sig(&burst_rep);
    assert!(s_burst > s_skilled * 1.20, "Expedition Burst dbuff 2464 must boost probe strength ({s_skilled} -> {s_burst})");

    // Negative check: after burst expires, strength returns to s_skilled
    ctx.advance_secs(65);
    let after_exp_rep = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (s_after, _) = extract_sig(&after_exp_rep);
    assert!((s_after - s_skilled).abs() < 1e-6, "Expired Expedition Burst must no longer apply ({s_after} vs {s_skilled})");
}
