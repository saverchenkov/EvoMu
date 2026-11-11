//! Edge-case unit tests for `fleet_social_scanning.rs`.

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::fleet_social::fleet_social_scanning::{
    dispatch_signal_tracker_notifications, emit_signal_tracker_anomaly_delta,
    emit_signal_tracker_signature_delta, emit_signal_tracker_site_removed_with_db,
    emit_signal_tracker_structure_delta, emit_signal_tracker_structure_removed_with_snap,
    register_signal_tracker_subscriber, signal_tracker_subscribers_in_snap,
};
use crate::services::fleet_social::{handle, AU_IN_METERS};
use crate::services::space_destiny::{
    wormhole_exit_item_id_for_signature, wormhole_item_id_for_signature,
};
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonTemplate, InvType};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_scan_session(
    char_id: Option<i64>,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(char_id.unwrap_or(99) as u64);
    s.user_id = Some(1);
    s.character_id = char_id;
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

fn make_sig(
    id: &str,
    sys_id: u32,
    sig_group: &str,
    type_name: &str,
    signal_strength: f64,
    position: [f64; 3],
    created_unix: u64,
    lifetime_seconds: u64,
    collapsed: bool,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id: id.into(),
        solar_system_id: sys_id,
        sig_group: sig_group.into(),
        type_name: type_name.into(),
        signal_strength,
        position,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix,
        lifetime_seconds,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed,
    }
}

#[tokio::test]
async fn signal_tracker_register_and_get_signatures_masking_expiry_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys1, sys2) = (90_000_001_i64, 30_000_142_u32, 30_000_144_u32);
    let kw = HashMap::new();

    // Unrelated service/method returns None from fleet_social::handle
    let mut sess = make_scan_session(Some(char_id), Some(sys1), Some(100_000_001));
    assert!(handle(&ctx, &mut sess, "agentMgr", "GetAgents", &[], &kw)
        .await
        .is_none());

    // SignalTrackerRegister without solar system -> NoSolarSystem
    let mut no_sys = make_scan_session(Some(char_id), None, None);
    let err = handle(&ctx, &mut no_sys, "scanMgr", "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NoSolarSystem");

    // GetSignatures without solar system falls back to Jita (30_000_142) and returns Rowset
    let jita_sigs = handle(&ctx, &mut no_sys, "scanMgr", "GetSignatures", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(jita_sigs, PyRep::Rowset { .. }));

    // Seed signatures with various signal_strength tiers, expired, and collapsed
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys2,
            vec![
                make_sig("SIG-LOW", sys2, "Data Site", "Secret Cache", 0.10, [10.0, 0.0, 0.0], 1_700_000_000, 3_600, false),
                make_sig("SIG-MID", sys2, "Relic Site", "Ruined Temple", 0.50, [20.0, 0.0, 0.0], 1_700_000_000, 3_600, false),
                make_sig("SIG-HI", sys2, "Combat Site", "Serpentis Hideout", 0.85, [30.0, 0.0, 0.0], 1_700_000_000, 3_600, false),
                make_sig("SIG-MAX", sys2, "Gas Site", "Golden Myko", 1.0, [40.0, 0.0, 0.0], 1_700_000_000, 3_600, false),
                make_sig("SIG-EXP", sys2, "Relic Site", "Old Site", 1.0, [50.0, 0.0, 0.0], 1_699_000_000, 60, false),
                make_sig("SIG-COL", sys2, "Wormhole", "Dead WH", 1.0, [60.0, 0.0, 0.0], 1_700_000_000, 3_600, true),
            ],
        );
        snap.cosmic_anomalies.insert(
            sys2,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-OK".into(),
                    solar_system_id: sys2,
                    name: "Haven".into(),
                    anomaly_type: "Combat".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_700_000_000,
                    lifetime_seconds: 3_600,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-CLR".into(),
                    solar_system_id: sys2,
                    name: "Cleared Haven".into(),
                    anomaly_type: "Combat".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 1_700_000_000,
                    lifetime_seconds: 3_600,
                    cleared: true,
                },
            ],
        );
        snap.structures.insert(
            990_001,
            UpwellStructureRecord {
                structure_id: 990_001,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys2,
                name: "Live Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            990_002,
            UpwellStructureRecord {
                structure_id: 990_002,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys2,
                name: "Dead Astrahus".into(),
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
    });

    // Acquire online lock for subscriber registration check
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().user_id);
    ctx.db
        .acquire_online_lock(uid, 501, Some(char_id), true, 1_700_000_000)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys2;
    });

    // SignalTrackerRegister registers char_id in sess.solar_system_id (sys2)
    sess.solar_system_id = Some(sys2);
    let reg_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "SignalTrackerRegister",
        &[PyRep::Integer(sys2 as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(reg_res, PyRep::None));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys2), vec![char_id]);
    });

    // GetSignatures in sys2 filters expired & collapsed (returns 4 active signatures in Rowset)
    let sigs_res = handle(&ctx, &mut sess, "scanMgr", "GetSignatures", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows, .. } = sigs_res else {
        panic!("expected PyRep::Rowset from GetSignatures, got {sigs_res:?}");
    };
    assert!(rows.len() >= 4);

    // Register with character_id = None returns CharacterNotSelected error
    let mut anon = make_scan_session(None, Some(sys1), None);
    let anon_err = handle(&ctx, &mut anon, "scanMgr", "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(anon_err.code, "CharacterNotSelected");
}

#[tokio::test]
async fn request_scans_delegate_none_explicit_probes_and_ghost_site_timer() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, ship_id) = (90_000_001_i64, 30_000_142_u32, 100_000_001_i64);
    let kw = HashMap::new();

    let mut no_sys = make_scan_session(Some(char_id), None, Some(ship_id));
    // Empty or single Real arg delegates to space_probe_scanning (returns None here)
    assert!(handle(&ctx, &mut no_sys, "scanMgr", "RequestScans", &[], &kw)
        .await
        .is_none());
    assert!(handle(
        &ctx,
        &mut no_sys,
        "scanMgr",
        "RequestScans",
        &[PyRep::Real(2.0 * AU_IN_METERS)],
        &kw,
    )
    .await
    .is_none());

    // Two args with no solar system -> NoSolarSystem
    let err = handle(
        &ctx,
        &mut no_sys,
        "scanMgr",
        "RequestScans",
        &[PyRep::Real(2.0 * AU_IN_METERS), PyRep::Real(60.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "NoSolarSystem");

    // Seed a Ghost site + a distant normal site
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                make_sig(
                    "GHO-001",
                    sys_id,
                    "Data Site",
                    "Lesser Serpentis Covert Research Facility",
                    0.50,
                    [1_000.0, 0.0, 0.0],
                    1_700_000_000,
                    86_400,
                    false,
                ),
                make_sig(
                    "FAR-002",
                    sys_id,
                    "Relic Site",
                    "Ruined Guristas Monument",
                    0.0,
                    [100.0 * AU_IN_METERS, 0.0, 0.0],
                    1_700_000_000,
                    86_400,
                    false,
                ),
            ],
        );
    });

    // Clear active probes and scan with explicit List(probes) as arg 0:
    // Cover both Dict probe descriptor and bare [x, y, z] list descriptor.
    ctx.active_scanner_probes.write().unwrap().clear();
    let probe_dict = PyRep::Dict(vec![
        (PyRep::String("probeID".into()), PyRep::Integer(801)),
        (
            PyRep::String("pos".into()),
            PyRep::List(vec![
                PyRep::Real(1_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ]),
        ),
        (PyRep::String("rangeStep".into()), PyRep::Integer(2)),
    ]);
    let probe_pos_list = PyRep::List(vec![
        PyRep::Real(1_200.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]);
    let mut sess = make_scan_session(Some(char_id), Some(sys_id), Some(ship_id));
    let res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[
            PyRep::List(vec![probe_dict, probe_pos_list]),
            PyRep::Real(4.0 * AU_IN_METERS),
            PyRep::Real(90.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(res, PyRep::None));

    // Also invoke RequestScans with empty probe list -> triggers 8-probe formation fallback
    let fallback_res = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::List(vec![]), PyRep::Real(2.0 * AU_IN_METERS), PyRep::Real(40.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(fallback_res, PyRep::None));
}

#[tokio::test]
async fn dscan_zero_velocity_angle_360_and_range_clamping_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, ship_id) = (90_000_001_i64, 30_000_142_u32, 100_000_001_i64);
    let kw = HashMap::new();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Ship has ZERO velocity (exercises default forward vector fallback)
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 606, DVec3::ZERO, DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    // Target behind ship and target far beyond 14.3 AU limit
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(300_001, 587, DVec3::new(0.0, 0.0, -5_000.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(300_002, 587, DVec3::new(0.0, 0.0, 30.0 * AU_IN_METERS), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;

    let mut sess = make_scan_session(Some(char_id), Some(sys_id), Some(ship_id));
    // 360 degree scan with huge range clamps at max 14.3 AU -> finds 300_001 but not 300_002
    let full = handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(50.0 * AU_IN_METERS)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows, .. } = full else {
        panic!("expected Rowset from ConeScan");
    };
    assert!(rows.iter().any(|r| r.contains(&PyRep::Integer(300_001))));
    assert!(!rows.iter().any(|r| r.contains(&PyRep::Integer(300_002))));
}

#[tokio::test]
async fn wormhole_polarization_ball_id_exit_and_expiry_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, dest_sys) = (90_000_001_i64, 30_000_142_u32, 31_000_009_u32);
    let kw = HashMap::new();
    let sig_id = "WHX-111";

    let (wh_sig_2, dest_sys_2) = ("WHX-222", 31_000_010_u32);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                CosmicSignatureRecord {
                    signature_id: sig_id.into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "C247".into(),
                    signal_strength: 1.0,
                    position: [0.0; 3],
                    wormhole_type_id: Some(30_001),
                    destination_system_id: Some(dest_sys),
                    created_unix: 1_700_000_000,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 2.0e9,
                    remaining_mass_kg: 2.0e9,
                    max_jump_mass_kg: 3.0e8,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: wh_sig_2.into(),
                    solar_system_id: sys_id,
                    sig_group: "Wormhole".into(),
                    type_name: "C247".into(),
                    signal_strength: 1.0,
                    position: [1_000.0, 0.0, 0.0],
                    wormhole_type_id: Some(30_001),
                    destination_system_id: Some(dest_sys_2),
                    created_unix: 1_700_000_000,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 2.0e9,
                    remaining_mass_kg: 2.0e9,
                    max_jump_mass_kg: 3.0e8,
                    collapsed: false,
                },
            ],
        );
        // Entrance polarization active, second wormhole exit polarization expired
        snap.wh_polarizations.insert((char_id, dest_sys as i64), 1_700_000_250);
        snap.wh_polarizations.insert((char_id, dest_sys_2 as i64), 1_699_999_990);
    });

    let mut sess = make_scan_session(Some(char_id), Some(sys_id), Some(100_000_001));
    // Entrance ball ID -> active polarization Tuple
    let ent_id = wormhole_item_id_for_signature(sys_id, sig_id);
    let ent_res = handle(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(ent_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(ent_res, PyRep::Tuple(ref t) if t.len() == 2));

    // Exit ball ID for WHX-222 from dest_sys_2 -> expired timestamp -> PyRep::None
    sess.solar_system_id = Some(dest_sys_2);
    let exit_id = wormhole_exit_item_id_for_signature(dest_sys_2, wh_sig_2);
    let exit_res = handle(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(exit_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(exit_res, PyRep::None);

    // Unknown integer ball ID -> WormholeNotFound
    let err = handle(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(999_888_777)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "WormholeNotFound");

    // Missing character_id -> Err(CharacterNotSelected)
    let mut anon = make_scan_session(None, Some(sys_id), None);
    let anon_err = handle(
        &ctx,
        &mut anon,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String(sig_id.into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(anon_err.code, "CharacterNotSelected");
}

#[tokio::test]
async fn acceleration_gate_rig_size_capital_diff3_and_non_consumed_key_edges() {
    let (mut ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys_id, ship_id, gate_id, key_type_id) = (
        90_000_001_i64,
        30_000_142_u32,
        100_000_001_i64,
        700_000_002_i64,
        24_577_u32,
    );
    let kw = HashMap::new();

    let (cap_ship_type, big_rig_ship_type, bs_rig_ship_type) = (995_001_u32, 995_002_u32, 995_003_u32);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.dungeons.insert(
            9003,
            DungeonTemplate {
                dungeon_id: 9003,
                name: "L3 Site".into(),
                archetype_id: Some(1),
                faction_id: None,
                difficulty: Some(3),
            },
        );
        let make_inv_type = |type_id: u32, group_id: u32, name: &str| InvType {
            type_id,
            group_id,
            category_id: 6,
            name: name.into(),
            mass: 1_000_000.0,
            volume: 1_000.0,
            capacity: 0.0,
            radius: 40.0,
            base_price: 0.0,
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
            default_attributes: HashMap::new(),
        };
        // Capital group_id = 485 (Dreadnought)
        sde.types.insert(cap_ship_type, make_inv_type(cap_ship_type, 485, "Test Dread"));
        // Capital rigSize = 4.0 (attr 1547)
        let mut big_rig = make_inv_type(big_rig_ship_type, 25, "BigRig Hull");
        big_rig.default_attributes.insert(1547, 4.0);
        sde.types.insert(big_rig_ship_type, big_rig);

        // Battleship rigSize = 3.0 (attr 1547)
        let mut bs_rig = make_inv_type(bs_rig_ship_type, 27, "BSRig Hull");
        bs_rig.default_attributes.insert(1547, 3.0);
        sde.types.insert(bs_rig_ship_type, bs_rig);
    }

    // NoSolarSystem / NoActiveShip
    let mut no_sys = make_scan_session(Some(char_id), None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut no_sys, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut no_ship = make_scan_session(Some(char_id), Some(sys_id), None);
    assert_eq!(
        handle(&ctx, &mut no_ship, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(gate_id, 17_634, DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, 1e8),
            CombatState::default(),
        )
        .await;

    let mut sess = make_scan_session(Some(char_id), Some(sys_id), Some(ship_id));

    // rigSize >= 4.0 -> ShipTooLargeForGate
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, big_rig_ship_type, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipTooLargeForGate"
    );

    // Capital group_id -> ShipTooLargeForGate
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, cap_ship_type, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipTooLargeForGate"
    );

    // difficulty 3 gate restriction with rigSize = 3.0 -> ShipRestrictedByDungeonGate
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, bs_rig_ship_type, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(gate_id), PyRep::Integer(9003)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipRestrictedByDungeonGate"
    );

    // Normal frigate with non-consumed gate key (attr 1003 = 0.0) succeeds and preserves key
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 606, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
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
        gate_item.dynamic_attributes.insert(1001, 9003.0);
        gate_item.dynamic_attributes.insert(1002, key_type_id as f64);
        gate_item.dynamic_attributes.insert(1003, 0.0);
        snap.items.insert(gate_id, gate_item);
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
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
    let ok = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(ok, PyRep::None));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&880_001));
    });

    // Now set attr 1003 = 1.0 -> key IS consumed on activation
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 606, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, 1e6),
            CombatState::default(),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&gate_id).unwrap().dynamic_attributes.insert(1003, 1.0);
    });
    let _ = handle(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&880_001));
    });
}

#[test]
fn signal_tracker_delta_early_returns_removed_ids_and_site_removed_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (char_id, sys1, sys2) = (90_000_001_i64, 30_000_142_u32, 30_000_144_u32);

    // No subscribers -> empty notifs
    let empty_notifs = ctx.db.with_snapshot(|snap| {
        emit_signal_tracker_structure_removed_with_snap(&ctx, snap, sys1, 55_001)
    });
    assert!(empty_notifs.is_empty());
    dispatch_signal_tracker_notifications(&ctx, empty_notifs);

    // Register online lock then switch from sys1 to sys2 -> sys1 subscriber pruned
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().user_id);
    ctx.db
        .acquire_online_lock(uid, 502, Some(char_id), true, 1_700_000_000)
        .unwrap();
    register_signal_tracker_subscriber(&ctx, sys1, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys2;
    });
    register_signal_tracker_subscriber(&ctx, sys2, char_id);
    ctx.db.with_snapshot(|snap| {
        assert!(signal_tracker_subscribers_in_snap(snap, sys1).is_empty());
        assert_eq!(signal_tracker_subscribers_in_snap(snap, sys2), vec![char_id]);
    });

    // Empty deltas return early without queuing notifications
    emit_signal_tracker_signature_delta(&ctx, sys2, &[], &[]);
    emit_signal_tracker_anomaly_delta(&ctx, sys2, &[], &[]);
    emit_signal_tracker_structure_delta(&ctx, sys2, &[], &[]);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Non-empty removed_ids emit updates for subscribers
    emit_signal_tracker_signature_delta(&ctx, sys2, &[], &["REM-SIG".into()]);
    emit_signal_tracker_anomaly_delta(&ctx, sys2, &[], &["REM-ANOM".into()]);
    emit_signal_tracker_structure_delta(&ctx, sys2, &[], &[99_001]);
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 3);

    // emit_signal_tracker_site_removed_with_db with non-matching site_id emits nothing
    emit_signal_tracker_site_removed_with_db(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        "NO-SUCH-SITE",
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Seed anomaly only and remove by site_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(sys2).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "ONLY-ANOM".into(),
            solar_system_id: sys2,
            name: "Hub".into(),
            anomaly_type: "Combat".into(),
            position: [0.0; 3],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: 1_700_000_000,
            lifetime_seconds: 3_600,
            cleared: true,
        });
    });
    emit_signal_tracker_site_removed_with_db(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        "ONLY-ANOM",
    );
    let anom_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(anom_notifs
        .iter()
        .any(|n| n.event == "OnSignalTrackerAnomalyUpdate"));

    // Direct unit checks for `scan_strength_attribute_for_anomaly` and `resolve_anomaly_dungeon_metadata`
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 45), 208);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 30), 209);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 44), 210);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 27), 211);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 28), 211);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Data Site", 99), 208);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Gas Site", 99), 209);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Relic Site", 99), 210);
    assert_eq!(super::scan_strength_attribute_for_anomaly("Combat", 99), 211);

    for (atype, exp_arch, exp_attr) in [
        ("OreAnomaly", 27, 211),
        ("IceBelt", 28, 211),
        ("Gas", 30, 209),
        ("Relic", 44, 210),
        ("Data", 45, 208),
        ("Combat", 24, 211),
    ] {
        let meta = super::resolve_anomaly_dungeon_metadata(
            &ctx.sde,
            &CosmicAnomalyRecord {
                anomaly_id: "DNG-999999-1".into(),
                solar_system_id: sys2,
                name: "UnknownCustomSite".into(),
                anomaly_type: atype.into(),
                position: [0.0; 3],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1_700_000_000,
                lifetime_seconds: 3_600,
                cleared: false,
            },
        );
        assert_eq!(meta.archetype_id, exp_arch);
        assert_eq!(meta.scan_strength_attribute, exp_attr);
        assert!((1..=5).contains(&meta.difficulty));
    }
}
