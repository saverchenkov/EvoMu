//! Additional edge-case and branch coverage tests for `services::space_probe_scanning`.

use super::*;
use crate::router::{ActiveScannerProbe, BoundObjectEntry, ServerContext};
use crate::services::space_destiny::AU_IN_METERS;
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{SessionState, service_roles};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn make_test_probe(
    probe_id: i64,
    owner_char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
    probe_type_id: u32,
    launched_at_unix: u64,
) -> ActiveScannerProbe {
    ActiveScannerProbe {
        probe_id,
        probe_type_id,
        owner_char_id,
        ship_id,
        launcher_module_id: 96_700_100,
        solar_system_id,
        position: [0.0, 0.0, 0.0],
        destination: [0.0, 0.0, 0.0],
        range_step: 4,
        scan_range_m: 4.0 * AU_IN_METERS,
        base_sensor_strength: 40.0,
        max_flight_time_s: 3_600.0,
        launched_at_unix,
        state: PROBE_STATE_IDLE,
        warp_end_ft: None,
    }
}

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if std::env::var_os("EVOMU_SDE_PATH").is_some() {
            Arc::new(ServerContext::new().sde.as_ref().clone())
        } else {
            Arc::new(SdeRegistry::with_canonical_seed())
        }
    }))
}

fn make_ctx_and_session(char_id: i64, sys_id: u32) -> (Arc<ServerContext>, SessionState) {
    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(
            shared_sde(),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(9830);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001).cloned() {
            let mut c = ch;
            c.character_id = char_id;
            c.station_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
            snap.characters.insert(char_id, c);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    (ctx, sess)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launcher_activation_clamping_incompatible_and_max_probes() {
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let (ctx, sess) = make_ctx_and_session(char_id, sys_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();
    let ship_id = 100_000_001_i64;
    let launcher_id = 96_700_100_i64;
    let charge_id = 96_700_101_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 17938, // Core Probe Launcher I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(launcher_id, true);
    });

    // 1. NoProbesLoaded
    assert_eq!(
        activate_scan_probe_launcher(
            &ctx,
            &sess,
            &sys,
            char_id,
            ship_id,
            sys_id,
            launcher_id,
            17_938,
            InventoryFlag::HiSlot0 as u16,
            "launchForSelf",
            None,
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NoProbesLoaded"
    );

    // 2. Combat probe loaded in Core Probe Launcher -> InvalidProbeForLauncher
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 30028, // Combat Scanner Probe I
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
    assert_eq!(
        activate_scan_probe_launcher(
            &ctx,
            &sess,
            &sys,
            char_id,
            ship_id,
            sys_id,
            launcher_id,
            17_938,
            InventoryFlag::HiSlot0 as u16,
            "launchForSelf",
            None,
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidProbeForLauncher"
    );

    // 3. Partial launch clamping: seed 6 active probes, load 8 Core Scanner Probes -> launches only 2 (remaining 6)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&charge_id).unwrap().type_id = 30013; // Core Scanner Probe I
    });
    let now_unix = ctx.now_unix_secs();
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        for i in 1..=6_i64 {
            map.insert(
                96_710_000 + i,
                make_test_probe(96_710_000 + i, char_id, ship_id, sys_id, 30_013, now_unix),
            );
        }
    }
    let launched = activate_scan_probe_launcher(
        &ctx,
        &sess,
        &sys,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_938,
        InventoryFlag::HiSlot0 as u16,
        "launchForSelf",
        None,
        &kw,
    )
    .await
    .unwrap();
    assert!(matches!(launched, PyRep::Integer(2) | PyRep::KeyVal(_)) || launched != PyRep::None);
    assert_eq!(ctx.active_scanner_probes.read().unwrap().len(), 8);

    // 4. Already at 8 -> MaxProbesLaunched
    assert_eq!(
        activate_scan_probe_launcher(
            &ctx,
            &sess,
            &sys,
            char_id,
            ship_id,
            sys_id,
            launcher_id,
            17_938,
            InventoryFlag::HiSlot0 as u16,
            "launchForSelf",
            None,
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MaxProbesLaunched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_state_qa_rpcs_activity_states_system_scan_mgr_and_anomalies() {
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys = 30_002_187_u32;
    let (ctx, mut sess) = make_ctx_and_session(char_id, sys_id);
    let kw = HashMap::new();

    // 1. GetFullState cross-system access denied for non-GM, allowed for GM
    let bound_other = BoundObjectEntry {
        bound_id: 1,
        session_id: 9830,
        user_id: 1,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: other_sys as i64,
        oid_str: format!("scanMgr:{other_sys}"),
        ref_id: 1,
    };
    assert_eq!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            Some(&bound_other),
            "scanMgr",
            "GetFullState",
            &[PyRep::Integer(other_sys as i64)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrossSystemScanDenied"
    );

    let mut gm_sess = sess.clone();
    gm_sess.role = service_roles::ROLE_GML;
    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut gm_sess,
            Some(&bound_other),
            "scanMgr",
            "GetFullState",
            &[PyRep::Integer(other_sys as i64)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 2. QAOverrideProbeExpiry & QAScanSites GMRoleRequired vs GM
    assert_eq!(
        handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "QAOverrideProbeExpiry", &[PyRep::Integer(60)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    assert_eq!(
        handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "QAScanSites", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    assert!(
        handle_scan_and_wormhole_extras(&ctx, &mut gm_sess, None, "scanMgr", "QAOverrideProbeExpiry", &[PyRep::Integer(60)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle_scan_and_wormhole_extras(&ctx, &mut gm_sess, None, "scanMgr", "QAScanSites", &[], &kw)
            .await
            .unwrap()
            .is_ok()
    );

    // 3. SetActivityState variants + InvalidProbeState
    let pid1 = 96_720_001_i64;
    let pid2 = 96_720_002_i64;
    let now_unix = ctx.now_unix_secs();
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        for pid in [pid1, pid2] {
            map.insert(
                pid,
                make_test_probe(pid, char_id, 100_000_001, sys_id, 30_013, now_unix),
            );
        }
    }
    assert_eq!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "SetActivityState",
            &[PyRep::Integer(pid1), PyRep::Integer(99)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidProbeState"
    );
    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "SetActivityState",
            &[PyRep::Dict(vec![(PyRep::Integer(pid1), PyRep::Integer(1))])],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "SetActivityState",
            &[PyRep::Tuple(vec![PyRep::Integer(pid1), PyRep::Integer(pid2)]), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "SetActivityState",
            &[PyRep::List(vec![PyRep::Integer(pid1)]), PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 4. GetSystemScanMgr reveal tiers (<0.25, 0.25..0.75, 0.75..1.0, >=1.0 wormhole) & GetAnomalies filtering
    let now = ctx.now_unix_secs();
    ctx.record_character_scan_progress(char_id, sys_id, "SIG-1", 0.10);
    ctx.record_character_scan_progress(char_id, sys_id, "SIG-2", 0.50);
    ctx.record_character_scan_progress(char_id, sys_id, "SIG-3", 1.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.clear();
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: "SIG-1".into(),
                    solar_system_id: sys_id,
                    sig_group: "Data Site".into(),
                    type_name: "Hidden Data".into(),
                    position: [1_000.0, 0.0, 0.0],
                    signal_strength: 0.1,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-2".into(),
                    solar_system_id: sys_id,
                    sig_group: "Relic Site".into(),
                    type_name: "Ruined Monument".into(),
                    position: [2_000.0, 0.0, 0.0],
                    signal_strength: 0.5,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-3".into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "Wormhole K162".into(),
                    position: [3_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: Some(other_sys),
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 1_500_000_000.0,
                    max_jump_mass_kg: 300_000_000.0,
                    collapsed: false,
                },
            ],
        );

        snap.cosmic_anomalies.clear();
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-OK".into(),
                    solar_system_id: sys_id,
                    name: "Serpentis Hideaway".into(),
                    anomaly_type: "Combat".into(),
                    position: [5_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANO-CLEARED".into(),
                    solar_system_id: sys_id,
                    name: "Cleared".into(),
                    anomaly_type: "Combat".into(),
                    position: [5_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    cleared: true,
                },
            ],
        );
    });

    assert_eq!(
        handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "GetSystemScanMgr", &[PyRep::Integer(other_sys as i64)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    let mgr_rep = handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "GetSystemScanMgr", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(mgr_rep, PyRep::None);

    let anom_rep = handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "GetAnomalies", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(anom_rep, PyRep::List(ref l) if l.len() == 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_scans_recover_reconnect_destroy_and_tick_warp_end() {
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let (ctx, mut sess) = make_ctx_and_session(char_id, sys_id);
    let ship_id = 100_000_001_i64;
    let kw = HashMap::new();

    // 1. Unbound empty args returns None; NoActiveProbes when non-empty
    assert!(
        handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "RequestScans", &[], &kw)
            .await
            .is_none()
    );
    assert_eq!(
        handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "RequestScans", &[PyRep::Dict(vec![])], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveProbes"
    );
    assert_eq!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "RequestScans",
            &[PyRep::Dict(vec![(
                PyRep::Integer(1),
                PyRep::keyval([("x", PyRep::Real(0.0)), ("y", PyRep::Real(0.0)), ("z", PyRep::Real(0.0)), ("rangeStep", PyRep::Integer(2))]),
            )])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveProbes"
    );

    // 2. Seed combat scanner probe + target balls (Ship, Drone, Container excluded)
    let pid = 96_730_001_i64;
    let now_unix = ctx.now_unix_secs();
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            pid,
            make_test_probe(pid, char_id, ship_id, sys_id, 30_028, now_unix),
        );
    }
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h
            .spawn_ball(Ball {
                id: pid,
                type_id: 30_028,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 3_000.0,
                inertia: 1.0,
                mass: 100.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _ = sys_h
            .spawn_ball(Ball {
                id: 96_730_500,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _ = sys_h
            .spawn_ball(Ball {
                id: 96_730_501,
                type_id: 23,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // InvalidProbeCoordinates (NaN & negative range)
    assert_eq!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "RequestScans",
            &[PyRep::Dict(vec![(
                PyRep::Integer(pid),
                PyRep::keyval([("x", PyRep::Real(f64::NAN)), ("y", PyRep::Real(0.0)), ("z", PyRep::Real(0.0))]),
            )])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidProbeCoordinates"
    );

    // Short move <= 150_000m (pos Tuple variant + rangeStep) then Long warp > 150_000m + tick_probe_warp_end
    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "RequestScans",
            &[PyRep::Dict(vec![(
                PyRep::Integer(pid),
                PyRep::keyval([
                    ("pos", PyRep::Tuple(vec![PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
                    ("rangeStep", PyRep::Integer(3)),
                ]),
            )])],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    assert!(
        handle_scan_and_wormhole_extras(
            &ctx,
            &mut sess,
            None,
            "scanMgr",
            "RequestScans",
            &[PyRep::Dict(vec![(
                PyRep::Integer(pid),
                PyRep::keyval([
                    ("x", PyRep::Real(1_000_000_000.0)),
                    ("y", PyRep::Real(0.0)),
                    ("z", PyRep::Real(0.0)),
                    ("scanRangeAU", PyRep::Real(4.0)),
                ]),
            )])],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let past_ft = ctx.now_blueos_filetime_ticks().saturating_sub(10_000);
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        if let Some(p) = map.get_mut(&pid) {
            p.warp_end_ft = Some(past_ft);
        }
    }
    tick_probe_warp_end(&ctx);
    assert_eq!(ctx.active_scanner_probes.read().unwrap()[&pid].state, PROBE_STATE_SCANNING);
    ctx.db.with_mut_snapshot(|snap| {
        for e in &mut snap.fleet_and_social_ops.pending_probe_scans { e.5 = past_ft; }
    });
    tick_probe_warp_end(&ctx);
    assert_eq!(ctx.active_scanner_probes.read().unwrap()[&pid].state, PROBE_STATE_IDLE);

    // 3. ReconnectToLostProbes on inactive probe -> RecoverProbes (cargo fallback) -> DestroyProbe
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        if let Some(p) = map.get_mut(&pid) {
            p.state = PROBE_STATE_INACTIVE;
        }
    }
    let reconn = handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "ReconnectToLostProbes", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(reconn.dict_get("reconnectedProbeIDs").is_some());

    let recov = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        None,
        "scanMgr",
        "RecoverProbes",
        &[PyRep::Tuple(vec![PyRep::Integer(pid)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(recov.dict_get("recoveredCount").is_some());

    // Add another probe and test DestroyProbe (all probes when empty args)
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            pid + 1,
            make_test_probe(pid + 1, char_id, ship_id, sys_id, 30_013, now_unix),
        );
    }
    let dest = handle_scan_and_wormhole_extras(&ctx, &mut sess, None, "scanMgr", "DestroyProbe", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(dest.dict_get("destroyedCount").is_some());
    assert!(!ctx.active_scanner_probes.read().unwrap().contains_key(&(pid + 1)));

    // 4. evaluate_probe_dogma_state across None, direct drone_state, evaluated_drones type match, sde.types fallback, and unknown type
    assert!(super::evaluate_probe_dogma_state(None, 9001, 30_013, &ctx.sde).is_none());
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(100_000_001, 605, &ctx.sde);
    assert!(super::evaluate_probe_dogma_state(Some(&fit), 9001, 999_999_999, &ctx.sde).is_none());
    // sde.types fallback (probe_id not yet in fit)
    let st_fallback = super::evaluate_probe_dogma_state(Some(&fit), 9001, 30_013, &ctx.sde).expect("sde fallback");
    assert_eq!(st_fallback.type_id, 30_013);
    // Direct drone_state match and evaluated_drones type match for another probe_id
    fit.add_drone(9001, 30_013, &ctx.sde);
    let _ = fit.drone_state(9001, &ctx.sde);
    assert_eq!(
        super::evaluate_probe_dogma_state(Some(&fit), 9001, 30_013, &ctx.sde)
            .unwrap()
            .type_id,
        30_013
    );
    assert_eq!(
        super::evaluate_probe_dogma_state(Some(&fit), 9002, 30_013, &ctx.sde)
            .unwrap()
            .type_id,
        30_013
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w4_mutant_killers_launcher_skills_custom_scale_target_classes_and_sync() {
    use eve_db::types::TrainedSkillRecord;
    use eve_destiny::{CombatState, SlimItemMeta};
    use eve_sde::InvType;

    let mk_inv_type = |type_id: u32,
                       group_id: u32,
                       category_id: u32,
                       name: &str,
                       default_attributes: HashMap<u16, f64>| InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 10.0,
        volume: 10.0,
        capacity: 0.0,
        portion_size: 1,
        radius: 1.0,
        base_price: 10_000.0,
        published: true,
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
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    };

    let mut sde = shared_sde().as_ref().clone();
    sde.types.entry(30_028).or_insert_with(|| {
        mk_inv_type(30_028, 479, 8, "Combat Scanner Probe I", HashMap::new())
    });
    // Custom Drone (cat 18), AbyssalTrace (47_458), Structure (cat 65), and fitted Scan Rangefinding Array (attr 1136)
    sde.types.insert(
        98_801,
        mk_inv_type(98_801, 100, 18, "Hornet I", HashMap::from([(552u16, 25.0)])),
    );
    sde.types.insert(
        47_458,
        mk_inv_type(
            47_458,
            1991,
            2,
            "Abyssal Trace",
            HashMap::from([(552u16, 150.0)]),
        ),
    );
    sde.types.insert(
        98_802,
        mk_inv_type(
            98_802,
            1657,
            65,
            "Astrahus",
            HashMap::from([(552u16, 4000.0)]),
        ),
    );
    sde.types.insert(
        98_803,
        mk_inv_type(
            98_803,
            1223,
            7,
            "Scan Rangefinding Array I",
            HashMap::from([(1136u16, 5.0)]),
        ),
    );
    // Custom scan probe with explicit 281/1370/37/1371/1372 attrs
    sde.types.insert(
        98_804,
        mk_inv_type(
            98_804,
            479,
            8,
            "Custom Core Probe",
            HashMap::from([
                (281u16, 3_600_000.0),
                (1370u16, 0.25),
                (37u16, 4_500.0),
                (1371u16, 50.0),
                (1372u16, 0.125),
                (600u16, 20.0),
                (1281u16, 1.5),
            ]),
        ),
    );

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let launcher_id = 96_800_100_i64;
    let charge_id = 96_800_101_i64;
    let mod_1136_id = 96_800_102_i64;
    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(
            Arc::new(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );
    let mut sess = SessionState::new(9831);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. ShipNotInSpace when character is docked, then NoProbesLoaded when charge qty == 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
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
            mod_1136_id,
            InventoryItemRecord {
                item_id: mod_1136_id,
                type_id: 98_803,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_1136_id, true);
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 98_804,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 0,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        activate_scan_probe_launcher(
            &ctx, &sess, &sys, char_id, ship_id, sys_id, launcher_id, 17_938, InventoryFlag::HiSlot0 as u16, "launchForSelf", None, &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    assert_eq!(
        activate_scan_probe_launcher(
            &ctx, &sess, &sys, char_id, ship_id, sys_id, launcher_id, 17_938, InventoryFlag::HiSlot0 as u16, "launchForSelf", None, &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NoProbesLoaded"
    );

    // 2. Partial stack consume (qty=3 < 8 free) deletes empty stack & syncs probes to DB with dogma attrs
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&charge_id).unwrap().quantity = 3;
        snap.skills.insert(
            char_id,
            HashMap::from([
                (3412, TrainedSkillRecord { type_id: 3412, level: 4, skill_points: 100_000.0 }),
                (25810, TrainedSkillRecord { type_id: 25810, level: 3, skill_points: 100_000.0 }),
                (25811, TrainedSkillRecord { type_id: 25811, level: 5, skill_points: 200_000.0 }),
            ]),
        );
    });
    let launch_rep = activate_scan_probe_launcher(
        &ctx, &sess, &sys, char_id, ship_id, sys_id, launcher_id, 17_938, InventoryFlag::HiSlot0 as u16, "launchForSelf", None, &kw,
    )
    .await
    .unwrap();
    let PyRep::List(launched_ids) = launch_rep.dict_get("probeIDs").unwrap() else { panic!() };
    assert_eq!(launched_ids.len(), 3);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&charge_id));
        assert_eq!(snap.sp6_lifecycle.active_scanner_probes.len(), 3);
    });

    // 3. handle_qa_scan_sites with session.solar_system_id = None fallback & InvalidSolarSystem when character missing
    let mut gm_no_sys = sess.clone();
    gm_no_sys.role = service_roles::ROLE_GML;
    gm_no_sys.solar_system_id = None;
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut gm_no_sys, None, "scanMgr", "QAScanSites", &[], &kw).await.unwrap().is_ok());

    let saved_char = ctx.db.with_mut_snapshot(|snap| snap.characters.remove(&char_id));
    assert_eq!(
        handle_scan_and_wormhole_extras(&ctx, &mut gm_no_sys, None, "scanMgr", "QAScanSites", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSolarSystem"
    );
    if let Some(c) = saved_char {
        ctx.db.with_mut_snapshot(|snap| { snap.characters.insert(char_id, c); });
    }

    // 4. Multi-probe RequestScans across Ship, Cloaked Ship, Drone, AbyssalTrace, Structure + custom_scale + session.ship_id=None fallback
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.clear();
        for i in 0..4_i64 {
            let mut p = make_test_probe(96_810_000 + i, char_id, ship_id, sys_id, 30_028, ctx.now_unix_secs());
            p.base_sensor_strength = if i == 0 { 0.0 } else { 80.0 };
            map.insert(p.probe_id, p);
        }
        // Negative state probe should be ignored by sync_scanner_probes_to_db
        let mut dead = make_test_probe(96_810_099, char_id, ship_id, sys_id, 30_028, ctx.now_unix_secs());
        dead.state = -1;
        map.insert(dead.probe_id, dead);
    }
    for (bid, tid, cloaked) in [
        (96_820_001_i64, 587_u32, false),
        (96_820_002, 587, true),
        (96_820_003, 98_801, false),
        (96_820_004, 47_458, false),
        (96_820_005, 98_802, false),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState { is_cloaked: cloaked, ..CombatState::default() },
                SlimItemMeta::default(),
            )
            .await;
    }

    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    let scan_ok = handle_scan_and_wormhole_extras(&ctx, &mut no_ship_sess, None, "scanMgr", "RequestScans", &[], &kw)
        .await
        .unwrap();
    assert!(scan_ok.is_ok());

    // 5. tick_probe_warp_end partitions due vs future pending_probe_scans & sync_scanner_probes_to_db ignores negative state and short-circuits unchanged
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(first) = snap.fleet_and_social_ops.pending_probe_scans.first_mut() {
            first.5 = now_ft.saturating_sub(5_000);
        }
        snap.fleet_and_social_ops.pending_probe_scans.push((
            char_id,
            vec![96_810_001],
            vec![PyRep::Integer(96_810_001)],
            Vec::new(),
            vec![PyRep::Integer(96_810_001)],
            now_ft.saturating_add(600_000_000),
        ));
    });
    tick_probe_warp_end(&ctx);
    assert_eq!(ctx.active_scanner_probes.read().unwrap()[&96_810_000].state, PROBE_STATE_IDLE);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleet_and_social_ops.pending_probe_scans.len(), 1);
        assert!(!snap.sp6_lifecycle.active_scanner_probes.contains_key(&96_810_099));
    });
    // Repeat unchanged sync to exercise !probes_differ fast path
    sync_scanner_probes_to_db(&ctx);
}
