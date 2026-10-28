//! Dedicated integration test suite (Part 2) for fighter lifecycle, war targets, high-sec criminal movement, wormhole aggression exemption, and wreck/jetcan item taking.

use crate::plan_c7_crimewatch_closure::{dict_get, far_celestial_id, item_rec, setup_pilot_in_space};
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
            sid != sys_id && s.security_status < 0.45 && sid < 31_000_000
                && ((s.galactic_pos_ly[0] - jita_ly[0]).powi(2) + (s.galactic_pos_ly[1] - jita_ly[1]).powi(2) + (s.galactic_pos_ly[2] - jita_ly[2]).powi(2)).sqrt() < 3.4
        })
        .unwrap().0;
    let beacon_id = 86_100_320i64;
    ctx.db.with_mut_snapshot(|snap| {
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
    assert_eq!(pre_err.code, "SafetyLevelRestricted");

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
    assert_eq!(warp_err.code, "CriminalWarpRestricted");

    let gate_crim_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)], &kw, None)
        .await.expect_err("High-sec criminal in non-capsule ship cannot jump stargate");
    assert_eq!(gate_crim_err.code, "CriminalActionRestricted");

    let wh_crim_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None)
        .await.expect_err("High-sec criminal with pending CONCORD response cannot enter wormhole");
    assert!(wh_crim_err.message.contains("Mysterious technology prevents a criminal"));

    let j1_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(j1_err.code, "CriminalActionRestricted");
    let j2_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(j2_err.code, "CriminalActionRestricted");
    let j3_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpThroughFleet", &[PyRep::Integer(char_id), PyRep::Integer(ship_id)], &kw, None).await.unwrap_err();
    assert_eq!(j3_err.code, "CriminalActionRestricted");
    let j4_err = ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(30_000_144), PyRep::Integer(struct_id)], &kw, None).await.unwrap_err();
    assert_eq!(j4_err.code, "CriminalActionRestricted");

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
    assert_eq!(gate_wpn_err.code, "WeaponsTimerActive");
    let dock_wpn_err = ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks docking at structure");
    assert_eq!(dock_wpn_err.code, "WeaponsTimerActive");
    let jump_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump drive");
    assert_eq!(jump_wpn_err.code, "WeaponsTimerActive");
    let bridge_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(char_id), PyRep::Integer(30_000_144), PyRep::Integer(1)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump bridge");
    assert_eq!(bridge_wpn_err.code, "WeaponsTimerActive");
    let portal_wpn_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdJumpThroughFleet", &[PyRep::Integer(char_id), PyRep::Integer(ship_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks jump portal");
    assert_eq!(portal_wpn_err.code, "WeaponsTimerActive");
    let struct_wpn_err = ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(30_000_144), PyRep::Integer(struct_id)], &kw, None)
        .await.expect_err("Active weapons timer blocks structure cyno jump");
    assert_eq!(struct_wpn_err.code, "WeaponsTimerActive");

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
    assert_eq!(g1_err.code, "SafetyLevelRestricted");
    let g2_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(item_cargo)]), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.unwrap_err();
    assert_eq!(g2_err.code, "SafetyLevelRestricted");
    let g3_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "Add", &[PyRep::Integer(jetcan_item), PyRep::Integer(jetcan_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.unwrap_err();
    assert_eq!(g3_err.code, "SafetyLevelRestricted");
    let g4_err = ctx.dispatch_rpc(&mut thief_sess, "invbroker", Some(ship_inv_bound), "LootAll", &[PyRep::Integer(wreck_id)], &kw, None).await.unwrap_err();
    assert_eq!(g4_err.code, "SafetyLevelRestricted");
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
