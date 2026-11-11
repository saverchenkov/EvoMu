//! Extra unit and mutation-killing tests for `fleet_social_scanning.rs`.

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::fleet_social::fleet_social_scanning::{
    build_signal_tracker_anomaly_kv, build_signal_tracker_signature_kv,
    build_signal_tracker_structure_kv, dispatch_signal_tracker_notifications,
    emit_signal_tracker_anomaly_delta, emit_signal_tracker_signature_delta,
    emit_signal_tracker_site_removed_with_db, emit_signal_tracker_structure_delta,
    emit_signal_tracker_structure_removed_with_snap, register_signal_tracker_subscriber,
    signal_tracker_subscribers_in_snap,
};
use crate::services::fleet_social::{handle, AU_IN_METERS};
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::DungeonTemplate;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_session(char_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

fn make_ball(id: i64, type_id: u32, pos: DVec3, vel: DVec3, mass: f64) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: vel,
        max_velocity: 300.0,
        inertia: 1.0,
        mass,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dscan_cone_angle_range_and_forbidden_systems() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, ship_id) = (90_000_001, 30_000_142, 100_000_001);
    let kw = HashMap::new();

    let mut no_sys = make_session(char_id, None, Some(ship_id));
    let err = handle(&ctx, &mut no_sys, "scanMgr", "ConeScan", &[], &kw).await.unwrap().unwrap_err();
    assert_eq!(err.code, "NoSolarSystem");

    // Forbidden systems: Zarzakh (30_100_000) and Abyssal (32_000_001)
    for bad_sys in [30_100_000_u32, 32_000_001_u32] {
        let mut bad_sess = make_session(char_id, Some(bad_sys), Some(ship_id));
        let err = handle(&ctx, &mut bad_sess, "scanMgr", "DirectionalScan", &[], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "CannotDscanInThisSystem");
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Caller ship moving along +Z axis; Target 1 ahead (+Z), Target 2 behind (-Z)
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO, DVec3::new(0.0, 0.0, 100.0), 1e6), CombatState::default())
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(200_001, 587, DVec3::new(0.0, 0.0, 10_000.0), DVec3::ZERO, 1e6), CombatState::default())
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(200_002, 587, DVec3::new(0.0, 0.0, -10_000.0), DVec3::ZERO, 1e6), CombatState::default())
        .await;

    let mut sess = make_session(char_id, Some(sys_id), Some(ship_id));
    let cone_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(30.0), PyRep::Real(20_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: lines, .. } = cone_res else { panic!("expected rowset") };
    assert_eq!(lines.len(), 1);

    let short_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "DirectionalScan",
        &[PyRep::Real(30.0), PyRep::Real(5_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: short_lines, .. } = short_res else { panic!("expected rowset") };
    assert!(short_lines.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_probes_formations_request_scans_and_wormhole_polarization() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id) = (90_000_001, 30_000_142);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3412,
            TrainedSkillRecord {
                type_id: 3412,
                level: 2, // max_probes = 4 + 2 = 6
                skill_points: 8_000.0,
            },
        );
    });

    let mut sess = make_session(char_id, Some(sys_id), Some(100_000_001));
    let err = handle(&ctx, &mut sess, "scanMgr", "LaunchProbes", &[PyRep::Integer(8)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "MaxProbesExceeded");

    // Launch 4 probes in Spread formation (1) and Custom formation (2) with custom offsets
    let spread = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "LaunchProbes",
        &[
            PyRep::Integer(4),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)]),
            PyRep::List(vec![PyRep::List(vec![PyRep::Real(10.0), PyRep::Real(20.0), PyRep::Real(30.0)])]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(spread.dict_get("formationName").and_then(|v| v.as_str()), Some("Spread"));
    assert_eq!(spread.dict_get("probeCount").and_then(|v| v.as_i64()), Some(4));

    let custom = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "LaunchProbes",
        &[PyRep::Integer(2), PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(custom.dict_get("formationName").and_then(|v| v.as_str()), Some("Custom"));

    // RequestScans using first arg as Real(radius) and active_probes fallback
    let scan_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::Real(2.0 * AU_IN_METERS), PyRep::Real(60.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(scan_res, PyRep::None));

    // Wormhole polarization: unknown string -> WormholeNotFound
    let wh_err = handle(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String("NO-SUCH-WH".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(wh_err.code, "WormholeNotFound");

    let (wh_sig_id, dest_sys) = ("WST-777", 31_000_005_u32);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: wh_sig_id.into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            signal_strength: 1.0,
            position: [0.0, 0.0, 0.0],
            wormhole_type_id: Some(30_001),
            destination_system_id: Some(dest_sys),
            created_unix: 1_700_000_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 2.0e9,
            remaining_mass_kg: 2.0e9,
            max_jump_mass_kg: 3.0e8,
            collapsed: false,
        });
    });

    let unpol = handle(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String(wh_sig_id.into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(unpol, PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.insert((char_id, dest_sys as i64), 1_700_000_300);
    });
    for arg in [PyRep::String(wh_sig_id.into()), PyRep::Integer(dest_sys as i64)] {
        let pol = handle(&ctx, &mut sess, "wormholeMgr", "GetWormholePolarization", &[arg], &kw)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(pol, PyRep::Tuple(ref t) if t.len() == 2));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceleration_gate_restrictions_keys_and_keeper_bind() {
    let (mut ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, ship_id, gate_id, key_type_id) =
        (90_000_001, 30_000_142, 100_000_001, 700_000_001_i64, 24_576_u32);
    let kw = HashMap::new();

    {
        let sde = Arc::make_mut(&mut ctx.sde);
        for (did, diff) in [(9001, 1), (9002, 2)] {
            sde.dungeons.insert(
                did,
                DungeonTemplate {
                    dungeon_id: did,
                    name: format!("Complex {did}"),
                    archetype_id: Some(1),
                    faction_id: None,
                    difficulty: Some(diff),
                },
            );
        }
    }

    let mut sess = make_session(char_id, Some(sys_id), Some(ship_id));
    assert_eq!(handle(&ctx, &mut sess, "keeper", "Bind", &[], &kw).await.unwrap().unwrap(), PyRep::None);

    // Docked -> NotInSpace
    sess.station_id = Some(60_003_760);
    let err = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NotInSpace");
    sess.station_id = None;

    // Ship not spawned -> ShipNotInSpace
    let err = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "ShipNotInSpace");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Capital mass ship -> ShipTooLargeForGate
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO, DVec3::ZERO, 600_000_000.0), CombatState::default())
        .await;
    let err = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "ShipTooLargeForGate");

    // Respawn as 60M kg battleship and gate at 3_000m (> 2_500m) -> OutsideGateActivationRange
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO, DVec3::ZERO, 60_000_000.0), CombatState::default())
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(gate_id, 17_634, DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO, 1e8), CombatState::default())
        .await;
    let err = handle(&ctx, &mut sess, "beyonce", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "OutsideGateActivationRange");

    // Move ship within 1_000m; test dungeon difficulty 1 & 2 restrictions against 60M kg ship
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(2_000.0, 0.0, 0.0)).await;
    for did in [9001_i64, 9002_i64] {
        let err = handle(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(gate_id), PyRep::Integer(did)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "ShipRestrictedByDungeonGate");
    }

    // Configure gate item with required key (1002) and consume_key = 1.0 (1003)
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, 1e6), CombatState::default())
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut gate_item = InventoryItemRecord {
            item_id: gate_id,
            type_id: 17_634,
            owner_id: 1,
            location_id: sys_id as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        gate_item.dynamic_attributes.insert(1001, 9001.0);
        gate_item.dynamic_attributes.insert(1002, key_type_id as f64);
        gate_item.dynamic_attributes.insert(1003, 1.0);
        snap.items.insert(gate_id, gate_item);
    });

    let err = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "MissingGateKey");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_000_099,
            InventoryItemRecord {
                item_id: 700_000_099,
                type_id: key_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let gate_ok = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gate_ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(gate_ok.dict_get("keyConsumed").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&700_000_099)));
}

#[test]
fn signal_tracker_subscriber_and_site_removed_helpers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys1, sys2) = (90_000_001, 30_000_142_u32, 30_000_144_u32);

    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().user_id);
    ctx.db.acquire_online_lock(uid, 501, Some(char_id), true, 1_700_000_000).unwrap();

    register_signal_tracker_subscriber(&ctx, sys1, char_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys1), vec![char_id]);
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys2;
        snap.cosmic_anomalies.entry(sys2).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "CLEARED-1".into(),
            solar_system_id: sys2,
            name: "Cleared Site".into(),
            anomaly_type: "Combat".into(),
            position: [0.0; 3],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: 1_000,
            lifetime_seconds: 3_600,
            cleared: true,
        });
        snap.cosmic_signatures.entry(sys2).or_default().push(CosmicSignatureRecord {
            signature_id: "CLEARED-1".into(),
            solar_system_id: sys2,
            sig_group: "Combat".into(),
            type_name: "Cleared Site".into(),
            signal_strength: 1.0,
            position: [0.0; 3],
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1_000,
            lifetime_seconds: 3_600,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: true,
        });
    });
    register_signal_tracker_subscriber(&ctx, sys2, char_id);
    ctx.db.with_snapshot(|snap| {
        assert!(signal_tracker_subscribers_in_snap(snap, sys1).is_empty());
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys2), vec![char_id]);
    });

    let notifs = ctx.db.with_snapshot(|snap| {
        emit_signal_tracker_structure_removed_with_snap(&ctx, snap, sys2, 1_000_000_000_999)
    });
    assert_eq!(notifs.len(), 1);
    dispatch_signal_tracker_notifications(&ctx, notifs);
    emit_signal_tracker_site_removed_with_db(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, "CLEARED-1");

    let drained = ctx.drain_notifications_for_char(char_id);
    assert!(drained.iter().any(|n| n.event == "OnSignalTrackerStructureUpdate"));
    assert!(drained.iter().any(|n| n.event == "OnSignalTrackerAnomalyUpdate"));
    assert!(drained.iter().any(|n| n.event == "OnSignalTrackerSignatureUpdate"));

    let mut sig = CosmicSignatureRecord {
        signature_id: "DEV-1".into(),
        solar_system_id: sys2,
        sig_group: "Data Site".into(),
        type_name: "Central Bank".into(),
        signal_strength: 1.0,
        position: [10.0, 20.0, 30.0],
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 1_000,
        lifetime_seconds: 3_600,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    assert_eq!(build_signal_tracker_signature_kv(&ctx.sde, &sig).dict_get("deviation").and_then(|v| v.as_f64()), Some(0.0));
    sig.signal_strength = 0.2;
    assert!(build_signal_tracker_signature_kv(&ctx.sde, &sig).dict_get("deviation").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    let anom = CosmicAnomalyRecord {
        anomaly_id: "A-2".into(),
        solar_system_id: sys2,
        name: "Den".into(),
        anomaly_type: "Combat".into(),
        position: [0.0; 3],
        signal_strength: 1.0,
        ore_types: vec![],
        remaining_volume: 0.0,
        created_unix: 1_000,
        lifetime_seconds: 3_600,
        cleared: false,
    };
    let st = UpwellStructureRecord {
        structure_id: 1_000_000_000_123,
        type_id: 999_999_999,
        owner_corp_id: 1_000_009,
        solar_system_id: sys2,
        name: "Custom St".into(),
        ..Default::default()
    };
    let st_kv = build_signal_tracker_structure_kv(&ctx, &st);
    assert_eq!(st_kv.dict_get("groupID").and_then(|v| v.as_i64()), Some(1657));
    assert_eq!(st_kv.dict_get("categoryID").and_then(|v| v.as_i64()), Some(65));
    let anom_kv = build_signal_tracker_anomaly_kv(&ctx.sde, &anom);
    assert!(anom_kv.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0));
    assert!(anom_kv.dict_get("archetypeID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0));
    assert!(anom_kv.dict_get("factionID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0));
    assert!(anom_kv.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()).is_some_and(|id| id > 0));

    emit_signal_tracker_signature_delta(&ctx, sys2, &[sig], &[]);
    emit_signal_tracker_anomaly_delta(&ctx, sys2, &[anom], &[]);
    emit_signal_tracker_structure_delta(&ctx, sys2, &[st], &[]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coverage_sweep_scanning_probes_dscan_wh_gates_and_delta_edge_cases() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sys_id: u32 = 30_000_142;
    let other_sys_id: u32 = 30_000_144;
    let char_id: i64 = 90_555_001;
    let ship_id: i64 = 88_555_001;
    let kw = HashMap::new();

    // Unauthenticated & NoSolarSystem checks (L28, L32, L179)
    let mut bad_sess = SessionState::new(999);
    assert!(handle(&ctx, &mut bad_sess, "scanMgr", "GetSignatures", &[], &kw).await.unwrap().is_err());
    let mut nosys_sess = make_session(char_id, None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut nosys_sess, "scanMgr", "SignalTrackerRegister", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    assert_eq!(
        handle(&ctx, &mut nosys_sess, "scanMgr", "RequestScans", &[PyRep::None, PyRep::None], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // RequestScans with empty/real single arg delegates to space_destiny (returns None), 2 args auto-populates signatures
    let mut sess = make_session(char_id, Some(30_000_003), Some(ship_id));
    assert!(handle(&ctx, &mut sess, "scanMgr", "RequestScans", &[], &kw).await.is_none());
    assert!(
        handle(&ctx, &mut sess, "scanMgr", "RequestScans", &[PyRep::None, PyRep::None], &kw)
            .await
            .unwrap()
            .is_ok()
    );

    // Seed structures + cosmic signatures (including Ghost & Wormhole) + exploration_sites
    sess.solar_system_id = Some(sys_id);
    let st_rec = UpwellStructureRecord {
        structure_id: 1_000_000_888_001,
        type_id: 35832,
        owner_corp_id: 1_000_009,
        solar_system_id: sys_id,
        name: "Live Astrahus".into(),
        state: "ShieldVulnerable".into(),
        ..Default::default()
    };
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_rec.structure_id, st_rec.clone());
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: "SIG-G1".into(),
                    solar_system_id: sys_id,
                    sig_group: "Data".into(),
                    type_name: "Lesser Sansha Covert Research Facility".into(),
                    signal_strength: 0.10,
                    position: [100.0, 0.0, 0.0],
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_u,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "WH-LOC".into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "Wormhole K162".into(),
                    signal_strength: 1.0,
                    position: [1_000.0, 0.0, 0.0],
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
        snap.cosmic_signatures.insert(
            other_sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "WH-REM".into(),
                solar_system_id: other_sys_id,
                sig_group: "Wormhole".into(),
                type_name: "Wormhole B274".into(),
                signal_strength: 1.0,
                position: [2_000.0, 0.0, 0.0],
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now_u,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            }],
        );
    });
    // Seed Ghost site in pve_exploration_reg
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "SIG-G1".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: [100.0, 0.0, 0.0],
                signal_strength: 0.1,
                ..Default::default()
            },
        );
    });

    // SignalTrackerRegister picks up structures (L67-76)
    let reg_res = handle(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(format!("{reg_res:?}").contains("Live Astrahus"));

    // RequestScans with 1 probe (0.05 mult), 2 probes (0.20 mult), then 4 probes (0.50 mult, resolving Ghost site L320-340)
    let p1 = PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let _ = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::List(vec![p1.clone()]), PyRep::Real(0.25)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::List(vec![p1.clone(), p1.clone()]), PyRep::Real(0.25)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    // Boost strength to 0.99 so 4 probes resolves >= 1.0 and triggers Ghost timer + tname
    ctx.db.with_mut_snapshot(|s| {
        if let Some(list) = s.cosmic_signatures.get_mut(&sys_id) {
            list[0].signal_strength = 0.99;
        }
    });
    let scan4 = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[
            PyRep::List(vec![p1.clone(), p1.clone(), p1.clone(), p1]),
            PyRep::Real(0.25),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(format!("{scan4:?}").contains("Lesser Sansha Covert Research Facility"));

    // LaunchProbes with unknown formation -> default Pinpoint arm + fallback center (L491, L501)
    let lp = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "LaunchProbes",
        &[PyRep::String("CustomUnknownFormation".into()), PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lp.dict_get("formationName"), Some(&PyRep::String("Pinpoint".into())));

    // ConeScan: ship + distant covert beacon in another bubble (L413, L431-434)
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = make_ball(ship_id, 587, DVec3::ZERO, DVec3::ZERO, 1_000_000.0);
    sys_h.spawn_ball(ship_ball).await.unwrap();
    let covert_id = 88_555_002_i64;
    let covert_ball = make_ball(
        covert_id,
        crate::services::mobile_deployables::TYPE_COVERT_MOBILE_CYNO_BEACON,
        DVec3::new(1_000_000_000.0, 0.0, 0.0),
        DVec3::ZERO,
        1_000.0,
    );
    sys_h.spawn_ball(covert_ball).await.unwrap();
    let dscan_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(2_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!format!("{dscan_res:?}").contains("88555002"));

    // GetWormholePolarization: local WH ball ID & remote WH ball ID, returning None (L603-636, L682)
    let wh_loc_bid = crate::services::space_destiny::wormhole_item_id_for_signature(sys_id, "WH-LOC");
    let wh_rem_bid = crate::services::space_destiny::wormhole_item_id_for_signature(other_sys_id, "WH-REM");
    assert_eq!(
        handle(&ctx, &mut sess, "wormholeMgr", "GetWormholePolarization", &[PyRep::Integer(wh_loc_bid)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    sess.solar_system_id = Some(other_sys_id);
    assert_eq!(
        handle(&ctx, &mut sess, "wormholeMgr", "GetWormholePolarization", &[PyRep::Integer(wh_rem_bid)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    sess.solar_system_id = Some(sys_id);

    // ActivateAccelerationGate missing solar system / missing ship / FW complex / insurgency raid (L699-729)
    assert_eq!(
        handle(&ctx, &mut nosys_sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut noship_sess = make_session(char_id, Some(sys_id), None);
    assert_eq!(
        handle(&ctx, &mut noship_sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // Collapsed signature site removed (L1220-1266): both empty subscribers and saturated queue
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            30_000_143,
            vec![CosmicSignatureRecord {
                signature_id: "COLLAPSED-1".into(),
                solar_system_id: 30_000_143,
                sig_group: "Wormhole".into(),
                type_name: "WH".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 1_000,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: true,
            }],
        );
    });
    // Empty subscribers (L1240)
    emit_signal_tracker_site_removed_with_db(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, "COLLAPSED-1");
    // Now subscribe char_id to 30_000_143 and fill notification queue to cap (L1266)
    register_signal_tracker_subscriber(&ctx, 30_000_143, char_id);
    {
        let mut qmap = ctx.pending_notifications.write().unwrap();
        let q = qmap.entry(char_id).or_default();
        for i in 0..crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            let (pkt, mpkt) = crate::router::ServerContext::build_macho_notification_packet(
                char_id,
                Some(1),
                "Dummy",
                "charid",
                vec![PyRep::Integer(i as i64)],
            );
            q.push(crate::router::cache_notifications::PendingMachoNotification { seq_id: 0,
                recipient_char_id: char_id,
                event: "Dummy".into(),
                id_type: "charid".into(),
                args: vec![],
                packet: pkt,
                marshaled_packet: mpkt,
                created_filetime: 0,
            });
        }
    }
    emit_signal_tracker_site_removed_with_db(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, "COLLAPSED-1");

    // Empty deltas return early (L1132, L1169)
    emit_signal_tracker_signature_delta(&ctx, sys_id, &[], &[]);
    emit_signal_tracker_anomaly_delta(&ctx, sys_id, &[], &[]);
    emit_signal_tracker_structure_delta(&ctx, sys_id, &[], &[]);
}

#[tokio::test]
async fn gate_cloak_break_subscriber_retention_and_anomaly_dungeon_metadata_matrix() {
    use crate::services::fleet_social::fleet_social_scanning::resolve_anomaly_dungeon_metadata;

    let (mut ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_a, char_b, sys1, sys2, ship_id, gate_id) = (
        90_200_001_i64,
        90_200_002_i64,
        30_000_142_u32,
        30_000_143_u32,
        100_200_001_i64,
        700_200_001_i64,
    );
    let kw = HashMap::new();

    // 1. ActivateAccelerationGate breaks gate cloak when !validated_gate.is_covert_cloaked (L877)
    let sys_h = ctx.systems.get_or_spawn(sys1);
    sys_h
        .spawn_ball_with_combat(
            make_ball(ship_id, 606, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState {
                gate_cloak_remaining_ticks: 5,
                undock_invulnerable_remaining_ticks: 5,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    sys_h
        .spawn_ball_with_combat(
            make_ball(gate_id, 17_634, DVec3::new(2_500.0, 0.0, 0.0), DVec3::ZERO, 1e8),
            CombatState::default(),
        )
        .await
        .unwrap();
    let mut sess = make_session(char_a, Some(sys1), Some(ship_id));
    let gate_ok = handle(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(gate_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(gate_ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        sys_h.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks,
        0
    );

    // 2. register_signal_tracker_subscriber retains peer in sys1 and prunes empty sys entry (L921, L922, L927)
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().unwrap().clone();
        for (cid, uid) in [(char_a, 901_u64), (char_b, 902_u64)] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.solar_system_id = sys1;
            snap.characters.insert(cid, ch);
            if !snap.accounts.values().any(|a| a.user_id == uid) {
                snap.accounts.insert(
                    format!("scan_user_{uid}"),
                    eve_db::AccountRecord {
                        user_id: uid,
                        username: format!("scan_user_{uid}"),
                        password_token: String::new(),
                        role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
            }
        }
    });
    let lock_now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(901, 90101, Some(char_a), true, lock_now)
        .unwrap();
    ctx.db
        .acquire_online_lock(902, 90201, Some(char_b), true, lock_now)
        .unwrap();
    register_signal_tracker_subscriber(&ctx, sys1, char_a);
    register_signal_tracker_subscriber(&ctx, sys1, char_b);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_a).unwrap().solar_system_id = sys2;
    });
    register_signal_tracker_subscriber(&ctx, sys2, char_a);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys1), vec![char_b]);
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys2), vec![char_a]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_b).unwrap().solar_system_id = sys2;
    });
    register_signal_tracker_subscriber(&ctx, sys2, char_b);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleet_and_social_ops.signal_tracker_subs.contains_key(&sys1));
        assert_eq!(
            signal_tracker_subscribers_in_snap(snap, sys2),
            vec![char_a, char_b]
        );
    });

    // 3. resolve_anomaly_dungeon_metadata & scan_strength_attribute_for_anomaly full matrix
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.dungeons.clear();
        sde.dungeons.insert(
            0,
            DungeonTemplate {
                dungeon_id: 0,
                name: "Sentinel Zero".into(),
                archetype_id: Some(45),
                faction_id: Some(500_099),
                difficulty: Some(5),
            },
        );
        sde.dungeons.insert(
            54_321,
            DungeonTemplate {
                dungeon_id: 54_321,
                name: "Exact Id Site".into(),
                archetype_id: Some(44),
                faction_id: Some(500_011),
                difficulty: Some(4),
            },
        );
        sde.dungeons.insert(
            99_777,
            DungeonTemplate {
                dungeon_id: 99_777,
                name: "".into(),
                archetype_id: Some(30),
                faction_id: Some(500_088),
                difficulty: Some(5),
            },
        );
        sde.dungeons.insert(
            99_881,
            DungeonTemplate {
                dungeon_id: 99_881,
                name: "Wrong Archetype".into(),
                archetype_id: Some(99),
                faction_id: Some(500_077),
                difficulty: Some(5),
            },
        );
        sde.dungeons.insert(
            99_882,
            DungeonTemplate {
                dungeon_id: 99_882,
                name: "Ore Archetype Match".into(),
                archetype_id: Some(27),
                faction_id: Some(500_021),
                difficulty: Some(3),
            },
        );
        for (did, nm) in [(99_901, "UnknownData"), (99_902, "UnknownGas"), (99_903, "UnknownRelic")] {
            sde.dungeons.insert(
                did,
                DungeonTemplate {
                    dungeon_id: did,
                    name: nm.into(),
                    archetype_id: Some(999),
                    faction_id: Some(500_010),
                    difficulty: Some(2),
                },
            );
        }
    }

    let mk_anom = |id: &str, name: &str, ty: &str| CosmicAnomalyRecord {
        anomaly_id: id.into(),
        solar_system_id: sys1,
        name: name.into(),
        anomaly_type: ty.into(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 1.0,
        ore_types: vec![],
        remaining_volume: 0.0,
        created_unix: 100,
        lifetime_seconds: 86_400,
        cleared: false,
    };

    // DNG-54321-1 matches exact id 54321 and archetype 44 -> 210 (kills < 0 filter & arm 44)
    let m_exact = resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("DNG-54321-1", "Unlisted", "Relic"));
    assert_eq!(m_exact.dungeon_id, 54_321);
    assert_eq!(m_exact.archetype_id, 44);
    assert_eq!(m_exact.scan_strength_attribute, 210);

    // DNG-0-1 must NOT pick dungeon_id == 0 (kills > -> == / >=)
    let m_zero = resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("DNG-0-1", "Unlisted", "Combat"));
    assert_ne!(m_zero.dungeon_id, 0);

    // Empty name must skip 99_777 (`!anom_name.is_empty()`) and resolve Ore archetype 27 -> 99_882 (L1053, L1082, arm 27|28)
    let m_ore = resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-1", "", "Ore"));
    assert_eq!(m_ore.dungeon_id, 99_882);
    assert_eq!(m_ore.archetype_id, 27);
    assert_eq!(m_ore.scan_strength_attribute, 211);

    // Archetype fallback scan strength for 45 (Data -> 208) and 30 (Gas -> 209) + type fallback with unknown archetype 999
    assert_eq!(
        resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-2", "Sentinel Zero", "Combat"))
            .scan_strength_attribute,
        208
    );
    Arc::make_mut(&mut ctx.sde).dungeons.insert(
        99_778,
        DungeonTemplate {
            dungeon_id: 99_778,
            name: "Named Gas".into(),
            archetype_id: Some(30),
            faction_id: Some(500_021),
            difficulty: Some(2),
        },
    );
    assert_eq!(
        resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-3", "Named Gas", "Combat"))
            .scan_strength_attribute,
        209
    );
    assert_eq!(
        resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-4", "UnknownData", "Data"))
            .scan_strength_attribute,
        208
    );
    assert_eq!(
        resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-5", "UnknownGas", "Gas Site"))
            .scan_strength_attribute,
        209
    );
    assert_eq!(
        resolve_anomaly_dungeon_metadata(&ctx.sde, &mk_anom("CUSTOM-6", "UnknownRelic", "Relic"))
            .scan_strength_attribute,
        210
    );

    let kv = build_signal_tracker_anomaly_kv(&ctx.sde, &mk_anom("DNG-54321-1", "Unlisted", "Relic"));
    assert_ne!(kv, PyRep::None);
    assert_eq!(kv.dict_get("dungeonID"), Some(&PyRep::Integer(54_321)));
    assert_eq!(kv.dict_get("scanStrengthAttribute"), Some(&PyRep::Integer(210)));
}
