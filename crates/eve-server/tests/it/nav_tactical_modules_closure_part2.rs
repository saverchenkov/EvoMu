//! End-to-End Integration Tests closing Interdiction & MJD/Propulsion parity rows (Part 2):
//! `feat:navigation.warp-bubble-drag-and-catch`, `feat:navigation.interdiction-nullification-module`,
//! `feat:modules.interdiction-probe-variants`, `feat:navigation.micro-jump-drive-spool`,
//! and `feat:navigation.warp-scramble-propulsion-shutoff`.

use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
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

    // 1a. Catch bubble at 20,000 km (20 km radius -> catches at 19,980 km)
    let catch_bubble_id: i64 = 88_702;
    sys.add_warp_bubble(catch_bubble_id, DVec3::new(20_000_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;

    let b_target = ctx.bind_object(&mut target_sess, "beyonce", null_sys as i64).unwrap();
    let b_shuttle = ctx.bind_object(&mut shuttle_sess, "beyonce", null_sys as i64).unwrap();

    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp into catch bubble");
    let (_, crane_catch) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_catch_dest, .. } = crane_catch.mode else {
        panic!("Expected Crane in Warp mode");
    };
    assert!((crane_catch_dest.x - 19_980_000.0).abs() < 10.0, "got {}", crane_catch_dest.x);

    // Shuttle (group 31) ignores catch bubble and warps straight to 30,000 km
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

    // 2. Interdiction Nullification Module (`feat:navigation.interdiction-nullification-module`)
    let mut bs_sess = seed_pilot(&ctx, 94_024, 84_024, 640, 30_000_142, true);
    assert!(
        fit_mod(&ctx, &mut bs_sess, 84_024, 74_201, 58_919, 27).await.is_err(),
        "Battleship must not fit Interdiction Nullifier I"
    );

    let mut crane_fit_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, 30_000_142, true);
    let pre_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    let pre_target_range = attr_from_item_info(&pre_null_info, 76);
    let pre_scan_res = attr_from_item_info(&pre_null_info, 564);

    fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_202, 58_919, 27)
        .await.expect("Crane fits 1st Interdiction Nullifier I");
    assert!(
        fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_203, 58_919, 28).await.is_err(),
        "maxGroupFitted=1 must reject 2nd Interdiction Nullifier"
    );
    let post_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(attr_from_item_info(&post_null_info, 76) < pre_target_range, "Nullifier passive penalty reduces maxTargetRange");
    assert!(attr_from_item_info(&post_null_info, 564) < pre_scan_res, "Nullifier passive penalty reduces scanResolution");

    // Also verify -50% droneBandwidth (1271) passive penalty on Nereus (650, group 28 Industrial with 15 Mbit/s bandwidth)
    let mut nereus_sess = seed_pilot(&ctx, 94_025, 84_025, 650, 30_000_142, true);
    let pre_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    let pre_drone_bw = attr_from_item_info(&pre_ner_info, 1271);
    fit_mod(&ctx, &mut nereus_sess, 84_025, 74_204, 58_919, 27).await.expect("Nereus fits Interdiction Nullifier I");
    let post_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    assert!((attr_from_item_info(&post_ner_info, 1271) - pre_drone_bw * 0.5).abs() < 1e-3, "Nullifier passive penalty reduces droneBandwidth by 50%");

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

    rpc(&ctx, &mut target_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74_202)])
        .await.expect("Deactivate Interdiction Nullifier I");
    let react_err = rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)])
        .await.unwrap_err();
    assert_eq!(react_err.code, "ModuleReactivationDelay");
    sys.remove_warp_bubble(drag_bubble_id).await.unwrap();

    // 3. Interdiction Sphere Probe Variants & Smartbomb Destruction (`feat:modules.interdiction-probe-variants`)
    let launcher_id: i64 = 74_301;
    let probe_stack_id: i64 = 74_302;
    let smartbomb_id: i64 = 74_303;
    let smartbomb_id_2: i64 = 74_306;
    let sabre_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, sabre_pos).await;
    give_item(&ctx, launcher_id, 22_782, sabre_char, sabre_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, smartbomb_id, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, smartbomb_id_2, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, probe_stack_id, 22_778, sabre_char, sabre_id, InventoryFlag::HiSlot0, 3);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(launcher_id, true);
        snap.module_online_states.insert(smartbomb_id, true);
        snap.module_online_states.insert(smartbomb_id_2, true);
    });
    ctx.invalidate_ship_fitting(sabre_id);

    let p1 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Warp Disrupt Probe 22778");
    let p1_id = p1.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(p1.dict_get("bubbleRadius"), Some(&PyRep::Real(20_000.0)));
    assert_eq!(p1.dict_get("lifetimeSeconds"), Some(&PyRep::Real(120.0)));
    let weap_timer = ctx.crimewatch.read().unwrap().states.get(&sabre_char).map(|s| s.weapon_timer_until_unix).unwrap_or(0);
    assert_eq!(weap_timer, 0, "Launching interdiction probe must not create aggression");

    let cadence_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.unwrap_err();
    assert_eq!(cadence_err.code, "ModuleReactivationDelay");

    let (_, probe_ball) = sys.get_ball_snapshot(p1_id).await.unwrap().expect("probe ball spawned");
    assert_eq!(probe_ball.type_id, 22_778);
    let probe_cs = sys.get_combat_state(p1_id).await.unwrap().unwrap();
    assert_eq!(probe_cs.max_structure_hp, 1_000.0);
    let lock_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(p1_id)])
        .await.unwrap_err();
    assert_eq!(lock_err.code, "InvalidTarget");

    // Smartbomb AoE destroys the 1,000 HP probe (repeating pulses over 70s < 120s lifetime) and removes its bubble
    assert!(sys.is_ball_scrambled(sabre_id).await.unwrap());
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.expect("Activate Smartbomb 1 AoE");
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id_2), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.expect("Activate Smartbomb 2 AoE");
    for _ in 0..70 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id)]).await;
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id_2)]).await;
    assert!(sys.get_ball_snapshot(p1_id).await.unwrap().is_none(), "Smartbomb AoE must destroy probe");
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap(), "Destroyed probe must remove bubble");

    // Stasis Webification Probe (56748: 15km radius, 30s lifetime, 3s arming, 30% web, does NOT block warp)
    let wubble_stack_id: i64 = 74_304;
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&probe_stack_id); });
    give_item(&ctx, wubble_stack_id, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 5);
    ctx.invalidate_ship_fitting(sabre_id);
    let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Stasis Webification Probe 56748");
    let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(pw.dict_get("isWubble"), Some(&PyRep::Bool(true)));
    assert_eq!(pw.dict_get("bubbleRadius"), Some(&PyRep::Real(15_000.0)));
    assert_eq!(pw.dict_get("lifetimeSeconds"), Some(&PyRep::Real(30.0)));

    let (_, sabre_pre_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    let pre_wub_vel = sabre_pre_wub.max_velocity;
    sys.step_manual(4.0).await.unwrap();
    let (_, sabre_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_wub.max_velocity - pre_wub_vel * 0.70).abs() < 1e-2);
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap());

    sys.step_manual(31.0).await.unwrap();
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none());
    let (_, sabre_post_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_post_wub.max_velocity - pre_wub_vel).abs() < 1e-2);

    // Surgical Warp Disrupt Probe (34260: 10km radius, 180s lifetime)
    let surg_stack_id: i64 = 74_305;
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&wubble_stack_id); });
    give_item(&ctx, surg_stack_id, 34_260, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
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

    let mut sess_mjd = seed_pilot(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, false); // Cyclone (BC)
    let mut sess_scram = seed_pilot(&ctx, char_scram, ship_scram, 640, jita_sys, false); // Raven (BS)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Micro Jump Drive Spool-up, Heading Turn During Spool, Skill Reduction, Capital MJD, & Scramble Abort
    // (`feat:navigation.micro-jump-drive-spool`)
    // Set Micro Jump Drive Operation (4385) to L0 -> 12.0s spool-up
    set_skill_level(&ctx, char_mjd, 4_385, 0);
    let med_mjd_id: i64 = 75_001;
    give_item(&ctx, med_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);

    let mjd_origin = DVec3::new(10_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, mjd_origin).await;
    let b_mjd = ctx.bind_object(&mut sess_mjd, "beyonce", jita_sys as i64).unwrap();
    let (_, init_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let init_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    let base_sig = init_cs.signature_radius;

    // Initial heading +X, activate Medium MJD (33915)
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)])
        .await.expect("Activate Medium MJD");

    // During spool-up (t = 6s < 12s): signature radius blooms +150% (2.5x), ship has NOT jumped 100 km yet
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, mid_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let mid_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    assert!(mid_ball.position.distance(mjd_origin) < 5_000.0, "MJD must not jump before 12s spool completes");
    assert!((mid_cs.signature_radius - base_sig * 2.5).abs() < 1.0, "MJD spool must bloom signature radius by +150%");

    // Turn heading to +Y mid-spool -> at t = 12s jump executes 100 km along NEW +Y heading!
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, post_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(
        (post_mjd_ball.position.y - 100_000.0).abs() < 5_000.0,
        "MJD must jump 100 km along new +Y heading after turn during spool: got {:?}",
        post_mjd_ball.position
    );
    // Reactivation delay (180s) rejects immediate reactivation
    let mjd_react_err = rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)])
        .await.unwrap_err();
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
    let act_l5 = rpc(&ctx, &mut sess_mjd_l5, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_l5_id)])
        .await.expect("Activate MJD with MJD Operation L5");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, pre9_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!(pre9_ball.position.distance(l5_origin) < 5_000.0, "MJD Operation L5 must not jump before 9s: act_l5={act_l5:?}, pre9={pre9_ball:?}");
    sys.step_manual(1.0).await.unwrap();
    let (_, l5_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!((l5_ball.position.x - (l5_origin.x + 100_000.0)).abs() < 5_000.0, "MJD Operation L5 must complete jump in 9s: act_l5={act_l5:?}, l5_ball={l5_ball:?}");

    // Capital Micro Jump Drive (83465) on Archon (23757) jumps 250 km
    let cap_mjd_id: i64 = 75_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 23_757; // Archon
    });
    give_item(&ctx, cap_mjd_id, 83_465, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cap_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);
    let cap_origin = DVec3::new(30_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 23_757, jita_sys, cap_origin).await;
    sys.set_ball_position(ship_mjd, cap_origin).await;
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(cap_mjd_id)])
        .await.expect("Activate Capital MJD");
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, cap_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((cap_ball.position.x - (cap_origin.x + 250_000.0)).abs() < 5_000.0, "Capital MJD must jump 250 km");

    // 2. Warp Scrambler Propulsion Shutoff vs Warp Disruptor & Afterburner (`feat:navigation.warp-scramble-propulsion-shutoff`)
    let mwd_id: i64 = 75_011;
    let ab_id: i64 = 75_012;
    let mid_spool_mjd_id: i64 = 75_013;
    let disruptor_id: i64 = 75_021;
    let scrambler_id: i64 = 75_022;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd && i.location_id != ship_scram);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 16_231; // Cyclone
        snap.items.get_mut(&ship_scram).unwrap().type_id = 640; // Raven
    });
    give_item(&ctx, mwd_id, 5_975, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1); // 50MN MWD
    give_item(&ctx, ab_id, 12_058, char_mjd, ship_mjd, InventoryFlag::MedSlot1, 1); // 10MN AB
    give_item(&ctx, mid_spool_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot2, 1);
    give_item(&ctx, disruptor_id, 3_242, char_scram, ship_scram, InventoryFlag::MedSlot0, 1); // Warp Disruptor (1 pt)
    give_item(&ctx, scrambler_id, 448, char_scram, ship_scram, InventoryFlag::MedSlot1, 1); // Warp Scrambler II (2 pts)
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mwd_id, ab_id, mid_spool_mjd_id, disruptor_id, scrambler_id] {
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
    let _ = init_ball;

    // Lock Cyclone from Raven
    sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
        target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // Activate MWD on Cyclone -> speed boosted > 4x
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)])
        .await.expect("Activate 50MN MWD");
    let (_, mwd_on_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(mwd_on_ball.max_velocity > base_vel * 4.0);

    // Warp Disruptor (1 pt, group 52, non-scram) scrambles warp ONLY; MWD stays active!
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(disruptor_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.expect("Activate Warp Disruptor I");
    assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
    let (_, under_disruptor_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(under_disruptor_ball.max_velocity > base_vel * 4.0, "Warp Disruptor must NOT shut off active MWD");
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(disruptor_id)])
        .await.expect("Deactivate Warp Disruptor I");

    // Warp Scrambler II (2 pts) IMMEDIATELY shuts off active MWD, restores unboosted velocity, and blocks MWD reactivation
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.expect("Activate Warp Scrambler II");
    let (_, scrammed_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((scrammed_ball.max_velocity - base_vel).abs() < 1e-2, "Warp Scrambler must shut off MWD and restore base velocity");
    let mwd_blocked = rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)])
        .await.unwrap_err();
    assert_eq!(mwd_blocked.code, "WarpScrambled");

    // Afterburner remains usable and active while warp scrambled (offline MWD first since maxGroupOnline=1 on propulsion)!
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_mjd);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)])
        .await.expect("Afterburner must activate while warp scrambled");
    sys.step_manual(2.0).await.unwrap();
    let (_, ab_scram_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(ab_scram_ball.max_velocity > base_vel * 1.8, "Afterburner must stay active under Warp Scrambler");
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(scrambler_id)]).await.unwrap();

    // Warp Scrambler applied MID-SPOOL aborts an active MJD before jump executes!
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)])
        .await.expect("Activate MJD before scram");
    sys.step_manual(3.0).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.expect("Scram Cyclone mid-MJD-spool");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, aborted_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(aborted_mjd_ball.position.distance(scram_arena) < 5_000.0, "Warp Scrambler mid-spool must abort MJD jump");
}
