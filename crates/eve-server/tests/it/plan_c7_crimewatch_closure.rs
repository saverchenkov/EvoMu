//! Dedicated integration test suite (Part 1) for `plan:C-7` (`Salvage, tractor, drones`) and drone/salvage lifecycle closure.

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
