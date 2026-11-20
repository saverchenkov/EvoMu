use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{CosmicAnomalyRecord, ServerClock, UpwellStructureRecord};

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
    session.ship_id = Some(500_000_001);
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

#[tokio::test]
async fn scanning_and_wormhole_read_purity_and_docked_dscan_guard() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_099_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let err_dscan_docked = "CannotDirectionalScanWhileDocked";

    let mut docked = make_session(char_id, sys_id);
    docked.station_id = Some(60_003_760);
    docked.ship_id = Some(200_000_099);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    for meth in ["ConeScan", "DirectionalScan"] {
        let err = handle_scanning_and_wormholes(&ctx, &mut docked, "scanMgr", meth, &[], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, err_dscan_docked);
    }
    for wh_arg in [PyRep::String("NO-SUCH-WH".into()), PyRep::Integer(40_000_999)] {
        let err = handle_scanning_and_wormholes(&ctx, &mut docked, "wormholeMgr", "GetWormholePolarization", &[wh_arg], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "WormholeNotFound");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Pre-seeded wormhole polarization lookup is strictly read-pure
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(eve_db::CosmicSignatureRecord {
            signature_id: "WST-PURE-1".into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            signal_strength: 1.0,
            position: [0.0; 3],
            wormhole_type_id: Some(30_001),
            destination_system_id: Some(31_000_005),
            created_unix: 1_700_000_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 2.0e9,
            remaining_mass_kg: 2.0e9,
            max_jump_mass_kg: 3.0e8,
            collapsed: false,
        });
    });
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();
    let pol = handle_scanning_and_wormholes(
        &ctx,
        &mut docked,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String("WST-PURE-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(pol, PyRep::None);
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);

    // Repeat GetSignatures and repeat SignalTrackerRegister do not mutate DB
    let mut sess = make_session(char_id, sys_id);
    assert!(handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[], &kw).await.unwrap().is_ok());
    let e2 = ctx.db.state_epoch();
    let c2 = ctx.db.tx_commit_count();
    assert!(handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw).await.unwrap().is_ok());
    assert!(handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[], &kw).await.unwrap().is_ok());
    assert_eq!(ctx.db.state_epoch(), e2);
    assert_eq!(ctx.db.tx_commit_count(), c2);

    // Undocked without ship -> NoActiveShip for D-scan
    let dscan_err = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "DirectionalScan", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(dscan_err.code, "NoActiveShip");
}

#[tokio::test]
async fn scan_mgr_launch_probes_request_scans_and_dispatch_cone_scan_guards() {
    let err_not_in_space = "NotInSpace";
    let err_no_solar_system = "NoSolarSystem";
    let err_no_active_ship = "NoActiveShip";
    let err_docked_dscan = "CannotDirectionalScanWhileDocked";
    let svc_scan_mgr = "scanMgr";
    let m_launch_probes = "LaunchProbes";
    let m_cone_scan = "ConeScan";
    let m_request_scans = "RequestScans";

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_099;
    let sys_id = 30_000_142;
    let kw = HashMap::new();
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // LaunchProbes while docked -> NotInSpace, zero probe or DB mutations
    let mut docked = make_session(char_id, sys_id);
    docked.station_id = Some(60_003_760);
    docked.ship_id = Some(500_000_099);
    let err = handle_scanning_and_wormholes(
        &ctx,
        &mut docked,
        svc_scan_mgr,
        m_launch_probes,
        &[PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, err_not_in_space);

    // LaunchProbes without solar system -> NoSolarSystem
    let mut no_sys = make_session(char_id, sys_id);
    no_sys.solar_system_id = None;
    no_sys.ship_id = Some(500_000_099);
    let err = handle_scanning_and_wormholes(
        &ctx,
        &mut no_sys,
        svc_scan_mgr,
        m_launch_probes,
        &[PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, err_no_solar_system);

    // LaunchProbes undocked without ship -> NoActiveShip
    let mut shipless = make_session(char_id, sys_id);
    shipless.ship_id = None;
    let err = handle_scanning_and_wormholes(
        &ctx,
        &mut shipless,
        svc_scan_mgr,
        m_launch_probes,
        &[PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, err_no_active_ship);
    assert!(ctx.social_extras.lock().unwrap().active_probes.get(&char_id).is_none());
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Real dispatch_rpc unbound ConeScan: docked -> CannotDirectionalScanWhileDocked; shipless -> NoActiveShip (0 DB mutations).
    let cone_args = [PyRep::Real(360.0), PyRep::Real(14.4 * 149_597_870_700.0)];
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();
    let err = ctx
        .dispatch_rpc(&mut docked, svc_scan_mgr, None, m_cone_scan, &cone_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_docked_dscan);

    let err = ctx
        .dispatch_rpc(&mut shipless, svc_scan_mgr, None, m_cone_scan, &cone_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_no_active_ship);
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);

    // Real dispatch_rpc unbound RequestScans: docked -> NotInSpace; shipless -> NoActiveShip.
    // Note: `services::mining_sites::handle` in `crates/eve-server/src/router/dispatch.rs`
    // intercepts RequestScans ahead of space_probe_scanning to call seed_system_mining_sites_from_sde once per system;
    // verify repeat docked & shipless calls stay read-pure afterward.
    let scan_args = [PyRep::Real(4.0 * 149_597_870_700.0), PyRep::Real(40.0)];
    let err = ctx
        .dispatch_rpc(&mut docked, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_not_in_space);

    let e2 = ctx.db.state_epoch();
    let c2 = ctx.db.tx_commit_count();
    let err = ctx
        .dispatch_rpc(&mut docked, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_not_in_space);

    let err = ctx
        .dispatch_rpc(&mut shipless, svc_scan_mgr, None, m_request_scans, &scan_args, &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, err_no_active_ship);

    assert_eq!(ctx.db.state_epoch(), e2);
    assert_eq!(ctx.db.tx_commit_count(), c2);
}

#[tokio::test]
async fn scan_mgr_get_signatures_expired_wormhole_runs_lifecycle_cleanup_in_seeded_system() {
    let base_ts = 1_700_000_000_u64;
    let (ctx, clock) = test_ctx_and_clock(base_ts);
    let char_id = 100_000_143_i64;
    let dest_char_id = 100_000_144_i64;
    let sys_id = 31_000_005_u32; // Thera: always seeds static THS-*/TLS-*/TNS-*/TWS-* + site signatures
    let mut sess = make_session(char_id, sys_id);
    let kw = HashMap::new();

    // 1. Seed system signatures and wormhole balls via GetSignatures.
    let seeded_rep = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw)
        .await
        .expect("handled")
        .expect("initial GetSignatures succeeds");
    assert!(matches!(seeded_rep, PyRep::Rowset { .. }));

    let orig_wh = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&sys_id)
            .and_then(|sigs| {
                sigs.iter()
                    .find(|s| s.sig_group == "Wormhole" && s.signature_id.starts_with("THS-") && !s.collapsed)
                    .cloned()
            })
            .expect("Thera static THS-* wormhole seeded")
    });
    let orig_sig_id = orig_wh.signature_id.clone();
    let dest_sys = orig_wh.destination_system_id.expect("THS-* has destination system");
    assert_ne!(dest_sys, sys_id);

    let ent_ball_id = crate::services::space_destiny::wormhole_item_id_for_signature(sys_id, &orig_sig_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle.get_ball_snapshot(ent_ball_id).await.ok().flatten().is_some(),
        "seeded wormhole entrance ball must exist prior to expiry"
    );

    // 2. Materialize reciprocal K162-* mirror & exit ball in destination system.
    let exit_ball_id = ctx.db.with_mut_snapshot(|snap| {
        crate::services::wormhole_lifecycle_ops::materialize_k162_exit_in_snap(
            &ctx, snap, &orig_wh, dest_sys,
        )
        .expect("materialize K162 exit mirror")
        .0
    });
    assert_eq!(
        exit_ball_id,
        crate::services::wormhole_lifecycle_ops::wormhole_exit_item_id_for_signature(dest_sys, &orig_sig_id)
    );
    let _ = ensure_system_signatures(&ctx, sys_id).await;
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    assert!(
        dest_handle.get_ball_snapshot(exit_ball_id).await.ok().flatten().is_some(),
        "materialized K162 exit ball must exist prior to expiry"
    );

    // Shorten only orig_wh lifetime so it expires while all other native signatures stay active
    // (confirming the system is already seeded when GetSignatures runs).
    ctx.db.provision_gm_account("wh_origin_1430", "pw", char_id, "WhOrigin");
    ctx.db.provision_gm_account("wh_dest_1430", "pw", dest_char_id, "WhDest");
    let (origin_uid, dest_uid) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(sig) = snap
            .cosmic_signatures
            .get_mut(&sys_id)
            .and_then(|l| l.iter_mut().find(|s| s.signature_id == orig_sig_id))
        {
            sig.lifetime_seconds = 3_600;
        }
        let o_uid = {
            let ch = snap.characters.get_mut(&char_id).expect("origin char provisioned");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.user_id
        };
        let d_uid = {
            let ch = snap.characters.get_mut(&dest_char_id).expect("dest char provisioned");
            ch.solar_system_id = dest_sys;
            ch.station_id = None;
            ch.user_id
        };
        (o_uid, d_uid)
    });
    ctx.db
        .acquire_online_lock(origin_uid, 14301, Some(char_id), true, base_ts)
        .expect("origin online lock");
    ctx.db
        .acquire_online_lock(dest_uid, 14302, Some(dest_char_id), true, base_ts)
        .expect("dest online lock");
    sess.user_id = Some(origin_uid);

    register_signal_tracker_subscriber(&ctx, sys_id, char_id);
    register_signal_tracker_subscriber(&ctx, dest_sys, dest_char_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(dest_char_id);

    // 3. Advance only the virtual clock so GetSignatures itself must run expiry lifecycle cleanup.
    clock.advance_secs(3_600);
    let rowset = handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw)
        .await
        .expect("handled")
        .expect("post-expiry GetSignatures succeeds");
    let PyRep::Rowset { rows, .. } = rowset else {
        panic!("expected Rowset from GetSignatures, got {rowset:?}");
    };
    let returned_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        !returned_ids.contains(&orig_sig_id),
        "expired wormhole {orig_sig_id} must not be returned"
    );
    let rep_sig_id = returned_ids
        .iter()
        .find(|id| id.starts_with("THS-") && **id != orig_sig_id)
        .cloned()
        .expect("static replacement THS-* wormhole must be spawned and returned");

    // 4. Verify entrance & exit balls despawned, K162 mirror collapsed, and replacement ball spawned.
    assert!(
        sys_handle.get_ball_snapshot(ent_ball_id).await.ok().flatten().is_none(),
        "expired wormhole entrance ball must be despawned"
    );
    assert!(
        dest_handle.get_ball_snapshot(exit_ball_id).await.ok().flatten().is_none(),
        "expired wormhole exit ball must be despawned"
    );
    let k162_id = format!("K162-{orig_sig_id}");
    ctx.db.with_snapshot(|snap| {
        let mirror = snap
            .cosmic_signatures
            .get(&dest_sys)
            .and_then(|l| l.iter().find(|s| s.signature_id == k162_id))
            .expect("K162 mirror record present in dest_sys");
        assert!(mirror.collapsed, "reciprocal {k162_id} mirror must be collapsed");
        let rep = snap
            .cosmic_signatures
            .get(&sys_id)
            .and_then(|l| l.iter().find(|s| s.signature_id == rep_sig_id))
            .expect("replacement static wormhole persisted");
        assert!(!rep.collapsed && rep.sig_group == "Wormhole");
    });
    let rep_ball_id = crate::services::space_destiny::wormhole_item_id_for_signature(sys_id, &rep_sig_id);
    assert!(
        sys_handle.get_ball_snapshot(rep_ball_id).await.ok().flatten().is_some(),
        "replacement static wormhole entrance ball must be spawned"
    );

    // 5. Verify signal-tracker deltas emitted for origin expiry/replacement and destination K162 collapse.
    let origin_notifs = ctx.drain_notifications_for_char(char_id);
    let origin_upd = origin_notifs
        .iter()
        .find(|n| n.event == "OnSignalTrackerSignatureUpdate")
        .expect("origin OnSignalTrackerSignatureUpdate emitted");
    let PyRep::Dict(ref added_entries) = origin_upd.args[1] else {
        panic!("expected added Dict, got {:?}", origin_upd.args[1]);
    };
    let PyRep::List(ref removed_entries) = origin_upd.args[2] else {
        panic!("expected removed List, got {:?}", origin_upd.args[2]);
    };
    assert!(
        added_entries.iter().any(|(k, _)| k.as_str() == Some(rep_sig_id.as_str())),
        "origin delta must add replacement {rep_sig_id}"
    );
    assert!(
        removed_entries.iter().any(|v| v.as_str() == Some(orig_sig_id.as_str())),
        "origin delta must remove expired {orig_sig_id}"
    );

    let dest_notifs = ctx.drain_notifications_for_char(dest_char_id);
    let dest_upd = dest_notifs
        .iter()
        .find(|n| n.event == "OnSignalTrackerSignatureUpdate")
        .expect("destination OnSignalTrackerSignatureUpdate emitted for K162 collapse");
    let PyRep::List(ref dest_removed) = dest_upd.args[2] else {
        panic!("expected removed List, got {:?}", dest_upd.args[2]);
    };
    assert!(
        dest_removed.iter().any(|v| v.as_str() == Some(k162_id.as_str())),
        "destination delta must remove collapsed mirror {k162_id}"
    );

    // 6. Immediate follow-up GetSignatures stays read-pure (0 DB mutations).
    let e_post = ctx.db.state_epoch();
    let c_post = ctx.db.tx_commit_count();
    assert!(
        handle_scanning_and_wormholes(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw)
            .await
            .expect("handled")
            .is_ok()
    );
    assert_eq!(ctx.db.state_epoch(), e_post);
    assert_eq!(ctx.db.tx_commit_count(), c_post);
}
