//! Dedicated integration test suite for `plan:C-7` (`Salvage, tractor, drones`) and CrimeWatch/lifecycle closure.

use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    PersistedNegKind, PersistedWarMeta, PersistedWarNegotiation, TrainedSkillRecord, WarRecord,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_drone_in_space, check_crimewatch_action_restriction,
    compute_drone_control_range, dispatch_combat_rpc, execute_wreck_salvage_cycle,
    is_legal_war_or_suspect_target, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

fn item_rec(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default()
    }
}

fn far_celestial_id(ctx: &ServerContext, sys_id: u32) -> i64 {
    ctx.sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.position.length() > 1_000_000.0)
        .unwrap()
        .celestial_id as i64
}

async fn setup_pilot_in_space(
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
                AccountRecord {
                    user_id,
                    username: format!("user_{user_id}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.items.insert(ship_id, item_rec(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::None, 1, true));
    });
    let _ = ctx.db.acquire_online_lock(user_id, 7_000 + user_id, Some(char_id), true, ctx.now_unix());
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
                max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 120.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
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
    let mut scooper_sess = setup_pilot_in_space(
        &ctx, 61002, scooper_id, 1_000_125, scooper_ship_id, 645, sys_id, DVec3::new(500.0, 0.0, 0.0),
    ).await;

    let (sentry_id, mobile_id) = (86_100_010i64, 86_100_011i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3436, TrainedSkillRecord {
            type_id: 3436, level: 5, skill_points: 256_000.0,
        });
        snap.items.insert(sentry_id, item_rec(sentry_id, 28215, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        snap.items.insert(mobile_id, item_rec(mobile_id, 2185, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
    });

    // 1. Immobile sentry drone (`Garde II`, 28215): speed 0, rejected at > 2,500m on both ReturnToDroneBay and ScoopToDroneBay, succeeds at <= 2,500m.
    let sentry_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sentry_id)])], &kw)
        .await.expect("Launch Garde II");
    let sentry_ball_id = match sentry_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, sentry_ball) = sys_handle.get_ball_snapshot(sentry_ball_id).await.unwrap().unwrap();
    assert_eq!(sentry_ball.mode, MovementMode::Stop);
    assert_eq!(sentry_ball.speed_fraction, 0.0);

    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(3_000.0, 0.0, 0.0), ..sentry_ball.clone() }).await;
    let ret_err = dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(sentry_ball_id)])], &kw,
    ).await.expect_err("Sentry > 2,500m must reject CmdReturnBay");
    assert_eq!(ret_err.code, "SentryDroneOutOfScoopRange");
    let scoop_err = dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(sentry_ball_id)], &kw)
        .await.expect_err("Sentry > 2,500m must reject ScoopToDroneBay");
    assert_eq!(scoop_err.code, "OutOfScoopRange");

    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(2_000.0, 0.0, 0.0), ..sentry_ball }).await;
    dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(sentry_ball_id)])], &kw,
    ).await.expect("Sentry <= 2,500m returns to DroneBay");
    assert_eq!(ctx.db.get_item(sentry_id).unwrap().flag, InventoryFlag::DroneBay);

    // 2. Mobile drone (`Hammerhead II`, 2185): recall/command only within same system and control range.
    let ctrl_range = compute_drone_control_range(&ctx, char_id, ship_id);
    assert!(ctrl_range >= 20_000.0);
    let mob_launch = dispatch_combat_rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(mobile_id)])], &kw)
        .await.expect("Launch Hammerhead II");
    let mob_ball_id = match mob_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let (_, mob_ball) = sys_handle.get_ball_snapshot(mob_ball_id).await.unwrap().unwrap();
    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(ctrl_range + 15_000.0, 0.0, 0.0), ..mob_ball.clone() }).await;

    dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(mob_ball_id)])], &kw,
    ).await.expect("CmdReturnBay skips out-of-range drone");
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&mob_ball_id].activity_state), 0);
    let oor_home = dispatch_combat_rpc(
        &ctx, &mut sess, "entity", "CmdReturnHome", &[PyRep::List(vec![PyRep::Integer(mob_ball_id)])], &kw,
    ).await.expect("CmdReturnHome returns count of recalled drones");
    assert_eq!(oor_home, PyRep::Integer(0));

    let _ = sys_handle.spawn_ball(Ball {
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::new(100.0, 0.0, 0.0),
        speed_fraction: 1.0,
        mode: MovementMode::Orbit { target_id: ship_id, range: 1_500.0 },
        ..mob_ball
    }).await;

    // 3. Leaving grid via warp (`CmdWarpToStuff`) immediately abandons active drones in space and stops their balls.
    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let celestial_id = far_celestial_id(&ctx, sys_id);
    ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("CmdWarpToStuff");

    let d_rec = ctx.db.get_item(mobile_id).unwrap();
    assert_eq!(d_rec.owner_id, 0, "Abandoned drone must have owner_id == 0");
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&mob_ball_id].is_abandoned));
    let (_, stopped_ball) = sys_handle.get_ball_snapshot(mob_ball_id).await.unwrap().unwrap();
    assert_eq!(stopped_ball.mode, MovementMode::Stop);
    assert_eq!(stopped_ball.speed_fraction, 0.0);

    dispatch_combat_rpc(&ctx, &mut scooper_sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(mob_ball_id)], &kw)
        .await.expect("Second pilot scoops abandoned drone");
    let scooped = ctx.db.get_item(mobile_id).unwrap();
    assert_eq!((scooped.owner_id, scooped.location_id, scooped.flag), (scooper_id, scooper_ship_id, InventoryFlag::DroneBay));
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

    let (sub1, sub2, sub3, gecko_id, aug_id, mut_id) = (
        86_100_101i64, 86_100_102i64, 86_100_103i64, 86_100_104i64, 86_100_105i64, 86_100_106i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3436, TrainedSkillRecord {
            type_id: 3436, level: 5, skill_points: 256_000.0,
        });
        for (iid, tid) in [(sub1, 48744u32), (sub2, 48744), (sub3, 48744), (gecko_id, 33681), (aug_id, 28266)] {
            snap.items.insert(iid, item_rec(iid, tid, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        }
        let mut rec = item_rec(mut_id, 2185, char_id, ship_id, InventoryFlag::DroneBay, 1, true);
        rec.dynamic_attributes = HashMap::from([(64, 2.75), (263, 1200.0), (265, 950.0), (9, 800.0)]);
        rec.source_mutaplasmid_id = Some(49766);
        snap.items.insert(mut_id, rec);
    });

    let gecko_type = ctx.sde.types.get(&33681).unwrap();
    assert_eq!((gecko_type.default_attributes.get(&1272).copied(), gecko_type.volume), (Some(50.0), 50.0));
    let sub_type = ctx.sde.types.get(&48744).unwrap();
    assert_eq!((sub_type.default_attributes.get(&1272).copied(), sub_type.volume), (Some(20.0), 20.0));

    // 1. Launching 3 'Subverted' JVN-UC49 (60 Mbit/s) on a 50 Mbit/s ship fails; 2 succeed (40 Mbit/s).
    let bw_err = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(sub1), PyRep::Integer(sub2), PyRep::Integer(sub3)])], &kw,
    ).await.expect_err("3 x 20 Mbit/s Subverted drones must exceed 50 Mbit/s bandwidth");
    assert_eq!(bw_err.code, "InsufficientDroneBandwidth");

    let launch_two = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(sub1), PyRep::Integer(sub2)])], &kw,
    ).await.expect("2 x 20 Mbit/s Subverted drones must launch");
    let sub_balls: Vec<i64> = match launch_two { PyRep::List(v) => v.iter().filter_map(PyRep::as_i64).collect(), _ => panic!() };
    assert_eq!(sub_balls.len(), 2);

    for bid in sub_balls {
        dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(bid)], &kw).await.unwrap();
    }
    let gecko_launch = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(gecko_id)])], &kw,
    ).await.expect("Gecko (50 Mbit/s) launches into 50 Mbit/s bandwidth");
    let gecko_ball_id = match gecko_launch { PyRep::List(v) => v[0].as_i64().unwrap(), _ => panic!() };
    let gecko_rt = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&gecko_ball_id].clone());
    assert_eq!(gecko_rt.bandwidth_used, 50.0);
    assert!(gecko_rt.damage_vector.em > 0.0 && gecko_rt.damage_vector.thermal > 0.0 && gecko_rt.damage_vector.kinetic > 0.0 && gecko_rt.damage_vector.explosive > 0.0);
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(gecko_ball_id)], &kw).await.unwrap();

    // 2. Launch 'Augmented' Hammerhead + Abyssal Mutated Hammerhead II, verify mutated HP & damage, damage and destruction.
    let launch_mut = dispatch_combat_rpc(
        &ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(aug_id), PyRep::Integer(mut_id)])], &kw,
    ).await.expect("Launch Augmented + Mutated drones");
    let (aug_ball_id, mut_ball_id) = match launch_mut { PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()), _ => panic!() };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut_cs = sys_handle.get_combat_state(mut_ball_id).await.unwrap().unwrap();
    assert_eq!((mut_cs.max_shield_hp, mut_cs.max_armor_hp, mut_cs.max_structure_hp), (1200.0, 950.0, 800.0));

    let (applied, destroyed) = apply_damage_to_drone_in_space(
        &ctx, sys_id, mut_ball_id, DamageVector { em: 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
    ).await;
    assert!(applied > 0.0 && !destroyed);
    let damaged_cs = sys_handle.get_combat_state(mut_ball_id).await.unwrap().unwrap();
    assert!(damaged_cs.shield_hp < 1200.0);
    dispatch_combat_rpc(&ctx, &mut sess, "ship", "ScoopToDroneBay", &[PyRep::Integer(mut_ball_id)], &kw).await.unwrap();
    let saved_mut = ctx.db.get_item(mut_id).unwrap();
    assert_eq!(saved_mut.dynamic_attributes[&64], 2.75);

    let (_, aug_destroyed) = apply_damage_to_drone_in_space(
        &ctx, sys_id, aug_ball_id, DamageVector::new(10_000.0, 10_000.0, 10_000.0, 10_000.0),
    ).await;
    assert!(aug_destroyed && ctx.db.get_item(aug_id).is_none());
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
    let (char_id, ship_id) = (96_100_020i64, 86_100_200i64);
    let mut sess = setup_pilot_in_space(&ctx, 61020, char_id, 1_000_125, ship_id, 645, sys_id, DVec3::ZERO).await;

    let (wreck_id, loot_id, tractor_id) = (86_100_201i64, 86_100_202i64, 86_100_203i64);
    let wreck_pos = DVec3::new(3_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3438, TrainedSkillRecord {
            type_id: 3438, level: 5, skill_points: 256_000.0, // Salvaging V
        });
        snap.items.insert(wreck_id, item_rec(wreck_id, 26561, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_id, item_rec(loot_id, 34, char_id, wreck_id, InventoryFlag::Cargo, 3_000_000, false));
        snap.items.insert(tractor_id, item_rec(tractor_id, 24348, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: wreck_id, type_id: 26561, position: wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let tr_res = dispatch_combat_rpc(
        &ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(tractor_id), PyRep::Integer(wreck_id)], &kw,
    ).await.expect("Activate Tractor Beam");
    assert_eq!(dict_get(&tr_res, "arrivedWithinLootRange"), Some(&PyRep::Bool(true)));
    let (_, pulled_wreck) = sys_handle.get_ball_snapshot(wreck_id).await.unwrap().unwrap();

    let over_err = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, Some(30836))
        .await.expect_err("Loot exceeding jetcan capacity must reject with SalvageTooMuchLootBody");
    assert_eq!(over_err.code, "SalvageTooMuchLootBody");
    assert!(ctx.db.get_item(wreck_id).is_some());

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&loot_id).unwrap().quantity = 5_000; });
    let salv_rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, Some(30836))
        .await.expect("Salvage cycle succeeds");
    assert_eq!(dict_get(&salv_rep, "salvaged"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.get_item(wreck_id).is_none());

    let moved_loot = ctx.db.get_item(loot_id).unwrap();
    let jetcan = ctx.db.get_item(moved_loot.location_id).expect("Spawned loot jetcan");
    assert_eq!((jetcan.type_id, jetcan.location_id), (23, sys_id as i64));
    let (_, jetcan_ball) = sys_handle.get_ball_snapshot(jetcan.item_id).await.unwrap().expect("Jetcan ball spawned");
    assert!((jetcan_ball.position - pulled_wreck.position).length() < 1.0);
}

// parity: feat:lifecycle.fighters-left-in-space
#[tokio::test]
async fn test_fighters_left_in_space_abandon_persist_and_scoop() {
    eve_testkit::attest(&["feat:lifecycle.fighters-left-in-space"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, carrier_id) = (96_100_030i64, 86_100_300i64);
    let mut sess = setup_pilot_in_space(&ctx, 61030, char_id, 1_000_125, carrier_id, 23757, sys_id, DVec3::ZERO).await;
    let (scooper_id, scooper_carrier_id) = (96_100_031i64, 86_100_301i64);
    let mut scooper_sess = setup_pilot_in_space(
        &ctx, 61031, scooper_id, 1_000_125, scooper_carrier_id, 23757, sys_id, DVec3::new(1_000.0, 0.0, 0.0),
    ).await;

    let fighter_id = 86_100_310i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fighter_id, item_rec(fighter_id, 23055, char_id, carrier_id, InventoryFlag::FighterBay, 9, false));
    });
    ctx.dispatch_rpc(
        &mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_id)], &kw, None,
    ).await.expect("LoadFightersToTube");

    let launch_rep = ctx.dispatch_rpc(
        &mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None,
    ).await.expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launch_rep else { panic!("Expected List") };
    let f_ball_id = balls[0].as_i64().unwrap();

    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let celestial_id = far_celestial_id(&ctx, sys_id);
    ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Carrier warps away");

    let f_state = ctx.active_fighters.read().unwrap()[&f_ball_id].clone();
    assert_eq!((f_state.is_abandoned, f_state.parent_ship_id, f_state.target_id), (true, 0, None));

    let reloaded_ctx = ServerContext::with_sde_and_repository(ctx.sde.clone(), ctx.db.clone());
    eve_server::sp6_lifecycle::seed_persisted_abandoned_drones(&reloaded_ctx);
    assert!(reloaded_ctx.active_fighters.read().unwrap().contains_key(&f_ball_id));
    let reloaded_sys = reloaded_ctx.systems.get_or_spawn(sys_id);
    let _ = reloaded_sys.spawn_ball(Ball {
        id: scooper_carrier_id, type_id: 23757, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 100.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    reloaded_ctx.dispatch_rpc(
        &mut scooper_sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(f_ball_id)], &kw, None,
    ).await.expect("Scoop abandoned fighter after restart");
    let scooped_f = reloaded_ctx.db.get_item(fighter_id).unwrap();
    assert_eq!((scooped_f.owner_id, scooped_f.location_id, scooped_f.flag), (scooper_id, scooper_carrier_id, InventoryFlag::FighterBay));
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
        snap.items.insert(gun_id, item_rec(gun_id, 2889, aggr_char, aggr_ship, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(86_100_411, item_rec(86_100_411, 182, aggr_char, aggr_ship, InventoryFlag::HiSlot0, 100, false));
        snap.module_online_states.insert(gun_id, true);
    });
    dispatch_combat_rpc(&ctx, &mut aggr_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();

    let now = ctx.now_unix_secs();
    // 1. Non-mutual war declared at `now` (within 24h spool-up): not yet legal in high-sec; weapon activation on Safety Green is rejected.
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(5001, WarRecord {
            war_id: 5001, aggressor_corp_id: aggr_corp, defender_corp_id: def_corp,
            started_unix: now, mutual: false, active: true,
        });
    });
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
    let pre_err = dispatch_combat_rpc(
        &ctx, &mut aggr_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(def_ship)], &kw,
    ).await.expect_err("Before 24h war spool-up, high-sec aggression on Safety Green must fail");
    assert_eq!(pre_err.code, "SafetyLevelRestricted");

    // 2. Active war (`started_unix + 86_400 <= now`) + accepted defender ally -> legal in high-sec for both defender and ally!
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&5001).unwrap().started_unix = now.saturating_sub(90_000);
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1, war_id: 5001, kind: PersistedNegKind::Ally, from_corp: ally_corp,
            isk: IskCents::from_isk(0), message: String::new(), status: "Accepted".into(),
            effective_unix: Some(now.saturating_sub(60)),
        });
    });
    assert!(ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
    assert!(ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, ally_char, aggr_char, now)));
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, ally_char, def_char, now)));

    dispatch_combat_rpc(
        &ctx, &mut aggr_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(def_ship)], &kw,
    ).await.expect("Active war target can be legally attacked in high-sec even on Safety Green");
    let cw_st = ctx.crimewatch.read().unwrap().states.get(&aggr_char).cloned().unwrap();
    assert_eq!((cw_st.criminal_flag_until_unix, cw_st.suspect_flag_until_unix), (0, 0));
    assert!(cw_st.weapon_timer_until_unix > now && cw_st.pvp_timer_until_unix > now);

    // 3. War finished (`finish_at_unix <= now`) -> no longer a legal war target.
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.wars.insert(5001, PersistedWarMeta {
            war_hq_id: Some(60_003_760), open_for_allies: true, finish_at_unix: Some(now),
        });
    });
    assert!(!ctx.db.with_snapshot(|s| is_legal_war_or_suspect_target(s, aggr_char, def_char, now)));
}

// parity: feat:lifecycle.crimewatch-highsec-criminal-movement
// parity: feat:navigation.wormhole-aggression-exemption
#[tokio::test]
async fn test_crimewatch_highsec_criminal_movement_and_wormhole_exemption() {
    eve_testkit::attest(&[
        "feat:lifecycle.crimewatch-highsec-criminal-movement",
        "feat:navigation.wormhole-aggression-exemption",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (sys_id, char_id, ship_id) = (30_000_142u32, 90_000_001i64, 600_000_001i64);

    let mut sess = SessionState::new(661);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("Select Pilot 1");
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&sys_id].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(sys_id, &wh_sig_id);
    let jita_sys = ctx.systems.get_or_spawn(sys_id);
    let _ = jita_sys.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
        velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 50.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        snap.items.insert(ship_id, item_rec(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    ctx.advance_secs(12);

    let now = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    for blocked in ["stargate_jump", "dock", "jump_drive", "jump_bridge", "jump_portal"] {
        let err = check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, blocked, now)
            .expect_err("Weapons timer must block action");
        assert_eq!(err.code, "WeaponsTimerActive");
    }
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "wormhole_jump", now).is_ok());

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 900;
    }
    let beyonce_bound = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let warp_err = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60_003_760), PyRep::Real(0.0)], &kw, None,
    ).await.expect_err("High-sec criminal in non-capsule ship cannot warp");
    assert_eq!(warp_err.code, "CriminalWarpRestricted");

    let wh_crim_err = ctx.dispatch_rpc(
        &mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None,
    ).await.expect_err("High-sec criminal with pending CONCORD response cannot enter wormhole");
    assert!(wh_crim_err.message.contains("Mysterious technology prevents a criminal"));

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().type_id = 670; });
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "warp", now).is_ok());
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "stargate_jump", now).is_ok());

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().type_id = 587; });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = 0;
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    ctx.dispatch_rpc(
        &mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None,
    ).await.expect("WormholeJump succeeds with active weapons and PvP timers");
    assert_eq!(sess.solar_system_id, Some(31_000_005));
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
    let _ = setup_pilot_in_space(
        &ctx, 61051, victim_id, 1_000_009, victim_ship, 587, sys_id, DVec3::new(500.0, 0.0, 0.0),
    ).await;

    let (wreck_id, item_unlocked, item_cargo, item_corp) = (86_100_510i64, 86_100_511i64, 86_100_512i64, 86_100_513i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_id, item_rec(wreck_id, 26561, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(item_unlocked, item_rec(item_unlocked, 34, victim_id, wreck_id, InventoryFlag::Unlocked, 10, false));
        snap.items.insert(item_cargo, item_rec(item_cargo, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(item_corp, item_rec(item_corp, 34, victim_id, wreck_id, InventoryFlag::Cargo, 10, false));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: wreck_id, type_id: 26561, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let ship_inv_bound = ctx.bind_object(&thief_sess, "invbroker", thief_ship).unwrap();
    dispatch_combat_rpc(&ctx, &mut thief_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();

    let add_green_err = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(item_unlocked), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect_err("Safety Green must reject invbroker.Add from foreign wreck");
    assert_eq!(add_green_err.code, "SafetyLevelRestricted");

    let multi_green_err = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(item_cargo)]), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect_err("Safety Green must reject invbroker.MultiAdd from foreign wreck");
    assert_eq!(multi_green_err.code, "SafetyLevelRestricted");
    assert_eq!(ctx.db.get_item(item_unlocked).unwrap().location_id, wreck_id);
    assert_eq!(ctx.db.get_item(item_cargo).unwrap().location_id, wreck_id);

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

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&thief_id).unwrap().corporation_id = 2_000_555;
        snap.characters.get_mut(&victim_id).unwrap().corporation_id = 2_000_555;
    });
    ctx.crimewatch.write().unwrap().get_or_create(thief_id).suspect_flag_until_unix = 0;
    dispatch_combat_rpc(&ctx, &mut thief_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)], &kw).await.unwrap();
    let corp_res = ctx.dispatch_rpc(
        &mut thief_sess, "invbroker", Some(ship_inv_bound), "Add",
        &[PyRep::Integer(item_corp), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None,
    ).await.expect("Same player corp wreck take is legal under Safety Green");
    assert_eq!(dict_get(&corp_res, "suspectFlagged"), Some(&PyRep::Bool(false)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&thief_id).unwrap().suspect_flag_until_unix, 0);
}
