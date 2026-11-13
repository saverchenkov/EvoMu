use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{CosmicAnomalyRecord, UpwellStructureRecord};

fn make_session(char_id: i64, sys_id: u32) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ..Default::default()
    }
}

#[tokio::test]
async fn signal_tracker_signatures_and_request_scans() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut session = make_session(char_id, sys_id);
    let kw = HashMap::new();
    let svc = "scanMgr";

    let reg = handle_scanning_and_wormholes(&ctx, &mut session, svc, "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reg.dict_get("registered").and_then(|v| v.as_bool()), Some(true));

    let sigs = handle_scanning_and_wormholes(&ctx, &mut session, svc, "GetSignatures", &[PyRep::Integer(sys_id as i64)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(sigs, PyRep::None));

    assert!(handle_scanning_and_wormholes(&ctx, &mut session, svc, "RequestScans", &[], &kw).await.is_none());

    let scans = handle_scanning_and_wormholes(
        &ctx,
        &mut session,
        svc,
        "RequestScans",
        &[PyRep::List(vec![]), PyRep::Real(AU_IN_METERS), PyRep::Real(50.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(scans, PyRep::None));

    let sig_rec = CosmicSignatureRecord {
        signature_id: "ABC-123".into(),
        solar_system_id: sys_id,
        sig_group: "Relic Site".into(),
        type_name: "Ruined Site".into(),
        signal_strength: 0.5,
        position: [1000.0, 2000.0, 3000.0],
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 1000,
        lifetime_seconds: 3600,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    let kv = build_signal_tracker_signature_kv(&ctx.sde, &sig_rec);
    assert_eq!(kv.dict_get("targetID").and_then(|v| v.as_str()), Some("ABC-123"));

    let anom_rec = CosmicAnomalyRecord {
        anomaly_id: "ANOM-1".into(),
        solar_system_id: sys_id,
        anomaly_type: "Combat".into(),
        name: "Hideaway".into(),
        signal_strength: 1.0,
        position: [10.0, 20.0, 30.0],
        ore_types: Vec::new(),
        remaining_volume: 0.0,
        created_unix: 1000,
        lifetime_seconds: 3600,
        cleared: false,
    };
    let akv = build_signal_tracker_anomaly_kv(&ctx.sde, &anom_rec);
    assert_eq!(akv.dict_get("instanceID").and_then(|v| v.as_str()), Some("ANOM-1"));
    assert!(
        akv.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer dungeonID (#747)"
    );
    assert!(
        akv.dict_get("archetypeID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer archetypeID (#747)"
    );
    assert!(
        akv.dict_get("factionID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer factionID (#747)"
    );
    assert!(
        akv.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer scanStrengthAttribute (#747)"
    );

    let ore_anom = CosmicAnomalyRecord {
        anomaly_id: "DNG-10778-42".into(),
        solar_system_id: sys_id,
        anomaly_type: "OreAnomaly".into(),
        name: "Veldspar Deposit".into(),
        signal_strength: 1.0,
        position: [10.0, 20.0, 30.0],
        ore_types: vec![1230],
        remaining_volume: 1000.0,
        created_unix: 1000,
        lifetime_seconds: 3600,
        cleared: false,
    };
    let ore_kv = build_signal_tracker_anomaly_kv(&ctx.sde, &ore_anom);
    assert_eq!(ore_kv.dict_get("dungeonID").and_then(|v| v.as_i64()), Some(10_778));
    assert_eq!(ore_kv.dict_get("archetypeID").and_then(|v| v.as_i64()), Some(27));
    assert_eq!(ore_kv.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()), Some(211));

    let st_rec = UpwellStructureRecord {
        structure_id: 1_000_000_000_555,
        type_id: 35832,
        owner_corp_id: 1_000_009,
        solar_system_id: sys_id,
        name: "Astrahus".into(),
        state: "ShieldVulnerable".into(),
        position: [0.0, 0.0, 0.0],
        quantum_core_installed: true,
        fuel_blocks: 100,
        ..Default::default()
    };
    let skv = build_signal_tracker_structure_kv(&ctx, &st_rec);
    assert_eq!(skv.dict_get("targetID").and_then(|v| v.as_i64()), Some(1_000_000_000_555));

    emit_signal_tracker_signature_delta(&ctx, sys_id, &[sig_rec], &["OLD-1".into()]);
    emit_signal_tracker_anomaly_delta(&ctx, sys_id, &[anom_rec], &["OLD-A".into()]);
    emit_signal_tracker_structure_delta(&ctx, sys_id, &[st_rec], &[1_000_000_000_444]);
}

#[cfg(test)]
#[path = "fleet_social_scanning_extra_unit_tests.rs"]
mod extra_unit_tests;


#[tokio::test]
async fn scanning_cone_scan_launch_probes_and_signatures_strength_edges() {
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let (ctx, _clock) = test_ctx_and_clock(1_700_001_000);
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Unknown method returns None
    let unknown_op = format!("{}{}", "NoSuch", "ScanOp");
    assert!(handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", &unknown_op, &[], &kw)
        .await
        .is_none());

    // SignalTrackerRegister without solar system -> NoSolarSystem; re-register across systems prunes prior sub key
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut no_sys, "scanMgr", "SignalTrackerRegister", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    register_signal_tracker_subscriber(&ctx, 30_000_143, char_id);
    let _ = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleet_and_social_ops.signal_tracker_subs.contains_key(&30_000_143));
        assert!(snap.fleet_and_social_ops.signal_tracker_subs.contains_key(&30_000_142));
    });

    // LaunchProbes respects Astrometrics skill level (skill 3412 level 1 -> max 5)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3412,
            eve_db::TrainedSkillRecord {
                type_id: 3412,
                level: 1,
                skill_points: 500.0,
            },
        );
    });
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "LaunchProbes", &[PyRep::Integer(6)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MaxProbesExceeded"
    );
    let lp_spread = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "scanMgr",
        "LaunchProbes",
        &[
            PyRep::Integer(4),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)]),
            PyRep::List(vec![PyRep::List(vec![PyRep::Real(10.0), PyRep::Real(0.0), PyRep::Real(0.0)])]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lp_spread.dict_get("formationName").and_then(|v| v.as_str()), Some("Spread"));
    assert_eq!(lp_spread.dict_get("probeCount").and_then(|v| v.as_i64()), Some(4));

    let lp_custom = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "scanMgr",
        "LaunchProbes",
        &[PyRep::Integer(1), PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lp_custom.dict_get("formationName").and_then(|v| v.as_str()), Some("Custom"));

    // RequestScans 3-arg tuple with 1 probe in range + expired signature filtering
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: "GHS-001".into(),
                    solar_system_id: sys_id,
                    sig_group: "Data".into(),
                    type_name: "Superior Covert Research Facility".into(),
                    position: [100.0, 200.0, 300.0],
                    signal_strength: 0.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 3600,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "EXP-999".into(),
                    solar_system_id: sys_id,
                    sig_group: "Relic".into(),
                    type_name: "Ruined".into(),
                    position: [100.0, 200.0, 300.0],
                    signal_strength: 0.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now.saturating_sub(4000),
                    lifetime_seconds: 100,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );
    });
    let scan_res = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[
            PyRep::List(vec![PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])]),
            PyRep::Real(AU_IN_METERS),
            PyRep::Real(100.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let scan_dbg = format!("{scan_res:?}");
    assert!(scan_dbg.contains("GHS-001"));
    assert!(!scan_dbg.contains("EXP-999"));
    ctx.db.with_snapshot(|snap| {
        let g = snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.signature_id == "GHS-001")
            .unwrap();
        assert!(g.signal_strength > 0.0);
    });

    // ConeScan / DirectionalScan: Zarzakh prohibition, 360 vs narrow cone with velocity direction, range filtering
    let mut zarzakh = sess.clone();
    zarzakh.solar_system_id = Some(30_100_000);
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut zarzakh, "scanMgr", "ConeScan", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(100.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.5,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: 200_000_001,
            type_id: 587,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: 200_000_002,
            type_id: 587,
            position: DVec3::new(-50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let dscan_narrow = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "scanMgr",
        "DirectionalScan",
        &[PyRep::Real(30.0), PyRep::Real(100_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let dscan_dbg = format!("{dscan_narrow:?}");
    assert!(dscan_dbg.contains("200000001"));
    assert!(!dscan_dbg.contains("200000002"));
}

#[tokio::test]
async fn wormhole_polarization_acceleration_gate_and_signal_tracker_removal_edges() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let (ctx, _clock) = test_ctx_and_clock(1_700_002_000);
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // keeper.Bind returns None
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "Bind", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // GetWormholePolarization: WormholeNotFound for unknown sig, entrance WH ball ID polarized vs None when expired, direct sys ID
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "K162-WST-101".into(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: "C1".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: Some(31_000_005),
                created_unix: now,
                lifetime_seconds: 7200,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            }],
        );
        let ent_id = crate::services::space_destiny::wormhole_item_id_for_signature(sys_id, "K162-WST-101");
        snap.wh_polarizations.insert((char_id, ent_id), now + 300);
        snap.wh_polarizations.insert((char_id, 31_000_099), now + 200);
    });
    assert_eq!(
        handle_scanning_and_wormholes(
            &ctx,
            &mut sess,
            "wormholeMgr",
            "GetWormholePolarization",
            &[PyRep::String("NOPE-999".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WormholeNotFound"
    );
    let pol_strip = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String("WST-101".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(pol_strip, PyRep::Tuple(_)));

    let pol_sys = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(31_000_099)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(pol_sys, PyRep::Tuple(_)));
    let pol_none = handle_scanning_and_wormholes(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(31_000_111)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(pol_none, PyRep::None);

    // ActivateAccelerationGate error checks: NotInSpace, NoSolarSystem, NoActiveShip, ShipNotInSpace
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut docked, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(5001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut no_ship, "beyonce", "ActivateAccelerationGate", &[PyRep::Integer(5001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(5001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn oversized ship -> ShipTooLargeForGate; spawn normal ship >2500m from gate -> OutsideGateActivationRange
    let sys = ctx.systems.get_or_spawn(sys_id);
    let gate_id = 700_000_001_i64;
    let _ = sys
        .spawn_ball(Ball {
            id: gate_id,
            type_id: 17_831,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 600_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipTooLargeForGate"
    );

    // Move ship within range (9_000 m), mass 20M -> triggers dungeon diff 1 restriction if present, plus missing/consumed key flow
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 20_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideGateActivationRange"
    );

    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(9_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 20_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    if let Some(&did1) = ctx.sde.dungeons.iter().find(|(_, d)| d.difficulty == Some(1)).map(|(id, _)| id) {
        assert_eq!(
            handle_scanning_and_wormholes(
                &ctx,
                &mut sess,
                "keeper",
                "ActivateAccelerationGate",
                &[PyRep::Integer(gate_id), PyRep::Integer(did1 as i64)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "ShipRestrictedByDungeonGate"
        );
    }

    // Reduce ship mass to frigate (<15M), gate requires key type 99_111 consumed on activation
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(9_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gate_id,
            InventoryItemRecord {
                item_id: gate_id,
                type_id: 17_831,
                owner_id: 1_000_009,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(1002, 99_111.0), (1003, 1.0)]),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingGateKey"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_000_002,
            InventoryItemRecord {
                item_id: 700_000_002,
                type_id: 99_111,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let activated = handle_scanning_and_wormholes(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(activated.dict_get("keyConsumed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&700_000_002)));

    // Subscriber filtering + structure & site removal notifications
    register_signal_tracker_subscriber(&ctx, sys_id, char_id);
    let _ = ctx.db.acquire_online_lock(1, 1, Some(char_id), true, now);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a_list) = snap.cosmic_anomalies.get_mut(&sys_id) {
            if let Some(first) = a_list.first_mut() {
                first.cleared = true;
            }
        } else {
            snap.cosmic_anomalies.insert(
                sys_id,
                vec![CosmicAnomalyRecord {
                    anomaly_id: "REM-ANOM-1".into(),
                    solar_system_id: sys_id,
                    name: "Cleared".into(),
                    anomaly_type: "Combat".into(),
                    position: [0.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    created_unix: now,
                    lifetime_seconds: 3600,
                    cleared: true,
                }],
            );
        }
    });
    let removed_notifs = ctx.db.with_snapshot(|snap| {
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys_id), vec![char_id]);
        emit_signal_tracker_structure_removed_with_snap(&ctx, snap, sys_id, 1_000_000_000_555)
    });
    assert_eq!(removed_notifs.len(), 1);
    dispatch_signal_tracker_notifications(&ctx, removed_notifs);
    let cleared_site = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.cleared)
            .map(|a| a.anomaly_id.clone())
            .unwrap()
    });
    emit_signal_tracker_site_removed_with_db(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, &cleared_site);
    let pending = ctx.pending_notifications.read().unwrap();
    let evs: Vec<&str> = pending
        .get(&char_id)
        .map(|v| v.iter().map(|n| n.event.as_str()).collect())
        .unwrap_or_default();
    assert!(evs.contains(&"OnSignalTrackerStructureUpdate"));
    assert!(evs.contains(&"OnSignalTrackerAnomalyUpdate"));
}

#[tokio::test]
async fn signal_tracker_delta_and_get_signatures_exclude_anomaly_colliding_signatures_gh1034() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_119_u32;
    let mut sess = make_session(char_id, sys_id);
    let kw = HashMap::new();

    let make_sig = |id: &str, group: &str| CosmicSignatureRecord {
        signature_id: id.into(),
        solar_system_id: sys_id,
        sig_group: group.into(),
        type_name: group.into(),
        signal_strength: if group == "Data Site" { 0.0 } else { 1.0 },
        position: [1_000.0, 2_000.0, 3_000.0],
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: now,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };

    let user_id = ctx.db.with_mut_snapshot(|snap| {
        let uid = if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.user_id
        } else {
            1
        };
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![CosmicAnomalyRecord {
                anomaly_id: "DNG-1347-1897095014".into(),
                solar_system_id: sys_id,
                anomaly_type: "OreAnomaly".into(),
                name: "Ordinary Moon Ore Deposit".into(),
                signal_strength: 1.0,
                position: [1_000.0, 2_000.0, 3_000.0],
                ore_types: vec![45_490],
                remaining_volume: 100_000.0,
                created_unix: now,
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                make_sig("DNG-1347-1897095014", "OreAnomaly"),
                make_sig("ICE-BELT-1", "IceBelt"),
                make_sig("SOV-ORE-1", "SovOre"),
                make_sig("SIG-REAL-1", "Data Site"),
            ],
        );
        uid
    });
    let _ = ctx.db.acquire_online_lock(user_id, 1, Some(char_id), true, now);
    sess.user_id = Some(user_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let reg = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys_id), vec![char_id]);
    });
    let reg_notifs = ctx.drain_notifications_for_char(char_id);
    let full = reg_notifs
        .iter()
        .find(|n| n.event == "OnSignalTrackerFullState")
        .expect("OnSignalTrackerFullState");
    let PyRep::Tuple(ref parts) = full.args[1] else {
        panic!("expected 4-tuple fullState, got {:?}", full.args[1]);
    };
    let PyRep::Dict(ref anoms) = parts[0] else {
        panic!("expected anoms Dict");
    };
    let PyRep::Dict(ref sigs) = parts[1] else {
        panic!("expected sigs Dict");
    };
    let anom_ids: Vec<&str> = anoms.iter().filter_map(|(k, _)| k.as_str()).collect();
    let sig_ids: Vec<&str> = sigs.iter().filter_map(|(k, _)| k.as_str()).collect();
    assert!(anom_ids.contains(&"DNG-1347-1897095014"));
    assert!(!sig_ids.contains(&"DNG-1347-1897095014"));
    assert!(!sig_ids.contains(&"ICE-BELT-1"));
    assert!(!sig_ids.contains(&"SOV-ORE-1"));
    assert!(sig_ids.contains(&"SIG-REAL-1"));
    assert_eq!(
        reg.dict_get("signatureCount").and_then(|v| v.as_i64()),
        Some(sig_ids.len() as i64)
    );

    // Delta emitter must skip anomaly-colliding signatures and omit empty notifications
    emit_signal_tracker_signature_delta(
        &ctx,
        sys_id,
        &[make_sig("DNG-1347-1897095014", "OreAnomaly"), make_sig("ICE-BELT-1", "IceBelt")],
        &[],
    );
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .all(|n| n.event != "OnSignalTrackerSignatureUpdate"),
        "anomaly-only delta must not emit OnSignalTrackerSignatureUpdate (#1034)"
    );

    emit_signal_tracker_signature_delta(
        &ctx,
        sys_id,
        &[
            make_sig("DNG-1347-1897095014", "OreAnomaly"),
            make_sig("SIG-NEW-1", "Relic Site"),
        ],
        &[],
    );
    let delta_notifs = ctx.drain_notifications_for_char(char_id);
    let sig_upd = delta_notifs
        .iter()
        .find(|n| n.event == "OnSignalTrackerSignatureUpdate")
        .expect("OnSignalTrackerSignatureUpdate");
    let PyRep::Dict(ref added_entries) = sig_upd.args[1] else {
        panic!("expected added Dict, got {:?}", sig_upd.args[1]);
    };
    let added_ids: Vec<&str> = added_entries.iter().filter_map(|(k, _)| k.as_str()).collect();
    assert_eq!(added_ids, vec!["SIG-NEW-1"]);

    // GetSignatures must exclude OreAnomaly/IceBelt/SovOre mirrors while preserving SIG-REAL-1
    let rowset = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows, .. } = rowset else {
        panic!("expected Rowset from GetSignatures, got {rowset:?}");
    };
    let row_ids: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.first().and_then(|v| v.as_str()))
        .collect();
    assert!(row_ids.contains(&"SIG-REAL-1"));
    assert!(!row_ids.contains(&"DNG-1347-1897095014"));
    assert!(!row_ids.contains(&"ICE-BELT-1"));
    assert!(!row_ids.contains(&"SOV-ORE-1"));
}
