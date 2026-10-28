//! Integration tests for Filaments, Pochven navigation, Zarzakh, Fulcrum, and Deathless Shipcaster.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_machonet::SessionState;

use crate::common;

#[tokio::test]
async fn test_filaments_and_pochven_navigation_mechanics() {
    // parity: feat:navigation.filament-pvp
    // parity: feat:navigation.pochven-home-standing-gate, feat:structures.pochven-anchoring-restrictions
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(883_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");

    let (jita_sys, ship_id) = (30_000_142_u32, 100_000_030_i64);
    let (pvp_fil_id, void_fil_id, pochven_border_fil_id, pochven_krai_fil_id, pochven_home_fil_id) =
        (100_000_031_i64, 100_000_032_i64, 100_000_033_i64, 100_000_034_i64, 100_000_037_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, eve_db::InventoryItemRecord {
            item_id: ship_id, type_id: 621, owner_id: char_id, location_id: jita_sys as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        for (fid, ftype) in [
            (pvp_fil_id, 72984), (void_fil_id, 62448), (pochven_border_fil_id, 56076),
            (pochven_krai_fil_id, 56070), (pochven_home_fil_id, 56080),
        ] {
            snap.items.insert(fid, eve_db::InventoryItemRecord {
                item_id: fid, type_id: ftype, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 5, singleton: false, ..Default::default()
            });
        }
    });

    sess.solar_system_id = Some(jita_sys); sess.station_id = None; sess.structure_id = None; sess.ship_id = Some(ship_id);
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let ship_ball = eve_destiny::Ball {
        id: ship_id, type_id: 621, position: glam::DVec3::new(50_000_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 621);
    let _ = jita_handle.spawn_ball_with_combat(ship_ball, cs).await;

    // 1. feat:navigation.filament-pvp: Hull restriction, 2v2 team size, match, hazard isolation, leaderboard, and exit
    // Negative test (R-PVP-FORMAT-01): Cruiser (621) rejected for 72984 ("2v2 Battleships Proving Filament")
    let bad_hull_err = ctx.dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(pvp_fil_id), PyRep::Integer(72984)], &kw, None)
        .await.expect_err("Cruiser must be rejected for 2v2 Battleships Proving Filament");
    assert_eq!(bad_hull_err.code, "InvalidShipForFilament");

    let (char_id2, char_id3, char_id4) = (90_000_002_i64, 90_000_003_i64, 90_000_004_i64);
    let (ship_id2, ship_id3, ship_id4) = (100_000_035_i64, 100_000_038_i64, 100_000_039_i64);
    let (pvp_fil_id2, pvp_fil_id3, pvp_fil_id4) = (100_000_036_i64, 100_000_041_i64, 100_000_042_i64);
    let mut sess2 = SessionState::new(883_203);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id2)], &kw, None)
        .await.expect("SelectCharacterID char2");

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.get(&char_id).unwrap().clone();
        snap.items.get_mut(&ship_id).unwrap().type_id = 638; // Raven Battleship
        for (cid, sid, fid) in [(char_id2, ship_id2, pvp_fil_id2), (char_id3, ship_id3, pvp_fil_id3), (char_id4, ship_id4, pvp_fil_id4)] {
            let ch = snap.characters.entry(cid).or_insert_with(|| { let mut r = tmpl.clone(); r.character_id = cid; r });
            ch.solar_system_id = jita_sys; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = sid;
            snap.items.insert(sid, eve_db::InventoryItemRecord {
                item_id: sid, type_id: 638, owner_id: cid, location_id: jita_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
            snap.items.insert(fid, eve_db::InventoryItemRecord {
                item_id: fid, type_id: 72984, owner_id: cid, location_id: sid,
                flag: InventoryFlag::Cargo, quantity: 5, singleton: false, ..Default::default()
            });
        }
    });
    sess2.solar_system_id = Some(jita_sys); sess2.station_id = None; sess2.structure_id = None; sess2.ship_id = Some(ship_id2);
    let mut sess3 = sess.clone(); sess3.session_id = 883_206; sess3.user_id = Some(1003); sess3.character_id = Some(char_id3); sess3.solar_system_id = Some(jita_sys); sess3.ship_id = Some(ship_id3);
    let mut sess4 = sess.clone(); sess4.session_id = 883_207; sess4.user_id = Some(1004); sess4.character_id = Some(char_id4); sess4.solar_system_id = Some(jita_sys); sess4.ship_id = Some(ship_id4);
    for (cid, sid, x_off) in [(char_id2, ship_id2, 60_000_000.0), (char_id3, ship_id3, 70_000_000.0), (char_id4, ship_id4, 80_000_000.0)] {
        let b = eve_destiny::Ball {
            id: sid, type_id: 638, position: glam::DVec3::new(x_off, 0.0, 0.0),
            velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 100_000_000.0,
            radius: 300.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let cs_b = ctx.build_ship_combat_state(cid, sid, 638);
        let _ = jita_handle.spawn_ball_with_combat(b, cs_b).await;
    }

    let join_rep1 = ctx.dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(pvp_fil_id), PyRep::Integer(72984)], &kw, None)
        .await.expect("pvpFilamentMgr.JoinPVPQueue pilot 1");
    assert_eq!(join_rep1, PyRep::Integer(30), "First pilot queued receives 30s countdown / queue status");

    // Negative/queue test (R-PVP-FORMAT-01): 2nd and 3rd pilots do NOT form a 2v2 match yet (requires 4 pilots)
    let join_rep2 = ctx.dispatch_rpc(&mut sess2, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(pvp_fil_id2), PyRep::Integer(72984)], &kw, None)
        .await.expect("pvpFilamentMgr.JoinPVPQueue pilot 2");
    assert_eq!(join_rep2, PyRep::Integer(30), "Second pilot stays queued until 4 pilots are ready for 2v2");
    let join_rep3 = ctx.dispatch_rpc(&mut sess3, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(pvp_fil_id3), PyRep::Integer(72984)], &kw, None)
        .await.expect("pvpFilamentMgr.JoinPVPQueue pilot 3");
    assert_eq!(join_rep3, PyRep::Integer(30), "Third pilot stays queued until 4 pilots are ready for 2v2");

    // 4th pilot joins -> 2v2 match forms with team_a (2 pilots) and team_b (2 pilots)
    let join_rep4 = ctx.dispatch_rpc(&mut sess4, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(pvp_fil_id4), PyRep::Integer(72984)], &kw, None)
        .await.expect("pvpFilamentMgr.JoinPVPQueue pilot 4");
    assert_eq!(join_rep4, PyRep::Integer(5), "Fourth pilot forms 2v2 match: 5s match warp initiated");
    let p1_sys = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);
    assert_eq!(p1_sys, 32000001, "Pilot 1 moved to arena 32000001 in DB");
    sess.solar_system_id = Some(32000001);
    assert_eq!(sess4.solar_system_id, Some(32000001), "Pilot 4 moved to arena 32000001");

    // R-ABY-LEAK-01: Verify real Tachyon Cloud (80876) in 32000001 does NOT affect PvP arena ships in 32000001
    let arena_handle = ctx.systems.get_or_spawn(32000001);
    let (_, p1_arena_ball) = arena_handle.get_ball_snapshot(ship_id).await.unwrap().expect("p1 arena ball");
    let arena_pos = p1_arena_ball.position;
    let vel_before_cloud = p1_arena_ball.max_velocity;
    let fake_cloud_ball = eve_destiny::Ball {
        id: 1_898_000_991, type_id: 80_876, position: arena_pos,
        velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e9, radius: 15_000.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = arena_handle.spawn_ball(fake_cloud_ball).await;
    let active_clouds = eve_server::services::pve_special_sites_abyssal::evaluate_abyssal_spatial_clouds_for_ship(&ctx, 32000001, ship_id).await;
    assert!(active_clouds.is_none(), "PvP arena ship must not be affected by Abyssal pocket clouds");
    {
        let mut bp = arena_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        eve_server::services::pve_special_sites_abyssal::step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), 32000001);
    }
    let vel_after_cloud = arena_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
    assert!((vel_after_cloud - vel_before_cloud).abs() < 1e-6, "PvP arena ship must not be buffed by Tachyon cloud");

    // Live multi-tick turret combat in 2v2 arena: Pilot 1 (team_a: [char_id, char_id2]) destroys team_b ([char_id3, char_id4])
    let gun_id = 100_000_043_i64;
    let ammo_id = 100_000_044_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_id, eve_db::InventoryItemRecord {
            item_id: gun_id, type_id: 10678, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ammo_id, eve_db::InventoryItemRecord {
            item_id: ammo_id, type_id: 222, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 50, singleton: false, ..Default::default()
        });
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.advance_secs(65); // clear location-change invulnerability before live dogmaIM.AddTarget RPC
    for (opp_sid, y_off) in [(ship_id3, 500.0), (ship_id4, -500.0)] {
        let _ = arena_handle.set_ball_position(opp_sid, arena_pos + glam::DVec3::new(1_000.0, y_off, 0.0)).await;
        let mut ocs = arena_handle.get_combat_state(opp_sid).await.unwrap().unwrap();
        (ocs.shield_hp, ocs.armor_hp, ocs.structure_hp, ocs.shield_recharge_time_ms) = (0.0, 0.0, 1.0, 1.0e15);
        let _ = arena_handle.set_combat_state(opp_sid, ocs).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(opp_sid)], &kw, None).await.expect("dogmaIM.AddTarget");
    }
    for _ in 0..20 { let _ = arena_handle.step_manual(1.0).await; }
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(ship_id3)], &kw, None)
        .await.expect("Activate turret on team_b[0]");
    for _ in 0..6 { let _ = arena_handle.step_manual(1.0).await; }
    assert!(arena_handle.get_ball_snapshot(ship_id3).await.unwrap().is_none(), "team_b[0] ship destroyed by multi-tick turret");
    let mid_exit_err = ctx.dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)], &kw, None)
        .await.expect_err("2v2 match must NOT conclude when only 1 of 2 opposing ships is eliminated");
    assert_eq!(mid_exit_err.code, "MatchNotConcluded");

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gun_id)], &kw, None).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(ship_id4)], &kw, None)
        .await.expect("Activate turret on team_b[1]");
    for _ in 0..6 { let _ = arena_handle.step_manual(1.0).await; }
    assert!(arena_handle.get_ball_snapshot(ship_id4).await.unwrap().is_none(), "team_b[1] ship destroyed by multi-tick turret");

    let match_ev_id = ctx.db.with_snapshot(|s| eve_server::services::pvp_filament_mgr::load_pvp_store(s).active_matches[&1].event_id);
    let lb_rep = ctx.dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetLeaderboard", &[PyRep::Integer(match_ev_id)], &kw, None)
        .await.expect("pvpFilamentMgr.GetLeaderboard");
    let PyRep::List(ref lb_rows) = lb_rep else { panic!("expected List of leaderboard rows"); };
    for win_cid in [char_id, char_id2] {
        let entry = lb_rows.iter().find(|e| e.dict_get("characterID").and_then(|v| v.as_i64()) == Some(win_cid)).expect("team_a pilot on leaderboard");
        assert_eq!(entry.dict_get("wins").and_then(|v| v.as_i64()), Some(1), "Both surviving/teammate pilots on team_a credited with win");
    }

    // Winner exits through arena end gate (70001) back to origin Jita system at exact pre-match coordinates
    let exit_rep = ctx.dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(70001)], &kw, None)
        .await.expect("AbyssalPVPEndGateActivation");
    assert_eq!(exit_rep.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.solar_system_id, Some(jita_sys));
    let (_, restored_jita_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().expect("ship restored in Jita");
    assert!((restored_jita_ball.position.x - 50_000_000.0).abs() < 1.0, "Origin coordinates restored on Proving Grounds exit");

    // 1b. 4-way Frigate FFA (type 63728) forms 4 single-pilot teams on distinct spatial offset (>= 5,000 km separation)
    let ffa_pilots = [(90_000_011_i64, 100_000_061_i64, 100_000_071_i64), (90_000_012, 100_000_062, 100_000_072), (90_000_013, 100_000_063, 100_000_073), (90_000_014, 100_000_064, 100_000_074)];
    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.get(&char_id).unwrap().clone();
        for (idx, &(cid, sid, fid)) in ffa_pilots.iter().enumerate() {
            let mut ch = tmpl.clone(); ch.character_id = cid; ch.user_id = 2_011 + idx as u64; ch.solar_system_id = jita_sys; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(sid, eve_db::InventoryItemRecord { item_id: sid, type_id: 587, owner_id: cid, location_id: jita_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
            snap.items.insert(fid, eve_db::InventoryItemRecord { item_id: fid, type_id: 63728, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, ..Default::default() });
        }
    });
    for (idx, &(cid, sid, fid)) in ffa_pilots.iter().enumerate() {
        let b = eve_destiny::Ball { id: sid, type_id: 587, position: glam::DVec3::new(90_000_000.0 + (idx as f64) * 5_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 300.0, inertia: 0.5, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let _ = jita_handle.spawn_ball_with_combat(b, ctx.build_ship_combat_state(cid, sid, 587)).await;
        let mut fs = sess.clone(); fs.session_id = 883_220 + idx as u64; fs.user_id = Some(2_011 + idx as u64); fs.character_id = Some(cid); fs.solar_system_id = Some(jita_sys); fs.ship_id = Some(sid);
        let r = ctx.dispatch_rpc(&mut fs, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(fid), PyRep::Integer(63728)], &kw, None).await.expect("FFA JoinPVPQueue");
        assert_eq!(r, PyRep::Integer(if idx < 3 { 30 } else { 5 }));
    }
    let (ffa_teams, ffa_sep) = ctx.db.with_snapshot(|s| {
        let st = eve_server::services::pvp_filament_mgr::load_pvp_store(s);
        let o1 = st.match_arena_offsets.get(&1).copied().unwrap_or([0.0; 3]);
        let o2 = st.match_arena_offsets.get(&2).copied().unwrap_or([0.0; 3]);
        (st.match_teams.get(&2).map(|t| t.len()).unwrap_or(0), (o2[0] - o1[0]).abs())
    });
    assert_eq!(ffa_teams, 4, "FFA filament forms 4 independent 1-pilot teams");
    assert!(ffa_sep >= 5_000_000.0, "Concurrent Proving Grounds matches separated by >= 5,000 km (got {ffa_sep})");
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&ship_id).unwrap().type_id = 621);

    // 2. feat:navigation.filament-void: Jump to Void Space
    let void_jump_rep = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(void_fil_id)], &kw, None)
        .await.expect("voidSpaceMgr.VoidSpacePlayerJump");
    let void_sys = void_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert_eq!(void_sys, 32_000_002, "Void space destination system 32000002");
    assert_eq!(sess.solar_system_id, Some(32_000_002));
    eve_server::sp6_lifecycle::clear_abyssal_session(&ctx, char_id);

    ctx.advance_secs(15);

    // 3. Pochven filaments and travel:
    ctx.advance_secs(15);
    let fleet_id = 99_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.standings.insert(500026, 0.0); // 0.0 Triglavian standing (< 6.0)
        }
        snap.fleets.insert(fleet_id, eve_db::FleetRecord {
            fleet_id, boss_id: char_id, members: vec![char_id], ..Default::default()
        });
    });
    sess.solar_system_id = Some(jita_sys);
    let ship_ball = eve_destiny::Ball {
        id: ship_id, type_id: 621, position: glam::DVec3::new(100_000_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = jita_handle.spawn_ball(ship_ball.clone()).await;

    // Home filament requires >= 6.0 Triglavian standing: low standing rejected
    let home_err = ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(pochven_home_fil_id)], &kw, None)
        .await.expect_err("Activating Pochven Home filament with standing < 6.0 must fail");
    assert_eq!(home_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    // Krai filament: low standing excludes home systems (Kino, Niarja, Archee), lands in other Pochven system
    let krai_jump_rep = ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(pochven_krai_fil_id)], &kw, None)
        .await.expect("Krai filament jump with low standing succeeds into non-home system");
    let krai_dest = krai_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(!matches!(krai_dest, 30_001_372 | 30_003_504 | 30_002_702), "Low standing pilot excluded from Pochven home systems");

    // High standing (7.0 >= 6.0) allows Home filament activation directly into a home system
    ctx.advance_secs(905);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.standings.insert(500026, 7.0);
    });
    sess.solar_system_id = Some(jita_sys);
    let _ = jita_handle.spawn_ball(ship_ball.clone()).await;
    let home_jump_rep = ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(pochven_home_fil_id)], &kw, None)
        .await.expect("Home filament jump with standing >= 6.0 succeeds");
    let home_dest = home_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(matches!(home_dest, 30_001_372 | 30_003_504 | 30_002_702), "Home filament lands in Pochven home system");

    // Border filament: available regardless of standing
    ctx.advance_secs(905);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.standings.insert(500026, 0.0);
    });
    sess.solar_system_id = Some(jita_sys);
    let _ = jita_handle.spawn_ball(ship_ball).await;

    let pochven_jump_rep = ctx.dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(pochven_border_fil_id)], &kw, None)
        .await.expect("randomJumpMgr.ActivateRandomJumpFilament Pochven Border");
    let poch_sys = pochven_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let poch_reg = ctx.sde.solar_systems.get(&poch_sys).map(|s| s.region_id).unwrap_or(0);
    assert_eq!(poch_reg, 10_000_070, "Pochven jump destination must be in Pochven region (10000070)");

    // 4. Pochven wormholes: Non-deadspace geometry (30km offset) and guards
    let pch_sys_id = 30_000_157_u32; // Senda (Pochven)
    let static_whs = eve_server::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(pch_sys_id, 0, ctx.now_unix());
    assert!(!static_whs.is_empty(), "Static wormholes generated for Pochven system");
    let wh_sig = &static_whs[0];
    assert!(eve_server::services::space_destiny::space_pochven_wormholes::is_pochven_wormhole(&ctx.sde, wh_sig));
    let wh_ball_pos = eve_server::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(&ctx.sde, wh_sig);
    let sig_pos = glam::DVec3::from_array(wh_sig.position);
    assert!((wh_ball_pos.distance(sig_pos) - 30_000.0).abs() < 1.0, "Pochven wormhole ball has ~30km offset from cosmic sig");

    let wh_item_id = 100_000_090_i64;
    let (_, guard_ids) = eve_server::services::space_destiny::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards(&ctx, pch_sys_id, wh_sig, wh_item_id).await;
    assert!(!guard_ids.is_empty(), "Pochven wormhole guards spawned");

    // 5. Pochven capital cyno restrictions:
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, pch_sys_id).is_some());
    let plan_err = eve_server::services::space_destiny::space_nav_travel::plan_jump_drive(&ctx, char_id, 621, jita_sys, pch_sys_id).expect_err("Jump drive to Pochven blocked");
    assert_eq!(plan_err.code, "CannotJumpToPochven");

    ctx.advance_secs(15);
    let poch_handle = ctx.systems.get_or_spawn(pch_sys_id);
    let dread_ball = eve_destiny::Ball {
        id: ship_id, type_id: 19720, position: glam::DVec3::new(0.0, 0.0, 0.0), velocity: glam::DVec3::ZERO,
        max_velocity: 80.0, inertia: 0.5, mass: 1_200_000_000.0, radius: 400.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = poch_handle.spawn_ball(dread_ball).await;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().solar_system_id = pch_sys_id);
    sess.solar_system_id = Some(pch_sys_id);
    let pch_beyonce = ctx.bind_object(&mut sess, "beyonce", pch_sys_id as i64).unwrap();
    let gate_jump_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(pch_beyonce), "CmdStargateJump", &[PyRep::Integer(50016520)], &kw, None)
        .await.expect_err("Capital ship jumping Pochven stargate must be rejected");
    assert_eq!(gate_jump_err.code, "CapitalShipCannotJumpStargate");

    // 6. feat:navigation.pochven-home-standing-gate:
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = pch_sys_id; ch.standings.insert(500026, 3.0); // 3.0 < 6.0
    });
    sess.solar_system_id = Some(pch_sys_id);
    // RF-08: Border conduit (50016522 Otela -> Ignebaener 30005005, 50016523 back) & internal conduit (50016518 Otela -> Nalvula 30001445, 50016519 back) allow < 6.0 standing
    for (from_sys, gate_id, expected_dest) in [
        (30_000_157_u32, 50_016_522_i64, 30_005_005_i64),
        (30_005_005_u32, 50_016_523_i64, 30_000_157_i64),
        (30_000_157_u32, 50_016_518_i64, 30_001_445_i64),
        (30_001_445_u32, 50_016_519_i64, 30_000_157_i64),
    ] {
        ctx.advance_secs(15);
        let h = ctx.systems.get_or_spawn(from_sys);
        let gpos = ctx.sde.jumps.get(&gate_id).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
        let b = eve_destiny::Ball { id: ship_id, type_id: 621, position: gpos + glam::DVec3::new(100.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let _ = h.spawn_ball(b).await;
        let bey = ctx.bind_object(&mut sess, "beyonce", from_sys as i64).unwrap();
        let rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey), "CmdStargateJump", &[PyRep::Integer(gate_id)], &kw, None)
            .await.expect("Border and internal Pochven conduits allow standing < 6.0");
        assert_eq!(rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(expected_dest));
    }

    let mut ship_b = poch_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.type_id = 621;
    let gate_pos = ctx.sde.jumps.get(&50016520).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
    ship_b.position = gate_pos + glam::DVec3::new(100.0, 0.0, 0.0);
    let _ = poch_handle.spawn_ball(ship_b).await;
    let pch_beyonce = ctx.bind_object(&mut sess, "beyonce", pch_sys_id as i64).unwrap();
    ctx.advance_secs(15);
    let low_standing_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(pch_beyonce), "CmdStargateJump", &[PyRep::Integer(50016520)], &kw, None)
        .await.expect_err("Pochven home gate jump with standing < 6.0");
    assert_eq!(low_standing_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().standings.insert(500026, 7.0); });
    ctx.advance_secs(15);
    let home_jump_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(pch_beyonce), "CmdStargateJump", &[PyRep::Integer(50016520)], &kw, None)
        .await.expect("Pochven home gate jump with standing >= 6.0");
    assert_eq!(home_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30001372));

    // 7. feat:structures.pochven-anchoring-restrictions:
    let anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832)], &kw, None)
        .await.expect_err("Anchoring Upwell structure in Pochven must be blocked");
    assert_eq!(anchor_err.code, "AnchoringDisallowedInPochven");

    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().standings.insert(500026, 0.5); });
    let upwell_block_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(2233)], &kw, None)
        .await.expect_err("Anchoring structure via structureDeployment in Pochven is disallowed");
    assert_eq!(upwell_block_err.code, "AnchoringDisallowedInPochven");

    let gantry_id = 700_000_901_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gantry_id, eve_db::InventoryItemRecord {
            item_id: gantry_id, type_id: 3962, owner_id: char_id, location_id: ship_id,
            flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let customs_err = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_id)], &kw, None)
        .await.expect_err("Anchoring Customs Office Gantry with < 1.5 standing");
    assert_eq!(customs_err.code, "InsufficientTriglavianStanding");
}

#[tokio::test]
async fn test_zarzakh_fulcrum_deathless_shipcaster_mechanics() {
    // parity: feat:navigation.zarzakh-bookmark-dscan-disabled, feat:navigation.zarzakh-capital-gate-ban, feat:navigation.zarzakh-gate-lock
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(883_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");

    // Verify real SDE types for Deathless Shipcaster and pirate FOBs
    assert_eq!(ctx.sde.types.get(&79018).unwrap().name, "Deathless Interstellar Shipcaster");
    assert_eq!(ctx.sde.types.get(&78260).unwrap().name, "Angel Cartel FOB");
    assert_eq!(ctx.sde.types.get(&79172).unwrap().name, "Guristas Pirates FOB");

    let turnur_sys = 30002086_u32;
    let zarzakh_sys = 30100000_u32;
    let ship_id = 100_000_040_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = zarzakh_sys; ch.station_id = None; ch.structure_id = None;
            ch.active_ship_id = ship_id; ch.balance = eve_db::IskCents::from_isk(500_000_000);
        }
        snap.items.insert(ship_id, eve_db::InventoryItemRecord {
            item_id: ship_id, type_id: 621, owner_id: char_id, location_id: zarzakh_sys as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    sess.solar_system_id = Some(zarzakh_sys); sess.ship_id = Some(ship_id);
    let zarzakh_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    let z_ball = eve_destiny::Ball {
        id: ship_id, type_id: 621, position: glam::DVec3::new(250_100.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 621);
    let _ = zarzakh_handle.spawn_ball_with_combat(z_ball, cs).await;
    ctx.advance_secs(15);

    // 2. FOB shipcaster:
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().war_faction_id = None);
    let non_pirate_err = ctx.dispatch_rpc(
        &mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30002056), PyRep::Integer(104_000_020)], &kw, None,
    ).await.expect_err("Deathless shipcaster must refuse non-pirates");
    assert_eq!(non_pirate_err.code, "NonPirateRefused");

    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_010));
    // Negative test: Angel pilot jumping to Guristas FOB (104_000_021) is rejected
    let mismatch_err = ctx.dispatch_rpc(
        &mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30002811), PyRep::Integer(104_000_021)], &kw, None,
    ).await.expect_err("Angel pilot jumping to Guristas FOB must be rejected");
    assert_eq!(mismatch_err.code, "FobFactionMismatch");

    let pirate_jump_rep = ctx.dispatch_rpc(
        &mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30002056), PyRep::Integer(104_000_020)], &kw, None,
    ).await.expect("Angel pilot jumps via Deathless Shipcaster to Angel FOB");
    assert!(matches!(pirate_jump_rep, PyRep::None));
    assert_eq!(sess.solar_system_id, Some(30002056));

    ctx.advance_secs(15);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = zarzakh_sys; ch.war_faction_id = None;
    });
    sess.solar_system_id = Some(zarzakh_sys);

    // 3. feat:navigation.zarzakh-bookmark-dscan-disabled:
    let bm_err = ctx.dispatch_rpc(&mut sess, "bookmark", None, "BookmarkLocation", &[PyRep::Integer(zarzakh_sys as i64), PyRep::None, PyRep::None, PyRep::String("bm".into())], &kw, None)
        .await.expect_err("Bookmark creation in Zarzakh");
    assert_eq!(bm_err.code, "CantbookmarkEveSystem");

    let dscan_err = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(100_000_000.0)], &kw, None)
        .await.expect_err("ConeScan in Zarzakh");
    assert_eq!(dscan_err.code, "CannotDscanInThisSystem");

    // 4. Zarzakh local horizon siege:
    let zarzakh_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832)], &kw, None)
        .await.expect_err("Anchoring structure in Zarzakh");
    assert_eq!(zarzakh_anchor_err.code, "AnchoringDisallowedInZarzakh");
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, zarzakh_sys).is_some());
    let jd_err = eve_server::services::space_destiny::space_nav_travel::plan_jump_drive(&ctx, char_id, 621, turnur_sys, zarzakh_sys).expect_err("Jump drive into Zarzakh");
    assert_eq!(jd_err.code, "JumpDriveForbiddenInZarzakh");

    // 5. feat:navigation.zarzakh-capital-gate-ban:
    let turnur_gate_id = 50016562_i64;
    let gate_pos = ctx.sde.jumps.get(&turnur_gate_id).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().solar_system_id = 30002086);
    sess.solar_system_id = Some(30002086);
    let t_handle = ctx.systems.get_or_spawn(30002086);

    let f_ball = eve_destiny::Ball {
        id: ship_id, type_id: 20185, position: gate_pos + glam::DVec3::new(100.0, 0.0, 0.0), velocity: glam::DVec3::ZERO,
        max_velocity: 60.0, inertia: 0.5, mass: 900_000_000.0, radius: 350.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = t_handle.spawn_ball(f_ball).await;
    let turnur_beyonce = ctx.bind_object(&mut sess, "beyonce", 30002086).unwrap();
    let freighter_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(turnur_beyonce), "CmdStargateJump", &[PyRep::Integer(turnur_gate_id)], &kw, None)
        .await.expect_err("Freighter gate jump into Zarzakh");
    assert!(matches!(freighter_err.code.as_str(), "ShipCannotUseStargate" | "CapitalShipCannotJumpStargate"));

    let mut orca_ball = t_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    orca_ball.type_id = 28606; orca_ball.mass = 150_000_000.0;
    let _ = t_handle.spawn_ball(orca_ball).await;

    // 6. Zarzakh gate toll:
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.balance = eve_db::IskCents::from_isk(0); ch.war_faction_id = None;
    });
    let toll_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(turnur_beyonce), "CmdStargateJump", &[PyRep::Integer(turnur_gate_id)], &kw, None)
        .await.expect_err("Insufficient funds for Zarzakh toll");
    assert_eq!(toll_err.code, "InsufficientFundsForZarzakhGateToll");

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().balance = eve_db::IskCents::from_isk(50_000_000); });
    ctx.advance_secs(15);
    let mut bystander_sess = SessionState::new(883_299);
    bystander_sess.character_id = Some(90_000_002);
    bystander_sess.solar_system_id = Some(zarzakh_sys);
    ctx.quasar_gateway.subscribe_session(&bystander_sess, Vec::new(), ctx.now_unix());
    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());
    let toll_jump_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(turnur_beyonce), "CmdStargateJump", &[PyRep::Integer(turnur_gate_id)], &kw, None)
        .await.expect("Orca jumps through Turnur gate paying toll");
    assert_eq!(toll_jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(zarzakh_sys as i64));
    let bal_after = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert!(bal_after < eve_db::IskCents::from_isk(50_000_000), "Toll deducted on entering Zarzakh");

    // RF-07: Assert Quasar ActivatedNotice delivered ONLY to jumping pilot (NoticeScope::Character), not bystander in Zarzakh
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("stargate.lock.api.ActivatedNotice") && !p.value.is_empty())),
        "Expected stargate lock ActivatedNotice delivered to pilot"
    );
    let bystander_notices = ctx.quasar_gateway.poll_notices(bystander_sess.session_id);
    assert!(
        !bystander_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("stargate.lock.api.ActivatedNotice"))),
        "Bystander in Zarzakh must NOT receive Pilot 1's personal ActivatedNotice"
    );

    // 7. Zarzakh emanation lock and gate lock:
    ctx.advance_secs(15);
    let z_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    let alsavoinon_gate_in_zarzakh = 50016567_i64;
    let alsa_pos = ctx.sde.jumps.get(&alsavoinon_gate_in_zarzakh).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = alsa_pos + glam::DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    sess.solar_system_id = Some(zarzakh_sys);
    let zarzakh_beyonce = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();

    let lock_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(zarzakh_beyonce), "CmdStargateJump", &[PyRep::Integer(alsavoinon_gate_in_zarzakh)], &kw, None)
        .await.expect_err("Exit via non-origin gate blocked by Zarzakh 6-hour lock");
    assert_eq!(lock_err.code, "ZarzakhGateLocked");

    let turnur_gate_in_zarzakh = 50016566_i64;
    let turnur_pos = ctx.sde.jumps.get(&turnur_gate_in_zarzakh).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = turnur_pos + glam::DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    ctx.advance_secs(15);
    let exit_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(zarzakh_beyonce), "CmdStargateJump", &[PyRep::Integer(turnur_gate_in_zarzakh)], &kw, None)
        .await.expect("Exit via allowed Turnur gate succeeds");
    assert_eq!(exit_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30002086));

    // R-NAV-P1-03: Re-enter Zarzakh, advance time past 21,600s, and assert gate lock expires
    ctx.advance_secs(15);
    let turnur_beyonce_2 = ctx.bind_object(&mut sess, "beyonce", 30002086).unwrap();
    let reenter = ctx.dispatch_rpc(&mut sess, "beyonce", Some(turnur_beyonce_2), "CmdStargateJump", &[PyRep::Integer(turnur_gate_id)], &kw, None)
        .await.expect("Orca re-enters Zarzakh");
    assert_eq!(reenter.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(zarzakh_sys as i64));

    ctx.advance_secs(15);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = alsa_pos + glam::DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    let zarzakh_beyonce_2 = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();

    let lock_active_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(zarzakh_beyonce_2), "CmdStargateJump", &[PyRep::Integer(alsavoinon_gate_in_zarzakh)], &kw, None)
        .await.expect_err("Exit via Alsavoinon gate blocked by 6-hour lock");
    assert_eq!(lock_active_err.code, "ZarzakhGateLocked");

    let _ = ctx.quasar_gateway.poll_notices(sess.session_id);
    ctx.advance_secs(21_601);
    let exp_notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        exp_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("stargate.lock.api.DeactivatedNotice"))),
        "Expected stargate lock DeactivatedNotice published when 6-hour Zarzakh gate lock expires"
    );
    let alsa_exit_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(zarzakh_beyonce_2), "CmdStargateJump", &[PyRep::Integer(alsavoinon_gate_in_zarzakh)], &kw, None)
        .await.expect("Exit via Alsavoinon gate succeeds after 21,600s gate lock expires");
    assert!(alsa_exit_rep.dict_get("solarSystemID").is_some());

    // 8. Fulcrum alignment services:
    ctx.advance_secs(15);
    let fulcrum_station_id = 60_015_187_i64;
    let fulcrum_pos = ctx.sde.stations.get(&fulcrum_station_id).map(|s| s.position).unwrap_or(glam::DVec3::ZERO);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = zarzakh_sys; ch.station_id = None; ch.war_faction_id = Some(500001);
    });
    sess.solar_system_id = Some(zarzakh_sys); sess.station_id = None;
    let fulcrum_beyonce = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();
    let fulcrum_ship_b = eve_destiny::Ball {
        id: ship_id, type_id: 621, position: fulcrum_pos + glam::DVec3::new(100.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = z_handle.spawn_ball(fulcrum_ship_b).await;

    let empire_dock_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(fulcrum_beyonce), "CmdDock", &[PyRep::Integer(fulcrum_station_id), PyRep::Integer(ship_id)], &kw, None)
        .await.expect_err("Fulcrum docking refused to Empire militia");
    assert_eq!(empire_dock_err.code, "FulcrumRefusesEmpireMilitia");

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500010); });
    ctx.advance_secs(15);
    let dock_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(fulcrum_beyonce), "CmdDock", &[PyRep::Integer(fulcrum_station_id), PyRep::Integer(ship_id)], &kw, None)
        .await.expect("Fulcrum docking allowed for pirate enlisted character");
    assert!(matches!(dock_rep, PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Integer(_)));

    let fulcrum_bonus = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::is_fulcrum_pirate_ship_bonus_applicable(
            &ctx, snap, Some(char_id), 17716, 1, fulcrum_station_id
        )
    });
    assert!(fulcrum_bonus, "Fulcrum pirate industry ship bonus active for pirate-enlisted character");
}

#[tokio::test]
async fn test_wormhole_bidirectional_polarization() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(883_205);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");

    let (sys_a, sys_b) = (30_000_142_u32, 31_000_001_u32);
    let (ship_id, wh_a_id, wh_b_id) = (100_000_050_i64, 100_000_051_i64, 100_000_052_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_a; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, eve_db::InventoryItemRecord {
            item_id: ship_id, type_id: 621, owner_id: char_id, location_id: sys_a as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        for (src, dst) in [(sys_a, sys_b), (sys_b, sys_a)] {
            let st = snap.system_states.entry(src).or_insert_with(|| eve_db::SystemDynamicState { solar_system_id: src, ..Default::default() });
            st.wormhole_destination_system_id = Some(dst);
            st.wormhole_remaining_mass_kg = 1_000_000_000.0;
            st.signature_position = Some([10_000.0, 0.0, 0.0]);
        }
    });
    sess.solar_system_id = Some(sys_a); sess.ship_id = Some(ship_id);
    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let handle_b = ctx.systems.get_or_spawn(sys_b);
    let ship_ball = eve_destiny::Ball { id: ship_id, type_id: 621, position: glam::DVec3::new(10_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    let _ = handle_a.spawn_ball_with_combat(ship_ball, ctx.build_ship_combat_state(char_id, ship_id, 621)).await;
    let _ = handle_a.spawn_ball(eve_destiny::Ball { id: wh_a_id, type_id: 26272, position: glam::DVec3::new(10_500.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0e12, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let _ = handle_b.spawn_ball(eve_destiny::Ball { id: wh_b_id, type_id: 26272, position: glam::DVec3::new(10_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0e12, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;

    // Expire 10s login session change timer before jumping wormhole
    ctx.advance_secs(15);

    let _jump_res = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_a_id)], &kw, None)
        .await.expect("WormholeJump from sys_a to sys_b");
    assert_eq!(sess.solar_system_id, Some(sys_b));

    // Expire 10s session change timer before jumping back
    ctx.advance_secs(15);

    // R-NAV-P1-02: Immediate jump back to sys_a must be rejected with WormholePolarized
    let jump_back_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_b_id)], &kw, None)
        .await.expect_err("Immediate jump back through wormhole must fail with WormholePolarized");
    assert_eq!(jump_back_err.code, "WormholePolarized");
}
