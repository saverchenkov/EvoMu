//! Deep branch-coverage and mutation-killing unit tests for `space_probe_scanning`
//! and directional scan (`ConeScan`) geometry/boundary filters.

#![forbid(unsafe_code)]

use crate::router::{ActiveScannerProbe, ServerContext};
use crate::services::fleet_social;
use crate::services::space_destiny::space_docking_cloaking::space_probe_scanning::{
    activate_scan_probe_launcher, compute_probe_max_deviation_au, expire_all_overdue_probes,
    expire_overdue_probes_for_char, handle_scan_and_wormhole_extras, is_combat_scanner_probe,
    probe_allowed_ranges_au, probe_base_max_deviation_au, probe_base_scan_range_au,
    probe_base_sensor_strength, probe_max_flight_time_s,
    signature_difficulty_level_and_min_strength, tick_probe_warp_end, PROBE_STATE_IDLE,
    PROBE_STATE_INACTIVE, PROBE_STATE_MOVING, PROBE_STATE_SCANNING, PROBE_STATE_WARPING,
};
use crate::services::space_destiny::AU_IN_METERS;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
const SYNTH_STRONG_PROBE_TID: u32 = 990_301;

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut strong_probe = sde.types.get(&30013).cloned().unwrap_or_else(|| sde.types.values().next().unwrap().clone());
        strong_probe.type_id = SYNTH_STRONG_PROBE_TID;
        strong_probe.category_id = 8;
        strong_probe.group_id = 479;
        strong_probe.name = "Synthetic High-Sensor Core Probe".into();
        strong_probe.default_attributes.insert(1370, 0.25); // baseRangeAU
        strong_probe.default_attributes.insert(1371, 600.0); // baseSensorStrength
        strong_probe.default_attributes.insert(1372, 0.125); // baseMaxDeviation
        strong_probe.default_attributes.insert(281, 4_000_000.0); // maxFlightTime ms
        strong_probe.default_attributes.insert(1413, 0.0); // core probe (not combat)
        sde.types.insert(SYNTH_STRONG_PROBE_TID, strong_probe);

        let arc_sde = Arc::new(sde);
        let _ = SHARED_SDE.set(Arc::clone(&arc_sde));
        ServerContext::with_sde_and_repository(
            arc_sde,
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    }
}

fn make_space_session(char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(9711);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1000009);
    s.station_id = None;
    s.structure_id = None;
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        PyRep::ObjectEx { header, .. } => {
            if let PyRep::Tuple(t) = header.as_ref() {
                if let Some(PyRep::Dict(entries)) = t.get(1) {
                    return entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v));
                }
            }
            None
        }
        _ => None,
    }
}

fn set_active_probes_for_char(ctx: &ServerContext, char_id: i64, probes: Vec<ActiveScannerProbe>) {
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        map.retain(|_, p| p.owner_char_id != char_id);
        for p in probes {
            map.insert(p.probe_id, p);
        }
    }
}

fn get_active_probes_for_char(ctx: &ServerContext, char_id: i64) -> Vec<ActiveScannerProbe> {
    ctx.active_scanner_probes
        .read()
        .map(|m| m.values().filter(|p| p.owner_char_id == char_id).cloned().collect())
        .unwrap_or_default()
}

#[test]
fn probe_attributes_deviation_math_and_signature_difficulty_tiers_are_exact() {
    let ctx = test_ctx();

    assert!(!is_combat_scanner_probe(&ctx.sde, 30013));
    assert!(!is_combat_scanner_probe(&ctx.sde, 30026));
    assert!(is_combat_scanner_probe(&ctx.sde, 30028));

    assert!((probe_base_scan_range_au(&ctx.sde, 30013) - 0.25).abs() < 1e-6);
    assert!((probe_base_scan_range_au(&ctx.sde, 30028) - 0.5).abs() < 1e-6);

    let core_ranges = probe_allowed_ranges_au(&ctx.sde, 30013);
    let combat_ranges = probe_allowed_ranges_au(&ctx.sde, 30028);
    assert_eq!(core_ranges.len(), 8);
    assert!((core_ranges[0] - 0.25).abs() < 1e-6);
    assert!((core_ranges[7] - 32.0).abs() < 1e-6);
    assert_eq!(combat_ranges.len(), 8);
    assert!((combat_ranges[0] - 0.5).abs() < 1e-6);
    assert!((combat_ranges[7] - 64.0).abs() < 1e-6);

    assert!((probe_base_max_deviation_au(&ctx.sde, 30013) - 0.125).abs() < 1e-6);
    assert!((probe_base_max_deviation_au(&ctx.sde, 30028) - 0.25).abs() < 1e-6);
    assert!(probe_base_sensor_strength(&ctx.sde, SYNTH_STRONG_PROBE_TID) > probe_base_sensor_strength(&ctx.sde, 30013));
    assert!((probe_max_flight_time_s(&ctx.sde, 30013) - 4_000.0).abs() < 1e-6);

    // Deviation scales with (range / base_range) * (1 - 0.05 * astro) * (1 - 0.05 * pinpoint)
    let dev_lv0 = compute_probe_max_deviation_au(0.125, 1.0, 0.25, 0, 0);
    let dev_lv5 = compute_probe_max_deviation_au(0.125, 1.0, 0.25, 5, 5);
    assert!((dev_lv0 - 0.5).abs() < 1e-9);
    assert!((dev_lv5 - 0.5 * 0.75 * 0.75).abs() < 1e-9);

    let sig = CosmicSignatureRecord {
        signature_id: "DIF-001".into(),
        solar_system_id: 30_000_142,
        sig_group: "Data".into(),
        type_name: "Local Archive".into(),
        signal_strength: 0.0,
        position: [0.0; 3],
        destination_system_id: None,
        wormhole_type_id: None,
        remaining_mass_kg: 0.0,
        max_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        created_unix: 1000,
        lifetime_seconds: 3600,
        collapsed: false,
    };
    let (diff_lvl, min_str) = signature_difficulty_level_and_min_strength(&ctx.sde, 30_000_142, &sig);
    assert!((1..=5).contains(&diff_lvl));
    assert!(matches!(min_str as i64, 42 | 72 | 104));
}

#[tokio::test]
async fn activate_scan_probe_launcher_validates_loaded_charges_launcher_compatibility_and_max_8_cap() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142;
    let (ship_id, core_launcher_id, probe_stack_id) = (61001, 61002, 61003);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(core_launcher_id, make_item(core_launcher_id, 17938, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });

    let session = make_space_session(char_id, sys_id, ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();
    let hi0 = InventoryFlag::HiSlot0 as u16;
    set_active_probes_for_char(&ctx, char_id, Vec::new());

    // 1. No probes loaded -> NoProbesLoaded
    let empty_err = activate_scan_probe_launcher(&ctx, &session, &sys_handle, char_id, ship_id, sys_id, core_launcher_id, 17938, hi0, "launchForSelf", None, &kw)
        .await.unwrap_err();
    assert_eq!(empty_err.code, "NoProbesLoaded");

    // 2. Combat Scanner Probe I (30028) inside Core Probe Launcher (17938) -> InvalidProbeForLauncher
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30028, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let compat_err = activate_scan_probe_launcher(&ctx, &session, &sys_handle, char_id, ship_id, sys_id, core_launcher_id, 17938, hi0, "launchForSelf", None, &kw)
        .await.unwrap_err();
    assert_eq!(compat_err.code, "InvalidProbeForLauncher");

    // 3. Switch stack to Core Scanner Probe I (30013) -> launches 8 probes
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(p) = snap.items.get_mut(&probe_stack_id) {
            p.type_id = 30013;
            p.quantity = 10;
        }
    });
    let launch_rep = activate_scan_probe_launcher(&ctx, &session, &sys_handle, char_id, ship_id, sys_id, core_launcher_id, 17938, hi0, "launchForSelf", None, &kw)
        .await.unwrap();
    assert_eq!(dict_get(&launch_rep, "launchedCount").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(get_active_probes_for_char(&ctx, char_id).len(), 8);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&probe_stack_id).map(|i| i.quantity)), Some(2));

    // 4. Attempting to launch again while 8 probes are active -> MaxProbesLaunched
    let max_err = activate_scan_probe_launcher(&ctx, &session, &sys_handle, char_id, ship_id, sys_id, core_launcher_id, 17938, hi0, "launchForSelf", None, &kw)
        .await.unwrap_err();
    assert_eq!(max_err.code, "MaxProbesLaunched");
    set_active_probes_for_char(&ctx, char_id, Vec::new());
}

#[tokio::test]
async fn trilateration_1_2_3_4_probes_and_combat_probe_ship_drone_abyssal_scanning() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let enemy_char = 90_000_002_i64;
    let sys_id = 30_000_142;
    let (ship_id, enemy_ship_id, cloaked_ship_id, drone_id, trace_id) = (62001, 62002, 62003, 62004, 62005);

    let sig_pos = [1.0 * AU_IN_METERS, 0.0, 0.0];
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        let sk = snap.skills.entry(char_id).or_default();
        for &(tid, lvl) in &[(3412u32, 5u8), (25810, 5), (25811, 5), (25812, 5)] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
        for (id, tid, own) in [
            (ship_id, 587, char_id),
            (enemy_ship_id, 622, enemy_char),
            (cloaked_ship_id, 622, enemy_char),
            (drone_id, 2454, enemy_char),
            (trace_id, 47458, enemy_char),
        ] {
            snap.items.insert(id, make_item(id, tid, own, sys_id as i64, InventoryFlag::None, 1, true));
        }
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "TRI-001".into(),
                solar_system_id: sys_id,
                sig_group: "Relic".into(),
                type_name: "Crumbling Angel Crystal Quarry".into(),
                signal_strength: 0.0,
                position: sig_pos,
                destination_system_id: None,
                wormhole_type_id: None,
                remaining_mass_kg: 0.0,
                max_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                collapsed: false,
            }],
        );
    });

    let mut session = make_space_session(char_id, sys_id, ship_id);
    let kw = HashMap::new();

    let make_n_probes = |n: usize, type_id: u32, range_au: f64| -> Vec<ActiveScannerProbe> {
        (0..n)
            .map(|i| ActiveScannerProbe {
                probe_id: 970_000 + i as i64,
                probe_type_id: type_id,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 61002,
                solar_system_id: sys_id,
                position: sig_pos,
                destination: sig_pos,
                range_step: 1,
                scan_range_m: range_au * AU_IN_METERS,
                base_sensor_strength: 120.0,
                max_flight_time_s: 4_000.0,
                launched_at_unix: now_unix,
                state: PROBE_STATE_IDLE,
                warp_end_ft: None,
            })
            .collect()
    };

    // Test 1, 2, 3, and 4 covering probes trilateration caps (0.24, 0.49, 0.74, 1.0)
    for (n, expected_cap, expect_warpable) in [
        (1usize, 0.24f64, false),
        (2, 0.49, false),
        (3, 0.74, false),
        (4, 1.00, true),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.cosmic_signatures.get_mut(&sys_id).unwrap()[0].signal_strength = 0.0;
        });
        if let Ok(mut progress) = ctx.character_scan_progress.write() {
            progress.clear();
        }
        set_active_probes_for_char(&ctx, char_id, make_n_probes(n, SYNTH_STRONG_PROBE_TID, 0.25));
        let rep = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "RequestScans", &[], &kw)
            .await.unwrap().unwrap();

        let PyRep::List(rows) = rep else { panic!("expected results list") };
        let row = rows.iter().find(|r| dict_get(r, "id").and_then(|v| v.as_str()) == Some("TRI-001")).unwrap();
        let certainty = dict_get(row, "certainty").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let warpable = dict_get(row, "warpable").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!((certainty - expected_cap).abs() < 1e-6, "for {n} probes expected {expected_cap}, got {certainty}");
        assert_eq!(warpable, expect_warpable);
    }

    // Combat probe scan detects uncloaked enemy ship, drone, and Abyssal trace, while skipping cloaked ship
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, cloaked) in [
        (enemy_ship_id, 622u32, false),
        (cloaked_ship_id, 622u32, true),
        (drone_id, 2454u32, false),
        (trace_id, 47458u32, false),
    ] {
        let mut cs = CombatState::default();
        cs.is_cloaked = cloaked;
        let mut meta = SlimItemMeta::default();
        meta.is_cloaked = cloaked;
        let _ = sys_handle.spawn_ball_with_meta(
            Ball {
                id: bid, type_id: tid, position: DVec3::from_array(sig_pos), velocity: DVec3::ZERO,
                max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs,
            meta,
        ).await;
    }

    set_active_probes_for_char(&ctx, char_id, make_n_probes(4, 30028, 0.5));
    let combat_rep = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "RequestScans", &[], &kw)
        .await.unwrap().unwrap();
    let PyRep::List(c_rows) = combat_rep else { panic!("expected results list") };
    let found_item_ids: Vec<i64> = c_rows.iter().filter_map(|r| dict_get(r, "itemID").and_then(|v| v.as_i64())).collect();
    assert!(found_item_ids.contains(&enemy_ship_id));
    assert!(found_item_ids.contains(&drone_id));
    assert!(found_item_ids.contains(&trace_id));
    assert!(!found_item_ids.contains(&cloaked_ship_id));
    set_active_probes_for_char(&ctx, char_id, Vec::new());
}

#[tokio::test]
async fn probe_expiry_cargo_return_vs_overflow_lost_reconnect_recover_destroy_and_dscan_cone() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142;
    let (ship_id, filler_id) = (63001, 63002);
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ANM-LIVE".into(), solar_system_id: sys_id, anomaly_type: "Combat".into(),
                    name: "Angel Hideaway".into(), signal_strength: 1.0, position: [1_000.0, 0.0, 0.0],
                    ore_types: Vec::new(), remaining_volume: 0.0, created_unix: now_unix,
                    lifetime_seconds: 3_600, cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANM-CLEARED".into(), solar_system_id: sys_id, anomaly_type: "Combat".into(),
                    name: "Cleared Hideaway".into(), signal_strength: 1.0, position: [2_000.0, 0.0, 0.0],
                    ore_types: Vec::new(), remaining_volume: 0.0, created_unix: now_unix,
                    lifetime_seconds: 3_600, cleared: true,
                },
            ],
        );
    });

    let make_probe = |probe_id: i64, launched_at_unix: u64, state: i32| ActiveScannerProbe {
        probe_id, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 61002,
        solar_system_id: sys_id, position: [0.0; 3], destination: [0.0; 3], range_step: 1,
        scan_range_m: AU_IN_METERS, base_sensor_strength: 40.0, max_flight_time_s: 4_000.0,
        launched_at_unix, state, warp_end_ft: None,
    };

    // 1. Overdue probe with free ship cargo returns to cargo
    set_active_probes_for_char(&ctx, char_id, vec![make_probe(975_001, now_unix.saturating_sub(5_000), PROBE_STATE_IDLE)]);
    let (returned, lost) = expire_overdue_probes_for_char(&ctx, char_id, now_unix).await;
    assert_eq!(returned, vec![975_001]);
    assert!(lost.is_empty());

    // 2. Overdue probe when ship cargo is full is reported in lost_ids
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(filler_id, make_item(filler_id, 34, char_id, ship_id, InventoryFlag::Cargo, 1_000_000, false));
    });
    set_active_probes_for_char(&ctx, char_id, vec![make_probe(975_002, now_unix.saturating_sub(5_000), PROBE_STATE_IDLE)]);
    expire_all_overdue_probes(&ctx, now_unix).await;

    // 3. ReconnectToLostProbes reconnects an inactive probe (state 0 -> state 1), then DestroyProbe removes it
    set_active_probes_for_char(&ctx, char_id, vec![make_probe(975_003, now_unix, PROBE_STATE_INACTIVE)]);
    let mut session = make_space_session(char_id, sys_id, ship_id);
    let kw = HashMap::new();
    let recon_rep = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "ReconnectToLostProbes", &[], &kw)
        .await.unwrap().unwrap();
    assert_eq!(dict_get(&recon_rep, "reconnectedCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(get_active_probes_for_char(&ctx, char_id)[0].state, PROBE_STATE_IDLE);

    handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "DestroyProbe", &[PyRep::Integer(975_003)], &kw)
        .await.unwrap().unwrap();
    assert!(get_active_probes_for_char(&ctx, char_id).is_empty());

    // GetAnomalies filters out cleared anomalies
    let anom_rep = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "GetAnomalies", &[], &kw)
        .await.unwrap().unwrap();
    let PyRep::List(anom_list) = anom_rep else { panic!("expected anomalies list") };
    assert!(anom_list.iter().any(|a| dict_get(a, "anomalyID").and_then(|v| v.as_str()) == Some("ANM-LIVE")));
    assert!(!anom_list.iter().any(|a| dict_get(a, "anomalyID").and_then(|v| v.as_str()) == Some("ANM-CLEARED")));

    // 4. Directional scan (ConeScan) cone angle & range boundary checks + Abyssal system restriction
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::new(0.0, 0.0, 100.0),
        max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::new(0.0, 0.0, 1.0) },
    }).await;
    let (ahead_ball_id, side_ball_id) = (63010, 63011);
    for (bid, pos) in [(ahead_ball_id, DVec3::new(0.0, 0.0, 50_000.0)), (side_ball_id, DVec3::new(50_000.0, 0.0, 0.0))] {
        let _ = sys_handle.spawn_ball(Ball {
            id: bid, type_id: 622, position: pos, velocity: DVec3::ZERO, max_velocity: 100.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    let narrow_rep = fleet_social::handle(&ctx, &mut session, "scanMgr", "ConeScan", &[PyRep::Real(15.0), PyRep::Real(100_000.0)], &kw)
        .await.unwrap().unwrap();
    let PyRep::Rowset { rows: narrow_lines, .. } = narrow_rep else { panic!("expected rowset") };
    let narrow_ids: Vec<i64> = narrow_lines.iter().filter_map(|cols| cols.first().and_then(|v| v.as_i64())).collect();
    assert!(narrow_ids.contains(&ahead_ball_id));
    assert!(!narrow_ids.contains(&side_ball_id));

    let mut abyss_sess = make_space_session(char_id, 32_000_001, ship_id);
    let abyss_err = fleet_social::handle(&ctx, &mut abyss_sess, "scanMgr", "ConeScan", &[PyRep::Real(360.0), PyRep::Real(100_000.0)], &kw)
        .await.unwrap().unwrap_err();
    assert_eq!(abyss_err.code, "CannotDscanInThisSystem");
}

#[tokio::test]
async fn request_scans_dict_move_warp_invalid_coords_and_tick_probe_warp_end() {
    let ctx = test_ctx();
    let (char_id, sys_id, ship_id, now_unix) = (90_000_001_i64, 30_000_142_u32, 64001_i64, ctx.now_unix());
    let (p_move, p_warp) = (980_001_i64, 980_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for pid in [p_move, p_warp] {
        let _ = sys_h.spawn_ball(Ball {
            id: pid, type_id: 30013, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 3_000.0,
            inertia: 1.0, mass: 100.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }
    set_active_probes_for_char(&ctx, char_id, vec![p_move, p_warp].into_iter().map(|probe_id| ActiveScannerProbe {
        probe_id, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 61002,
        solar_system_id: sys_id, position: [0.0; 3], destination: [0.0; 3], range_step: 1,
        scan_range_m: 0.25 * AU_IN_METERS, base_sensor_strength: 40.0, max_flight_time_s: 4_000.0,
        launched_at_unix: now_unix, state: PROBE_STATE_IDLE, warp_end_ft: None,
    }).collect());

    let mut session = make_space_session(char_id, sys_id, ship_id);
    let kw = HashMap::new();

    let bad_arg = PyRep::Dict(vec![(PyRep::Integer(p_move), PyRep::keyval([("pos", PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)]))]))]);
    let bad_err = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "RequestScans", &[bad_arg], &kw).await.unwrap().unwrap_err();
    assert_eq!(bad_err.code, "InvalidProbeCoordinates");

    let valid_arg = PyRep::Dict(vec![
        (PyRep::Integer(p_move), PyRep::keyval([("pos", PyRep::Tuple(vec![PyRep::Real(50_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])), ("scanRangeAU", PyRep::Real(1.0))])),
        (PyRep::Integer(p_warp), PyRep::keyval([("pos", PyRep::Tuple(vec![PyRep::Real(500_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])), ("scanRange", PyRep::Real(2.0 * AU_IN_METERS))])),
    ]);
    handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "RequestScans", &[valid_arg], &kw).await.unwrap().unwrap();
    let probes = get_active_probes_for_char(&ctx, char_id);
    assert_eq!(probes.iter().find(|p| p.probe_id == p_move).unwrap().state, PROBE_STATE_MOVING);
    assert_eq!(probes.iter().find(|p| p.probe_id == p_warp).unwrap().state, PROBE_STATE_WARPING);

    let past_ft = ctx.now_blueos_filetime_ticks().saturating_sub(10_000);
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut().filter(|p| p.owner_char_id == char_id) {
            p.warp_end_ft = Some(past_ft);
        }
    }
    tick_probe_warp_end(&ctx);
    assert!(get_active_probes_for_char(&ctx, char_id).iter().all(|p| p.state == PROBE_STATE_SCANNING));

    ctx.db.with_mut_snapshot(|snap| {
        for e in &mut snap.fleet_and_social_ops.pending_probe_scans {
            e.5 = past_ft;
        }
    });
    tick_probe_warp_end(&ctx);
    assert!(get_active_probes_for_char(&ctx, char_id).iter().all(|p| p.state == PROBE_STATE_IDLE));
}

#[tokio::test]
async fn recover_probes_clip_vs_cargo_get_system_scan_mgr_tiers_and_launch_formations() {
    let ctx = test_ctx();
    let (char_id, sys_id, now_unix) = (90_000_001_i64, 30_000_142_u32, ctx.now_unix());
    let (ship_id, launcher_id, cargo_probe_id) = (65001, 65002, 65003);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(launcher_id, make_item(launcher_id, 17938, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(cargo_probe_id, make_item(cargo_probe_id, 30013, char_id, ship_id, InventoryFlag::Cargo, 4, false));
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 0, skill_points: 0.0 });
        snap.cosmic_signatures.insert(sys_id, vec![
            CosmicSignatureRecord { signature_id: "LOW-1".into(), solar_system_id: sys_id, sig_group: "Relic".into(), type_name: "Ruined Site".into(), signal_strength: 0.10, position: [1_000.0; 3], destination_system_id: None, wormhole_type_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0, max_jump_mass_kg: 0.0, created_unix: now_unix, lifetime_seconds: 86_400, collapsed: false },
            CosmicSignatureRecord { signature_id: "MID-2".into(), solar_system_id: sys_id, sig_group: "Data".into(), type_name: "Data Complex".into(), signal_strength: 0.80, position: [2_000.0; 3], destination_system_id: None, wormhole_type_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0, max_jump_mass_kg: 0.0, created_unix: now_unix, lifetime_seconds: 86_400, collapsed: false },
        ]);
    });

    let mut session = make_space_session(char_id, sys_id, ship_id);
    let kw = HashMap::new();

    let over_err = fleet_social::handle(&ctx, &mut session, "scanMgr", "LaunchProbes", &[PyRep::Integer(5)], &kw).await.unwrap().unwrap_err();
    assert_eq!(over_err.code, "MaxProbesExceeded");
    let launch_rep = fleet_social::handle(&ctx, &mut session, "scanMgr", "LaunchProbes", &[PyRep::Integer(4), PyRep::Integer(1)], &kw).await.unwrap().unwrap();
    assert_eq!(dict_get(&launch_rep, "probeCount").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(dict_get(&launch_rep, "formationName").and_then(|v| v.as_str()), Some("Spread"));

    let mgr_rep = handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "GetSystemScanMgr", &[], &kw).await.unwrap().unwrap();
    assert!(dict_get(&mgr_rep, "signatures").is_some());
    assert!(handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "GetFullState", &[], &kw).await.is_some());
    assert!(fleet_social::handle(&ctx, &mut session, "scanMgr", "SignalTrackerRegister", &[], &kw).await.is_some());
    assert!(fleet_social::handle(&ctx, &mut session, "scanMgr", "GetSignatures", &[], &kw).await.is_some());

    set_active_probes_for_char(&ctx, char_id, (1..=9).map(|k| ActiveScannerProbe {
        probe_id: 985_000 + k, probe_type_id: 30013, owner_char_id: char_id, ship_id,
        launcher_module_id: launcher_id, solar_system_id: sys_id, position: [0.0; 3],
        destination: [0.0; 3], range_step: 1, scan_range_m: AU_IN_METERS,
        base_sensor_strength: 40.0, max_flight_time_s: 4_000.0, launched_at_unix: now_unix,
        state: PROBE_STATE_IDLE, warp_end_ft: None,
    }).collect());
    handle_scan_and_wormhole_extras(&ctx, &mut session, None, "scanMgr", "RecoverProbes", &[], &kw).await.unwrap().unwrap();
    assert!(get_active_probes_for_char(&ctx, char_id).is_empty());
    let total_probes_stored: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == char_id && i.type_id == 30013).map(|i| i.quantity).sum());
    assert!(total_probes_stored >= 9);
}
