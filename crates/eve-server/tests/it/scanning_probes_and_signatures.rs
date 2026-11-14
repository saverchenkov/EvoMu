//! Integration tests for Scanning Probes & Signatures:
//! - feat:scanning.moon-survey-probe-launch-rules
//! - feat:scanning.probe-range-deviation
//! - feat:scanning.probe-target-classes
//! - feat:scanning.signature-reveal-thresholds
//! - feat:scanning.signature-strength-levels

#![forbid(unsafe_code)]

use crate::common;
use eve_db::types::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::space_probe_scanning::{
    compute_probe_max_deviation_au, probe_allowed_ranges_au, probe_base_max_deviation_au,
    probe_base_scan_range_au, probe_max_flight_time_s,
    signature_difficulty_level_and_min_strength,
};
use eve_server::services::space_destiny::wormhole_item_id_for_signature;
use glam::DVec3;
use std::collections::HashMap;

const AU_IN_METERS: f64 = 149_597_870_700.0;

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
        ..Default::default()
    }
}

async fn spawn_test_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    mass: f64,
    cloaked: bool,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.shield_hp = 10_000.0;
    cs.max_shield_hp = 10_000.0;
    cs.is_cloaked = cloaked;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    solar_system_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                solar_system_id as i64,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
    });
    spawn_test_ball(ctx, solar_system_id, ship_id, ship_type_id, DVec3::ZERO, 1_200_000.0, false).await;
    sess
}

// parity: feat:scanning.moon-survey-probe-launch-rules
#[tokio::test]
async fn test_scanning_moon_survey_probe_launch_rules() {
    eve_testkit::attest(&["feat:scanning.moon-survey-probe-launch-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let station_id = 60_003_760i64;
    let mut session = SessionState::new(1001);
    common::login_pilot(&ctx, &mut session, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut session,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Launcher: 1 per ship, needs no hardpoint, auto-reloads, does not auto-repeat
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3412, 3402, 3410, 25810, 25811] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
        let sid = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(100_000_001);
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.type_id = 32880; // Venture (0 launcher hardpoints)
            ship.dynamic_attributes.insert(48, 500.0);
            ship.dynamic_attributes.insert(11, 500.0);
        }
        snap.items.retain(|_, item| item.location_id != sid);
        snap.items.insert(800_000_301, make_item(800_000_301, 33270, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_302, make_item(800_000_302, 33272, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_303, make_item(800_000_303, 18635, char_id, sid, InventoryFlag::Cargo, 3, false));
        sid
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(800_000_301), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot0 as i64)],
        &kw,
        None,
    )
    .await
    .expect("Survey Probe Launcher I fits on ship with 0 launcher hardpoints");

    let second_launcher_err = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(800_000_302), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot1 as i64)],
            &kw,
            None,
        )
        .await
        .expect_err("Second Survey Probe Launcher must fail maxGroupFitted == 1");
    assert_eq!(second_launcher_err.code, "FittingValidationFailed");
    assert!(second_launcher_err.message.contains("MaxGroupFittedExceeded"));

    // 2. Launching: must be launched aligned at moon within 100,000 km; misses return no result; probe consumed
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    let jita = ctx.systems.get_or_spawn(30000142);
    let moon_40009087_pos = jita.get_ball_snapshot(40009087).await.unwrap().unwrap().1.position;
    let ship_near_moon = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;

    // Launch aligned away from moon -> miss returns no result
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = -(moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_unaligned = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)],
            &kw,
            None,
        )
        .await
        .expect("Launch while aligned away");
    assert_eq!(miss_unaligned.dict_get("moonID"), Some(&PyRep::None));
    assert_eq!(miss_unaligned.dict_get("flightTimeSec").and_then(|v| v.as_i64()), Some(300));
    assert_eq!(miss_unaligned.dict_get("remainingInLauncher").and_then(|v| v.as_i64()), Some(1));
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&800_000_301)));

    let miss_probe_id = miss_unaligned.dict_get("probeID").unwrap().as_i64().unwrap();
    let launch_notifs = session.drain_notifications();
    assert!(launch_notifs.iter().any(|n| n.0 == "OnNewMoonProbe"));
    assert!(!launch_notifs.iter().any(|n| n.0 == "OnMoonScanComplete"));

    // Probes are consumed and cannot be recalled
    let recover_res = ctx
        .dispatch_rpc(
            &mut session,
            "scanMgr",
            None,
            "RecoverProbes",
            &[PyRep::List(vec![PyRep::Integer(miss_probe_id)])],
            &kw,
            None,
        )
        .await
        .expect("RecoverProbes");
    assert_eq!(recover_res.dict_get("recoveredCount").and_then(|v| v.as_i64()), Some(0));

    ctx.advance_secs(300);
    let miss_arr_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(miss_arr_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!miss_arr_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

    // Launch at moon > 100,000 km away -> miss returns no result
    let moon_40009081_pos = jita.get_ball_snapshot(40009081).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009081_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_range = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009081)],
            &kw,
            None,
        )
        .await
        .expect("Launch at moon > 100,000 km");
    assert_eq!(miss_range.dict_get("moonID"), Some(&PyRep::None));
    assert_eq!(miss_range.dict_get("remainingInLauncher").and_then(|v| v.as_i64()), Some(1));
    ctx.advance_secs(300);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 3. Results: arrive after probe flight time (T2 launcher -10%); pilot must stay undocked in system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&800_000_301) {
            m.type_id = 33272; // Survey Probe Launcher II (-10% flight time)
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.item_id != 800_000_301));
        snap.items.insert(800_000_304, make_item(800_000_304, 18637, char_id, ship_id, InventoryFlag::HiSlot0, 1, false)); // Gaze (150s)
        snap.items.insert(800_000_305, make_item(800_000_305, 18626, char_id, ship_id, InventoryFlag::Cargo, 1, false)); // Quest (1200s)
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let gaze_launch = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)],
            &kw,
            None,
        )
        .await
        .expect("Launch Gaze Survey Probe I from T2 launcher");
    assert_eq!(gaze_launch.dict_get("moonID").and_then(|v| v.as_i64()), Some(40009087));
    // T2 duration bonus: 150s * 0.9 = 135s
    assert_eq!(gaze_launch.dict_get("flightTimeSec").and_then(|v| v.as_i64()), Some(135));

    // If pilot docks before arrival, results are not delivered
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None)
        .await
        .expect("Dock before probe arrives");
    let _ = session.drain_notifications();
    ctx.advance_secs(135);
    let docked_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(!docked_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

    // Now launch aligned and stay undocked in system: results delivered
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock again");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let aligned_launch = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)],
            &kw,
            None,
        )
        .await
        .expect("Launch aligned from T2 launcher");
    // Quest Survey Probe I (1200s) was auto-reloaded into launcher; T2 launcher -10% => 1080s
    assert_eq!(aligned_launch.dict_get("flightTimeSec").and_then(|v| v.as_i64()), Some(1080));

    // Advance 1080s in space: OnMoonScanComplete arrives with moon products
    ctx.advance_secs(1080);
    let arrival_notifs = ctx.drain_notifications_for_char(char_id);
    let scan_notif = arrival_notifs.iter().find(|n| n.event == "OnMoonScanComplete");
    assert!(scan_notif.is_some(), "OnMoonScanComplete must arrive for undocked pilot");
    let payload = &scan_notif.unwrap().args;
    assert_eq!(payload.first().and_then(|v| v.as_i64()), Some(40009087));
    let Some(PyRep::Dict(entries)) = payload.get(1) else { panic!("expected Dict of moon products") };
    assert!(!entries.is_empty(), "Moon survey must report products with typeID and fraction");
    for (k, v) in entries {
        assert!(k.as_i64().is_some() && k.as_i64().unwrap() > 0, "Product must have typeID");
        let frac = v.as_f64().unwrap();
        assert!(frac > 0.0 && frac <= 1.0, "Product fraction must be between 0 and 1");
    }
}

// parity: feat:scanning.probe-range-deviation, feat:scanning.probe-target-classes, feat:scanning.signature-reveal-thresholds, feat:scanning.signature-strength-levels
#[tokio::test]
async fn test_scanning_probe_range_deviation_targets_thresholds_and_levels() {
    eve_testkit::attest(&[
        "feat:scanning.probe-range-deviation",
        "feat:scanning.probe-target-classes",
        "feat:scanning.signature-reveal-thresholds",
        "feat:scanning.signature-strength-levels",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, sys_id, ship_id, launcher_id) = (90_000_001, 30_000_142u32, 610_000_001i64, 610_000_010i64);
    let mut sess = setup_pilot_in_space(&ctx, 701, char_id, sys_id, ship_id, 606).await;

    // 1. Core vs Combat probe range steps (0.25..=32 AU vs 0.5..=64 AU) & flight time from SDE
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30013), [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0]);
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30028), [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]);
    assert_eq!((probe_base_scan_range_au(&ctx.sde, 30013), probe_base_scan_range_au(&ctx.sde, 30028)), (0.25, 0.5));
    assert_eq!((probe_base_max_deviation_au(&ctx.sde, 30013), probe_base_max_deviation_au(&ctx.sde, 30028)), (0.125, 0.25));
    assert_eq!(
        (
            probe_max_flight_time_s(&ctx.sde, 32246),
            probe_max_flight_time_s(&ctx.sde, 30013),
            probe_max_flight_time_s(&ctx.sde, 30028),
        ),
        (1_000.0, 4_000.0, 4_000.0)
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(48, 2_000.0);
            sh.dynamic_attributes.insert(11, 2_000.0);
        }
        snap.items.insert(launcher_id, make_item(launcher_id, 17901, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(610_000_011, make_item(610_000_011, 32246, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let launch_rss = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None)
        .await
        .expect("Launch RSS");
    assert_eq!(launch_rss.dict_get("maxFlightTimeS").and_then(|v| v.as_f64()), Some(1_000.0));
    assert_eq!(launch_rss.dict_get("canScanShips"), Some(&PyRep::Bool(false)));

    // Advance 1,001s (> 16m40s): background cluster tick expires RSS probes and returns them to cargo
    ctx.advance_secs(1_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let rss_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 32246).map(|i| i.quantity).sum()
    });
    assert_eq!(rss_in_cargo, 8, "Background cluster tick returned 8 expired RSS probes to cargo");

    // Standard probes (4000s = 1h6m40s) when cargo is full -> probes are lost
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 0.1);
        snap.items.insert(610_000_012, make_item(610_000_012, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    ctx.advance_secs(4_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let std_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 30013).map(|i| i.quantity).sum()
    });
    assert_eq!(std_in_cargo, 0, "Expired probes are lost when cargo hold is full");

    // 2. Live RPC range steps & deviation scaling + difficulty levels + reveal thresholds
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.unwrap();
    let sig_pos = [300_000.0, 0.0, 0.0];
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 1_000.0);
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&3412);
            sk.remove(&25810);
            sk.remove(&25811);
        }
        let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
        sigs.clear();
        for (id, grp, name) in [
            ("TST-LV1", "Relic Site", "Desolate Collapsed Conduit"),
            ("TST-LV4", "Data Site", "Abandoned Research Complex DA005"),
            ("TST-LV5", "Data Site", "Unsecured Core Backup Array"),
        ] {
            sigs.push(CosmicSignatureRecord {
                signature_id: id.into(),
                solar_system_id: sys_id,
                sig_group: grp.into(),
                type_name: name.into(),
                signal_strength: 0.0,
                position: sig_pos,
                wormhole_type_id: None,
                destination_system_id: None,
                remaining_mass_kg: 0.0,
                max_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                created_unix: now_u,
                lifetime_seconds: 86_400,
                collapsed: false,
            });
        }
        snap.items.insert(610_000_013, make_item(610_000_013, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });

    let sigs_snap = ctx.db.with_snapshot(|s| s.cosmic_signatures[&sys_id].clone());
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[0]), (1, 42.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[1]), (4, 72.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[2]), (5, 104.0));

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();

    let make_coords_step = |covering: usize, step: i64| -> PyRep {
        PyRep::List(
            (0..8)
                .map(|i| {
                    let p = if i < covering { sig_pos } else { [9.0e12, 9.0e12, 9.0e12] };
                    PyRep::List(vec![PyRep::Real(p[0]), PyRep::Real(p[1]), PyRep::Real(p[2]), PyRep::Integer(step)])
                })
                .collect(),
        )
    };
    let find_row = |rep: &PyRep, id: &str| -> PyRep {
        let PyRep::List(rows) = rep else { panic!() };
        rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(id)).unwrap().clone()
    };

    // Untrained character (Astrometrics 0, Pinpointing 0) at step 4 (2.0 AU): maxDeviationAU = 1.0 AU
    let scan_untrained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_untrained = find_row(&scan_untrained_step4, "TST-LV1");
    assert_eq!(row_untrained.dict_get("scanRangeAU").and_then(|v| v.as_f64()), Some(2.0));
    assert!((row_untrained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-9);
    let sig_untrained_s = row_untrained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();

    // Train Astrometrics IV + Pinpointing III at step 4 (2.0 AU): maxDeviationAU = 0.68 AU
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3412, TrainedSkillRecord { type_id: 3412, level: 4, skill_points: 45_255.0 });
        sk.insert(25811, TrainedSkillRecord { type_id: 25811, level: 3, skill_points: 16_000.0 });
    });
    let expected_dev_au = compute_probe_max_deviation_au(0.125, 2.0, 0.25, 4, 3);
    let scan_trained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_trained = find_row(&scan_trained_step4, "TST-LV1");
    assert!((row_trained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - expected_dev_au).abs() < 1e-9);
    let sig_trained_s = row_trained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();
    assert!(sig_trained_s > sig_untrained_s);
    let expected_dev_m = (1.0 - sig_trained_s) * expected_dev_au * AU_IN_METERS;
    assert!((row_trained.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() - expected_dev_m).abs() < 1.0);

    // Set Astrometrics II for threshold tests
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 2, skill_points: 8_000.0 });
    });

    // < 25%: only signature ID and deviation shown
    let scan_1 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(1, 1)], &kw, None).await.unwrap();
    let lv1_r1 = find_row(&scan_1, "TST-LV1");
    assert!(lv1_r1.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap() < 0.25);
    assert_eq!(lv1_r1.dict_get("group").and_then(|v| v.as_str()), Some("Unknown"));
    assert_eq!(lv1_r1.dict_get("groupName"), Some(&PyRep::None));
    assert_eq!(lv1_r1.dict_get("siteName"), Some(&PyRep::None));
    assert_eq!(lv1_r1.dict_get("warpable"), Some(&PyRep::Bool(false)));
    assert!(lv1_r1.dict_get("difficultyLevel").is_none());
    assert!(lv1_r1.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() > 0.0);

    // 25%..75%: group shown, siteName hidden, not warpable
    let scan_2 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 1)], &kw, None).await.unwrap();
    let lv1_r2 = find_row(&scan_2, "TST-LV1");
    assert!((0.25..0.75).contains(&lv1_r2.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv1_r2.dict_get("group").and_then(|v| v.as_str()), Some("Relic Site"));
    assert_eq!(lv1_r2.dict_get("siteName"), Some(&PyRep::None));
    assert_eq!(lv1_r2.dict_get("warpable"), Some(&PyRep::Bool(false)));

    // 75%..100% & strength-level gating: Lv I resolved (100%), Lv IV at 75-99% (unresolved)
    let scan_low = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_low, "TST-LV1").dict_get("warpable"), Some(&PyRep::Bool(true)));
    let lv4_low = find_row(&scan_low, "TST-LV4");
    assert!((0.75..1.0).contains(&lv4_low.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv4_low.dict_get("siteName").and_then(|v| v.as_str()), Some("Abandoned Research Complex DA005"));
    assert_eq!(lv4_low.dict_get("warpable"), Some(&PyRep::Bool(false)));

    // Warping to unresolved Lv IV (<100%) fails; warping to resolved Lv I (100%) succeeds
    let beyonce_bound = ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap();
    let (lv4_ball_id, lv1_ball_id) = (wormhole_item_id_for_signature(sys_id, "TST-LV4"), wormhole_item_id_for_signature(sys_id, "TST-LV1"));
    for bid in [lv4_ball_id, lv1_ball_id] {
        spawn_test_ball(&ctx, sys_id, bid, 26272, DVec3::new(300_000.0, 0.0, 0.0), 1_000.0, false).await;
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv4_ball_id)], &kw, None).await.unwrap_err().code,
        "InvalidWarpDestination"
    );
    assert!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv1_ball_id)], &kw, None).await.is_ok());

    // Astrometrics V -> resolves Lv IV (>=72), fails Lv V (<104)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 5, skill_points: 256_000.0 });
    });
    let scan_astro5 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_astro5, "TST-LV4").dict_get("warpable"), Some(&PyRep::Bool(true)));
    assert_eq!(find_row(&scan_astro5, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(false)));

    // Sisters Core Probes + Astrometric Rangefinding V -> resolves Lv V (>=104)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(25810, TrainedSkillRecord { type_id: 25810, level: 5, skill_points: 512_000.0 });
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_015, make_item(610_000_015, 30488, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    let scan_105 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_105, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(true)));

    // 3. Target classes: Core probes resolve CosmicSignatures only; Combat probes also resolve Ships, Drones, Structures;
    // wrecks, cargo containers, and cloaked ships never appear
    let (target_ship, cloaked_ship, drone_id, structure_id, wreck_id, cargo_can) = (610_000_101, 610_000_102, 610_000_103, 610_000_104, 610_000_105, 610_000_106);
    for (bid, tid, cloaked) in [
        (target_ship, 587u32, false),
        (cloaked_ship, 11188, true),
        (drone_id, 2454, false),
        (structure_id, 35832, false),
        (wreck_id, 26561, false),
        (cargo_can, 23, false),
    ] {
        spawn_test_ball(&ctx, sys_id, bid, tid, DVec3::from_array(sig_pos), 1_000.0, cloaked).await;
    }
    let PyRep::List(rows_105) = &scan_105 else { panic!() };
    assert!(rows_105.iter().all(|r| r.dict_get("targetClass").and_then(|v| v.as_str()) == Some("CosmicSignature")));

    // Launch Combat Probes
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_014, make_item(610_000_014, 30486, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let launch_combat = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    assert_eq!(launch_combat.dict_get("canScanShips"), Some(&PyRep::Bool(true)));

    let combat_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    let PyRep::List(combat_rows) = &combat_scan else { panic!() };
    let scanned_ids: Vec<i64> = combat_rows.iter().filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64())).collect();
    assert!(scanned_ids.contains(&target_ship) && scanned_ids.contains(&drone_id) && scanned_ids.contains(&structure_id));
    assert!(!scanned_ids.contains(&cloaked_ship) && !scanned_ids.contains(&wreck_id) && !scanned_ids.contains(&cargo_can));
}

#[tokio::test]
async fn test_gh962_probe_launcher_sublocations_godma_and_ship_state() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(7962);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let hi_slot = InventoryFlag::HiSlot0 as i64;

    let launcher_id = 610_962_010i64;
    let cargo_core_id = 610_962_011i64;
    let cargo_combat_id = 610_962_012i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(48, 2_000.0);
            sh.dynamic_attributes.insert(11, 2_000.0);
        }
        snap.items
            .retain(|_, i| !(i.location_id == ship_id && matches!(i.flag, InventoryFlag::HiSlot0)));
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3412u32, 3426, 3413] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        // Expanded Probe Launcher I (18639, 8.0 m3 capacity) supports both Core and Combat Scanner Probes.
        snap.items.insert(
            launcher_id,
            make_item(launcher_id, 18639, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            cargo_core_id,
            make_item(cargo_core_id, 30013, char_id, ship_id, InventoryFlag::Cargo, 8, false),
        );
        snap.items.insert(
            cargo_combat_id,
            make_item(cargo_combat_id, 30028, char_id, ship_id, InventoryFlag::Cargo, 8, false),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let subloc_key = |charge_type_id: u32| -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(hi_slot),
            PyRep::Integer(i64::from(charge_type_id)),
        ])
    };
    let collect_notes = |s: &mut SessionState| -> Vec<(String, Vec<PyRep>)> {
        let mut all = s.drain_notifications();
        all.extend(
            ctx.drain_notifications_for_char(char_id)
                .into_iter()
                .map(|n| (n.event, n.args)),
        );
        all
    };
    let assert_ship_state_subloc =
        |ship_state: &[PyRep], expected: Option<(u32, i32)>| {
            assert_eq!(ship_state.len(), 4);
            let PyRep::Dict(ref cache) = ship_state[1] else {
                panic!("shipState[1] must be Dict, got {:?}", ship_state[1]);
            };
            let flag_dict = cache
                .iter()
                .find(|(k, _)| k.as_i64() == Some(ship_id))
                .map(|(_, v)| v);
            match expected {
                None => {
                    let has_hi = flag_dict.is_some_and(|fd| {
                        matches!(fd, PyRep::Dict(m) if m.iter().any(|(fk, _)| fk.as_i64() == Some(hi_slot)))
                    });
                    assert!(!has_hi, "empty launcher must not emit shipState[1] sublocation row");
                }
                Some((exp_tid, exp_qty)) => {
                    let Some(PyRep::Dict(ref fdict)) = flag_dict else {
                        panic!("shipState[1] missing ship_id {ship_id}: {cache:?}");
                    };
                    let row = fdict
                        .iter()
                        .find(|(fk, _)| fk.as_i64() == Some(hi_slot))
                        .map(|(_, rv)| rv)
                        .expect("shipState[1][ship_id] missing HiSlot0");
                    assert!(
                        row.dbrow_columns().is_some(),
                        "shipState[1] entry must be a packed DBRow: {row:?}"
                    );
                    assert_eq!(row.dbrow_get("locationID").and_then(|v| v.as_i64()), Some(ship_id));
                    assert_eq!(row.dbrow_get("flagID").and_then(|v| v.as_i64()), Some(hi_slot));
                    assert_eq!(row.dbrow_get("typeID").and_then(|v| v.as_i64()), Some(i64::from(exp_tid)));
                    assert_eq!(row.dbrow_get("quantity").and_then(|v| v.as_i64()), Some(i64::from(exp_qty)));
                }
            }
        };

    // 1. Negative check: empty launcher must have empty `shipState[1]` for HiSlot0.
    let empty_all = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("GetAllInfo empty");
    let PyRep::Tuple(ref empty_ship_state) =
        empty_all.dict_get("shipState").expect("shipState on GetAllInfo")
    else {
        panic!("shipState must be 4-tuple");
    };
    assert_ship_state_subloc(empty_ship_state, None);
    let _ = collect_notes(&mut sess);

    // 2. Live `dogmaIM.LoadAmmo` (Core Scanner Probe I, 30013 x8): must prime sublocation BEFORE `OnChargeLoadedToModule`.
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(launcher_id)]),
            PyRep::List(vec![PyRep::Integer(cargo_core_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo 30013");
    let load_notes = collect_notes(&mut sess);
    let prime_idx = load_notes
        .iter()
        .position(|(ev, args)| {
            ev == "OnGodmaPrimeItem"
                && args.get(1).and_then(|info| info.dict_get("itemID")) == Some(&subloc_key(30013))
        })
        .expect("LoadAmmo must emit OnGodmaPrimeItem for sublocation (ship_id, HiSlot0, 30013)");
    let loaded_idx = load_notes
        .iter()
        .position(|(ev, _)| ev == "OnChargeLoadedToModule")
        .expect("LoadAmmo must emit OnChargeLoadedToModule");
    assert!(
        prime_idx < loaded_idx,
        "OnGodmaPrimeItem sublocation prime must precede OnChargeLoadedToModule"
    );
    let prime_kv = &load_notes[prime_idx].1[1];
    assert_eq!(prime_kv.dict_get("invItem"), Some(&PyRep::None));
    let PyRep::Dict(ref prime_attrs) = prime_kv.dict_get("attributes").expect("prime attributes") else {
        panic!("prime attributes must be Dict");
    };
    assert!(
        prime_attrs
            .iter()
            .any(|(k, v)| k.as_i64() == Some(805) && v.as_f64() == Some(8.0)),
        "primed sublocation must include attributeQuantity (805) = 8.0: {prime_attrs:?}"
    );
    assert!(
        !prime_attrs.iter().any(|(k, _)| k.as_i64() == Some(80)),
        "primed sublocation must not emit speedBonus (80): {prime_attrs:?}"
    );

    // 3. Cold reconnect `GetAllInfo`, `ItemGetInfo` sublocation key lookup, and `ship.Board` prime.
    let loaded_all = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("GetAllInfo loaded");
    let PyRep::Dict(ref ship_info) = loaded_all.dict_get("shipInfo").expect("shipInfo") else {
        panic!("shipInfo must be Dict");
    };
    let (_, subloc_entry) = ship_info
        .iter()
        .find(|(k, _)| *k == subloc_key(30013))
        .expect("GetAllInfo.shipInfo must contain sublocation tuple key (ship_id, HiSlot0, 30013)");
    assert_eq!(subloc_entry.dict_get("invItem"), Some(&PyRep::None));
    let PyRep::Dict(ref sub_attrs) = subloc_entry.dict_get("attributes").expect("subloc attributes") else {
        panic!("sublocation attributes must be Dict");
    };
    assert!(
        sub_attrs
            .iter()
            .any(|(k, v)| k.as_i64() == Some(805) && v.as_f64() == Some(8.0)),
        "GetAllInfo sublocation must carry attributeQuantity (805) = 8.0: {sub_attrs:?}"
    );
    assert!(
        !sub_attrs.iter().any(|(k, _)| k.as_i64() == Some(80)),
        "GetAllInfo sublocation must not carry speedBonus (80): {sub_attrs:?}"
    );
    let PyRep::Tuple(ref loaded_ship_state) =
        loaded_all.dict_get("shipState").expect("shipState")
    else {
        panic!("shipState must be 4-tuple");
    };
    assert_ship_state_subloc(loaded_ship_state, Some((30013, 8)));

    let get_info_sub = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[subloc_key(30013)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo with 3-tuple sublocation key");
    assert_eq!(get_info_sub.dict_get("invItem"), Some(&PyRep::None));
    let PyRep::Dict(ref item_info_attrs) =
        get_info_sub.dict_get("attributes").expect("ItemGetInfo attributes")
    else {
        panic!("ItemGetInfo attributes must be Dict");
    };
    assert!(
        item_info_attrs
            .iter()
            .any(|(k, v)| k.as_i64() == Some(805) && v.as_f64() == Some(8.0)),
        "ItemGetInfo sublocation must carry attributeQuantity (805) = 8.0: {item_info_attrs:?}"
    );
    assert!(
        !item_info_attrs.iter().any(|(k, _)| k.as_i64() == Some(80)),
        "ItemGetInfo sublocation must not carry speedBonus (80): {item_info_attrs:?}"
    );

    // Re-board ship to verify `ship.Board` returns `shipState[1]` DBRow and emits `ProcessGodmaPrimeLocation`.
    ctx.advance_secs(31);
    let _ = collect_notes(&mut sess);
    let board_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("ship.Board");
    let PyRep::Tuple(ref board_ship_state) = board_rep else {
        panic!("ship.Board must return 4-tuple shipState");
    };
    assert_ship_state_subloc(board_ship_state, Some((30013, 8)));
    let board_notes = collect_notes(&mut sess);
    let prime_loc = board_notes
        .iter()
        .find(|(ev, _)| ev == "ProcessGodmaPrimeLocation")
        .expect("ship.Board must emit ProcessGodmaPrimeLocation");
    let PyRep::Dict(ref loc_items) = prime_loc.1[1] else {
        panic!("ProcessGodmaPrimeLocation[1] must be Dict");
    };
    assert!(
        loc_items.iter().any(|(k, _)| *k == subloc_key(30013)),
        "ProcessGodmaPrimeLocation must prime probe sublocation key"
    );

    // 4. Probe type swap (`LoadAmmo` with 30028 Combat Scanner Probe I): must expel old 30013 (`attr 805 -> 0.0`) & prime 30028.
    let _ = collect_notes(&mut sess);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(launcher_id)]),
            PyRep::List(vec![PyRep::Integer(cargo_combat_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo swap to 30028");
    let swap_notes = collect_notes(&mut sess);
    let has_attr805_delta = |notes: &[(String, Vec<PyRep>)], charge_tid: u32, expected_new: f64| -> bool {
        let key = subloc_key(charge_tid);
        notes.iter().any(|(ev, args)| {
            ev == "OnModuleAttributeChanges"
                && matches!(args.first(), Some(PyRep::List(entries)) if entries.iter().any(|entry| {
                    matches!(entry, PyRep::Tuple(t)
                        if t.get(2) == Some(&key)
                            && t.get(3).and_then(|v| v.as_i64()) == Some(805)
                            && t.get(5).and_then(|v| v.as_f64()) == Some(expected_new))
                }))
        })
    };
    assert!(
        has_attr805_delta(&swap_notes, 30013, 0.0),
        "swapping probe types must expel old 30013 sublocation with attr 805 = 0.0: {swap_notes:?}"
    );
    assert!(
        swap_notes.iter().any(|(ev, args)| {
            ev == "OnGodmaPrimeItem"
                && args.get(1).and_then(|info| info.dict_get("itemID")) == Some(&subloc_key(30028))
        }),
        "swapping probe types must prime new 30028 sublocation: {swap_notes:?}"
    );

    // 5. Undock -> `launchProbe` (`attr 805 -> 0.0`) -> `RecoverProbes` (`OnGodmaPrimeItem` + `OnChargeLoadedToModule`) -> `UnloadAmmo` (`attr 805 -> 0.0`).
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let _ = collect_notes(&mut sess);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())],
        &kw,
        None,
    )
    .await
    .expect("launchProbe");
    let launch_notes = collect_notes(&mut sess);
    assert!(
        has_attr805_delta(&launch_notes, 30028, 0.0),
        "launching last 8 probes must update sublocation attr 805 -> 0.0: {launch_notes:?}"
    );

    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None)
        .await
        .expect("RecoverProbes");
    let recover_notes = collect_notes(&mut sess);
    assert!(
        recover_notes.iter().any(|(ev, args)| {
            ev == "OnGodmaPrimeItem"
                && args.get(1).and_then(|info| info.dict_get("itemID")) == Some(&subloc_key(30028))
        }),
        "RecoverProbes into empty launcher must emit OnGodmaPrimeItem sublocation prime: {recover_notes:?}"
    );
    assert!(
        recover_notes.iter().any(|(ev, _)| ev == "OnChargeLoadedToModule"),
        "RecoverProbes must emit OnChargeLoadedToModule: {recover_notes:?}"
    );

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnloadAmmo",
        &[PyRep::Integer(ship_id), PyRep::PySet(vec![PyRep::Integer(launcher_id)])],
        &kw,
        None,
    )
    .await
    .expect("UnloadAmmo");
    let unload_notes = collect_notes(&mut sess);
    assert!(
        has_attr805_delta(&unload_notes, 30028, 0.0),
        "UnloadAmmo must expel sublocation with attr 805 -> 0.0: {unload_notes:?}"
    );
}

#[tokio::test]
async fn test_gh1034_mining_anomaly_not_duplicated_in_signal_tracker_or_probe_scan() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, sys_id, ship_id, launcher_id) =
        (90_000_001_i64, 30_000_119_u32, 610_103_401_i64, 610_103_410_i64);
    let mut sess = setup_pilot_in_space(&ctx, 71034, char_id, sys_id, ship_id, 606).await;

    // Trigger real SDE mining-site seeding on sys 30000119 (the #1034 system),
    // which inserts DNG-* OreAnomaly records into both cosmic_anomalies and cosmic_signatures.
    common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("dungeonInstanceCacheMgr", None, "GetOreAnomalyInstances", &[], &kw)
        .await;

    let sig_pos = [1.0e9_f64, 0.0, 0.0];
    let now = ctx.now_unix();
    let seeded_ore_ids: Vec<String> = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3412_u32, 3426, 25810, 25811] {
            sk.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(48, 2_000.0);
            sh.dynamic_attributes.insert(11, 2_000.0);
        }
        snap.items.insert(
            launcher_id,
            make_item(launcher_id, 17901, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            610_103_411,
            make_item(610_103_411, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false),
        );

        {
            let anoms = snap.cosmic_anomalies.entry(sys_id).or_default();
            anoms.push(eve_db::types::CosmicAnomalyRecord {
                anomaly_id: "ICE-1034".into(),
                solar_system_id: sys_id,
                anomaly_type: "IceBelt".into(),
                name: "Ice Belt".into(),
                signal_strength: 1.0,
                position: [2.0e9, 0.0, 0.0],
                ore_types: vec![16262],
                remaining_volume: 100_000.0,
                created_unix: now,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        }
        {
            let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
            sigs.push(CosmicSignatureRecord {
                signature_id: "ICE-1034".into(),
                solar_system_id: sys_id,
                sig_group: "IceBelt".into(),
                type_name: "Ice Belt".into(),
                signal_strength: 1.0,
                position: [2.0e9, 0.0, 0.0],
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
            sigs.push(CosmicSignatureRecord {
                signature_id: "SIG-OK-1034".into(),
                solar_system_id: sys_id,
                sig_group: "Relic Site".into(),
                type_name: "Ruined Guristas Monument".into(),
                signal_strength: 0.0,
                position: sig_pos,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 3_600,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        }
        snap.cosmic_anomalies
            .get(&sys_id)
            .map(|anoms| {
                anoms
                    .iter()
                    .filter(|a| matches!(a.anomaly_type.as_str(), "OreAnomaly" | "IceBelt"))
                    .map(|a| a.anomaly_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert!(
        seeded_ore_ids.iter().any(|id| id.starts_with("DNG-")) && seeded_ore_ids.iter().any(|id| id == "ICE-1034"),
        "expected seeded DNG-* OreAnomaly and ICE-1034 in sys {sys_id}, got {seeded_ore_ids:?}"
    );
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    let mut harness = common::wrap_wire_harness(&ctx, sess);

    // 1. SignalTrackerRegister -> OnSignalTrackerFullState must have zero overlap between anomalies and signatures.
    harness
        .call_ok("scanMgr", None, "SignalTrackerRegister", &[], &kw)
        .await;
    let notes = harness.drain_notifications();
    let (_, st_args) = notes
        .into_iter()
        .find(|(ev, _)| ev == "OnSignalTrackerFullState")
        .expect("OnSignalTrackerFullState notification");
    let PyRep::Tuple(ref parts) = st_args[1] else {
        panic!("expected 4-tuple fullState, got {:?}", st_args[1]);
    };
    let PyRep::Dict(ref anom_dict) = parts[0] else {
        panic!("expected anom Dict");
    };
    let PyRep::Dict(ref sig_dict) = parts[1] else {
        panic!("expected sig Dict");
    };
    let st_anom_ids: Vec<&str> = anom_dict.iter().filter_map(|(k, _)| k.as_str()).collect();
    let st_sig_ids: Vec<&str> = sig_dict.iter().filter_map(|(k, _)| k.as_str()).collect();
    for id in &seeded_ore_ids {
        assert!(st_anom_ids.contains(&id.as_str()), "missing anomaly {id} in fullState[0]");
        assert!(
            !st_sig_ids.contains(&id.as_str()),
            "anomaly {id} leaked into OnSignalTrackerFullState signatures dict (#1034): {st_sig_ids:?}"
        );
    }
    assert!(st_sig_ids.contains(&"SIG-OK-1034"));

    // 2. GetSystemScanMgr (wire SubStruct), GetFullState, GetSignatures, and RequestScans must never return anomaly-mirrored signatures.
    let scan_mgr = harness
        .call_ok("scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(i64::from(sys_id))], &kw)
        .await;
    let scan_mgr_oid = eve_testkit::client_harness::extract_bound_id_from_wire_rep(&scan_mgr)
        .expect("GetSystemScanMgr must return bound SubStruct over wire");

    let full_state = harness
        .call_ok("scanMgr", Some(scan_mgr_oid), "GetFullState", &[], &kw)
        .await;
    let PyRep::Tuple(ref fs_parts) = full_state else {
        panic!("expected GetFullState tuple");
    };
    let PyRep::Dict(ref fs_sigs) = fs_parts[1] else {
        panic!("expected GetFullState sigs Dict");
    };
    for id in &seeded_ore_ids {
        assert!(
            !fs_sigs.iter().any(|(k, _)| k.as_str() == Some(id.as_str())),
            "GetFullState leaked {id}"
        );
    }

    let get_sigs = harness
        .call_ok("scanMgr", None, "GetSignatures", &[], &kw)
        .await;
    let PyRep::Rowset { rows: sig_rows, .. } = get_sigs else {
        panic!("expected Rowset from GetSignatures");
    };
    for id in &seeded_ore_ids {
        assert!(
            !sig_rows.iter().any(|r| r.first().and_then(|v| v.as_str()) == Some(id.as_str())),
            "GetSignatures leaked {id}"
        );
    }

    harness
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())],
            &kw,
        )
        .await;
    let probe_coords = PyRep::List(
        (0..8)
            .map(|_| PyRep::List(vec![PyRep::Real(sig_pos[0]), PyRep::Real(sig_pos[1]), PyRep::Real(sig_pos[2]), PyRep::Integer(1)]))
            .collect(),
    );
    let scan_res = harness
        .call_ok("scanMgr", None, "RequestScans", &[probe_coords], &kw)
        .await;
    let PyRep::List(ref scan_rows) = scan_res else {
        panic!("expected List from RequestScans");
    };
    let scanned_ids: Vec<&str> = scan_rows
        .iter()
        .filter_map(|r| r.dict_get("signatureID").and_then(|v| v.as_str()))
        .collect();
    for id in &seeded_ore_ids {
        assert!(!scanned_ids.contains(&id.as_str()), "RequestScans leaked {id}");
    }
    assert!(scanned_ids.contains(&"SIG-OK-1034"));
}
