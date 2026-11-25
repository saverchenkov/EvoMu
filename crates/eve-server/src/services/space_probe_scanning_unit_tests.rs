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

    // 4. GetSystemScanMgr, GetAnomalies, GetFullState, SetActivityState (seed real PvE anomaly; no synthetic ANO-)
    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
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
    assert!(matches!(&recover, PyRep::List(ids) if ids.len() == 7));
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
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            // Isolate baseline scan from racial starter probe skills so the Rangefinding/Pinpointing delta is exact.
            for tid in [3412u32, 25_739, 25_810, 25_811] {
                sk.remove(&tid);
            }
        }
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
    ctx.scanners_superweapons.with_ship_dbuffs_mut(|dbuffs| {
        dbuffs
            .entry(ship_id)
            .or_default()
            .push(crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id: 2464,
                value: 25.0,
                expires_at_unix: now_u + 60,
                source_module_id: 888_001,
            });
    });
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_activity_state_blocks_sentinel_resurrection_and_foreign_probes() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let bound = BoundObjectEntry {
        bound_id: 950,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 950,
    };

    let active_pid = 890_700_001_i64;
    let cargo_sentinel_pid = 890_700_002_i64;
    let lost_sentinel_pid = 890_700_003_i64;
    let foreign_pid = 890_700_004_i64;
    let other_sys_pid = 890_700_005_i64;
    let nonexistent_pid = 890_700_999_i64;

    let now_unix = ctx.now_unix();
    let make_probe = |probe_id: i64, owner_char_id: i64, probe_sys: u32, state: i32| ActiveScannerProbe {
        probe_id,
        probe_type_id: 30_013,
        owner_char_id,
        ship_id: if owner_char_id == char_id { ship_id } else { 100_000_099 },
        launcher_module_id: 0,
        solar_system_id: probe_sys,
        position: [1_000.0, 0.0, 0.0],
        destination: [1_000.0, 0.0, 0.0],
        range_step: 4,
        scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
        base_sensor_strength: 40.0,
        max_flight_time_s: 60.0,
        launched_at_unix: if state < 0 { 1 } else { now_unix },
        state,
        warp_end_ft: None,
    };

    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(active_pid, make_probe(active_pid, char_id, sys_id, PROBE_STATE_IDLE));
        map.insert(cargo_sentinel_pid, make_probe(cargo_sentinel_pid, char_id, sys_id, -1));
        map.insert(lost_sentinel_pid, make_probe(lost_sentinel_pid, char_id, sys_id, -2));
        map.insert(foreign_pid, make_probe(foreign_pid, 2_199_002, sys_id, PROBE_STATE_IDLE));
        map.insert(other_sys_pid, make_probe(other_sys_pid, char_id, 30_000_144, PROBE_STATE_IDLE));
    }

    let cargo_probe_qty = || -> i64 {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.location_id == ship_id
                        && it.flag == InventoryFlag::Cargo
                        && it.type_id == 30_013
                })
                .map(|it| i64::from(it.quantity))
                .sum()
        })
    };
    let baseline_qty = cargo_probe_qty();

    // 1. Bulk SetActivityState updates active probe only and leaves -1/-2 sentinels untouched.
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(PROBE_STATE_SCANNING as i64)],
        &kw,
    )
    .await
    .unwrap()
    .expect("bulk SetActivityState");
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert_eq!(map.get(&active_pid).map(|p| p.state), Some(PROBE_STATE_SCANNING));
        assert_eq!(map.get(&cargo_sentinel_pid).map(|p| p.state), Some(-1));
        assert_eq!(map.get(&lost_sentinel_pid).map(|p| p.state), Some(-2));
        assert_eq!(map.get(&foreign_pid).map(|p| p.state), Some(PROBE_STATE_IDLE));
        assert_eq!(map.get(&other_sys_pid).map(|p| p.state), Some(PROBE_STATE_IDLE));
    }

    // 2. Explicit 2-arg, list, and dict forms targeting sentinel, foreign, wrong-system, or unknown probes fail atomically with ProbeNotFound.
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();
    for bad_args in [
        vec![PyRep::Integer(cargo_sentinel_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)],
        vec![PyRep::Integer(lost_sentinel_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)],
        vec![PyRep::Integer(foreign_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)],
        vec![PyRep::Integer(other_sys_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)],
        vec![PyRep::Integer(nonexistent_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)],
        vec![
            PyRep::List(vec![PyRep::Integer(active_pid), PyRep::Integer(cargo_sentinel_pid)]),
            PyRep::Integer(PROBE_STATE_IDLE as i64),
        ],
        vec![
            PyRep::List(vec![PyRep::Integer(active_pid), PyRep::Integer(foreign_pid)]),
            PyRep::Integer(PROBE_STATE_IDLE as i64),
        ],
        vec![PyRep::Dict(vec![(
            PyRep::Integer(cargo_sentinel_pid),
            PyRep::Integer(PROBE_STATE_IDLE as i64),
        )])],
        vec![PyRep::Dict(vec![(
            PyRep::Integer(foreign_pid),
            PyRep::Integer(PROBE_STATE_IDLE as i64),
        )])],
        vec![PyRep::Dict(vec![
            (PyRep::Integer(active_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)),
            (PyRep::Integer(other_sys_pid), PyRep::Integer(PROBE_STATE_IDLE as i64)),
        ])],
    ] {
        let err = handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            Some(&bound),
            "scanMgr",
            "SetActivityState",
            &bad_args,
            &kw,
        )
        .await
        .unwrap()
        .expect_err("invalid SetActivityState probe target must fail");
        assert_eq!(err.code, "ProbeNotFound", "args={bad_args:?}");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap().get(&active_pid).map(|p| p.state),
        Some(PROBE_STATE_SCANNING),
        "failed multi-probe SetActivityState must not partially mutate valid probe"
    );

    // 3. Subsequent RecoverProbes / GetSystemScanMgr expiry returns active_pid (+1) once, never minting from sentinels;
    //    re-running SetActivityState(1) + RecoverProbes() afterward cannot mint another probe.
    handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "RecoverProbes", &[], &kw)
        .await
        .unwrap()
        .expect("bulk RecoverProbes");
    assert_eq!(cargo_probe_qty(), baseline_qty + 1);

    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(PROBE_STATE_IDLE as i64)],
        &kw,
    )
    .await
    .unwrap()
    .expect("bulk SetActivityState after recovery");
    handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "GetSystemScanMgr", &[], &kw)
        .await
        .unwrap()
        .expect("GetSystemScanMgr after recovery");
    assert_eq!(
        cargo_probe_qty(),
        baseline_qty + 1,
        "repeated SetActivityState(1) + expiry/recovery must never resurrect sentinels or mint extra probes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_scanning_read_purity_and_docked_guards() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let kw = HashMap::new();
    let err_not_in_space = "NotInSpace";
    let bound = BoundObjectEntry {
        bound_id: 77,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: sess.character_id.unwrap_or(90_000_001),
        service: "scanMgr".into(),
        object_id: 30_000_142,
        oid_str: "scanMgr:30000142".into(),
        ref_id: 77,
    };

    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    assert!(handle_scan_and_wormhole_extras(&ctx, &mut docked, Some(&bound), "scanMgr", "GetAnomalies", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut docked, Some(&bound), "scanMgr", "GetFullState", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut docked, None, "scanMgr", "GetSystemScanMgr", &[], &kw).await.unwrap().is_ok());
    for meth in ["RequestScans", "RecoverProbes", "ReconnectToLostProbes", "DestroyProbe"] {
        let err = handle_scan_and_wormhole_extras(&ctx, &mut docked, Some(&bound), "scanMgr", meth, &[], &kw)
            .await
            .expect("handled")
            .expect_err("docked pilot cannot actuate probes");
        assert_eq!(err.code, err_not_in_space, "{meth} while docked");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Undocked with 0 probes: NoActiveProbes / no-ops / unchanged sync do not seed or mutate DB
    let req_err = handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "RequestScans", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(req_err.code, "NoActiveProbes");
    for meth in ["RecoverProbes", "ReconnectToLostProbes", "DestroyProbe"] {
        assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", meth, &[], &kw).await.unwrap().is_ok());
    }
    sync_scanner_probes_to_db(&ctx);
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Undocked GetAnomalies and GetFullState remain strictly read-pure on an unseeded system;
    // GetSystemScanMgr seeds once when undocked with an active ship, then repeats are strictly read-pure.
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "GetAnomalies", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "GetFullState", &[], &kw).await.unwrap().is_ok());
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "GetSystemScanMgr", &[], &kw).await.unwrap().is_ok());
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();
    assert!(e1 > e0 && c1 > c0, "GetSystemScanMgr undocked with active ship seeds unseeded system once");
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "GetAnomalies", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&bound), "scanMgr", "GetFullState", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "GetSystemScanMgr", &[], &kw).await.unwrap().is_ok());
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);

    // Docked GetSystemScanMgr still materializes existing wormhole Ballpark balls without mutating EveDb.
    let (ctx_wh, mut docked_wh) = make_scan_ctx_and_session();
    docked_wh.station_id = Some(60_003_760);
    ctx_wh.db.with_mut_snapshot(|snap| {
        let now_u = ctx_wh.now_unix();
        snap.cosmic_signatures.insert(30_000_142, vec![CosmicSignatureRecord {
            signature_id: "WH-TST-1".into(),
            solar_system_id: 30_000_142,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole (K162)".into(),
            position: [1_000_000.0, 2_000_000.0, 3_000_000.0],
            signal_strength: 0.25,
            wormhole_type_id: Some(26_272),
            destination_system_id: Some(31_000_005),
            created_unix: now_u,
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 375_000_000.0,
            collapsed: false,
        }]);
    });
    let ew0 = ctx_wh.db.state_epoch();
    let cw0 = ctx_wh.db.tx_commit_count();
    assert!(handle_scan_and_wormhole_extras(&ctx_wh, &mut docked_wh, None, "scanMgr", "GetSystemScanMgr", &[], &kw).await.unwrap().is_ok());
    assert_eq!(ctx_wh.db.state_epoch(), ew0);
    assert_eq!(ctx_wh.db.tx_commit_count(), cw0);
    let wh_item_id = crate::services::space_destiny::wormhole_item_id_for_signature(30_000_142, "WH-TST-1");
    let sys_wh = ctx_wh.systems.get_or_spawn(30_000_142);
    assert!(sys_wh.get_ball_snapshot(wh_item_id).await.unwrap().is_some());
}

#[tokio::test]
async fn unbound_request_scans_rejects_docked_and_shipless_before_fallthrough() {
    let err_not_in_space = "NotInSpace";
    let err_no_active_ship = "NoActiveShip";
    let svc_scan_mgr = "scanMgr";
    let m_request_scans = "RequestScans";

    let (ctx, mut sess) = make_scan_ctx_and_session();
    let kw = HashMap::new();
    let scan_args = [PyRep::Real(4.0 * 149_597_870_700.0), PyRep::Real(40.0)];

    // Docked session must be rejected with NotInSpace before any fallthrough/DB mutation.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
    });
    sess.station_id = Some(60_003_760);
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();
    let direct_err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        None,
        svc_scan_mgr,
        m_request_scans,
        &scan_args,
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(direct_err.code, err_not_in_space);
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // First dispatch_rpc runs fleet_social_notify::tick_fleet_lifecycle once; capture post-warmup epochs and check repeat docked + shipless guard calls stay pure.
    let err = ctx
        .dispatch_rpc(&mut sess, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_not_in_space);
    let e_warm = ctx.db.state_epoch();
    let c_warm = ctx.db.tx_commit_count();
    let err = ctx
        .dispatch_rpc(&mut sess, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_not_in_space);
    assert_eq!(ctx.db.state_epoch(), e_warm);
    assert_eq!(ctx.db.tx_commit_count(), c_warm);

    // Undocked without an active ship and with no active probes must fail with NoActiveShip before fallthrough.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.active_ship_id = 0;
        }
    });
    sess.station_id = None;
    sess.ship_id = None;
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();
    let err = ctx
        .dispatch_rpc(&mut sess, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_no_active_ship);
    assert!(ctx.active_scanner_probes.read().unwrap().is_empty());
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_solar_system_rejected_purely_across_probe_rpcs_and_multi_arg_unbound_falls_through() {
    let err_no_solar_system = "NoSolarSystem";
    let svc_scan_mgr = "scanMgr";
    let (ctx, _) = make_scan_ctx_and_session();
    let kw = HashMap::new();

    // Character absent from DB with solar_system_id == None -> resolve_caller_solar_system == 0.
    let mut sess = SessionState::new(92_099);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_999);
    sess.station_id = None;
    sess.solar_system_id = None;
    sess.ship_id = Some(500_000_999);

    let bound_entry = BoundObjectEntry {
        bound_id: 1,
        session_id: 92_099,
        user_id: 1,
        character_id: 90_000_999,
        service: svc_scan_mgr.into(),
        object_id: 30_000_142,
        oid_str: "N=1:scanMgr".into(),
        ref_id: 1,
    };

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    for (bound, meth, args) in [
        (None, "RequestScans", vec![]),
        (
            Some(&bound_entry),
            "RequestScans",
            vec![PyRep::Integer(42), PyRep::Real(149_597_870_700.0)],
        ),
        (None, "RecoverProbes", vec![PyRep::List(vec![PyRep::Integer(1)])]),
        (None, "ReconnectToLostProbes", vec![]),
        (None, "DestroyProbe", vec![PyRep::Integer(1)]),
    ] {
        let err = handle_scan_and_wormhole_extras(&ctx, &mut sess, bound, svc_scan_mgr, meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("NoSolarSystem must reject missing active system");
        assert_eq!(err.code, err_no_solar_system, "{meth} missing solar system");
    }

    assert!(ctx.systems.get_handle(0).is_none(), "must never spawn system 0");
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Unbound multi-arg RequestScans([Real, Real]) returns None so fleet_social_scanning handles fleet/scan fallback.
    let (_, mut undocked) = make_scan_ctx_and_session();
    let res = handle_scan_and_wormhole_extras(
        &ctx,
        &mut undocked,
        None,
        svc_scan_mgr,
        "RequestScans",
        &[PyRep::Real(1.0), PyRep::Real(2.0)],
        &kw,
    )
    .await;
    assert!(res.is_none(), "unbound [Real, Real] RequestScans must fall through");
}

#[tokio::test]
async fn docked_expired_signatures_and_pochven_and_scan_inhibitor_persist() {
    let (ctx, mut docked) = make_scan_ctx_and_session();
    let kw = HashMap::new();
    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE contains Pochven solar system");
    assert_eq!(
        crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, pochven_sys).0,
        25
    );

    docked.solar_system_id = Some(pochven_sys);
    docked.station_id = Some(60_003_760);
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            pochven_sys,
            vec![
                CosmicSignatureRecord {
                    signature_id: "WH-EXP-1".into(),
                    solar_system_id: pochven_sys,
                    sig_group: "Wormhole".into(),
                    type_name: "Unstable Wormhole (K162)".into(),
                    position: [1_000_000.0, 2_000_000.0, 3_000_000.0],
                    signal_strength: 1.0,
                    wormhole_type_id: Some(26_272),
                    destination_system_id: Some(30_000_142),
                    created_unix: now_u.saturating_sub(90_000),
                    lifetime_seconds: 1_000,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 2_000_000_000.0,
                    max_jump_mass_kg: 375_000_000.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "DAT-EXP-1".into(),
                    solar_system_id: pochven_sys,
                    sig_group: "Data Site".into(),
                    type_name: "Expired Test Data Site".into(),
                    position: [4_000_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_u.saturating_sub(90_000),
                    lifetime_seconds: 1_000,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "DAT-LIVE-1".into(),
                    solar_system_id: pochven_sys,
                    sig_group: "Data Site".into(),
                    type_name: "Live Test Data Site".into(),
                    position: [5_000_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_u,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );
        snap.cosmic_anomalies.insert(
            pochven_sys,
            vec![
                eve_db::types::CosmicAnomalyRecord {
                    anomaly_id: "ANOM-EXP-1".into(),
                    solar_system_id: pochven_sys,
                    name: "Expired Combat Anomaly".into(),
                    anomaly_type: "CombatAnomaly".into(),
                    position: [6_000_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_u.saturating_sub(90_000),
                    lifetime_seconds: 1_000,
                    cleared: false,
                },
                eve_db::types::CosmicAnomalyRecord {
                    anomaly_id: "ANOM-LIVE-1".into(),
                    solar_system_id: pochven_sys,
                    name: "Live Combat Anomaly".into(),
                    anomaly_type: "CombatAnomaly".into(),
                    position: [7_000_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now_u,
                    lifetime_seconds: 86_400,
                    cleared: false,
                },
            ],
        );
    });

    let bound = BoundObjectEntry {
        bound_id: 21,
        session_id: docked.session_id,
        user_id: docked.user_id.unwrap_or(1),
        character_id: docked.character_id.unwrap_or(90_000_001),
        service: "scanMgr".into(),
        object_id: pochven_sys as i64,
        oid_str: "N=1:21".into(),
        ref_id: 21,
    };
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    let scan_mgr = handle_scan_and_wormhole_extras(
        &ctx,
        &mut docked,
        None,
        "scanMgr",
        "GetSystemScanMgr",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetSystemScanMgr");
    let Some(PyRep::List(mgr_sigs)) = scan_mgr.dict_get("signatures") else {
        panic!("expected signatures list from GetSystemScanMgr");
    };
    let mgr_sig_ids: Vec<&str> = mgr_sigs
        .iter()
        .filter_map(|r| r.dict_get("id").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(mgr_sig_ids, vec!["DAT-LIVE-1"]);
    let Some(PyRep::List(mgr_anoms)) = scan_mgr.dict_get("anomalies") else {
        panic!("expected anomalies list from GetSystemScanMgr");
    };
    assert_eq!(mgr_anoms.len(), 1);
    assert!(
        !mgr_anoms.iter().any(|r| {
            r.dict_get("dungeonName")
                .and_then(|v| v.as_str())
                .is_some_and(|n| n.contains("Dense Veldspar") || n.contains("Expired"))
        }),
        "Pochven scanMgr must filter expired anomalies and never synthesize Dense Veldspar"
    );

    let full = handle_scan_and_wormhole_extras(
        &ctx,
        &mut docked,
        Some(&bound),
        "scanMgr",
        "GetFullState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetFullState");
    let PyRep::Tuple(parts) = full else { panic!("expected 4-tuple") };
    let PyRep::Dict(anom_dict) = &parts[0] else { panic!("expected anom dict") };
    let PyRep::Dict(sig_dict) = &parts[1] else { panic!("expected sig dict") };
    let anom_keys: Vec<&str> = anom_dict.iter().filter_map(|(k, _)| k.as_str()).collect();
    let sig_keys: Vec<&str> = sig_dict.iter().filter_map(|(k, _)| k.as_str()).collect();
    assert_eq!(anom_keys, vec!["ANOM-LIVE-1"]);
    assert_eq!(sig_keys, vec!["DAT-LIVE-1"]);

    let anoms_rep = handle_scan_and_wormhole_extras(
        &ctx,
        &mut docked,
        Some(&bound),
        "scanMgr",
        "GetAnomalies",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetAnomalies");
    let PyRep::List(anoms_list) = anoms_rep else { panic!("expected anomalies list") };
    assert_eq!(anoms_list.len(), 1);
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    let wh_exp_ball = crate::services::space_destiny::wormhole_item_id_for_signature(pochven_sys, "WH-EXP-1");
    assert!(
        ctx.systems
            .get_or_spawn(pochven_sys)
            .get_ball_snapshot(wh_exp_ball)
            .await
            .unwrap()
            .is_none(),
        "expired wormhole signature must not materialize Ballpark ball"
    );

    // 2. P2 #2: Mobile Scan Inhibitor early return in RequestScans still persists updated probes to sp6_lifecycle.
    let (ctx2, mut sess2) = make_scan_ctx_and_session();
    let sys2 = 30_000_142;
    let char2 = 90_000_001;
    let ship2 = 100_000_001;
    let inhib_id = 880_001;
    let probe_id = 700_980;
    let now2 = ctx2.now_unix();
    ctx2.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys2,
            vec![CosmicSignatureRecord {
                signature_id: "INH-101".into(),
                solar_system_id: sys2,
                sig_group: "Data Site".into(),
                type_name: "Inhibited Site".into(),
                position: [1_000_000.0, 0.0, 0.0],
                signal_strength: 0.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now2,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            }],
        );
        snap.items.insert(
            inhib_id,
            eve_db::InventoryItemRecord {
                item_id: inhib_id,
                type_id: 33_589,
                owner_id: char2,
                location_id: sys2 as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.deployable_state.deployables.insert(
            inhib_id,
            eve_db::MobileDeployableRecord {
                item_id: inhib_id,
                type_id: 33_589,
                group_id: crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
                owner_id: char2,
                solar_system_id: sys2,
                anchor_start_unix: now2.saturating_sub(120),
                anchor_duration_secs: 60,
                anchor_completed: true,
                is_unanchored: false,
                expires_at_unix: now2 + 3_600,
                last_interaction_unix: now2,
                reinforced_until_unix: None,
                last_ping_unix: None,
                position: [1_000.0, 0.0, 0.0],
                deployed_fleet_id: None,
            },
        );
    });
    let sys2_handle = ctx2.systems.get_or_spawn(sys2);
    let _ = sys2_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship2,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx2.build_ship_combat_state(char2, ship2, 587),
        )
        .await;
    let _ = sys2_handle
        .spawn_ball(eve_destiny::Ball {
            id: inhib_id,
            type_id: 33_589,
            position: glam::DVec3::new(1_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys2_handle
        .spawn_ball(eve_destiny::Ball {
            id: probe_id,
            type_id: 30_013,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 100_000.0,
            inertia: 0.1,
            mass: 50.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    ctx2.active_scanner_probes.write().unwrap().insert(
        probe_id,
        ActiveScannerProbe {
            probe_id,
            probe_type_id: 30_013,
            owner_char_id: char2,
            ship_id: ship2,
            launcher_module_id: 0,
            solar_system_id: sys2,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 2.0 * crate::services::space_destiny::AU_IN_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: now2,
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );
    let bound2 = BoundObjectEntry {
        bound_id: 22,
        session_id: sess2.session_id,
        user_id: sess2.user_id.unwrap_or(1),
        character_id: char2,
        service: "scanMgr".into(),
        object_id: sys2 as i64,
        oid_str: "N=1:22".into(),
        ref_id: 22,
    };
    let target_dest = [1.0e10, 2.0e9, -3.0e9];
    let probe_updates = PyRep::Dict(vec![(
        PyRep::Integer(probe_id),
        PyRep::keyval([
            (
                "destination",
                PyRep::Tuple(target_dest.into_iter().map(PyRep::Real).collect()),
            ),
            ("rangeStep", PyRep::Integer(2)),
        ]),
    )]);
    let inh_rep = handle_scan_and_wormhole_extras(
        &ctx2,
        &mut sess2,
        Some(&bound2),
        "scanMgr",
        "RequestScans",
        &[probe_updates],
        &kw,
    )
    .await
    .unwrap()
    .expect("inhibited RequestScans returns empty results");
    assert_eq!(inh_rep, PyRep::List(Vec::new()));
    let persisted = ctx2.db.with_snapshot(|snap| {
        snap.sp6_lifecycle
            .active_scanner_probes
            .get(&probe_id)
            .map(|p| (p.destination, p.range_step))
    });
    assert_eq!(
        persisted,
        Some((target_dest, 2)),
        "scan-inhibited RequestScans early return must still persist moved probe state"
    );
}

#[tokio::test]
async fn scan_mgr_negative_guards_and_read_purity_across_probes_and_getters() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    // Seed an active probe owned by caller AND an overdue probe that pre-check errors must NOT expire/mutate.
    let active_pid = 880_001_i64;
    let overdue_pid = 880_002_i64;
    let now_unix = ctx.now_unix();
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            active_pid,
            ActiveScannerProbe {
                probe_id: active_pid,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 0,
                solar_system_id: sys_id,
                position: [0.0; 3],
                destination: [0.0; 3],
                range_step: 4,
                scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: now_unix,
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
        map.insert(
            overdue_pid,
            ActiveScannerProbe {
                probe_id: overdue_pid,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 0,
                solar_system_id: sys_id,
                position: [1_000.0, 0.0, 0.0],
                destination: [1_000.0, 0.0, 0.0],
                range_step: 4,
                scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: 10.0,
                launched_at_unix: now_unix.saturating_sub(1_000),
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
        map.insert(
            890_500_009,
            ActiveScannerProbe {
                probe_id: 890_500_009,
                probe_type_id: 30_013,
                owner_char_id: 2_199_002,
                ship_id: 100_000_099,
                launcher_module_id: 0,
                solar_system_id: sys_id,
                position: [2_000.0, 0.0, 0.0],
                destination: [2_000.0, 0.0, 0.0],
                range_step: 4,
                scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: now_unix,
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
    }

    let foreign_bound = BoundObjectEntry {
        bound_id: 901,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: 2_199_002,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 901,
    };
    let own_bound = BoundObjectEntry {
        character_id: char_id,
        ..foreign_bound.clone()
    };

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Unseeded read getters (GetAnomalies, GetFullState) do not mutate DB or synthesize fake ANO- ore anomalies in Jita.
    let PyRep::List(anom_rows) = handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&own_bound), "scanMgr", "GetAnomalies", &[], &kw)
        .await
        .unwrap()
        .expect("undocked GetAnomalies")
    else {
        panic!("expected anomalies list");
    };
    assert!(anom_rows.is_empty(), "unseeded Jita must not synthesize fake zero-asteroid ANO- anomaly");
    let PyRep::Tuple(full_parts) = handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&own_bound), "scanMgr", "GetFullState", &[], &kw)
        .await
        .unwrap()
        .expect("undocked GetFullState")
    else {
        panic!("expected GetFullState 4-tuple");
    };
    assert_eq!(full_parts.len(), 4);
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 2. Foreign bound entry and invalid/foreign scalar arguments fail BEFORE mutating DB or expiring overdue probe.
    let foreign_char_arg = PyRep::Integer(2_199_002);
    let foreign_corp_arg = PyRep::Integer(98_099_001);
    let foreign_station_arg = PyRep::Integer(60_003_761);
    let neg_cases: Vec<(Option<&BoundObjectEntry>, &str, Vec<PyRep>)> = vec![
        (Some(&foreign_bound), "GetSystemScanMgr", vec![]),
        (Some(&foreign_bound), "GetAnomalies", vec![]),
        (Some(&foreign_bound), "GetFullState", vec![]),
        (Some(&foreign_bound), "RequestScans", vec![]),
        (Some(&foreign_bound), "RecoverProbes", vec![]),
        (Some(&foreign_bound), "ReconnectToLostProbes", vec![]),
        (Some(&foreign_bound), "DestroyProbe", vec![]),
        (Some(&foreign_bound), "SetActivityState", vec![PyRep::Integer(1)]),
        (None, "GetSystemScanMgr", vec![foreign_char_arg.clone()]),
        (None, "GetAnomalies", vec![foreign_corp_arg.clone()]),
        (None, "GetFullState", vec![foreign_station_arg.clone()]),
        (None, "GetAnomalies", vec![PyRep::Bool(true)]),
        (None, "GetFullState", vec![PyRep::Integer(sys_id as i64), PyRep::Integer(1)]),
        (None, "SetActivityState", vec![]),
        (None, "SetActivityState", vec![PyRep::Bool(true)]),
        (None, "SetActivityState", vec![PyRep::List(vec![PyRep::Bool(true)]), PyRep::Integer(1)]),
        (None, "SetActivityState", vec![foreign_char_arg.clone()]),
        (Some(&own_bound), "RequestScans", vec![foreign_station_arg.clone()]),
        (Some(&own_bound), "RequestScans", vec![PyRep::Dict(vec![(PyRep::Integer(999_999_999), PyRep::keyval([("rangeStep", PyRep::Integer(2))]))])]),
        (Some(&own_bound), "RecoverProbes", vec![foreign_station_arg.clone()]),
        (Some(&own_bound), "RecoverProbes", vec![PyRep::Bool(true)]),
        (Some(&own_bound), "ReconnectToLostProbes", vec![foreign_station_arg]),
        (Some(&own_bound), "DestroyProbe", vec![PyRep::Integer(char_id)]),
        (Some(&own_bound), "DestroyProbe", vec![PyRep::Integer(890_500_009)]),
        (Some(&own_bound), "DestroyProbe", vec![PyRep::Bool(true)]),
    ];
    for (bound_opt, meth, args) in neg_cases {
        let err = handle_scan_and_wormhole_extras(&ctx, &mut sess, bound_opt, "scanMgr", meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("negative scanMgr probe must fail");
        assert!(!err.code.is_empty(), "{meth} {args:?} produced empty error code");
    }
    assert_eq!(ctx.db.state_epoch(), e0, "negative probes must not mutate DB");
    assert_eq!(ctx.db.tx_commit_count(), c0, "negative probes must not commit DB tx");
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert!(map.contains_key(&overdue_pid), "rejected calls must not expire overdue probe early");
        assert_eq!(map.get(&active_pid).map(|p| p.state), Some(PROBE_STATE_IDLE));
    }

    // 3. Docked SetActivityState rejects NotInSpace without mutating probe state or DB.
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    let docked_err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut docked,
        Some(&own_bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(PROBE_STATE_INACTIVE as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "NotInSpace");
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap().get(&active_pid).map(|p| p.state),
        Some(PROBE_STATE_IDLE)
    );
    assert_eq!(ctx.db.state_epoch(), e0);

    // 4. Legitimate targeted DestroyProbe succeeds on owned active probe.
    let destroyed = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "DestroyProbe",
        &[PyRep::Integer(active_pid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("owned DestroyProbe succeeds");
    assert_eq!(destroyed.dict_get("destroyedCount"), Some(&PyRep::Integer(1)));
    assert!(!ctx.active_scanner_probes.read().unwrap().contains_key(&active_pid));
}

#[tokio::test]
async fn recover_and_destroy_probe_guards_and_range_step_clamping() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let foreign_char = 2_199_002;
    let sys_id = 30_000_142;
    let other_sys = 30_000_144;
    let ship_id = 100_000_001;
    let kw = HashMap::new();
    let now_unix = ctx.now_unix();

    let own_bound = BoundObjectEntry {
        bound_id: 951,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 951,
    };

    let active_pid = 890_601_001_i64;
    let overdue_pid = 890_601_002_i64;
    let inactive_pid = 890_601_003_i64;
    let other_sys_pid = 890_601_004_i64;
    let foreign_pid = 890_601_005_i64;
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        let mk = |pid: i64, owner: i64, s_id: u32, st: i32, max_t: f64, launched: u64| {
            ActiveScannerProbe {
                probe_id: pid,
                probe_type_id: 30_013,
                owner_char_id: owner,
                ship_id,
                launcher_module_id: 0,
                solar_system_id: s_id,
                position: [1_000.0, 0.0, 0.0],
                destination: [1_000.0, 0.0, 0.0],
                range_step: 4,
                scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: max_t,
                launched_at_unix: launched,
                state: st,
                warp_end_ft: None,
            }
        };
        map.insert(active_pid, mk(active_pid, char_id, sys_id, PROBE_STATE_IDLE, 3_600.0, now_unix));
        map.insert(
            overdue_pid,
            mk(overdue_pid, char_id, sys_id, PROBE_STATE_IDLE, 10.0, now_unix.saturating_sub(1_000)),
        );
        map.insert(inactive_pid, mk(inactive_pid, char_id, sys_id, 0, 3_600.0, now_unix));
        map.insert(other_sys_pid, mk(other_sys_pid, char_id, other_sys, PROBE_STATE_IDLE, 3_600.0, now_unix));
        map.insert(foreign_pid, mk(foreign_pid, foreign_char, sys_id, PROBE_STATE_IDLE, 3_600.0, now_unix));
    }
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Explicit RecoverProbes([]) is a read-pure no-op (does NOT recover probes or expire overdue probes).
    let empty_rec = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "RecoverProbes",
        &[PyRep::List(vec![])],
        &kw,
    )
    .await
    .unwrap()
    .expect("RecoverProbes([]) must succeed as no-op");
    assert_eq!(empty_rec, PyRep::List(Vec::new()));
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&active_pid));
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&overdue_pid));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 2. RecoverProbes rejects foreign, wrong-system, or inactive probe IDs before expiring overdue probes.
    for bad_recover in [
        vec![PyRep::List(vec![PyRep::Integer(foreign_pid)])],
        vec![PyRep::List(vec![PyRep::Integer(other_sys_pid)])],
        vec![PyRep::List(vec![PyRep::Integer(inactive_pid)])],
        vec![PyRep::Integer(inactive_pid)],
    ] {
        let err = handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            Some(&own_bound),
            "scanMgr",
            "RecoverProbes",
            &bad_recover,
            &kw,
        )
        .await
        .unwrap()
        .expect_err("foreign/wrong-system/inactive RecoverProbes must fail with ProbeNotFound");
        assert_eq!(err.code, "ProbeNotFound");
    }
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&overdue_pid));
    assert_eq!(ctx.db.state_epoch(), e0);

    // 3. P2 clamping: large rangeStep i64 values clamp without overflowing i32.
    let mut max_kw = HashMap::new();
    max_kw.insert("rangeStep".into(), PyRep::Integer(i64::MAX));
    let _ = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "RequestScans",
        &[PyRep::None],
        &max_kw,
    )
    .await
    .unwrap()
    .expect("RequestScans clamps i64::MAX rangeStep to 8");
    assert_eq!(ctx.active_scanner_probes.read().unwrap().get(&active_pid).map(|p| p.range_step), Some(8));

    let mut min_kw = HashMap::new();
    min_kw.insert("rangeStep".into(), PyRep::Integer(i64::MIN));
    let _ = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "RequestScans",
        &[PyRep::None],
        &min_kw,
    )
    .await
    .unwrap()
    .expect("RequestScans clamps i64::MIN rangeStep to 1");
    assert_eq!(ctx.active_scanner_probes.read().unwrap().get(&active_pid).map(|p| p.range_step), Some(1));

    for (key, val) in [("scanRangeAU", f64::MAX), ("scanRange", f64::MAX)] {
        let huge_dict = PyRep::Dict(vec![(
            PyRep::Integer(active_pid),
            PyRep::keyval([(key, PyRep::Real(val))]),
        )]);
        let _ = handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            Some(&own_bound),
            "scanMgr",
            "RequestScans",
            &[huge_dict],
            &kw,
        )
        .await
        .unwrap()
        .expect("RequestScans clamps finite f64::MAX radius to step 8 without overflowing");
        assert_eq!(
            ctx.active_scanner_probes.read().unwrap().get(&active_pid).map(|p| p.range_step),
            Some(8)
        );
    }

    // 4. DestroyProbe rejects wrong-system probe, leaves cargo sentinel (-1) as no-op, and spares other-system probes on bulk destroy.
    let sentinel_pid = 890_601_006_i64;
    ctx.active_scanner_probes.write().unwrap().insert(
        sentinel_pid,
        ActiveScannerProbe {
            probe_id: sentinel_pid,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: now_unix,
            state: -1,
            warp_end_ft: None,
        },
    );
    let sentinel_res = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "DestroyProbe",
        &[PyRep::Integer(sentinel_pid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("in-cargo sentinel probe (-1) DestroyProbe is idempotent no-op");
    assert_eq!(sentinel_res.dict_get("destroyedCount"), Some(&PyRep::Integer(0)));
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&sentinel_pid));

    let sys_err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "DestroyProbe",
        &[PyRep::Integer(other_sys_pid)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("wrong-system DestroyProbe must fail with ProbeNotFound");
    assert_eq!(sys_err.code, "ProbeNotFound");

    let bulk_dest = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&own_bound),
        "scanMgr",
        "DestroyProbe",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("bulk DestroyProbe succeeds for current system");
    assert_eq!(bulk_dest.dict_get("destroyedCount"), Some(&PyRep::Integer(2)));
    assert!(
        ctx.active_scanner_probes.read().unwrap().contains_key(&other_sys_pid),
        "bulk DestroyProbe in sys_id must preserve probes in other systems"
    );
    assert!(
        ctx.active_scanner_probes.read().unwrap().contains_key(&sentinel_pid),
        "bulk DestroyProbe must preserve negative-state cargo sentinels"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_activity_state_enforces_8_probe_cap_across_relaunch_per_probe_and_bulk() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001_i64;
    ctx.db
        .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let launcher_id = 710_951_i64;
    let charge_id = 710_952_i64;

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
                type_id: 30_013,
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

    let bound = BoundObjectEntry {
        bound_id: 960,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 960,
    };
    let kw = HashMap::new();
    let mut launch_kw = HashMap::new();
    launch_kw.insert("probeCount".into(), PyRep::Integer(8));

    // 1. Launch 8 probes -> SetActivityState(0) -> relaunch 8 more probes.
    activate_scan_probe_launcher(
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
        &launch_kw,
    )
    .await
    .expect("first 8-probe launch");

    let mut wave1_ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
        .map(|p| p.probe_id)
        .collect();
    wave1_ids.sort_unstable();
    assert_eq!(wave1_ids.len(), 8);

    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(i64::from(PROBE_STATE_INACTIVE))],
        &kw,
    )
    .await
    .unwrap()
    .expect("deactivate wave 1");
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert!(wave1_ids.iter().all(|id| map[id].state == PROBE_STATE_INACTIVE));
    }

    activate_scan_probe_launcher(
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
        &launch_kw,
    )
    .await
    .expect("second 8-probe launch while wave 1 is inactive");

    let mut wave2_ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
        .map(|p| p.probe_id)
        .collect();
    wave2_ids.sort_unstable();
    assert_eq!(wave2_ids.len(), 8);

    // Unrelated active probe in another system must not count toward sys_id's 8-probe cap.
    let other_sys_pid = 890_800_999_i64;
    ctx.active_scanner_probes.write().unwrap().insert(
        other_sys_pid,
        ActiveScannerProbe {
            probe_id: other_sys_pid,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: launcher_id,
            solar_system_id: 30_000_144,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: ctx.now_unix(),
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );

    let active_in_sys = || -> usize {
        ctx.active_scanner_probes
            .read()
            .unwrap()
            .values()
            .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
            .count()
    };

    // 2. Per-probe reactivation exceeding 8 active probes rejects atomically with MaxProbesLaunched.
    let _ = ctx.drain_notifications_for_char(char_id);
    let c0 = ctx.db.tx_commit_count();
    for bad_args in [
        vec![PyRep::Integer(wave1_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_IDLE))],
        vec![
            PyRep::List(vec![PyRep::Integer(wave1_ids[0]), PyRep::Integer(wave1_ids[1])]),
            PyRep::Integer(i64::from(PROBE_STATE_IDLE)),
        ],
        vec![PyRep::Dict(vec![
            (PyRep::Integer(wave2_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_SCANNING))),
            (PyRep::Integer(wave1_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_IDLE))),
        ])],
    ] {
        let err = handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            Some(&bound),
            "scanMgr",
            "SetActivityState",
            &bad_args,
            &kw,
        )
        .await
        .unwrap()
        .expect_err("reactivating >8 probes must reject with MaxProbesLaunched");
        assert_eq!(err.code, "MaxProbesLaunched");
    }
    assert_eq!(ctx.db.tx_commit_count(), c0, "rejected SetActivityState must not write DB");
    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "rejected SetActivityState must not emit OnProbeStateChanged"
    );
    assert_eq!(active_in_sys(), 8);
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert_eq!(map[&wave2_ids[0]].state, PROBE_STATE_IDLE, "failed dict must not partially mutate wave2");
        assert_eq!(map[&wave1_ids[0]].state, PROBE_STATE_INACTIVE);
    }

    // Per-probe 1-for-1 swap keeping projected active count at 8 succeeds atomically.
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Dict(vec![
            (PyRep::Integer(wave2_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_INACTIVE))),
            (PyRep::Integer(wave1_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_IDLE))),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .expect("atomic 1-for-1 probe swap stays within 8-probe cap");
    assert_eq!(active_in_sys(), 8);
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Dict(vec![
            (PyRep::Integer(wave1_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_INACTIVE))),
            (PyRep::Integer(wave2_ids[0]), PyRep::Integer(i64::from(PROBE_STATE_IDLE))),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .expect("swap back succeeds");

    // 3. Bulk SetActivityState(>0) when 8 are active updates already-active probes and leaves inactive probes at 0.
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(i64::from(PROBE_STATE_SCANNING))],
        &kw,
    )
    .await
    .unwrap()
    .expect("bulk SetActivityState(4) succeeds with 8-probe cap");
    assert_eq!(active_in_sys(), 8);
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        assert!(wave2_ids.iter().all(|id| map[id].state == PROBE_STATE_SCANNING));
        assert!(wave1_ids.iter().all(|id| map[id].state == PROBE_STATE_INACTIVE));
    }

    // 4. Deactivate 3 active probes (5 active, 11 inactive) -> bulk SetActivityState(1) reactivates
    //    only the lowest 3 inactive probes by probe_id so total active count remains capped at 8.
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[
            PyRep::List(wave2_ids[..3].iter().copied().map(PyRep::Integer).collect()),
            PyRep::Integer(i64::from(PROBE_STATE_INACTIVE)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("deactivate 3 probes");
    assert_eq!(active_in_sys(), 5);

    let _ = ctx.drain_notifications_for_char(char_id);
    handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "SetActivityState",
        &[PyRep::Integer(i64::from(PROBE_STATE_IDLE))],
        &kw,
    )
    .await
    .unwrap()
    .expect("bulk SetActivityState(1) caps reactivations to 3 remaining slots");
    assert_eq!(active_in_sys(), 8, "bulk reactivation must never exceed 8 active probes");

    let mut all_inactive_before: Vec<i64> = wave1_ids
        .iter()
        .copied()
        .chain(wave2_ids[..3].iter().copied())
        .collect();
    all_inactive_before.sort_unstable();
    let expected_reactivated = &all_inactive_before[..3];
    let expected_still_inactive = &all_inactive_before[3..];
    {
        let map = ctx.active_scanner_probes.read().unwrap();
        for pid in &wave2_ids[3..] {
            assert_eq!(map[pid].state, PROBE_STATE_IDLE);
        }
        for pid in expected_reactivated {
            assert_eq!(map[pid].state, PROBE_STATE_IDLE);
        }
        for pid in expected_still_inactive {
            assert_eq!(map[pid].state, PROBE_STATE_INACTIVE);
        }
        assert_eq!(map[&other_sys_pid].state, PROBE_STATE_IDLE);
    }
    let state_notifs: Vec<i64> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnProbeStateChanged")
        .filter_map(|n| n.args.first().and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(state_notifs.len(), 8, "5 updated + 3 reactivated = 8 OnProbeStateChanged events");
    for pid in expected_still_inactive {
        assert!(!state_notifs.contains(pid), "capped probe {pid} must not emit OnProbeStateChanged");
    }
}

#[tokio::test]
async fn anomaly_payloads_populate_dungeon_name_id_and_jita_skips_synthetic_ore() {
    let mut sde = eve_sde::SdeRegistry::default();
    let mut buf = vec![0u8; 72];
    buf[40..48].copy_from_slice(&1u64.to_le_bytes());
    buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    buf[56..64].copy_from_slice(&((72 - 32) as u64).to_le_bytes());
    buf[64..72].copy_from_slice(&1u64.to_le_bytes());
    buf.extend_from_slice(&[0u8; 120]);
    buf[72..76].copy_from_slice(&1344u32.to_le_bytes());
    buf[152..156].copy_from_slice(&112_326u32.to_le_bytes());
    let path = std::env::temp_dir().join(format!(
        "evomu_anom_nid_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&path, &buf).unwrap();
    sde.resources.insert(
        "res:/staticdata/dungeons.fsdbinary".into(),
        eve_sde::SdeResourceInfo {
            res_path: "res:/staticdata/dungeons.fsdbinary".into(),
            cdn_relative_path: path.display().to_string(),
            md5: String::new(),
            byte_size: buf.len() as u64,
            format: "cfsd".into(),
            record_count: 1,
            spot: String::new(),
        },
    );

    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.provision_gm_account("nid_p", "pw", 90_153_401, "NidP");
    let uid = db.with_mut_snapshot(|s| {
        let c = s.characters.get_mut(&90_153_401).unwrap();
        c.station_id = None;
        c.solar_system_id = 30_000_142;
        c.user_id
    });
    db.acquire_online_lock(uid, 1534, Some(90_153_401), true, 1)
        .unwrap();
    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::new(sde), db));

    assert!(
        crate::services::space_destiny::ensure_system_signatures_and_anomalies(&ctx, 30_000_142)
            .await
            .1
            .is_empty(),
        "unseeded Jita must not synthesize fake zero-asteroid ore anomalies"
    );

    let rec = eve_db::CosmicAnomalyRecord {
        anomaly_id: "DNG-1344-1".into(),
        solar_system_id: 30_000_142,
        name: "Average Hedbergite Deposit".into(),
        anomaly_type: "Ore".into(),
        position: [1e9, 0.0, 0.0],
        signal_strength: 1.0,
        ore_types: vec![21],
        remaining_volume: 100.0,
        created_unix: 1,
        lifetime_seconds: 0,
        cleared: false,
    };
    let unknown_rec = eve_db::CosmicAnomalyRecord {
        anomaly_id: "DNG-999-1".into(),
        name: "Unknown Ore Deposit".into(),
        ..rec.clone()
    };
    assert_eq!(
        crate::services::fleet_social::fleet_social_scanning::build_signal_tracker_anomaly_kv(
            &ctx.sde, &rec,
        )
        .dict_get("dungeonNameID"),
        Some(&PyRep::Integer(112_326))
    );
    assert_eq!(
        crate::services::fleet_social::fleet_social_scanning::build_signal_tracker_anomaly_kv(
            &ctx.sde,
            &unknown_rec,
        )
        .dict_get("dungeonNameID"),
        Some(&PyRep::None)
    );

    ctx.db.with_mut_snapshot(|s| {
        s.cosmic_anomalies
            .entry(30_000_142)
            .or_default()
            .extend([rec, unknown_rec]);
    });
    let _ = ctx.drain_notifications_for_char(90_153_401);
    crate::services::notify_emitters_tournaments_anomalies::emit_signal_tracker_full_state_for_system(
        &ctx,
        90_153_401,
        30_000_142,
        &[],
    );
    crate::services::notify_emitters_tournaments_anomalies::emit_cosmic_anomaly_added_for_system(
        &ctx,
        30_000_142,
        "DNG-1344-1",
        1,
        1344,
        "Average Hedbergite Deposit",
    );
    crate::services::notify_emitters_tournaments_anomalies::emit_cosmic_anomaly_added_for_system(
        &ctx,
        30_000_142,
        "DNG-999-1",
        2,
        999,
        "Unknown Ore Deposit",
    );
    let evs = ctx.drain_notifications_for_char(90_153_401);
    let _ = std::fs::remove_file(&path);

    let full_state_anoms = evs
        .iter()
        .find(|n| n.event == "OnSignalTrackerFullState")
        .and_then(|n| match n.args.get(1) {
            Some(PyRep::Tuple(parts)) => parts.first(),
            _ => None,
        })
        .expect("OnSignalTrackerFullState full_state anomalies dict");
    assert_eq!(
        full_state_anoms
            .dict_get("DNG-1344-1")
            .and_then(|kv| kv.dict_get("dungeonNameID")),
        Some(&PyRep::Integer(112_326))
    );
    assert_eq!(
        full_state_anoms
            .dict_get("DNG-999-1")
            .and_then(|kv| kv.dict_get("dungeonNameID")),
        Some(&PyRep::None)
    );
    assert!(evs.iter().any(|n| {
        n.event == "OnCosmicAnomalyAdded"
            && n.args.first().and_then(|v| v.dict_get("targetID"))
                == Some(&PyRep::String("DNG-1344-1".into()))
            && n.args.first().and_then(|v| v.dict_get("dungeonNameID"))
                == Some(&PyRep::Integer(112_326))
    }));
    assert!(evs.iter().any(|n| {
        n.event == "OnCosmicAnomalyAdded"
            && n.args.first().and_then(|v| v.dict_get("targetID"))
                == Some(&PyRep::String("DNG-999-1".into()))
            && n.args.first().and_then(|v| v.dict_get("dungeonNameID")) == Some(&PyRep::None)
    }));
}

#[tokio::test]
async fn zero_anomaly_system_read_paths_stay_readonly_after_initialization() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let now_unix = ctx.now_unix();
    let kw = HashMap::new();

    // Pre-mark PvE exploration seeded for Jita with zero active anomalies, and pre-insert only a
    // non-native DNG- signature so readiness gating still rejects uninitialized native signatures.
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(sys_id, Vec::new());
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "DNG-999-ONLY".into(),
                solar_system_id: sys_id,
                sig_group: "Ore Site".into(),
                type_name: "Dummy".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
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

    // First GetSystemScanMgr initializes native signatures (readiness gate must still catch
    // DNG-only uninitialized state) while keeping cosmic_anomalies empty.
    let e_pre_init = ctx.db.state_epoch();
    let c_pre_init = ctx.db.tx_commit_count();
    let first_mgr = handle_scan_and_wormhole_extras(
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
    .expect("initial GetSystemScanMgr");
    assert_eq!(
        first_mgr.dict_get("solarSystemID"),
        Some(&PyRep::Integer(sys_id as i64))
    );
    assert!(ctx.db.state_epoch() > e_pre_init);
    assert!(ctx.db.tx_commit_count() > c_pre_init);

    // Ensure zero-anomaly system with absent cosmic_anomalies map entry, register SignalTracker
    // subscriber once, and seed one synced active scanner probe for RequestScans.
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.remove(&sys_id);
    });
    let first_reg = crate::services::fleet_social::handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "SignalTrackerRegister",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("initial SignalTrackerRegister");
    assert_eq!(first_reg.dict_get("anomalyCount"), Some(&PyRep::Integer(0)));
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            890_500_011,
            ActiveScannerProbe {
                probe_id: 890_500_011,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 0,
                solar_system_id: sys_id,
                position: [1_000.0, 0.0, 0.0],
                destination: [1_000.0, 0.0, 0.0],
                range_step: 4,
                scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: now_unix,
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
    }
    sync_scanner_probes_to_db(&ctx);

    let bound = BoundObjectEntry {
        bound_id: 11,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 11,
    };

    // Repeated GetSystemScanMgr, SignalTrackerRegister, GetSignatures, GetAnomalies, and
    // GetFullState calls on initialized zero-anomaly Jita must stay on the read-only fast path.
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();
    for _ in 0..2 {
        let mgr = handle_scan_and_wormhole_extras(
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
        .expect("repeated GetSystemScanMgr");
        assert_eq!(mgr.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));

        let reg = crate::services::fleet_social::handle(
            &ctx,
            &mut sess,
            "scanMgr",
            "SignalTrackerRegister",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .expect("repeated SignalTrackerRegister");
        assert_eq!(reg.dict_get("anomalyCount"), Some(&PyRep::Integer(0)));
        assert!(
            matches!(reg.dict_get("signatureCount"), Some(&PyRep::Integer(n)) if n > 0),
            "expected native signatures on read-only SignalTrackerRegister fast path"
        );

        let sigs_rep = crate::services::fleet_social::handle(
            &ctx,
            &mut sess,
            "scanMgr",
            "GetSignatures",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .expect("GetSignatures");
        assert!(
            matches!(&sigs_rep, PyRep::Rowset { rows, .. } if !rows.is_empty()),
            "expected non-empty GetSignatures rowset on read-only path, got {sigs_rep:?}"
        );

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
        assert_eq!(anoms, PyRep::List(Vec::new()));

        let full = handle_scan_and_wormhole_extras(
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
        assert!(matches!(full, PyRep::Tuple(parts) if parts.len() == 4));
    }
    assert_eq!(ctx.db.state_epoch(), e0, "read paths must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), c0, "read paths must not commit DB writes");

    // RequestScans on zero-anomaly Jita skips ensure_system_signatures_and_anomalies fallback
    // (same DB commit/epoch delta as a system whose cosmic_anomalies map has an active anomaly,
    // and leaves cosmic_anomalies absent without synthesizing fake ANO- rows).
    let sig_ids_before: Vec<String> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&sys_id)
            .into_iter()
            .flatten()
            .map(|s| s.signature_id.clone())
            .collect()
    });
    let e_scan0 = ctx.db.state_epoch();
    let c_scan0 = ctx.db.tx_commit_count();
    let scan_zero = handle_scan_and_wormhole_extras(
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
    .expect("RequestScans on zero-anomaly Jita");
    assert!(matches!(scan_zero, PyRep::List(_)));
    let zero_anom_e_delta = ctx.db.state_epoch() - e_scan0;
    let zero_anom_c_delta = ctx.db.tx_commit_count() - c_scan0;
    assert!(
        ctx.db.with_snapshot(|s| !s.cosmic_anomalies.contains_key(&sys_id)),
        "RequestScans must not synthesize cosmic_anomalies on zero-anomaly Jita"
    );
    let sig_ids_after: Vec<String> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&sys_id)
            .into_iter()
            .flatten()
            .map(|s| s.signature_id.clone())
            .collect()
    });
    assert_eq!(sig_ids_after, sig_ids_before);

    // Baseline comparison: reset probe to IDLE and insert one active anomaly; RequestScans must
    // execute the exact same commit count (no extra ensure_system_signatures_and_anomalies write).
    if let Some(p) = ctx.active_scanner_probes.write().unwrap().get_mut(&890_500_011) {
        p.state = PROBE_STATE_IDLE;
    }
    sync_scanner_probes_to_db(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![eve_db::CosmicAnomalyRecord {
                anomaly_id: "DNG-1344-1".into(),
                solar_system_id: sys_id,
                anomaly_type: "Ore".into(),
                name: "Veldspar Deposit".into(),
                position: [5_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
    });
    let e_scan1 = ctx.db.state_epoch();
    let c_scan1 = ctx.db.tx_commit_count();
    let scan_anom = handle_scan_and_wormhole_extras(
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
    .expect("RequestScans baseline with populated anomaly");
    assert!(matches!(scan_anom, PyRep::List(_)));
    assert_eq!(ctx.db.state_epoch() - e_scan1, zero_anom_e_delta);
    assert_eq!(ctx.db.tx_commit_count() - c_scan1, zero_anom_c_delta);
}

#[tokio::test]
async fn bf01_space_probe_scanning_rejects_active_upwell_structure_control() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let st_id = 1_000_000_009_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(st_id);
            ch.active_ship_id = st_id;
        }
    });
    sess.structure_id = Some(st_id);
    sess.ship_id = Some(st_id);
    let kw = HashMap::new();

    let err_not_in_space = "NotInSpace";
    for (method, args) in [
        ("RequestScans", vec![]),
        ("RecoverProbes", vec![]),
        ("ReconnectToLostProbes", vec![]),
        ("DestroyProbe", vec![]),
        ("SetActivityState", vec![PyRep::Integer(0)]),
    ] {
        let err = handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", method, &args, &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, err_not_in_space, "{method}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rt_109_poisoned_probe_locks_recover_cleanly_on_read_and_write() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let probe_id = 890_999_001_i64;
    let now_unix = ctx.now_unix();
    let kw = HashMap::new();

    ctx.active_scanner_probes.write().unwrap().insert(
        probe_id,
        ActiveScannerProbe {
            probe_id,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 0,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 4.0 * crate::services::space_destiny::AU_IN_METERS,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: now_unix,
            state: PROBE_STATE_IDLE,
            warp_end_ft: None,
        },
    );

    let probes_lock = Arc::clone(&ctx.active_scanner_probes);
    let _ = std::thread::spawn(move || {
        let _guard = probes_lock.write().unwrap();
        panic!("intentional RT-109 probe lock poison");
    })
    .join();
    let scan_ctx_lock = Arc::clone(&ctx.active_probe_scan_contexts);
    let _ = std::thread::spawn(move || {
        let _guard = scan_ctx_lock.write().unwrap();
        panic!("intentional RT-109 scan context lock poison");
    })
    .join();
    assert!(ctx.active_scanner_probes.read().is_err());
    assert!(ctx.active_probe_scan_contexts.write().is_err());

    let bound = BoundObjectEntry {
        bound_id: 991,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 991,
    };

    // RequestScans and DestroyProbe must still recover poisoned locks rather than dropping/failing.
    let scan_res = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("poisoned probe locks must recover cleanly in RequestScans");
    assert!(matches!(scan_res, PyRep::List(_)));

    let destroyed = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "DestroyProbe",
        &[PyRep::Integer(probe_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("poisoned probe locks must recover cleanly in DestroyProbe");
    assert_eq!(destroyed.dict_get("destroyedCount"), Some(&PyRep::Integer(1)));
    assert!(
        !ctx.active_scanner_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&probe_id)
    );
}
