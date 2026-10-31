//! End-to-End Integration Tests closing Interdiction & MJD/Propulsion parity rows (Part 2):
//! `feat:navigation.warp-bubble-drag-and-catch`, `feat:navigation.interdiction-nullification-module`,
//! `feat:modules.interdiction-probe-variants`, `feat:navigation.micro-jump-drive-spool`,
//! and `feat:navigation.warp-scramble-propulsion-shutoff`.

use crate::modules_advanced_tactical::*;
use eve_db::{FleetRecord, InventoryFlag};
use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_server::services::insurgency_services::{
    add_system_corruption, init_insurgency_campaign, FACTION_ANGEL_CARTEL, FORECASTING_DURATION_SECS,
};
use eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect;
use std::collections::HashMap;

#[tokio::test]
async fn test_warp_bubble_drag_catch_nullifier_and_probe_variants() {
    // parity: feat:navigation.warp-bubble-drag-and-catch, feat:navigation.interdiction-nullification-module, feat:modules.interdiction-probe-variants
    eve_testkit::attest(&[
        "feat:navigation.warp-bubble-drag-and-catch",
        "feat:navigation.interdiction-nullification-module",
        "feat:modules.interdiction-probe-variants",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
    let sabre_char: i64 = 94_021;
    let sabre_id: i64 = 84_021;
    let target_char: i64 = 94_022;
    let target_ship_id: i64 = 84_022;
    let shuttle_char: i64 = 94_023;
    let shuttle_id: i64 = 84_023;

    let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
    let mut target_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, null_sys, false);
    let mut shuttle_sess = seed_pilot(&ctx, shuttle_char, shuttle_id, 672, null_sys, false);
    let sys = ctx.systems.get_or_spawn(null_sys);

    // 1. Warp Bubble Catch & Drag + Shuttle Immunity (`feat:navigation.warp-bubble-drag-and-catch`)
    let origin = DVec3::new(10_000_000.0, 0.0, 0.0);
    let dest = DVec3::new(30_000_000.0, 0.0, 0.0);
    let dest_beacon_id: i64 = 88_701;
    sys.spawn_ball(Ball {
        id: dest_beacon_id, type_id: 15, position: dest, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();

    // 1a. Mid-path bubble at 20,000 km on a 30,000 km warp (10,000 km from destination) must NOT catch
    let mid_bubble_id: i64 = 88_702;
    sys.add_warp_bubble(mid_bubble_id, DVec3::new(20_000_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;
    // Step 1 tick first to prove reconcile_tackle_and_bubbles preserves Shuttle passive SDE 1538 immunity!
    sys.step_manual(1.0).await.unwrap();

    let b_target = ctx.bind_object(&mut target_sess, "beyonce", null_sys as i64).unwrap();
    let b_shuttle = ctx.bind_object(&mut shuttle_sess, "beyonce", null_sys as i64).unwrap();

    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp past mid-path bubble");
    let (_, crane_mid) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_mid_dest, .. } = crane_mid.mode else {
        panic!("Expected Crane in Warp mode");
    };
    assert!((crane_mid_dest.x - 30_000_000.0).abs() < 10.0, "mid-path bubble must not catch; got {}", crane_mid_dest.x);

    // 1b. Exit-corridor catch bubble within 150 km in front of destination (at 29,900 km, 20 km radius -> catches at 29,880 km)
    let catch_bubble_id: i64 = 88_704;
    sys.add_warp_bubble(catch_bubble_id, DVec3::new(29_900_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp into exit-corridor catch bubble");
    let (_, crane_catch) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_catch_dest, .. } = crane_catch.mode else {
        panic!("Expected Crane in Warp mode");
    };
    assert!((crane_catch_dest.x - 29_880_000.0).abs() < 10.0, "got {}", crane_catch_dest.x);

    // Shuttle (group 31, SDE attr 1538 = 1.0) ignores catch bubble AND warps out from inside a bubble
    ctx.dispatch_rpc(
        &mut shuttle_sess, "beyonce", Some(b_shuttle), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Shuttle warp past catch bubble");
    let (_, shuttle_catch) = sys.get_ball_snapshot(shuttle_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: shuttle_dest, .. } = shuttle_catch.mode else {
        panic!("Expected Shuttle in Warp mode");
    };
    assert!((shuttle_dest.x - 30_000_000.0).abs() < 10.0, "got {}", shuttle_dest.x);
    sys.remove_warp_bubble(catch_bubble_id).await.unwrap();

    // 1b. Drag bubble 100 km BEHIND destination at 30,100 km (20 km radius -> drags Crane to 30,080 km!)
    let drag_bubble_id: i64 = 88_703;
    sys.add_warp_bubble(drag_bubble_id, DVec3::new(30_100_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp into drag bubble");
    let (_, crane_drag) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_drag_dest, .. } = crane_drag.mode else {
        panic!("Expected Crane in Warp mode for drag");
    };
    assert!((crane_drag_dest.x - 30_080_000.0).abs() < 10.0, "got {}", crane_drag_dest.x);

    // 1c. Capital cyno jump blocked inside bubble, while disconnect emergency logoff warp succeeds!
    let near_null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id != null_sys
                && s.security_status < 0.0
                && s.solar_system_id < 31_000_000
                && {
                    let pa = ctx.sde.solar_systems[&null_sys].galactic_pos_ly;
                    let pb = s.galactic_pos_ly;
                    let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    (0.1..5.0).contains(&d)
                }
        })
        .expect("nearby null-sec cyno destination")
        .solar_system_id;
    let cyno_char: i64 = 94_026;
    let cyno_ship: i64 = 84_026;
    let dread_char: i64 = 94_027;
    let dread_ship: i64 = 84_027;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, false); // Falcon
    let mut dread_sess = seed_pilot(&ctx, dread_char, dread_ship, 19_720, null_sys, false); // Revelation
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_701,
            FleetRecord { fleet_id: 77_701, boss_id: cyno_char, members: vec![cyno_char, dread_char], ..Default::default() },
        );
    });
    spawn_in_space(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, DVec3::ZERO).await;
    give_item(&ctx, 74_190, 21_096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 74_191, 16_273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(74_190, true); });
    ctx.invalidate_ship_fitting(cyno_ship);
    let cyno_rep = rpc(&ctx, &mut cyno_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(74_190), PyRep::String("cynosuralGeneration".into()),
    ]).await.expect("Light cyno in near_null_sys");
    let cyno_beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    spawn_in_space(&ctx, dread_char, dread_ship, 19_720, null_sys, DVec3::new(30_100_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 74_192, 16_274, dread_char, dread_ship, InventoryFlag::Cargo, 50_000); // Helium Isotopes
    let jump_blocked = rpc(&ctx, &mut dread_sess, "beyonce", "CmdJumpToFleetModuleBeacon", vec![
        PyRep::Integer(cyno_char), PyRep::Integer(near_null_sys as i64), PyRep::Integer(cyno_beacon_id),
    ]).await.unwrap_err();
    assert_eq!(jump_blocked.code, "WarpScrambled", "Warp disruption bubble must block capital cyno jump");

    // Disconnect emergency logoff warp inside bubble is NOT blocked when ship has no point scramble
    let disc_entry = handle_character_logoff_or_disconnect(&ctx, dread_char)
        .await
        .expect("Disconnect emergency warp entry");
    assert!(disc_entry.emergency_warp_dest.is_some(), "Bubble alone must NOT prevent disconnect emergency logoff warp");

    // 2. Interdiction Nullification Module (`feat:navigation.interdiction-nullification-module`)
    let mut cruiser_sess = seed_pilot(&ctx, 94_028, 84_028, 621, 30_000_142, true); // Caracal (T1 Cruiser)
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_028, 74_200, 58_919, 27).await.is_err(), "T1 Cruiser must not fit Interdiction Nullifier I");
    let mut bs_sess = seed_pilot(&ctx, 94_024, 84_024, 640, 30_000_142, true);
    assert!(fit_mod(&ctx, &mut bs_sess, 84_024, 74_201, 58_919, 27).await.is_err(), "Battleship must not fit Interdiction Nullifier I");

    let mut crane_fit_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, 30_000_142, true);
    let pre_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    let pre_target_range = attr_from_item_info(&pre_null_info, 76);
    let pre_scan_res = attr_from_item_info(&pre_null_info, 564);

    fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_202, 58_919, 27)
        .await.expect("Crane fits 1st Interdiction Nullifier I");
    assert!(fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_203, 58_919, 28).await.is_err(), "maxGroupFitted=1 must reject 2nd Nullifier");
    let post_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(attr_from_item_info(&post_null_info, 76) < pre_target_range);
    assert!(attr_from_item_info(&post_null_info, 564) < pre_scan_res);

    let mut nereus_sess = seed_pilot(&ctx, 94_025, 84_025, 650, 30_000_142, true);
    let pre_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    let pre_drone_bw = attr_from_item_info(&pre_ner_info, 1271);
    fit_mod(&ctx, &mut nereus_sess, 84_025, 74_204, 58_919, 27).await.expect("Nereus fits Interdiction Nullifier I");
    let post_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    assert!((attr_from_item_info(&post_ner_info, 1271) - pre_drone_bw * 0.5).abs() < 1e-3);

    let far_beacon_id: i64 = 88_704;
    sys.spawn_ball(Ball {
        id: far_beacon_id, type_id: 15, position: DVec3::new(50_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();
    target_sess.station_id = None;
    target_sess.solar_system_id = Some(null_sys);
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, DVec3::new(30_100_000.0, 0.0, 0.0)).await;
    sys.set_ball_position(target_ship_id, DVec3::new(30_100_000.0, 0.0, 0.0)).await;
    let blocked_err = ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap_err();
    assert_eq!(blocked_err.code, "WarpScrambled");

    rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)])
        .await.expect("Activate Interdiction Nullifier I");
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Active Interdiction Nullifier allows warp out of bubble");

    rpc(&ctx, &mut target_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74_202)]).await.unwrap();
    let react_err = rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)]).await.unwrap_err();
    assert_eq!(react_err.code, "ModuleReactivationDelay");
    sys.remove_warp_bubble(drag_bubble_id).await.unwrap();

    // 3. Interdiction Sphere Probe Variants, Security/Corruption Gates, Capacity/Reload, & Wubble (`feat:modules.interdiction-probe-variants`)
    // 3a. Max-1 Interdiction Sphere Launcher I (`22782`, `maxGroupFitted = 1`)
    let mut sabre_fit_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, true);
    let launcher_id: i64 = 74_301;
    fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, launcher_id, 22_782, 27).await.expect("Sabre fits 1st Interdiction Sphere Launcher I");
    assert!(fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, 74_399, 22_782, 28).await.is_err(), "maxGroupFitted=1 blocks 2nd launcher");

    // 3b. High-sec refusal (`30_000_142`) and Low-sec (`30_002_537` Amamake) Corruption Stage 0 vs Stage 5
    sabre_sess.station_id = None;
    sabre_sess.solar_system_id = Some(30_000_142);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 74_302, 22_778, sabre_char, sabre_id, InventoryFlag::HiSlot0, 3);
    ctx.invalidate_ship_fitting(sabre_id);
    let hs_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(hs_err.code, "CannotLaunchInterdictionProbeInEmpire");

    let low_sys: u32 = 30_002_537; // Amamake (0.4 low-sec)
    sabre_sess.solar_system_id = Some(low_sys);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, low_sys, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
    let ls_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(ls_err.code, "CannotLaunchInterdictionProbeInEmpire");

    init_insurgency_campaign(&ctx, 501, FACTION_ANGEL_CARTEL, 500_002, low_sys, vec![low_sys]);
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    let (_, corr_stage) = add_system_corruption(&ctx, 501, low_sys, 1_000);
    assert_eq!(corr_stage, 5);
    let ls_stage5 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Low-sec Corruption Stage 5 allows interdiction probe launch");
    assert_eq!(ls_stage5.dict_get("bubbleRadius"), Some(&PyRep::Real(20_000.0)));
    ctx.advance_secs(6);

    // 3c. Move to null-sec (`null_sys`), test 15 m3 (3-probe) capacity + 60s auto-reload when last loaded probe is fired!
    sabre_sess.solar_system_id = Some(null_sys);
    let sabre_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, sabre_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.items.get_mut(&74_302) {
            st.quantity = 1; // 1 probe left in launcher
        }
    });
    give_item(&ctx, 74_310, 22_778, sabre_char, sabre_id, InventoryFlag::Cargo, 4); // 4 in cargo -> 3 (15 m3) auto-reloaded
    let smartbomb_id: i64 = 74_303;
    let smartbomb_id_2: i64 = 74_306;
    let ab_sabre_id: i64 = 74_307;
    give_item(&ctx, smartbomb_id, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, smartbomb_id_2, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, ab_sabre_id, 439, sabre_char, sabre_id, InventoryFlag::MedSlot0, 1); // 1MN Afterburner I (439)
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[launcher_id, smartbomb_id, smartbomb_id_2, ab_sabre_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(sabre_id);

    let p1 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch last loaded Warp Disrupt Probe -> triggers 60s auto-reload");
    let p1_id = p1.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(p1.dict_get("reloading"), Some(&PyRep::Bool(true)));
    assert_eq!(p1.dict_get("remainingInLauncher"), Some(&PyRep::Integer(3)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&sabre_char).map(|s| s.weapon_timer_until_unix).unwrap_or(0), 0);

    // At +10s (> 5s cadence, < 60s reload): blocked with ModuleIsReloading!
    ctx.advance_secs(10);
    let reload_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(reload_err.code, "ModuleIsReloading");

    // Smartbomb AoE destroys the 1,000 HP untargetable probe and removes its bubble
    assert_eq!(rpc(&ctx, &mut sabre_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(p1_id)]).await.unwrap_err().code, "InvalidTarget");
    assert!(sys.is_ball_scrambled(sabre_id).await.unwrap());
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id_2), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..70 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id)]).await;
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id_2)]).await;
    assert!(sys.get_ball_snapshot(p1_id).await.unwrap().is_none(), "Smartbomb AoE must destroy probe");
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap(), "Destroyed probe must remove bubble");

    // After 60s reload completed, 3 probes (15 m3 / 5 m3) were auto-loaded from the 4-probe cargo stack (1 left in cargo)!
    let p_reloaded = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch after 60s auto-reload");
    assert!(p_reloaded.dict_get("probeID").is_some());
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&74_310).map(|i| i.quantity)), Some(1), "15m3 launcher reloads max 3 of 4 probes");
    let cadence_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(cadence_err.code, "ModuleReactivationDelay");
    ctx.advance_secs(6);
    // Move away from p_reloaded bubble before testing Wubble
    let wub_pos = DVec3::new(55_000_000.0, 0.0, 0.0);
    sys.set_ball_position(sabre_id, wub_pos).await;

    // 3d. Stasis Webification Probe (`56748`: SDE 15km, 30s lifetime, 2.0s arming delay, -20% speedFactor = 0.80x, composes with active AB!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&smartbomb_id);
        snap.items.remove(&smartbomb_id_2);
        snap.items.retain(|_, i| !(i.location_id == sabre_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.module_online_states.insert(ab_sabre_id, true);
    });
    give_item(&ctx, 74_304, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 5);
    ctx.invalidate_ship_fitting(sabre_id);
    sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();

    // Activate 1MN Afterburner II on Sabre BEFORE launching Wubble
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(ab_sabre_id)])
        .await.expect("Activate 1MN Afterburner II on Sabre");
    let (_, sabre_ab_ball) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    let ab_vel = sabre_ab_ball.max_velocity;
    assert!(ab_vel > 700.0, "Active Afterburner must boost Sabre max_velocity: got {ab_vel}");

    let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Stasis Webification Probe 56748");
    let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(pw.dict_get("isWubble"), Some(&PyRep::Bool(true)));
    assert_eq!(pw.dict_get("bubbleRadius"), Some(&PyRep::Real(15_000.0)));
    assert_eq!(pw.dict_get("lifetimeSeconds"), Some(&PyRep::Real(30.0)));

    // At t = 1.0s (< 2.0s SDE arming delay attr 2262): NOT webbed yet!
    sys.step_manual(1.0).await.unwrap();
    let (_, sabre_t1) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_t1.max_velocity - ab_vel).abs() < 1e-2, "Wubble must not web during 2.0s arming delay");

    // At t = 2.0s (armed): SDE attr 20 = -20.0 -> 0.80x of AB-boosted velocity, and NOT warp scrambled!
    sys.step_manual(1.0).await.unwrap();
    let (_, sabre_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_wub.max_velocity - ab_vel * 0.80).abs() < 1e-2, "Wubble must apply SDE 0.80x to AB velocity: got {} vs {}", sabre_wub.max_velocity, ab_vel * 0.80);
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap());

    // Keep capacitor topped up and step 31s past 30s Wubble lifetime -> restores FULL AB-boosted velocity!
    for _ in 0..31 {
        sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none());
    let (_, sabre_post_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_post_wub.max_velocity - ab_vel).abs() < 1e-2, "Wubble expiry must restore active AB velocity ({ab_vel}), got {}", sabre_post_wub.max_velocity);

    // 3e. Surgical Warp Disrupt Probe (34260: 10km radius, 180s lifetime)
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&74_304); });
    give_item(&ctx, 74_305, 34_260, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
    ctx.invalidate_ship_fitting(sabre_id);
    let ps = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Surgical Warp Disrupt Probe 34260");
    assert_eq!(ps.dict_get("bubbleRadius"), Some(&PyRep::Real(10_000.0)));
    assert_eq!(ps.dict_get("lifetimeSeconds"), Some(&PyRep::Real(180.0)));
}

#[tokio::test]
async fn test_mjd_spool_and_warp_scramble_propulsion_shutoff() {
    // parity: feat:navigation.micro-jump-drive-spool, feat:navigation.warp-scramble-propulsion-shutoff
    eve_testkit::attest(&[
        "feat:navigation.micro-jump-drive-spool",
        "feat:navigation.warp-scramble-propulsion-shutoff",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_mjd: i64 = 94_031;
    let ship_mjd: i64 = 84_031;
    let char_scram: i64 = 94_032;
    let ship_scram: i64 = 84_032;

    let mut sess_mjd = seed_pilot(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, false);
    let mut sess_scram = seed_pilot(&ctx, char_scram, ship_scram, 640, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Micro Jump Drive Spool-up, Turn During Spool, Velocity/Heading Preservation, & SetBallPosition Event
    set_skill_level(&ctx, char_mjd, 4_385, 0);
    let med_mjd_id: i64 = 75_001;
    give_item(&ctx, med_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);

    let mjd_origin = DVec3::new(10_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, mjd_origin).await;
    let b_mjd = ctx.bind_object(&mut sess_mjd, "beyonce", jita_sys as i64).unwrap();
    let init_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    let base_sig = init_cs.signature_radius;

    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)], &kw, None,
    ).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)])
        .await.expect("Activate Medium MJD");

    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, mid_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let mid_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    assert!(mid_ball.position.distance(mjd_origin) < 5_000.0);
    assert!((mid_cs.signature_radius - base_sig * 2.5).abs() < 1.0);

    // Turn heading toward +Y mid-spool -> at t = 12s jump executes 100 km along the turned velocity heading, emits SetBallPosition, and preserves velocity/heading!
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    let mut saw_set_ball_pos = false;
    for _ in 0..6 {
        let tb = sys.step_manual(1.0).await.unwrap();
        if tb.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_mjd)) {
            saw_set_ball_pos = true;
        }
    }
    assert!(saw_set_ball_pos, "Same-grid 100 km MJD jump must emit DestinyEvent::SetBallPosition");
    let (_, post_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let jump_vec = post_mjd_ball.position - mid_ball.position;
    assert!((jump_vec.length() - 100_000.0).abs() < 2_500.0, "MJD must jump 100 km: got {}", jump_vec.length());
    assert!(post_mjd_ball.position.y > 40_000.0, "Turning toward +Y mid-spool must steer MJD jump into +Y: got {:?}", post_mjd_ball.position);
    assert!(jump_vec.normalize().dot(post_mjd_ball.velocity.normalize()) > 0.98, "MJD jump must align with current ship velocity heading");
    assert!(post_mjd_ball.velocity.y > 10.0, "Post-MJD ship must preserve +Y velocity component: got {:?}", post_mjd_ball.velocity);
    assert!(matches!(post_mjd_ball.mode, MovementMode::GotoDirection { direction } if direction.y > 0.99));

    let mjd_react_err = rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)]).await.unwrap_err();
    assert_eq!(mjd_react_err.code, "ModuleReactivationDelay");

    // Micro Jump Drive Operation L5 reduces spool time by 25% (12s -> 9s)
    let char_mjd_l5: i64 = 94_033;
    let ship_mjd_l5: i64 = 84_033;
    let med_mjd_l5_id: i64 = 75_002;
    let l5_origin = DVec3::new(20_000_000.0, 0.0, 0.0);
    let mut sess_mjd_l5 = seed_pilot(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, false);
    set_skill_level(&ctx, char_mjd_l5, 4_385, 5);
    give_item(&ctx, med_mjd_l5_id, 33_915, char_mjd_l5, ship_mjd_l5, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_l5_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd_l5);
    spawn_in_space(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, l5_origin).await;
    let b_mjd_l5 = ctx.bind_object(&mut sess_mjd_l5, "beyonce", jita_sys as i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd_l5, "beyonce", Some(b_mjd_l5), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd_l5, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_l5_id)]).await.unwrap();
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, pre9_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!(pre9_ball.position.distance(l5_origin) < 5_000.0);
    sys.step_manual(1.0).await.unwrap();
    let (_, l5_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!((l5_ball.position.x - (l5_origin.x + 100_000.0)).abs() < 5_000.0);

    // Capital Micro Jump Drive (83465) on Archon (23757) jumps 250 km
    let cap_mjd_id: i64 = 75_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 23_757;
    });
    give_item(&ctx, cap_mjd_id, 83_465, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cap_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);
    let cap_origin = DVec3::new(30_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 23_757, jita_sys, cap_origin).await;
    sys.set_ball_position(ship_mjd, cap_origin).await;
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)], &kw, None,
    ).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(cap_mjd_id)]).await.unwrap();
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, cap_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((cap_ball.position.x - (cap_origin.x + 250_000.0)).abs() < 5_000.0);

    // 2. Warp Scrambler Propulsion Shutoff vs Warp Disruptor, Bubble, WCS, & Afterburner (`feat:navigation.warp-scramble-propulsion-shutoff`)
    let mwd_id: i64 = 75_011;
    let ab_id: i64 = 75_012;
    let mid_spool_mjd_id: i64 = 75_013;
    let wcs_id: i64 = 75_014;
    let disruptor_id: i64 = 75_021;
    let scrambler_id: i64 = 75_022;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd && i.location_id != ship_scram);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 16_231;
        snap.items.get_mut(&ship_scram).unwrap().type_id = 640;
    });
    give_item(&ctx, mwd_id, 5_975, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, ab_id, 12_058, char_mjd, ship_mjd, InventoryFlag::MedSlot1, 1);
    give_item(&ctx, mid_spool_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot2, 1);
    give_item(&ctx, wcs_id, 10_998, char_mjd, ship_mjd, InventoryFlag::LoSlot0, 1); // Warp Core Stabilizer I (+2 core strength)
    give_item(&ctx, disruptor_id, 3_242, char_scram, ship_scram, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, scrambler_id, 448, char_scram, ship_scram, InventoryFlag::MedSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mwd_id, ab_id, mid_spool_mjd_id, wcs_id, disruptor_id, scrambler_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_mjd);
    ctx.invalidate_ship_fitting(ship_scram);

    let scram_arena = DVec3::new(40_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, scram_arena).await;
    spawn_in_space(&ctx, char_scram, ship_scram, 640, jita_sys, scram_arena + DVec3::new(3_000.0, 0.0, 0.0)).await;
    let (_, base_cyclone) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let base_vel = base_cyclone.max_velocity;

    sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
        target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    let (_, mwd_on_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(mwd_on_ball.max_velocity > base_vel * 4.0);

    // 2a. Warp Disruptor keeps MWD active AND allows MJD activation, 12s spool (skill lvl 0), and 100 km jump
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(disruptor_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
    let (_, under_disruptor_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(under_disruptor_ball.max_velocity > base_vel * 4.0);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    for _ in 0..12 {
        sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, disruptor_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(
        disruptor_jumped_ball.position.distance(scram_arena) > 95_000.0,
        "MJD must complete 100 km jump under Warp Disruptor: got {:?}",
        disruptor_jumped_ball.position
    );
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(disruptor_id)]).await.unwrap();
    ctx.advance_secs(200);

    // 2b. Interdiction bubble allows MJD activation, 12s spool, and 100 km jump out of bubble
    let bubble_pos = scram_arena + DVec3::new(200_000.0, 0.0, 0.0);
    sys.set_ball_position(ship_mjd, bubble_pos).await;
    sys.add_warp_bubble(980_101, bubble_pos, 20_000.0).await.unwrap();
    assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    for _ in 0..12 {
        sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, bubble_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(
        bubble_jumped_ball.position.distance(bubble_pos) > 95_000.0,
        "MJD must complete 100 km jump inside an interdiction bubble: got {:?}",
        bubble_jumped_ball.position
    );
    sys.remove_warp_bubble(980_101).await.unwrap();

    // Reset ship back to scram_arena and verify Warp Scrambler shuts down MWD, preserves AB,
    // and blocks/aborts MJD even when a Warp Core Stabilizer (+2 core strength) is active!
    sys.set_ball_position(ship_mjd, scram_arena).await;
    sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
        target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, scrammed_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((scrammed_ball.max_velocity - base_vel).abs() < 1e-2);
    assert_eq!(rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap_err().code, "WarpScrambled");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_mjd);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    sys.step_manual(2.0).await.unwrap();
    let (_, ab_scram_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(ab_scram_ball.max_velocity > base_vel * 1.8);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(scrambler_id)]).await.unwrap();

    // Activate Warp Core Stabilizer I (+2 core strength -> total 3 > 2 scram points) and verify scrambler STILL blocks & aborts MJD!
    ctx.advance_secs(200);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(wcs_id)]).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    sys.step_manual(3.0).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    let cs_wcs_scram = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    assert!(!cs_wcs_scram.is_warp_scrambled(), "WCS (+2) overcomes 2 scram points for warping");
    assert!(cs_wcs_scram.is_mwd_mjd_disabled, "Scrambler still sets is_mwd_mjd_disabled with WCS active");
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, aborted_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(aborted_mjd_ball.position.distance(scram_arena) < 5_000.0);
    ctx.advance_secs(200);
    assert_eq!(
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap_err().code,
        "WarpScrambled"
    );
}
