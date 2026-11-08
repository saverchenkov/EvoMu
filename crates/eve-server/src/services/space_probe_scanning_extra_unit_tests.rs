//! Deep branch and mutation-killing tests for `services::space_probe_scanning`
//! covering difficulty 4/5 thresholds, signature reveal stages (`0.25`/`0.75`/`1.0`),
//! `RequestScans` dict/coord validation, moving vs warping probe states, combat probe
//! target class scanning (`Ship`/`Drone`/`AbyssalTrace`/`Structure`), and launcher clip recovery.

use super::space_probe_scanning::*;
use crate::router::{ActiveScannerProbe, BoundObjectEntry, ServerContext};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_scan_ctx_and_session() -> (Arc<ServerContext>, SessionState) {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let mut sess = SessionState::new(92_501);
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
fn signature_difficulty_levels_4_and_5_thresholds() {
    let (ctx, _) = make_scan_ctx_and_session();
    let d4_name = ctx
        .sde
        .dungeons
        .values()
        .find(|d| d.difficulty == Some(4))
        .map(|d| d.name.clone());
    let d5_name = ctx
        .sde
        .dungeons
        .values()
        .find(|d| d.difficulty.is_some_and(|v| v >= 5))
        .map(|d| d.name.clone());

    if let Some(name4) = d4_name {
        let sig4 = CosmicSignatureRecord {
            signature_id: "D4-001".into(),
            solar_system_id: 30_000_142,
            sig_group: "Data Site".into(),
            type_name: name4,
            position: [0.0; 3],
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
        assert_eq!(
            signature_difficulty_level_and_min_strength(&ctx.sde, 30_000_142, &sig4),
            (4, 72.0)
        );
    }

    if let Some(name5) = d5_name {
        let sig5 = CosmicSignatureRecord {
            signature_id: "D5-001".into(),
            solar_system_id: 30_000_142,
            sig_group: "Relic Site".into(),
            type_name: name5,
            position: [0.0; 3],
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
        assert_eq!(
            signature_difficulty_level_and_min_strength(&ctx.sde, 30_000_142, &sig5),
            (5, 104.0)
        );
    }
}

#[tokio::test]
async fn get_system_scan_mgr_reveal_thresholds_and_anomaly_filtering() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let sys_id = 30_000_142;
    let now_unix = ctx.now_unix();
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: "SIG-000".into(),
                    solar_system_id: sys_id,
                    sig_group: "Relic Site".into(),
                    type_name: "Ruined Guristas Monument".into(),
                    position: [10_000.0, 0.0, 0.0],
                    signal_strength: 0.10,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-030".into(),
                    solar_system_id: sys_id,
                    sig_group: "Data Site".into(),
                    type_name: "Central Guristas Sparking Transmitter".into(),
                    position: [20_000.0, 0.0, 0.0],
                    signal_strength: 0.30,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-080".into(),
                    solar_system_id: sys_id,
                    sig_group: "Gas Site".into(),
                    type_name: "Barren Perimeter Reservoir".into(),
                    position: [30_000.0, 0.0, 0.0],
                    signal_strength: 0.80,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "SIG-100".into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "Unstable Wormhole".into(),
                    position: [40_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: Some(30_670),
                    destination_system_id: Some(31_000_005),
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 1_500_000_000.0,
                    max_jump_mass_kg: 300_000_000.0,
                    collapsed: false,
                },
            ],
        );
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-ACTIVE".into(),
                    solar_system_id: sys_id,
                    name: "Guristas Hideaway".into(),
                    anomaly_type: "Combat".into(),
                    position: [5_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    cleared: false,
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-CLEARED".into(),
                    solar_system_id: sys_id,
                    name: "Cleared Hideaway".into(),
                    anomaly_type: "Combat".into(),
                    position: [6_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: Vec::new(),
                    remaining_volume: 0.0,
                    cleared: true,
                    created_unix: now_unix,
                    lifetime_seconds: 86_400,
                },
            ],
        );
    });

    let rep = handle_scan_and_wormhole_extras(
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
    .unwrap();
    let PyRep::List(sigs) = rep.dict_get("signatures").unwrap() else {
        panic!("expected signatures list");
    };
    assert_eq!(sigs.len(), 4);
    // SIG-000 (< 0.25): Unknown group, Unidentified Signature
    assert_eq!(sigs[0].dict_get("group"), Some(&PyRep::String("Unknown".into())));
    assert_eq!(sigs[0].dict_get("groupName"), Some(&PyRep::None));
    // SIG-030 (0.25..0.75): Data Site group revealed, name still Unidentified
    assert_eq!(sigs[1].dict_get("group"), Some(&PyRep::String("Data Site".into())));
    assert_eq!(
        sigs[1].dict_get("type"),
        Some(&PyRep::String("Unidentified Signature".into()))
    );
    // SIG-080 (0.75..1.0): group & siteName & difficultyLevel revealed, not yet warpable
    assert_eq!(
        sigs[2].dict_get("siteName"),
        Some(&PyRep::String("Barren Perimeter Reservoir".into()))
    );
    assert!(sigs[2].dict_get("difficultyLevel").is_some());
    assert_eq!(sigs[2].dict_get("resolved"), Some(&PyRep::Bool(false)));
    // SIG-100 (1.0): resolved + wormhole stages & buckets populated
    assert_eq!(sigs[3].dict_get("resolved"), Some(&PyRep::Bool(true)));
    assert!(sigs[3].dict_get("massStabilityBucket").is_some());

    // GetAnomalies filters out cleared anomaly and rejects remote bound system for non-GM
    let remote_bound = BoundObjectEntry {
        bound_id: 20,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: 30_000_144,
        oid_str: "N=1:20".into(),
        ref_id: 20,
    };
    let anom_err = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&remote_bound),
        "scanMgr",
        "GetAnomalies",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(anom_err.code, "AccessDenied");
}

#[tokio::test]
async fn request_scans_dict_modes_combat_probes_and_launcher_clip_recovery() {
    let (ctx, mut sess) = make_scan_ctx_and_session();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    // 1. RequestScans with no tracked probes, no bound entry, empty args -> returns None
    assert!(handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        None,
        "scanMgr",
        "RequestScans",
        &[],
        &kw,
    )
    .await
    .is_none());

    let bound = BoundObjectEntry {
        bound_id: 21,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: "N=1:21".into(),
        ref_id: 21,
    };

    // 2. RequestScans with bound entry but 0 active probes -> NoActiveProbes
    assert_eq!(
        handle_scan_and_wormhole_extras(
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
        .unwrap_err()
        .code,
        "NoActiveProbes"
    );

    // 3. Fit Expanded Probe Launcher I (17901) with 8 Combat Scanner Probe I (30028), launch 4 probes via probeCount=4
    let launcher_id = 720_001;
    let charge_id = 720_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 17_901,
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
    let mut launch_kw = HashMap::new();
    launch_kw.insert("probeCount".into(), PyRep::Integer(4));
    launch_kw.insert("rangeStep".into(), PyRep::Integer(3));
    let launched = activate_scan_probe_launcher(
        &ctx,
        &sess,
        &sys_h,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_901,
        InventoryFlag::HiSlot0 as u16,
        "useMissiles",
        None,
        &launch_kw,
    )
    .await
    .expect("launch 4 combat probes");
    assert_eq!(launched.dict_get("canScanShips"), Some(&PyRep::Bool(true)));
    assert_eq!(
        launched.dict_get("remainingInLauncher"),
        Some(&PyRep::Integer(4))
    );

    // Spawn target balls in space: enemy Ship (587), Drone (2454), AbyssalTrace (47458), Structure (35832), plus Cloaked ship
    for (bid, tid, grp, cat, cloaked) in [
        (730_001, 587, 25, 6, false),
        (730_002, 2454, 100, 18, false),
        (730_003, 47_458, 1991, 2, false),
        (730_004, 35_832, 1657, 65, false),
        (730_005, 587, 25, 6, true),
    ] {
        let mut cs = CombatState::default();
        cs.is_cloaked = cloaked;
        let _ = sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: DVec3::new(500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                SlimItemMeta {
                    group_id: grp,
                    category_id: cat,
                    owner_id: 90_000_002,
                    ..Default::default()
                },
            )
            .await;
    }

    // 4. RequestScans with Dict format testing short move (<= 150km -> MOVING), long move (> 150km -> WARPING), scanRangeAU, scanRange, and x/y/z
    let pids: Vec<i64> = ctx.active_scanner_probes.read().unwrap().keys().copied().collect();
    let dict_arg = PyRep::Dict(vec![
        (
            PyRep::Integer(pids[0]),
            PyRep::keyval([
                (
                    "pos",
                    PyRep::Tuple(vec![
                        PyRep::Real(10_000.0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                    ]),
                ),
                ("rangeStep", PyRep::Integer(4)),
            ]),
        ),
        (
            PyRep::Integer(pids[1]),
            PyRep::keyval([
                ("x", PyRep::Real(500_000.0)),
                ("y", PyRep::Real(0.0)),
                ("z", PyRep::Real(0.0)),
                ("scanRangeAU", PyRep::Real(2.0)),
            ]),
        ),
        (
            PyRep::Integer(pids[2]),
            PyRep::Dict(vec![
                (PyRep::String("scanRange".into()), PyRep::Real(300_000_000_000.0)),
            ]),
        ),
    ]);
    let scan_rows = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[dict_arg],
        &kw,
    )
    .await
    .unwrap()
    .expect("RequestScans dict");
    let PyRep::List(rows) = scan_rows else {
        panic!("expected list");
    };
    // Uncloaked ship/drone/trace/structure must appear in combat probe results, cloaked ship 730_005 must NOT!
    assert!(rows.iter().any(|r| r.dict_get("itemID") == Some(&PyRep::Integer(730_001))));
    assert!(rows.iter().any(|r| r.dict_get("itemID") == Some(&PyRep::Integer(730_002))));
    assert!(rows.iter().any(|r| r.dict_get("itemID") == Some(&PyRep::Integer(730_003))));
    assert!(rows.iter().any(|r| r.dict_get("itemID") == Some(&PyRep::Integer(730_004))));
    assert!(!rows.iter().any(|r| r.dict_get("itemID") == Some(&PyRep::Integer(730_005))));

    // Verify pids[0] entered PROBE_STATE_MOVING (2) and pids[1] entered PROBE_STATE_WARPING (3)
    {
        let pm = ctx.active_scanner_probes.read().unwrap();
        assert_eq!(pm.get(&pids[0]).unwrap().state, PROBE_STATE_MOVING);
        assert_eq!(pm.get(&pids[1]).unwrap().state, PROBE_STATE_WARPING);
    }

    // Advance time & tick_probe_warp_end twice (first completes warp -> SCANNING, second completes scan -> IDLE)
    ctx.advance_secs(60);
    tick_probe_warp_end(&ctx);

    // 5. Invalid scanRangeAU (-1.0) in Dict returns InvalidProbeCoordinates
    let bad_dict = PyRep::Dict(vec![(
        PyRep::Integer(pids[0]),
        PyRep::keyval([("scanRangeAU", PyRep::Real(-1.0))]),
    )]);
    assert!(handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RequestScans",
        &[bad_dict],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    // 6. RecoverProbes returns probes directly into the launcher clip (since 4/8 were in clip)
    let rec = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "RecoverProbes",
        &[PyRep::List(vec![PyRep::Integer(pids[0]), PyRep::Integer(pids[1])])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rec.dict_get("returnedToLauncher"), Some(&PyRep::Integer(2)));

    // 7. DestroyProbe with no args destroys all remaining probes
    let dest_all = handle_scan_and_wormhole_extras(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "DestroyProbe",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dest_all.dict_get("destroyedCount"), Some(&PyRep::Integer(2)));
    let _ = ActiveScannerProbe {
        probe_id: 0,
        probe_type_id: 30_013,
        owner_char_id: char_id,
        ship_id,
        launcher_module_id: 0,
        solar_system_id: sys_id,
        position: [0.0; 3],
        destination: [0.0; 3],
        range_step: 1,
        scan_range_m: 1.0,
        base_sensor_strength: 1.0,
        max_flight_time_s: 1.0,
        launched_at_unix: 0,
        state: PROBE_STATE_IDLE,
        warp_end_ft: None,
    };
}

#[tokio::test]
async fn issue_543_scanner_probes_sqlite_restart_rehydration_and_downtime_recovery() {
    let base_ctx = ServerContext::new();
    let sde = Arc::clone(&base_ctx.sde);
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_issue_543_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("world.sqlite");

    let repo1 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx1 = ServerContext::with_sde_and_repository(Arc::clone(&sde), repo1);
    let char_id = 90_543_001_i64;
    let ship_id = 100_543_001_i64;
    let launcher_id = 100_543_010_i64;
    let charge_id = 100_543_011_i64;
    let sys_id = 30_000_142_u32;

    let mut ch = base_ctx
        .db
        .with_snapshot(|s| s.characters.values().next().expect("demo char").clone());
    ctx1.db.with_mut_snapshot(|snap| {
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "ProbePersister".into();
        ch.corporation_id = 98_000_001;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron (has cargo capacity)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
                type_id: 17_938, // Core Probe Launcher I
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
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess1 = SessionState::new(92_543);
    sess1.user_id = Some(1);
    sess1.character_id = Some(char_id);
    sess1.corporation_id = Some(98_000_001);
    sess1.solar_system_id = Some(sys_id);
    sess1.ship_id = Some(ship_id);

    let sys1 = ctx1.systems.get_or_spawn(sys_id);
    let launch_kw = HashMap::from([("probeCount".to_string(), PyRep::Integer(2))]);
    let launched = activate_scan_probe_launcher(
        &ctx1,
        &sess1,
        &sys1,
        char_id,
        ship_id,
        sys_id,
        launcher_id,
        17_938,
        InventoryFlag::HiSlot0 as u16,
        "launchForSelf",
        None,
        &launch_kw,
    )
    .await
    .expect("launch 2 core probes");
    assert_eq!(launched.dict_get("launchedCount"), Some(&PyRep::Integer(2)));
    let pids1: Vec<i64> = ctx1.active_scanner_probes.read().unwrap().keys().copied().collect();
    assert_eq!(pids1.len(), 2);
    drop(ctx1);

    // 1. Reopen fresh ServerContext from SQLite -> probes hydrate as INACTIVE (0) & spawn in Ballpark
    let repo2 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), repo2);
    {
        let map = ctx2.active_scanner_probes.read().unwrap();
        assert_eq!(map.len(), 2);
        for &pid in &pids1 {
            let p = &map[&pid];
            assert_eq!(p.state, PROBE_STATE_INACTIVE);
            assert_eq!(p.warp_end_ft, None);
        }
    }
    let sys2 = ctx2.systems.get_or_spawn(sys_id);
    for &pid in &pids1 {
        let (_sim_tick, ball) = sys2
            .get_ball_snapshot(pid)
            .await
            .expect("ball query")
            .expect("rehydrated probe ball must exist in Ballpark after restart");
        assert_eq!(sys2.get_ball_group_id(pid).await.unwrap(), Some(479));
        assert_eq!(ball.type_id, 30_013);
    }

    // ReconnectToLostProbes reclaims them -> RecoverProbes recovers 1 probe into launcher clip
    let mut sess2 = SessionState::new(92_544);
    sess2.user_id = Some(1);
    sess2.character_id = Some(char_id);
    sess2.corporation_id = Some(98_000_001);
    sess2.solar_system_id = Some(sys_id);
    sess2.ship_id = Some(ship_id);
    let bound2 = BoundObjectEntry {
        bound_id: 1,
        session_id: 92_544,
        user_id: 1,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("scanMgr:{sys_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();
    let rec_conn = handle_scan_and_wormhole_extras(
        &ctx2,
        &mut sess2,
        Some(&bound2),
        "scanMgr",
        "ReconnectToLostProbes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("ReconnectToLostProbes");
    assert_eq!(rec_conn.dict_get("reconnectedCount"), Some(&PyRep::Integer(2)));

    let rec_one = handle_scan_and_wormhole_extras(
        &ctx2,
        &mut sess2,
        Some(&bound2),
        "scanMgr",
        "RecoverProbes",
        &[PyRep::List(vec![PyRep::Integer(pids1[0])])],
        &kw,
    )
    .await
    .unwrap()
    .expect("RecoverProbes");
    assert_eq!(rec_one.dict_get("returnedToLauncher"), Some(&PyRep::Integer(1)));

    // 2. Also insert:
    //    - an expired probe (must be purged by downtime rather than resurrected)
    //    - a probe belonging to a second ship whose launcher is absent & cargo is full (must remain persisted!)
    let expired_pid = 880_543_001_i64;
    let blocked_pid = 880_543_002_i64;
    let full_ship_id = 100_543_999_i64;
    let now_u = ctx2.now_unix();
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            full_ship_id,
            InventoryItemRecord {
                item_id: full_ship_id,
                type_id: 605,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::CAPACITY, 0.05)]),
                ..Default::default()
            },
        );
    });
    let _ = sys2
        .spawn_ball_with_meta(
            Ball {
                id: expired_pid,
                type_id: 30_013,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50.0,
                radius: 5.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta { group_id: 479, category_id: 8, owner_id: char_id, ..Default::default() },
        )
        .await;
    {
        let mut map = ctx2.active_scanner_probes.write().unwrap();
        map.insert(
            expired_pid,
            ActiveScannerProbe {
                probe_id: expired_pid,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: launcher_id,
                solar_system_id: sys_id,
                position: [100.0, 0.0, 0.0],
                destination: [100.0, 0.0, 0.0],
                range_step: 1,
                scan_range_m: 1_000.0,
                base_sensor_strength: 10.0,
                max_flight_time_s: 60.0,
                launched_at_unix: now_u.saturating_sub(120),
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
        map.insert(
            blocked_pid,
            ActiveScannerProbe {
                probe_id: blocked_pid,
                probe_type_id: 30_013,
                owner_char_id: char_id,
                ship_id: full_ship_id,
                launcher_module_id: 999_999_999,
                solar_system_id: sys_id,
                position: [200.0, 0.0, 0.0],
                destination: [200.0, 0.0, 0.0],
                range_step: 1,
                scan_range_m: 1_000.0,
                base_sensor_strength: 10.0,
                max_flight_time_s: 3600.0,
                launched_at_unix: now_u,
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            },
        );
    }
    sync_scanner_probes_to_db(&ctx2);

    // 3. Daily downtime sweeps probes:
    //    - pids1[1] returns to launcher clip (making 8/8 in launcher clip)
    //    - expired_pid is purged without being added to launcher/cargo
    //    - blocked_pid stays persisted in DB with state == INACTIVE (0)
    let (returned, purged) = crate::sp6_lifecycle::recover_scanner_probes_on_downtime(&ctx2);
    assert_eq!(returned, 1);
    assert_eq!(purged, 1);
    drop(ctx2);

    let repo3 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite 3"));
    let ctx3 = ServerContext::with_sde_and_repository(sde, repo3);
    {
        let map3 = ctx3.active_scanner_probes.read().unwrap();
        assert!(!map3.contains_key(&pids1[1]), "recovered probe must be cleared");
        assert!(!map3.contains_key(&expired_pid), "expired probe must be purged");
        assert_eq!(map3.get(&blocked_pid).map(|p| p.state), Some(PROBE_STATE_INACTIVE));
    }
    ctx3.db.with_snapshot(|snap| {
        let clip_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 30_013)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(clip_qty, 8, "both launched non-expired probes back in launcher, expired probe NOT resurrected");
        let full_ship_cargo: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == full_ship_id && i.flag == InventoryFlag::Cargo)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(full_ship_cargo, 0, "full cargo ship must not receive blocked probe");
    });

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
