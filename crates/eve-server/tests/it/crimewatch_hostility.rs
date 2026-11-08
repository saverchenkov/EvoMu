//! Dedicated integration test suite for `plan:C-7` (`Salvage, tractor, drones`) and drone/salvage lifecycle closure.

use eve_db::{AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_drone_in_space, compute_drone_control_range, dispatch_combat_rpc,
    execute_wreck_salvage_cycle, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

pub(crate) fn item_rec(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

pub(crate) fn far_celestial_id(ctx: &ServerContext, sys_id: u32) -> i64 {
    ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.position.length() > 1_000_000.0).unwrap().celestial_id as i64
}

pub(crate) async fn setup_pilot_in_space(
    ctx: &ServerContext, user_id: u64, char_id: i64, corp_id: i64, ship_id: i64, ship_type_id: u32, solar_system_id: u32, pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id, user_id, name: format!("Pilot {char_id}"), corporation_id: corp_id,
                alliance_id: None, war_faction_id: None, station_id: None, structure_id: None,
                solar_system_id, constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000), plex_balance: 0, loyalty_points: HashMap::new(),
                standings: HashMap::new(), security_status: 0.0, clone_grade: 2, home_station_id: 60_003_760,
                race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1, neural: NeuralAttributes::default(),
                skill_points: 5_000_000.0, activity_status: None, free_skill_points: 0.0, last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                description: String::new(), title: String::new(), gender: 1, create_date_unix: 1_700_000_000,
                fleet_id: None, autopilot_waypoints: Vec::new(), version: 1, system_epoch: 0,
            },
        );
        if !snap.accounts.values().any(|a| a.user_id == user_id) {
            snap.accounts.insert(
                format!("user_{user_id}"),
                AccountRecord { user_id, username: format!("user_{user_id}"), password_token: String::new(), role: 0, banned_until_unix: None, ban_reason: None },
            );
        }
        snap.items.insert(ship_id, item_rec(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::None, 1, true));
    });
    let _ = ctx.db.acquire_online_lock(user_id, 7_000 + user_id, Some(char_id), true, ctx.now_unix());
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys_handle.spawn_ball_with_combat(
        Ball {
            id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 120.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
    ).await;
    let mut sess = SessionState::new(7_000 + user_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.constellation_id = Some(20_000_020);
    sess.region_id = Some(10_000_002);
    sess
}

// parity: plan:C-7
// parity: feat:drones.sentry-drone-scoop-distance
// parity: feat:lifecycle.drones-recall-same-grid
// parity: feat:lifecycle.drones-leave-grid-abandon
#[tokio::test]
async fn test_plan_c7_sentry_scoop_and_same_grid_recall_and_abandon() {
    eve_testkit::attest(&[
        "plan:C-7",
        "feat:drones.sentry-drone-scoop-distance",
        "feat:lifecycle.drones-recall-same-grid",
        "feat:lifecycle.drones-leave-grid-abandon",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, ship_id) = (96_100_001i64, 86_100_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 61001, char_id, 1_000_125, ship_id, 645, sys_id, DVec3::ZERO).await;
    let (scooper_id, scooper_ship_id) = (96_100_002i64, 86_100_002i64);
    let mut scooper_sess = setup_pilot_in_space(&ctx, 61002, scooper_id, 1_000_125, scooper_ship_id, 645, sys_id, DVec3::new(500.0, 0.0, 0.0)).await;

    let (sentry_id, mobile_id, dummy_target_id) = (86_100_010i64, 86_100_011i64, 86_100_012i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 });
        snap.items.insert(sentry_id, item_rec(sentry_id, 28215, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        snap.items.insert(mobile_id, item_rec(mobile_id, 2185, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_combat(
        Ball {
            id: dummy_target_id, type_id: 587, position: DVec3::new(2_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(587),
    ).await;

    // 1. Immobile sentry drone (`Garde II`, 28215): max_velocity 0, ignores orbit on CmdReturnHome, rejected > 2,500m, succeeds <= 2,500m.
    let sentry_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sentry_id)])], &kw).await.expect("Launch Garde II");
    let sentry_ball_id = match sentry_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let (_, sentry_ball) = sys_handle.get_ball_snapshot(sentry_ball_id).await.unwrap().unwrap();
    assert_eq!((sentry_ball.max_velocity, sentry_ball.mode.clone(), sentry_ball.speed_fraction), (0.0, MovementMode::Stop, 0.0));

    dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdReturnHome", &[PyRep::List(vec![PyRep::Integer(sentry_ball_id)])], &kw).await.expect("CmdReturnHome on sentry");
    let (_, sentry_after_home) = sys_handle.get_ball_snapshot(sentry_ball_id).await.unwrap().unwrap();
    assert_eq!(sentry_after_home.mode, MovementMode::Stop, "Sentry must ignore orbit movement on CmdReturnHome");

    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(3_000.0, 0.0, 0.0), ..sentry_ball.clone() }).await;
    let ret_err = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(sentry_ball_id)])], &kw)
        .await.expect_err("Sentry > 2,500m must reject CmdReturnBay");
    assert_eq!(ret_err.code, "SentryDroneOutOfScoopRange");
    let scoop_err = dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(sentry_ball_id)], &kw)
        .await.expect_err("Sentry > 2,500m must reject ScoopToDroneBay");
    assert_eq!(scoop_err.code, "OutOfScoopRange");

    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(2_000.0, 0.0, 0.0), ..sentry_ball }).await;
    dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(sentry_ball_id)])], &kw)
        .await.expect("Sentry <= 2,500m returns to DroneBay via CmdReturnBay");
    assert_eq!(ctx.db.get_item(sentry_id).unwrap().flag, InventoryFlag::DroneBay);

    let sentry_relaunch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sentry_id)])], &kw).await.expect("Re-launch Garde II");
    let sentry_ball_2 = match sentry_relaunch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(sentry_ball_2)], &kw)
        .await.expect("Sentry <= 2,500m scoops via ScoopToDroneBay");
    assert_eq!(ctx.db.get_item(sentry_id).unwrap().flag, InventoryFlag::DroneBay);

    // 2. Mobile drone (`Hammerhead II`, 2185): recall/command only within same system and control range.
    let ctrl_range = compute_drone_control_range(&ctx, char_id, ship_id);
    assert!(ctrl_range >= 20_000.0);
    let mob_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(mobile_id)])], &kw).await.expect("Launch Hammerhead II");
    let mob_ball_id = match mob_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let (_, mob_ball) = sys_handle.get_ball_snapshot(mob_ball_id).await.unwrap().unwrap();
    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(ctrl_range + 15_000.0, 0.0, 0.0), ..mob_ball.clone() }).await;

    dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(mob_ball_id)])], &kw).await.expect("CmdReturnBay skips out-of-range drone");
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&mob_ball_id].activity_state), 0);
    let oor_home = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdReturnHome", &[PyRep::List(vec![PyRep::Integer(mob_ball_id)])], &kw).await.expect("CmdReturnHome returns count of recalled drones");
    assert_eq!(oor_home, PyRep::Integer(0));
    let _ = sys_handle.add_target_lock(ship_id, eve_destiny::TargetLock { target_id: dummy_target_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let engage_oor = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(mob_ball_id)]), PyRep::Integer(dummy_target_id)], &kw)
        .await.expect("CmdEngage skips out-of-range drone");
    assert_eq!(dict_get(&engage_oor, "totalDamageDealt"), Some(&PyRep::Real(0.0)));
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&mob_ball_id].activity_state), 0);

    let _ = sys_handle.spawn_ball(Ball {
        position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::new(100.0, 0.0, 0.0),
        speed_fraction: 1.0, mode: MovementMode::Orbit { target_id: ship_id, range: 1_500.0 }, ..mob_ball
    }).await;

    // 3. Leaving grid via warp (`CmdWarpToStuff`) immediately abandons active drones in space and stops their balls.
    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let celestial_id = far_celestial_id(&ctx, sys_id);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("CmdWarpToStuff");

    let d_rt = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&mob_ball_id].clone());
    assert!(d_rt.is_abandoned && d_rt.owner_char_id == 0 && d_rt.parent_ship_id == 0);
    let (_, stopped_ball) = sys_handle.get_ball_snapshot(mob_ball_id).await.unwrap().unwrap();
    assert_eq!((stopped_ball.mode, stopped_ball.speed_fraction, stopped_ball.velocity), (MovementMode::Stop, 0.0, DVec3::ZERO));

    dispatch_combat_rpc(&ctx, &mut scooper_sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(mob_ball_id)], &kw).await.expect("Second pilot scoops abandoned drone");
    let scooped = ctx.db.get_item(mobile_id).unwrap();
    assert_eq!((scooped.owner_id, scooped.location_id, scooped.flag), (scooper_id, scooper_ship_id, InventoryFlag::DroneBay));
}

// parity: plan:C-7
// parity: feat:lifecycle.drones-leave-grid-abandon
// parity: feat:lifecycle.drones-recall-same-grid
#[tokio::test]
async fn test_plan_c7_drones_stargate_jump_reconnect_dock_logoff_death() {
    eve_testkit::attest(&[
        "plan:C-7",
        "feat:lifecycle.drones-leave-grid-abandon",
        "feat:lifecycle.drones-recall-same-grid",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let jita_sys = 30_000_142u32;
    let jita_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).cloned().expect("Jita stargate");
    let dest_sys = jita_gate.to_solar_system_id;
    let return_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == dest_sys && j.to_solar_system_id == jita_sys).cloned().expect("Return stargate");

    // Ship 628 (`Arbitrator`) has 50 Mbit/s drone bandwidth (`droneBandwidth = 50.0`).
    let (char_id, ship_id) = (96_100_005i64, 86_100_050i64);
    let mut sess = setup_pilot_in_space(&ctx, 61005, char_id, 1_000_125, ship_id, 628, jita_sys, jita_gate.position + DVec3::new(1_000.0, 0.0, 0.0)).await;

    let drone_ids: Vec<i64> = (0..7).map(|i| 86_100_060 + i).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 });
        for &did in &drone_ids {
            snap.items.insert(did, item_rec(did, 2185, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        }
    });

    // 1. Launch 2 medium drones (`Hammerhead II`, 10 Mbit/s each = 20 Mbit/s total) in Jita, then jump stargate to `dest_sys`.
    let launch_jita = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_ids[0]), PyRep::Integer(drone_ids[1])])], &kw,
    ).await.expect("Launch 2 Hammerhead II in Jita");
    let jita_balls: Vec<i64> = match launch_jita { PyRep::List(v) => v.iter().filter_map(PyRep::as_i64).collect(), _ => panic!() };
    assert_eq!(jita_balls.len(), 2);

    ctx.advance_secs(12);
    let beyonce_jita = ctx.bind_object(&sess, "beyonce", jita_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_jita), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)], &kw, None)
        .await.expect("Stargate jump to neighboring system");
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    for &bid in &jita_balls {
        let st = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&bid].clone());
        assert!(st.is_abandoned && st.owner_char_id == 0 && st.parent_ship_id == 0);
    }
    let cross_sys_home = dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnHome", &[PyRep::List(jita_balls.iter().map(|&b| PyRep::Integer(b)).collect())], &kw,
    ).await.expect("CmdReturnHome from another system");
    assert_eq!(cross_sys_home, PyRep::Integer(0));
    let cross_sys_bay = dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(jita_balls.iter().map(|&b| PyRep::Integer(b)).collect())], &kw,
    ).await.expect("CmdReturnBay from another system");
    assert_eq!(cross_sys_bay, PyRep::Dict(vec![]));
    for &did in &drone_ids[0..2] {
        assert_eq!(
            ctx.db.get_item(did).unwrap().location_id,
            jita_sys as i64,
            "CmdReturnBay from another system must leave drone item in space in origin system"
        );
    }

    // 2. Launch 5 medium drones (50 Mbit/s total) in `dest_sys`, proving the 2 left-behind drones in Jita no longer consume bandwidth or active count.
    let launch_dest = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(drone_ids[2..7].iter().map(|&d| PyRep::Integer(d)).collect())], &kw,
    ).await.expect("Launch 5 Hammerhead II (50 Mbit/s) in destination system");
    let dest_balls: Vec<i64> = match launch_dest { PyRep::List(v) => v.iter().filter_map(PyRep::as_i64).collect(), _ => panic!() };
    assert_eq!(dest_balls.len(), 5);
    for bid in dest_balls {
        dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(bid)], &kw).await.unwrap();
    }

    // 3. Jump back to Jita and reconnect to the 2 left-behind drones via `entity.CmdReconnectToDrones`.
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    dest_handle.set_ball_position(ship_id, return_gate.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.advance_secs(12);
    let beyonce_dest = ctx.bind_object(&sess, "beyonce", dest_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_dest), "CmdStargateJump", &[PyRep::Integer(return_gate.stargate_id)], &kw, None)
        .await.expect("Jump back to Jita");
    assert_eq!(sess.solar_system_id, Some(jita_sys));

    let recon = ctx.dispatch_rpc(&mut sess, "entity", None, "CmdReconnectToDrones", &[], &kw, None).await.expect("CmdReconnectToDrones in Jita");
    assert_eq!(recon, PyRep::Dict(vec![]));
    for &bid in &jita_balls {
        let st = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&bid].clone());
        assert!(!st.is_abandoned && st.owner_char_id == char_id && st.parent_ship_id == ship_id);
    }

    // 4. Verify docking (`stationSvc.Dock`), logoff (`handle_character_disconnect_in_space`), and death (`execute_death_pipeline`) all abandon active drones.
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let station_pos = ctx.sde.stations[&60_003_760].position;
    jita_handle.set_ball_position(ship_id, station_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Dock at Jita 4-4");
    for &bid in &jita_balls {
        assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&bid].is_abandoned));
    }

    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock from Jita 4-4");
    let launch_logoff = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_ids[2]), PyRep::Integer(drone_ids[3])])], &kw,
    ).await.expect("Launch 2 drones for logoff/death checks");
    let (logoff_ball, death_ball) = match launch_logoff { PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()), _ => panic!() };
    eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&logoff_ball].is_abandoned));
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&death_ball].is_abandoned));

    ctx.dispatch_rpc(&mut sess, "entity", None, "CmdReconnectToDrones", &[], &kw, None).await.expect("Reconnect before death");
    assert!(!with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&death_ball].is_abandoned));
    eve_server::combat::death_pipeline::execute_death_pipeline_sync(&ctx, jita_sys, char_id, ship_id, 0, DVec3::ZERO, 1).expect("execute_death_pipeline_sync");
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&death_ball].is_abandoned));
}

// parity: plan:C-7
// parity: feat:drones.gecko-augmented-mutated-drones
// parity: feat:drones.gecko-subverted-bandwidth
#[tokio::test]
async fn test_plan_c7_gecko_augmented_mutated_subverted_drones() {
    eve_testkit::attest(&[
        "plan:C-7",
        "feat:drones.gecko-augmented-mutated-drones",
        "feat:drones.gecko-subverted-bandwidth",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, ship_id) = (96_100_010i64, 86_100_100i64);
    let mut sess = setup_pilot_in_space(&ctx, 61010, char_id, 1_000_125, ship_id, 628, sys_id, DVec3::ZERO).await;

    let (sub1, sub2, sub3, gecko_id, aug_id, base_hob_id, mut_hob_id, track_hob_id, target_id) = (
        86_100_101i64, 86_100_102i64, 86_100_103i64, 86_100_104i64, 86_100_105i64, 86_100_106i64, 86_100_107i64, 86_100_108i64, 86_100_109i64,
    );
    let base_hob_mult = ctx.sde.types[&2456].default_attributes.get(&64).copied().unwrap_or(1.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 });
        for (iid, tid) in [(sub1, 48744u32), (sub2, 48744), (sub3, 48744), (gecko_id, 33681), (aug_id, 28288), (base_hob_id, 2456)] {
            snap.items.insert(iid, item_rec(iid, tid, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        }
        let mut rec = item_rec(mut_hob_id, 2456, char_id, ship_id, InventoryFlag::DroneBay, 1, true);
        rec.dynamic_attributes = HashMap::from([(64, base_hob_mult * 2.25), (37, 4500.0), (160, 4.5), (263, 1200.0), (265, 950.0), (9, 800.0)]);
        rec.source_mutaplasmid_id = Some(49766);
        snap.items.insert(mut_hob_id, rec);

        // Mutated Hobgoblin II with identical damageMultiplier (`base_hob_mult`) but boosted trackingSpeed (`160 -> 4.5`)
        let mut track_rec = item_rec(track_hob_id, 2456, char_id, ship_id, InventoryFlag::DroneBay, 1, true);
        track_rec.dynamic_attributes = HashMap::from([(64, base_hob_mult), (160, 4.5)]);
        track_rec.source_mutaplasmid_id = Some(49766);
        snap.items.insert(track_hob_id, track_rec);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut target_cs = ctx.build_type_combat_state(645);
    target_cs.shield_hp = 50_000.0;
    target_cs.max_shield_hp = 50_000.0;
    let _ = sys_handle.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 645, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 10_000_000.0, radius: 200.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        target_cs,
    ).await;

    let gecko_type = ctx.sde.types.get(&33681).unwrap();
    assert_eq!((gecko_type.default_attributes.get(&1272).copied(), gecko_type.volume), (Some(50.0), 50.0));
    let sub_type = ctx.sde.types.get(&48744).unwrap();
    assert_eq!((sub_type.default_attributes.get(&1272).copied(), sub_type.volume), (Some(20.0), 20.0));

    // 1. Launching 3 'Subverted' JVN-UC49 (60 Mbit/s) on a 50 Mbit/s ship fails; 2 succeed (40 Mbit/s) and deal omni split damage per cycle.
    let bw_err = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sub1), PyRep::Integer(sub2), PyRep::Integer(sub3)])], &kw,
    ).await.expect_err("3 x 20 Mbit/s Subverted drones must exceed 50 Mbit/s bandwidth");
    assert_eq!(bw_err.code, "MaxBandwidthExceeded");

    let launch_two = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sub1), PyRep::Integer(sub2)])], &kw,
    ).await.expect("2 x 20 Mbit/s Subverted drones must launch");
    let sub_balls: Vec<i64> = match launch_two { PyRep::List(v) => v.iter().filter_map(PyRep::as_i64).collect(), _ => panic!() };
    assert_eq!(sub_balls.len(), 2);
    let _ = sys_handle.add_target_lock(ship_id, eve_destiny::TargetLock { target_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let sub_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::Integer(target_id)], &kw).await.unwrap();
    assert!(dict_get(&sub_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap() > 0.0);
    for bid in sub_balls {
        dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(bid)], &kw).await.unwrap();
    }

    let gecko_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(gecko_id)])], &kw)
        .await.expect("Gecko (50 Mbit/s) launches into 50 Mbit/s bandwidth");
    let gecko_ball_id = match gecko_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let gecko_rt = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&gecko_ball_id].clone());
    assert_eq!(gecko_rt.bandwidth_used, 50.0);
    assert!(gecko_rt.damage_vector.em > 0.0 && gecko_rt.damage_vector.thermal > 0.0 && gecko_rt.damage_vector.kinetic > 0.0 && gecko_rt.damage_vector.explosive > 0.0);
    let gecko_eng1 = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::Integer(target_id)], &kw).await.unwrap();
    let gecko_volley = dict_get(&gecko_eng1, "totalDamageDealt").and_then(PyRep::as_f64).unwrap();
    assert!(gecko_volley > 0.0);
    // Spamming CmdEngage at the same clock timestamp is rate-limited by SDE cycle duration (`attr 51`) -> 0 extra damage
    let gecko_eng_spam = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::Integer(target_id)], &kw).await.unwrap();
    assert_eq!(dict_get(&gecko_eng_spam, "totalDamageDealt").and_then(PyRep::as_f64), Some(0.0));
    // Advancing clock by 4s (`attr 51 == 4000 ms`) and ticking fires the next cycle volley
    let hp_before_tick = sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp;
    ctx.advance_secs(4);
    eve_server::services::combat_crimewatch::combat_npcs_drones::step_engaged_drones(&ctx, ctx.now_unix()).await;
    let hp_after_tick = sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp;
    assert!((hp_before_tick - hp_after_tick - gecko_volley).abs() < 1e-2, "Tick after 4s cycle must apply next Gecko volley");
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(gecko_ball_id)], &kw).await.unwrap();

    // 2. Compare combat attack cycle (`entity.CmdEngage`) of unmutated vs Abyssal mutated `Hobgoblin II` (2456) and `Augmented Hammerhead` (28266).
    let base_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(base_hob_id)])], &kw).await.expect("Launch unmutated Hobgoblin II");
    let base_ball_id = match base_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let base_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(base_ball_id)]), PyRep::Integer(target_id)], &kw).await.expect("Unmutated Hobgoblin II engages target");
    let base_dmg = dict_get(&base_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap();
    assert!(base_dmg > 0.0);
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(base_ball_id)], &kw).await.unwrap();

    let launch_mut = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(aug_id), PyRep::Integer(mut_hob_id)])], &kw).await.expect("Launch Augmented Hammerhead + Mutated Hobgoblin II");
    let (aug_ball_id, mut_ball_id) = match launch_mut { PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()), _ => panic!() };
    let (aug_rt, mut_rt) = with_combat_runtime(&ctx, |rt| (rt.active_drones_in_space[&aug_ball_id].clone(), rt.active_drones_in_space[&mut_ball_id].clone()));
    assert!(aug_rt.damage_vector.thermal > 0.0 && aug_rt.damage_vector.kinetic > 0.0, "Augmented Hammerhead must have split thermal+kinetic damage");
    assert_eq!((mut_rt.max_velocity, mut_rt.effective_tracking_speed(&ctx)), (4500.0, 4.5));
    let (_, mut_ball) = sys_handle.get_ball_snapshot(mut_ball_id).await.unwrap().unwrap();
    assert_eq!(mut_ball.max_velocity, 4500.0);
    let mut_cs = sys_handle.get_combat_state(mut_ball_id).await.unwrap().unwrap();
    assert_eq!((mut_cs.max_shield_hp, mut_cs.max_armor_hp, mut_cs.max_structure_hp), (1200.0, 950.0, 800.0));

    let aug_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(aug_ball_id)]), PyRep::Integer(target_id)], &kw).await.unwrap();
    assert!(dict_get(&aug_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap() > 0.0, "Augmented Hammerhead must deal split combat damage");

    let mut_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(mut_ball_id)]), PyRep::Integer(target_id)], &kw).await.expect("Mutated Hobgoblin II engages target");
    let mut_dmg = dict_get(&mut_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap();
    assert!((mut_dmg - base_dmg * 2.25).abs() < 1e-2, "Mutated drone with damageMultiplier=2.25 must deal 2.25x base damage");

    let (applied, destroyed) = apply_damage_to_drone_in_space(&ctx, sys_id, mut_ball_id, DamageVector { em: 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 }).await;
    assert!(applied > 0.0 && !destroyed);
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(mut_ball_id)], &kw).await.unwrap();
    let saved_mut = ctx.db.get_item(mut_hob_id).unwrap();
    assert_eq!((saved_mut.dynamic_attributes[&64], saved_mut.dynamic_attributes[&37]), (base_hob_mult * 2.25, 4500.0));

    let (_, aug_destroyed) = apply_damage_to_drone_in_space(&ctx, sys_id, aug_ball_id, DamageVector::new(10_000.0, 10_000.0, 10_000.0, 10_000.0)).await;
    assert!(aug_destroyed && ctx.db.get_item(aug_id).is_none());

    // 3. Prove mutated `trackingSpeed` (`attr 160`) directly affects combat damage against a fast-moving small-signature target (`RV-C7-07`).
    let mut fast_cs = ctx.build_type_combat_state(587);
    fast_cs.shield_hp = 50_000.0;
    fast_cs.max_shield_hp = 50_000.0;
    fast_cs.signature_radius = 35.0;
    let _ = sys_handle.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::new(0.0, 1_500.0, 0.0),
            max_velocity: 1_500.0, inertia: 1.0, mass: 1_000_000.0, radius: 35.0, speed_fraction: 1.0, mode: MovementMode::Stop,
        },
        fast_cs,
    ).await;
    let _ = sys_handle.add_target_lock(ship_id, eve_destiny::TargetLock { target_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let launch_pair = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(base_hob_id), PyRep::Integer(track_hob_id)])], &kw).await.unwrap();
    let (b_ball, t_ball) = match launch_pair { PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()), _ => panic!() };
    let b_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(b_ball)]), PyRep::Integer(target_id)], &kw).await.unwrap();
    let t_eng = dispatch_combat_rpc(&ctx, &mut sess, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(t_ball)]), PyRep::Integer(target_id)], &kw).await.unwrap();
    let b_moving_dmg = dict_get(&b_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap();
    let t_moving_dmg = dict_get(&t_eng, "totalDamageDealt").and_then(PyRep::as_f64).unwrap();
    assert!(b_moving_dmg < base_dmg * 0.75, "Fast transversal target must reduce unmutated drone damage via tracking formula");
    assert!(t_moving_dmg > b_moving_dmg * 1.25, "Mutated trackingSpeed (4.5 vs SDE base) with identical damageMultiplier must deal >25% higher combat damage on fast target (got {t_moving_dmg} vs {b_moving_dmg})");
}

// parity: plan:C-7
// parity: feat:lifecycle.wreck-salvage_with_loot
#[tokio::test]
async fn test_plan_c7_salvage_with_loot_and_tractor_beams() {
    eve_testkit::attest(&["plan:C-7", "feat:lifecycle.wreck-salvage_with_loot"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, ship_id, wreck_owner_id) = (96_100_020i64, 86_100_200i64, 96_100_099i64);
    let mut sess = setup_pilot_in_space(&ctx, 61020, char_id, 1_000_125, ship_id, 645, sys_id, DVec3::ZERO).await;

    let (wreck_id, loot_id, tractor_id) = (86_100_201i64, 86_100_202i64, 86_100_203i64);
    let wreck_pos = DVec3::new(3_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3438, TrainedSkillRecord { type_id: 3438, level: 1, skill_points: 1_000.0 });
        snap.items.insert(wreck_id, item_rec(wreck_id, 26563, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_id, item_rec(loot_id, 34, wreck_owner_id, wreck_id, InventoryFlag::Cargo, 3_000_000, false));
        snap.items.insert(tractor_id, item_rec(tractor_id, 24348, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: wreck_id, type_id: 26563, position: wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let tr_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(tractor_id), PyRep::Integer(wreck_id)], &kw).await.expect("Activate Tractor Beam");
    assert_eq!(dict_get(&tr_res, "arrivedWithinLootRange"), Some(&PyRep::Bool(true)));
    let (_, pulled_wreck) = sys_handle.get_ball_snapshot(wreck_id).await.unwrap().unwrap();
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&wreck_id).unwrap().owner_id = wreck_owner_id; });

    // 1. Over-capacity loot (> 27,500 m³) rejects with `SalvageTooMuchLootBody` and leaves wreck intact.
    let over_err = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, Some(30836))
        .await.expect_err("Loot exceeding jetcan capacity must reject with SalvageTooMuchLootBody");
    assert_eq!(over_err.code, "SalvageTooMuchLootBody");
    assert!(ctx.db.get_item(wreck_id).is_some());

    // 2. Reduce loot volume, exercise a failed salvager roll (`salvaged == false`), then a successful roll (`salvaged == true`).
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&loot_id).unwrap().quantity = 5_000; });
    let mut saw_fail = false;
    for s in 1..50u64 {
        ctx.set_mechanic_rng_seed(Some(s));
        let rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, None).await.expect("Salvager cycle executes");
        if dict_get(&rep, "salvaged") == Some(&PyRep::Bool(false)) {
            saw_fail = true;
            assert!(ctx.db.get_item(wreck_id).is_some(), "Wreck must remain in space after failed salvager cycle");
            break;
        }
    }
    assert!(saw_fail, "Must observe a failed salvager probability roll");

    ctx.db.with_mut_snapshot(|snap| { snap.skills.get_mut(&char_id).unwrap().get_mut(&3438).unwrap().level = 5; });
    for s in 1..100u64 {
        ctx.set_mechanic_rng_seed(Some(s));
        let rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, Some(30836)).await.expect("Salvage cycle");
        if dict_get(&rep, "salvaged") == Some(&PyRep::Bool(true)) {
            break;
        }
    }
    assert!(ctx.db.get_item(wreck_id).is_none(), "Wreck must be removed once salvaged");

    let moved_loot = ctx.db.get_item(loot_id).unwrap();
    let jetcan = ctx.db.get_item(moved_loot.location_id).expect("Spawned loot jetcan");
    assert_eq!((jetcan.type_id, jetcan.location_id, jetcan.owner_id), (23, sys_id as i64, wreck_owner_id));
    let (_, jetcan_ball) = sys_handle.get_ball_snapshot(jetcan.item_id).await.unwrap().expect("Jetcan ball spawned");
    assert!((jetcan_ball.position - pulled_wreck.position).length() < 1.0);
}

pub(crate) mod fighter_abandon_war_targets_and_wrecks {
//! Dedicated integration test suite for fighter lifecycle, war targets, high-sec criminal movement, wormhole aggression exemption, and wreck/jetcan item taking.

use super::{dict_get, far_celestial_id, item_rec, setup_pilot_in_space};
use eve_db::{InventoryFlag, IskCents, PersistedNegKind, PersistedWarMeta, PersistedWarNegotiation, UpwellStructureRecord, WarRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{check_crimewatch_action_restriction, dispatch_combat_rpc, is_legal_war_or_suspect_target};
use glam::DVec3;
use std::collections::HashMap;

// parity: feat:lifecycle.fighters-left-in-space
#[tokio::test]
async fn test_fighters_left_in_space_abandon_persist_and_scoop() {
    eve_testkit::attest(&["feat:lifecycle.fighters-left-in-space"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let station_pos = ctx.sde.stations[&60_003_760].position;
    let (char_id, carrier_id) = (96_100_030i64, 86_100_300i64);
    let mut sess = setup_pilot_in_space(&ctx, 61030, char_id, 1_000_125, carrier_id, 23757, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let (scooper_id, scooper_carrier_id) = (96_100_031i64, 86_100_301i64);
    let mut scooper_sess = setup_pilot_in_space(&ctx, 61031, scooper_id, 1_000_125, scooper_carrier_id, 23757, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    let (fighter_id, fighter_logoff_id, fighter_death_id, fighter_jump_id) = (86_100_310i64, 86_100_311i64, 86_100_312i64, 86_100_313i64);
    ctx.db.with_mut_snapshot(|snap| {
        for fid in [fighter_id, fighter_logoff_id, fighter_death_id, fighter_jump_id] {
            snap.items.insert(fid, item_rec(fid, 23055, char_id, carrier_id, InventoryFlag::FighterBay, 9, false));
        }
    });
    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_id)], &kw, None).await.expect("LoadFightersToTube");

    let launch_rep = ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launch_rep else { panic!("Expected List") };
    let f_ball_id = balls[0].as_i64().unwrap();

    // 1. Carrier warping within the same system does NOT abandon fighter squadrons (`RV-C7-04`).
    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let celestial_id = far_celestial_id(&ctx, sys_id);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("Carrier warps within system");
    let f_after_warp = ctx.active_fighters.read().unwrap()[&f_ball_id].clone();
    assert!(!f_after_warp.is_abandoned && f_after_warp.parent_ship_id == carrier_id);

    // 2. Carrier docking at station abandons the fighter squadron in space, frees tube 0, and keeps the fighter ball in space.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, mut carrier_ball) = sys_handle.get_ball_snapshot(carrier_id).await.unwrap().unwrap();
    carrier_ball.position = station_pos + DVec3::new(1_000.0, 0.0, 0.0);
    carrier_ball.velocity = DVec3::ZERO;
    carrier_ball.speed_fraction = 0.0;
    carrier_ball.mode = MovementMode::Stop;
    let _ = sys_handle.spawn_ball(carrier_ball).await;
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Carrier docks at station");

    let f_item_docked = ctx.db.get_item(fighter_id).unwrap();
    assert_eq!((f_item_docked.location_id, f_item_docked.flag), (sys_id as i64, InventoryFlag::None));
    let f_state = ctx.active_fighters.read().unwrap()[&f_ball_id].clone();
    assert_eq!((f_state.is_abandoned, f_state.parent_ship_id, f_state.target_id), (true, 0, None));
    assert!(ctx.fighter_tube_loads.read().unwrap().get(&(carrier_id, 0)).is_none(), "Carrier fighter tube must be cleared when squadron is abandoned");
    assert!(sys_handle.get_ball_snapshot(f_ball_id).await.unwrap().is_some());

    // 3. Server restart: `get_or_spawn` populates the persisted fighter ball into the ballpark, and another carrier scoops, loads, and re-launches it.
    let reloaded_ctx = ServerContext::with_sde_and_repository(ctx.sde.clone(), ctx.db.clone());
    eve_server::sp6_lifecycle::seed_persisted_abandoned_drones(&reloaded_ctx);
    assert!(reloaded_ctx.active_fighters.read().unwrap().contains_key(&f_ball_id));
    let reloaded_sys = reloaded_ctx.systems.get_or_spawn(sys_id);
    let (_, reloaded_f_ball) = reloaded_sys.get_ball_snapshot(f_ball_id).await.unwrap().expect("Fighter ball must be populated in ballpark on restart");
    let _ = reloaded_sys.spawn_ball(Ball {
        id: scooper_carrier_id, type_id: 23757, position: reloaded_f_ball.position + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 500.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    reloaded_ctx.dispatch_rpc(&mut scooper_sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(f_ball_id)], &kw, None)
        .await.expect("Scoop abandoned fighter after restart");
    let scooped_f = reloaded_ctx.db.get_item(fighter_id).unwrap();
    assert_eq!((scooped_f.owner_id, scooped_f.location_id, scooped_f.flag), (scooper_id, scooper_carrier_id, InventoryFlag::FighterBay));
    reloaded_ctx.dispatch_rpc(&mut scooper_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_id)], &kw, None)
        .await.expect("Load scooped fighter into tube 0");
    let relaunch_rep = reloaded_ctx.dispatch_rpc(&mut scooper_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await.expect("Re-launch scooped fighter from tube 0");
    let PyRep::List(relaunched) = relaunch_rep else { panic!("Expected List") };
    assert_eq!(relaunched.len(), 1);

    // 4. Verify fighter abandonment on carrier jump (`CmdJumpToFleetDeployableBeacon`), logoff (`handle_character_disconnect_in_space`), and carrier death (`execute_death_pipeline`).
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Carrier undocks");
    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_jump_id)], &kw, None).await.unwrap();
    let PyRep::List(jump_balls) = ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap() else { panic!() };
    let f_jump_ball = jump_balls[0].as_i64().unwrap();
    let jita_ly = ctx.sde.solar_systems[&sys_id].galactic_pos_ly;
    let dest_sys = *ctx.sde.solar_systems.iter()
        .find(|(&sid, s)| {
            sid != sys_id && s.security_status < 0.45 && sid < 31_000_000 && s.region_id != 10_000_070 && sid != 30_100_000
                && ((s.galactic_pos_ly[0] - jita_ly[0]).powi(2) + (s.galactic_pos_ly[1] - jita_ly[1]).powi(2) + (s.galactic_pos_ly[2] - jita_ly[2]).powi(2)).sqrt() < 6.9
        })
        .unwrap().0;
    let beacon_id = 86_100_320i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(21611, eve_db::TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
        snap.items.insert(beacon_id, item_rec(beacon_id, 56099, char_id, dest_sys as i64, InventoryFlag::None, 1, true));
        snap.items.insert(86_100_321, item_rec(86_100_321, 16274, char_id, carrier_id, InventoryFlag::Cargo, 50_000, false));
    });
    ctx.advance_secs(12);
    let beyonce_jita = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_jita), "CmdJumpToFleetDeployableBeacon", &[PyRep::Integer(char_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)], &kw, None)
        .await.expect("Carrier jumps to deployable cyno beacon");
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(ctx.active_fighters.read().unwrap()[&f_jump_ball].is_abandoned);
    assert_eq!(ctx.db.get_item(fighter_jump_id).unwrap().location_id, sys_id as i64);
    eve_server::services::space_destiny::break_location_change_invulnerability(&ctx, char_id);
    let sys_dest = ctx.systems.get_or_spawn(dest_sys);
    if let Ok(Some(mut cs)) = sys_dest.get_combat_state(carrier_id).await {
        cs.is_cloaked = false;
        let _ = sys_dest.set_combat_state(carrier_id, cs).await;
    }

    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_logoff_id)], &kw, None).await.unwrap();
    let PyRep::List(logoff_balls) = ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap() else { panic!() };
    let f_logoff_ball = logoff_balls[0].as_i64().unwrap();
    eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await;
    assert!(ctx.active_fighters.read().unwrap()[&f_logoff_ball].is_abandoned);
    let _ = sys_dest.set_movement_mode(carrier_id, eve_destiny::MovementMode::Stop).await;

    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_death_id)], &kw, None).await.unwrap();
    let PyRep::List(death_balls) = ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap() else { panic!() };
    let f_death_ball = death_balls[0].as_i64().unwrap();
    eve_server::combat::death_pipeline::execute_death_pipeline_sync(&ctx, dest_sys, char_id, carrier_id, 0, DVec3::ZERO, 1).expect("execute_death_pipeline_sync on carrier");
    assert!(ctx.active_fighters.read().unwrap()[&f_death_ball].is_abandoned);
    let f_death_item = ctx.db.get_item(fighter_death_id).expect("Abandoned fighter in space must not be destroyed by loot fairy on carrier death");
    assert_eq!((f_death_item.location_id, f_death_item.flag), (dest_sys as i64, InventoryFlag::None));
}

// parity: feat:war.war-targets
#[tokio::test]
async fn test_war_targets_mutual_allies_and_retraction_crimewatch() {
    eve_testkit::attest(&["feat:war.war-targets"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (aggr_char, def_char, ally_char) = (96_100_040i64, 96_100_041i64, 96_100_042i64);
    let (aggr_ship, def_ship, ally_ship) = (86_100_400i64, 86_100_401i64, 86_100_402i64);
    let (aggr_corp, def_corp, ally_corp) = (2_000_101i64, 2_000_102i64, 2_000_103i64);
    let mut aggr_sess = setup_pilot_in_space(&ctx, 61040, aggr_char, aggr_corp, aggr_ship, 587, sys_id, DVec3::ZERO).await;
    let _ = setup_pilot_in_space(&ctx, 61041, def_char, def_corp, def_ship, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = setup_pilot_in_space(&ctx, 61042, ally_char, ally_corp, ally_ship, 587, sys_id, DVec3::new(2_000.0, 0.0, 0.0)).await;

    let gun_id = 86_100_410i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&aggr_char).unwrap().security_status = 2.5;
        snap.items.insert(gun_id, item_rec(gun_id, 2889, aggr_char, aggr_ship, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(86_100_411, item_rec(86_100_411, 182, aggr_char, aggr_ship, InventoryFlag::HiSlot0, 100, false));
        snap.module_online_states.insert(gun_id, true);
    });
    dispatch_combat_rpc(&ctx, &mut aggr_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();
    let _ = ctx.systems.get_or_spawn(sys_id).add_target_lock(aggr_ship, eve_destiny::TargetLock { target_id: def_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;

    let now = ctx.now_unix_secs();
    // 1. Non-mutual war declared at `now` (within 24h spool-up): not yet legal in high-sec; weapon activation on Safety Green is rejected.
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(5001, WarRecord {
            war_id: 5001, aggressor_corp_id: aggr_corp, defender_corp_id: def_corp, started_unix: now, mutual: false, active: true,
        });
    });
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
    let pre_err = dispatch_combat_rpc(&ctx, &mut aggr_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(def_ship)], &kw)
        .await.expect_err("Before 24h war spool-up, high-sec aggression on Safety Green must fail");
    assert_eq!(pre_err.code, "CantDoInCurrentSafetyLevel");

    // 2. Active war (`started_unix + 86_400 <= now`) + accepted defender ally -> legal in high-sec for both defender and ally with zero sec status loss!
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&5001).unwrap().started_unix = now.saturating_sub(90_000);
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1, war_id: 5001, kind: PersistedNegKind::Ally, from_corp: ally_corp,
            isk: IskCents::from_isk(0), message: String::new(), status: "Accepted".into(), effective_unix: Some(now.saturating_sub(60)),
        });
    });
    assert!(ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
    assert!(ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, ally_char, aggr_char, now)));
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, ally_char, def_char, now)));

    let sec_before = ctx.db.get_character(aggr_char).unwrap().security_status;
    dispatch_combat_rpc(&ctx, &mut aggr_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(def_ship)], &kw)
        .await.expect("Active war target can be legally attacked in high-sec even on Safety Green");
    let sec_after = ctx.db.get_character(aggr_char).unwrap().security_status;
    assert_eq!(sec_after, sec_before, "Attacking a legal war target in high-sec must incur 0.0 sec status loss");
    let cw_st = ctx.crimewatch.read().unwrap().states.get(&aggr_char).cloned().unwrap();
    assert_eq!((cw_st.criminal_flag_until_unix, cw_st.suspect_flag_until_unix), (0, 0));
    assert!(cw_st.weapon_timer_until_unix > now && cw_st.pvp_timer_until_unix > now);

    // 3. War finished (`finish_at_unix <= now`) -> no longer a legal war target.
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.wars.insert(5001, PersistedWarMeta { war_hq_id: Some(60_003_760), open_for_allies: true, finish_at_unix: Some(now) });
    });
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
}

// parity: feat:navigation.wormhole-aggression-exemption
#[tokio::test]
async fn test_crimewatch_highsec_criminal_movement_and_wormhole_exemption() {
    eve_testkit::attest(&[
        "feat:navigation.wormhole-aggression-exemption",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (sys_id, char_id, ship_id) = (30_000_142u32, 90_000_001i64, 600_000_001i64);

    let mut sess = SessionState::new(661);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("Select Pilot 1");
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(eve_db::CosmicSignatureRecord {
            signature_id: "JMM-142".into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole (K162)".into(),
            position: [149_597_870_700.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(26_272),
            destination_system_id: Some(31_000_005),
            created_unix: ctx.now_unix(),
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 375_000_000.0,
            collapsed: false,
        });
    });
    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&sys_id].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(sys_id, &wh_sig_id);
    let jita_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == sys_id).cloned().unwrap();
    let jita_sys = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]);
    let _ = jita_sys.spawn_ball_with_combat(
        Ball {
            id: ship_id, type_id: 587, position: ship_pos, velocity: DVec3::ZERO,
            max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(587),
    ).await;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    let fitted_mod_id = 86_100_490i64;
    let struct_id = 1_040_000_000_490i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        snap.items.insert(ship_id, item_rec(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(fitted_mod_id, item_rec(fitted_mod_id, 2889, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id, type_id: 35832, solar_system_id: sys_id, owner_corp_id: ch.corporation_id,
                name: "Test Astrahus".into(), position: ship_pos.to_array(), state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
    ctx.advance_secs(12);

    // 1. High-sec Criminal timer in non-capsule ship: dispatch RPC handlers and assert warp, stargate jump, wormhole jump, cyno jump, bridge, portal jump, and structure beacon jump are all rejected!
    let now = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 900;
    }
    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let warp_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760), PyRep::Real(0.0)], &kw, None)
        .await.expect_err("High-sec criminal in non-capsule ship cannot warp");
    assert_eq!(warp_err.code, "CantWarpAwayGlobalCriminalFlag");

    let gate_crim_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)], &kw, None)
        .await.expect_err("High-sec criminal in non-capsule ship cannot jump stargate");
    assert_eq!(gate_crim_err.code, "CantJumpGlobalCriminalFlag");

    let wh_crim_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None)
        .await.expect_err("High-sec criminal with pending CONCORD response cannot enter wormhole");
    assert_eq!(wh_crim_err.code, "CantEnterWormholeGlobalCriminalFlag");

    let j1_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(j1_err.code, "CantBeaconJumpGlobalCriminalFlag");
    let j2_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(j2_err.code, "CantBeaconJumpGlobalCriminalFlag");
    let j3_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpThroughFleet", &[PyRep::Integer(char_id), PyRep::Integer(ship_id)], &kw, None).await.unwrap_err();
    assert_eq!(j3_err.code, "CantBeaconJumpGlobalCriminalFlag");
    let j4_err = ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(30_000_144), PyRep::Integer(struct_id)], &kw, None).await.unwrap_err();
    assert_eq!(j4_err.code, "CantBeaconJumpGlobalCriminalFlag");

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().type_id = 670; });
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "warp", now).is_ok());
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "stargate_jump", now).is_ok());
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().type_id = 587; });

    // 2. Active Weapons Timer (no criminal timer): blocks stargate jump, dock, cyno jump/bridge/portal, structure tether, and in-space module refit, while allowing WormholeJump (with 5-minute polarization).
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = 0;
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Active weapons timer must break/prevent Upwell structure tethering");

    let ship_inv_bound = ctx.bind_object(&sess, "invbroker", ship_id).unwrap();
    let refit_err = ctx.dispatch_rpc(
        &mut sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(fitted_mod_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect_err("Unfitting/refitting module in space must be blocked under active weapons timer");
    assert_eq!(refit_err.code, "CannotRefitWithWeaponsTimer");

    let gate_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)], &kw, None)
        .await.expect_err("Weapons timer blocks stargate jump");
    assert_eq!(gate_wpn_err.code, "CannotJumpWithWeaponsTimer");
    let dock_wpn_err = ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks docking at structure");
    assert_eq!(dock_wpn_err.code, "WeaponsTimerActive");
    let jump_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump drive");
    assert_eq!(jump_wpn_err.code, "CannotJumpWithWeaponsTimer");
    let bridge_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump bridge");
    assert_eq!(bridge_wpn_err.code, "CannotJumpWithWeaponsTimer");
    let portal_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpThroughFleet", &[PyRep::Integer(char_id), PyRep::Integer(ship_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump portal");
    assert_eq!(portal_wpn_err.code, "CannotJumpWithWeaponsTimer");
    let struct_wpn_err = ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(30_000_144), PyRep::Integer(struct_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks structure cyno jump");
    assert_eq!(struct_wpn_err.code, "CannotJumpWithWeaponsTimer");

    ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None)
        .await.expect("WormholeJump succeeds with active weapons and PvP timers");
    assert_eq!(sess.solar_system_id, Some(31_000_005));
    let pol_exp = ctx.db.with_snapshot(|snap| snap.wh_polarizations.get(&(char_id, 31_000_005)).copied());
    assert_eq!(pol_exp, Some(now + 300), "Wormhole jump must record 300s polarization timer");
}

// parity: feat:lifecycle.wreck-take_item
#[tokio::test]
async fn test_wreck_take_item_add_multiadd_lootall_crimewatch() {
    eve_testkit::attest(&["feat:lifecycle.wreck-take_item"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (thief_id, thief_ship, victim_id, victim_ship) = (96_100_050i64, 86_100_500i64, 96_100_051i64, 86_100_501i64);
    let mut thief_sess = setup_pilot_in_space(&ctx, 61050, thief_id, 1_000_009, thief_ship, 587, sys_id, DVec3::ZERO).await;
    let _ = setup_pilot_in_space(&ctx, 61051, victim_id, 1_000_009, victim_ship, 587, sys_id, DVec3::new(500.0, 0.0, 0.0)).await;

    let (wreck_id, item_unlocked, item_cargo, item_corp, item_fleet, item_aband, jetcan_id, jetcan_item) = (
        86_100_510i64, 86_100_511i64, 86_100_512i64, 86_100_513i64, 86_100_514i64, 86_100_515i64, 86_100_520i64, 86_100_521i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_id, item_rec(wreck_id, 26561, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(item_unlocked, item_rec(item_unlocked, 34, victim_id, wreck_id, InventoryFlag::Unlocked, 10, false));
        snap.items.insert(item_cargo, item_rec(item_cargo, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(item_corp, item_rec(item_corp, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(item_fleet, item_rec(item_fleet, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(item_aband, item_rec(item_aband, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(jetcan_id, item_rec(jetcan_id, 23, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(jetcan_item, item_rec(jetcan_item, 34, victim_id, jetcan_id, InventoryFlag::Cargo, 10, false));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (cid, tid) in [(wreck_id, 26561u32), (jetcan_id, 23u32)] {
        let _ = sys_handle.spawn_ball(Ball {
            id: cid, type_id: tid, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    let ship_inv_bound = ctx.bind_object(&thief_sess, "invbroker", thief_ship).unwrap();
    dispatch_combat_rpc(&ctx, &mut thief_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();

    // 1. Foreign wreck & foreign jetcan under Safety Green (`2`) must reject `Add`, `MultiAdd`, and `LootAll` with no state change.
    let g1_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "Add", &[PyRep::Integer(item_unlocked), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.unwrap_err();
    assert_eq!(g1_err.code, "CantDoInCurrentSafetyLevel");
    let g2_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(item_cargo)]), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.unwrap_err();
    assert_eq!(g2_err.code, "CantDoInCurrentSafetyLevel");
    let g3_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "Add", &[PyRep::Integer(jetcan_item), PyRep::Integer(jetcan_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.unwrap_err();
    assert_eq!(g3_err.code, "CantDoInCurrentSafetyLevel");
    let g4_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "LootAll", &[PyRep::Integer(wreck_id)], &kw, None).await.unwrap_err();
    assert_eq!(g4_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(ctx.db.get_item(item_unlocked).unwrap().location_id, wreck_id);
    assert_eq!(ctx.db.get_item(jetcan_item).unwrap().location_id, jetcan_id);

    // 2. Under Safety Yellow (`1`), foreign wreck `Add`, `MultiAdd`, and foreign jetcan `Add` succeed, transfer ownership, set 900s suspect flag, and emit `OnSystemCriminalFlagUpdates`.
    dispatch_combat_rpc(&ctx, &mut thief_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)], &kw).await.unwrap();
    let _ = ctx.drain_notifications_for_char(thief_id);

    let add_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(item_unlocked), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Safety Yellow allows Add from foreign wreck");
    assert_eq!(dict_get(&add_res, "suspectFlagged"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.get_item(item_unlocked).unwrap().owner_id, thief_id);
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, ctx.now_unix_secs() + 900);
    assert!(ctx.drain_notifications_for_char(thief_id).iter().any(|n| n.event == "OnSystemCriminalFlagUpdates"));

    let multi_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(item_cargo)]), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Safety Yellow allows MultiAdd from foreign wreck");
    assert_eq!(dict_get(&multi_res, "suspectFlagged"), Some(&PyRep::Bool(true)));

    ctx.crimewatch.write().unwrap().get_or_create(thief_id).suspect_flag_until_unix = 0;
    let jetcan_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(jetcan_item), PyRep::Integer(jetcan_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Safety Yellow allows Add from foreign jetcan");
    assert_eq!(dict_get(&jetcan_res, "suspectFlagged"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, ctx.now_unix_secs() + 900);

    // 3. Same player corporation, same fleet, and abandoned (`owner_id == 0`) wreck takes are legal under Safety Green without suspect flag.
    dispatch_combat_rpc(&ctx, &mut thief_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();
    ctx.crimewatch.write().unwrap().get_or_create(thief_id).suspect_flag_until_unix = 0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&thief_id).unwrap().corporation_id = 2_000_555;
        snap.characters.get_mut(&victim_id).unwrap().corporation_id = 2_000_555;
    });
    let corp_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(item_corp), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Same player corp wreck take is legal under Safety Green");
    assert_eq!(dict_get(&corp_res, "suspectFlagged"), Some(&PyRep::Bool(false)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&thief_id).unwrap().corporation_id = 1_000_009;
        snap.characters.get_mut(&victim_id).unwrap().corporation_id = 1_000_009;
        snap.characters.get_mut(&thief_id).unwrap().fleet_id = Some(77_001);
        snap.characters.get_mut(&victim_id).unwrap().fleet_id = Some(77_001);
    });
    let fleet_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(item_fleet), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Same fleet wreck take is legal under Safety Green");
    assert_eq!(dict_get(&fleet_res, "suspectFlagged"), Some(&PyRep::Bool(false)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&thief_id).unwrap().fleet_id = None;
        snap.characters.get_mut(&victim_id).unwrap().fleet_id = None;
        snap.items.insert(86_100_516, item_rec(86_100_516, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.get_mut(&wreck_id).unwrap().owner_id = 0;
    });
    let explicit_aband = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(86_100_516), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Explicitly abandoned wreck (owner_id == 0) take is legal under Safety Green");
    assert_eq!(dict_get(&explicit_aband, "suspectFlagged"), Some(&PyRep::Bool(false)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, 0);

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&wreck_id).unwrap().owner_id = victim_id; });
    ctx.crimewatch.write().unwrap().get_or_create(victim_id).criminal_flag_until_unix = ctx.now_unix_secs() + 900;
    ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "LootAll", &[PyRep::Integer(wreck_id)], &kw, None)
        .await.expect("Abandoned wreck LootAll is legal under Safety Green");
    assert_eq!(ctx.db.get_item(item_aband).unwrap().location_id, thief_ship);
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, 0);
}

}

// Folded submodule: parity_crimewatch_closure
#[allow(unused_imports, dead_code)]
pub(crate) mod crimewatch_sec_status_and_kill_rights {
    use super::*;

// Closure tests for CrimeWatch Security Status Loss/Gains, Transactions,
// Kill Rights Gain/Sell, and Clone Soldier Tag Tier Bands (`C-8`).
//
// Covers:
// - `feat:crimewatch.sec-status-loss`
// - `feat:crimewatch.sec-status-transactions`
// - `feat:crimewatch.kill-rights-gain`
// - `feat:crimewatch.kill-rights-sell`
// - `feat:crimewatch.sec-status-gain-caps`
// - `feat:crimewatch.sec-tag-tier-bands`


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, CrimeWatchDirector,
};
use eve_server::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, String> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .map_err(|e| format!("{e:?}"))
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or(PyRep::None)
}

fn spawn_ship(
    ctx: &ServerContext,
    owner_id: i64,
    sys_id: u32,
    type_id: u32,
    _pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_and_kill_rights_e2e() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32; // Jita (0.95)
    let lowsec_sys = 30_000_012_u32;  // Asabona (0.32)
    let jita_sec = ctx.sde.solar_systems[&highsec_sys].security_status;
    let lowsec_sec = ctx.sde.solar_systems[&lowsec_sys].security_status;

    let criminal_id = 90_000_101_i64;
    let victim_id = 90_000_102_i64;
    let buyer_id = 90_000_103_i64;
    let victim2_id = 90_000_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, uid, name, sec, bal) in &[
            (criminal_id, 101, "Criminal Pilot", 0.0, 50_000_000.0),
            (victim_id, 102, "Victim Pilot", 5.0, 10_000_000.0),
            (buyer_id, 103, "Buyer Pilot", 0.0, 20_000_000.0),
            (victim2_id, 104, "Victim2 Pilot", 5.0, 10_000_000.0),
        ] {
            if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
                ch.character_id = cid;
                ch.user_id = uid;
                ch.name = name.to_string();
                ch.security_status = sec;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
                snap.characters.insert(cid, ch);
            }
        }
    });

    // 1. Exact mathematical formula verification for security status loss
    // Formula: Mod% = -2.5% (ship) or -25% (pod) * System sec * (1 + (Target sec - Current sec) / 100)
    let loss_ship = compute_sec_status_loss(0.0, 5.0, false, jita_sec);
    let expected_ship_loss = (10.0 - 0.0) * 0.025 * jita_sec * (1.0 + (5.0 - 0.0) / 100.0);
    assert!(
        (loss_ship - expected_ship_loss).abs() < 1e-6,
        "Ship loss formula must match canonical EVE calculation: got {loss_ship}, expected {expected_ship_loss}"
    );

    let loss_pod = compute_sec_status_loss(-2.0, 5.0, true, lowsec_sec);
    let expected_pod_loss = (10.0 - (-2.0)) * 0.25 * lowsec_sec * (1.0 + (5.0 - (-2.0)) / 100.0);
    assert!(
        (loss_pod - expected_pod_loss).abs() < 1e-6,
        "Pod loss formula must match canonical EVE calculation: got {loss_pod}, expected {expected_pod_loss}"
    );

    // Nullsec yields zero sec loss
    assert_eq!(compute_sec_status_loss(0.0, 5.0, false, 0.0), 0.0);
    assert_eq!(compute_sec_status_loss(0.0, 5.0, true, -0.5), 0.0);
    // At or below -10.0, no further loss
    assert_eq!(compute_sec_status_loss(-10.0, 5.0, true, 0.95), 0.0);

    // 2. Highsec illegal aggression triggers sec-status-loss, sec-status-transactions, and kill-rights-gain
    let crim_ship = spawn_ship(&ctx, criminal_id, highsec_sys, 587, DVec3::ZERO);
    let vic_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let buyer_ship = spawn_ship(&ctx, buyer_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));

    let jita_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [(crim_ship, DVec3::ZERO), (vic_ship, DVec3::new(1_000.0, 0.0, 0.0)), (buyer_ship, DVec3::new(2_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    let aggr_res = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_res.is_ok(), "evaluate_and_record_pvp_aggression in highsec must succeed");

    // Re-attack within same engagement must not double-penalize
    let aggr_repeat = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_repeat.is_ok());

    let mut cw = CrimeWatchDirector::new();
    cw.states = ctx.crimewatch.read().unwrap().states.clone();
    let concord_reports = cw.step_concord(&ctx, now + 10, 42).await;
    assert_eq!(concord_reports.len(), 1, "CONCORD must respond to criminal aggression in highsec");
    let hs_kr_id = concord_reports[0].kill_right_id;
    assert!(hs_kr_id > 0, "CONCORD response must award a kill right against highsec criminal");

    let sec_after = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let expected_sec = 0.0 - expected_ship_loss;
    assert!(
        (sec_after - expected_sec).abs() < 1e-6,
        "Security status must decrease once by exact loss: got {sec_after}, expected {expected_sec}"
    );

    // Verify kill right attributes earned on highsec unprovoked attack
    let (kr_owner, kr_target, kr_fee, kr_exp) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.get(&hs_kr_id).expect("kill right in db");
        let exp = s.combat_crimewatch_state.kill_right_expires_unix.get(&hs_kr_id).copied().unwrap_or(0);
        (kr.owner_id, kr.target_id, kr.activation_fee.0, exp)
    });
    assert_eq!(kr_owner, victim_id, "Kill right owner must be the victim");
    assert_eq!(kr_target, criminal_id, "Kill right target must be the aggressor");
    assert_eq!(kr_fee, 0, "Earned kill right starts private with zero activation fee");
    assert_eq!(kr_exp, now + 30 * 86_400, "Kill right must carry exactly 30-day expiration");

    // 3. Lowsec Rules: ship aggression grants Suspect flag & NO kill right; pod aggression awards kill right
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, criminal_id, lowsec_sys, 587, DVec3::ZERO);
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim2_id, lowsec_sys, 670, DVec3::new(600.0, 0.0, 0.0));

    for (id, tid, pos) in [(low_crim_ship, 587, DVec3::ZERO), (low_vic_ship, 587, DVec3::new(500.0, 0.0, 0.0)), (low_vic_pod, 670, DVec3::new(600.0, 0.0, 0.0))] {
        let cs = ctx.build_type_combat_state(tid);
        let _ = lowsec_handle.spawn_ball_with_combat(Ball {
            id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    // Unprovoked ship aggression in lowsec: gives suspect flag, deducts sec, NO kill right
    let kr_count_pre = ctx.db.with_snapshot(|s| s.kill_rights.len());
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, victim_id, false,
    );
    let kr_count_after_ship = ctx.db.with_snapshot(|s| s.kill_rights.len());
    assert_eq!(kr_count_after_ship, kr_count_pre, "Lowsec ship aggression must not award a kill right");

    // Unprovoked pod aggression in lowsec against victim2: awards a 30-day kill right
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, victim2_id, true,
    );
    let (kr_count_after_pod, low_pod_kr_id) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.values().find(|k| k.owner_id == victim2_id && k.target_id == criminal_id).unwrap();
        (s.kill_rights.len(), kr.kill_right_id)
    });
    assert_eq!(kr_count_after_pod, kr_count_pre + 1, "Lowsec pod aggression must award a kill right");
    let low_kr_exp = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&low_pod_kr_id).copied()).unwrap();
    assert_eq!(low_kr_exp, now + 30 * 86_400, "Lowsec pod kill right carries 30-day expiration");

    // 4. Verify GetSecurityStatusTransactions audit log
    let mut sess_crim = SessionState::default();
    sess_crim.user_id = Some(101);
    sess_crim.character_id = Some(criminal_id);
    sess_crim.solar_system_id = Some(highsec_sys);

    let tx_res = rpc(&ctx, &mut sess_crim, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = tx_res else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    for col in &["eventDate", "eventTypeID", "locationID", "modification", "newValue", "otherOwnerID", "otherTypeID"] {
        assert!(columns.iter().any(|c| c == col), "missing column {col}");
    }
    assert!(!rows.is_empty(), "Transactions log must record aggression entries");
    let row0: HashMap<&str, &PyRep> = columns.iter().map(|s| s.as_str()).zip(rows[0].iter()).collect();
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(379)), "Aggression must have eventTypeID = 379");
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(highsec_sys as i64)));
    let modif = match row0.get("modification") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected Real modification, got {other:?}"),
    };
    assert!((modif - (-expected_ship_loss)).abs() < 1e-6);
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::Integer(victim_id)), "otherOwnerID must identify the victim");

    // 5. Kill rights sell, cancel, buy, and activate lifecycle
    let mut sess_vic = SessionState::default();
    sess_vic.user_id = Some(102);
    sess_vic.character_id = Some(victim_id);
    sess_vic.solar_system_id = Some(highsec_sys);

    // Victim lists kill right for 2,000,000 ISK
    let list_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(hs_kr_id), PyRep::Real(2_000_000.0)],
    )
    .await
    .expect("SellKillRight");
    if let PyRep::List(items) = list_res {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_000_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    // Victim cancels sale
    let cancel_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(hs_kr_id)],
    )
    .await
    .expect("CancelSellKillRight");
    assert_eq!(field(&cancel_res, "forSale"), PyRep::Bool(false));
    assert_eq!(ctx.db.with_snapshot(|s| s.kill_rights[&hs_kr_id].activation_fee.0), 0);

    // Re-list for 3,000,000 ISK
    let _ = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(hs_kr_id), PyRep::Real(3_000_000.0)],
    )
    .await
    .expect("SellKillRight re-list");

    // Error checks on purchase
    ctx.db.with_mut_snapshot(|s| {
        let crim = s.characters.get_mut(&criminal_id).unwrap();
        crim.solar_system_id = highsec_sys;
        crim.active_ship_id = crim_ship;
        crim.station_id = None;
    });

    let mut sess_crim_buy = SessionState::default();
    sess_crim_buy.user_id = Some(101);
    sess_crim_buy.character_id = Some(criminal_id);
    sess_crim_buy.solar_system_id = Some(highsec_sys);
    let self_buy_err = rpc(&ctx, &mut sess_crim_buy, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)]).await;
    assert!(self_buy_err.unwrap_err().contains("CannotBuyKillRightAgainstSelf"));

    let owner_buy_err = rpc(&ctx, &mut sess_vic, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)]).await;
    assert!(owner_buy_err.unwrap_err().contains("AlreadyKillRightOwner"));

    // Third-party pilot purchases and activates kill right in space
    let mut sess_buyer = SessionState::default();
    sess_buyer.user_id = Some(103);
    sess_buyer.character_id = Some(buyer_id);
    sess_buyer.solar_system_id = Some(highsec_sys);

    let buy_res = rpc(&ctx, &mut sess_buyer, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)])
        .await
        .expect("BuyKillRight");
    assert_eq!(field(&buy_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&buy_res, "feePaidISK"), PyRep::Real(3_000_000.0));

    // Verify ISK transfer: buyer debited 3M, victim credited 3M
    let (vic_bal, buyer_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&victim_id].balance.to_wire_f64(),
            s.characters[&buyer_id].balance.to_wire_f64(),
        )
    });
    assert!((vic_bal - 13_000_000.0).abs() < 1e-2, "Victim balance must receive 3M ISK fee");
    assert!((buyer_bal - 17_000_000.0).abs() < 1e-2, "Buyer balance must be charged 3M ISK fee");

    // Target pilot has active 15-minute Suspect flag
    let suspect_exp = ctx.crimewatch.read().unwrap().states[&criminal_id].suspect_flag_until_unix;
    assert_eq!(suspect_exp, now + 900, "BuyKillRight must apply a 15-minute suspect flag to target");

    // Owner direct activation of lowsec pod kill right in space
    ctx.db.with_mut_snapshot(|s| {
        let vic2 = s.characters.get_mut(&victim2_id).unwrap();
        vic2.solar_system_id = lowsec_sys;
        vic2.active_ship_id = low_vic_pod;
        vic2.station_id = None;

        let crim = s.characters.get_mut(&criminal_id).unwrap();
        crim.solar_system_id = lowsec_sys;
        crim.active_ship_id = low_crim_ship;
        crim.station_id = None;
    });
    let mut sess_vic2 = SessionState::default();
    sess_vic2.user_id = Some(104);
    sess_vic2.character_id = Some(victim2_id);
    sess_vic2.solar_system_id = Some(lowsec_sys);
    let act_res = rpc(&ctx, &mut sess_vic2, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(low_pod_kr_id)])
        .await
        .expect("ActivateKillRight");
    assert_eq!(field(&act_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&act_res, "suspectFlagged"), PyRep::Bool(true));
}

#[tokio::test]
async fn test_crimewatch_sec_status_gain_caps_and_tag_tier_bands_e2e() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let char_id = 90_000_201_i64;

    // Find a lowsec station offering Security Office (service 27)
    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
            0.0 < sec && sec < 0.45 && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27)
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec Security Office station in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
            ch.character_id = char_id;
            ch.user_id = 201;
            ch.name = "SecTag Pilot".to_string();
            ch.security_status = -9.5;
            ch.balance = IskCents::from_f64_isk(100_000_000.0);
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
            snap.characters.insert(char_id, ch);
        }
        // Seed 5x of each Clone Soldier Tag type in lowsec station hangar
        for &tag_type in &[33138_u32, 33139, 33140, 33141] {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tag_type,
                    owner_id: char_id,
                    location_id: lowsec_station_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // 1. NPC Kill Security Status Gain Caps at +5.0
    let make_npc = |ball_id: i64| NpcCombatEntity {
        ball_id, type_id: 12000, faction_id: 500_010, state: NpcAiState::Idle,
        spawn_group_id: 1, spawn_pos: DVec3::ZERO, target_id: None, bounty_isk: 10_000_000.0,
        optimal_range: 10_000.0, orbit_range: 5_000.0, base_dps: 50.0,
        damage_vector: DamageVector { em: 10.0, thermal: 10.0, kinetic: 15.0, explosive: 15.0 },
        last_attack_tick: 0, has_shield_rep: false, has_armor_rep: false,
    };
    let mut npc_mgr = NpcSpawnManager::new();

    // Gain below +5.0 increases towards +5.0 cap
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 4.95);
    npc_mgr.entities.insert(85_000_001, make_npc(85_000_001));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_001, char_id, now);
    let sec_gain = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec_gain > 4.95 && sec_gain <= 5.0, "NPC kill sec gain must increase towards +5.0 cap (got {sec_gain})");

    // Gain that would exceed +5.0 caps exactly at +5.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 4.9999);
    npc_mgr.entities.insert(85_000_002, make_npc(85_000_002));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_002, char_id, now);
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_at_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must cap at +5.0 (got {sec_at_cap})");

    // Rat kill while already at +5.0 does not exceed +5.0
    npc_mgr.entities.insert(85_000_003, make_npc(85_000_003));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_003, char_id, now);
    let sec_still_cap = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_still_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must not exceed +5.0");

    // Rat kill when sec > +5.0 (e.g. +6.0) does not clamp down to +5.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 6.0);
    npc_mgr.entities.insert(85_000_004, make_npc(85_000_004));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_004, char_id, now);
    let sec_above = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_above - 6.0).abs() < 1e-6, "NPC kill must preserve sec status above +5.0");

    // 2. Highsec station rejection
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(60_003_760);
        ch.solar_system_id = 30_000_142;
        ch.security_status = -9.0;
    });
    let mut sess = SessionState::default();
    sess.user_id = Some(201);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760); // Jita 4-4 (highsec station)
    sess.solar_system_id = Some(30_000_142);

    let highsec_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
    )
    .await;
    assert!(
        highsec_err.unwrap_err().contains("SecurityOfficeRequired"),
        "Highsec station without Security Office must reject tag turn-in"
    );

    // 3. Lowsec Station: All 4 Clone Soldier Tag Bands, Fees, and Out-of-Band Refusal
    let bal_before = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_sys_id;
        ch.balance
    });
    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);

    // Test matrix: (start_sec, valid_tag, invalid_tag, target_sec, expected_sec, fee_isk)
    // Tier 1 Trainer (33138): -10 to -8, 1M ISK
    // Tier 2 Recruiter (33139): -8 to -5, 2M ISK
    // Tier 3 Transporter/Ranger (33140): -5 to -2, 5M ISK
    // Tier 4 Negotiator (33141): -2 to 0, 10M ISK
    for (start_sec, valid_tag, invalid_tag, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, -8.5_f64, -8.5_f64),
        (-6.5_f64, 33139_i64, 33140_i64, -6.0_f64, -6.0_f64),
        (-3.5_f64, 33140_i64, 33141_i64, -3.0_f64, -3.0_f64),
        (-0.5_f64, 33141_i64, 33138_i64, 0.0_f64, 0.0_f64),
    ] {
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = start_sec);

        // Out-of-band refusal
        let bad_res = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await;
        assert!(bad_res.unwrap_err().contains("SecurityTagBandMismatch"), "Out-of-band tag must be refused");

        // Valid in-band turn-in
        let ok_res = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(valid_tag), PyRep::Integer(1))])],
        )
        .await
        .expect("ApplyTagsForSecStatus in-band");
        let new_s = match field(&ok_res, "newSecurityStatus") {
            PyRep::Real(v) => v,
            other => panic!("expected Real newSecurityStatus, got {other:?}"),
        };
        assert!((new_s - expected_sec).abs() < 1e-6);
    }

    // 4. Tags cannot raise security status above 0.0 and excess tags are preserved
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 0.0);
    let at_zero_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
    )
    .await;
    assert!(at_zero_err.unwrap_err().contains("InvalidTargetSecurityStatus"));

    // Partial consumption: starting at -0.2 with 5 Negotiator tags requested -> consumes only 1 tag to reach 0.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = -0.2);
    let excess_res = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(5))])],
    )
    .await
    .expect("ApplyTagsForSecStatus with excess tags");
    assert_eq!(field(&excess_res, "newSecurityStatus"), PyRep::Real(0.0));

    // Verify 3 Negotiator tags remain (5 initial - 1 in band 4 - 1 in excess test = 3)
    let rem_neg_tags = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == char_id && i.type_id == 33141).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(rem_neg_tags, 3, "Excess tags beyond 0.0 cap must not be consumed");

    // Total fee: 1M + 2M + 5M + 10M + 10M = 28M ISK
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(28_000_000).0, "Exact ISK fees must be charged");

    // Wallet journal entry for SecurityTagProcessing
    let has_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "SecurityTagProcessing" && j.amount == IskCents::from_isk(-10_000_000))
    });
    assert!(has_journal, "Wallet journal SecurityTagProcessing entry must exist");

    // 5. Argument validation checks
    for bad_q in [0_i64, -1_i64, 4_294_966_296_i64] {
        let err = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(-0.1), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(bad_q))])],
        )
        .await;
        assert!(err.unwrap_err().contains("InvalidQuantity"));
    }
    let bad_type_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-0.1), PyRep::Dict(vec![(PyRep::Integer(587), PyRep::Integer(1))])],
    )
    .await;
    assert!(bad_type_err.unwrap_err().contains("InvalidSecurityTagType"));

    // 6. Security status transaction log records tag turn-in
    let sec_tx = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = sec_tx else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    assert!(!rows.is_empty(), "Tag turn-in transactions must be recorded");
    let tag_row = rows
        .iter()
        .find(|r| {
            columns
                .iter()
                .zip(r.iter())
                .any(|(c, v)| c == "eventTypeID" && v == &PyRep::Integer(385))
        })
        .expect("Tag turn-in eventTypeID=385 row must be present");
    let row0: HashMap<&str, &PyRep> = columns.iter().map(|s| s.as_str()).zip(tag_row.iter()).collect();
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(385)), "Tag turn-in eventTypeID must be 385");
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(lowsec_sys_id as i64)));
    assert_eq!(row0.get("modification"), Some(&&PyRep::Real(0.5)));
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::None), "Tag turn-in otherOwnerID must be None");
}

#[tokio::test]
async fn test_crimewatch_binding_scope_and_v24_two_float_apply_tags_rpc() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(71_201);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
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

    let sys_id = sess.solar_system_id.expect("solar_system_id") as i64;
    let st_id = sess.station_id.expect("station_id");

    // 1. crimewatch binds to current solar_system_id or docked location, but denies foreign systems/structures
    let bind_denied = ["BindAuthorization", "Denied"].concat();
    assert!(ctx.bind_object(&sess, "crimewatch", sys_id).is_ok());
    assert!(ctx.bind_object(&sess, "crimewatch", st_id).is_ok());
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", 30_000_144)
            .unwrap_err()
            .code,
        bind_denied
    );
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", 1_030_000_099_999)
            .unwrap_err()
            .code,
        bind_denied
    );

    // 2. ApplyTagsForSecStatus in high-sec Jita fails cleanly with SecurityOfficeRequired
    let sec_office_err = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Real(-1.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(sec_office_err.code, "SecurityOfficeRequired");
}
}
