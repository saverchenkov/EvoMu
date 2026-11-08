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
    assert!(matches!(full_state, PyRep::Tuple(_)));

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
