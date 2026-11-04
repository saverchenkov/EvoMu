//! Integration tests for Space Navigation, Sub-Warp Flight Commands, Station/Structure Docking & Tethering:
//! - `feat:navigation.undock`
//! - `feat:navigation.dock-station`
//! - `feat:navigation.dock-structure`
//! - `feat:navigation.tether`
//! - `feat:navigation.approach`
//! - `feat:navigation.orbit`
//! - `feat:navigation.keep-at-range`
//! - `feat:navigation.align-to`
//! - `feat:navigation.goto-point`
//! - `feat:navigation.stop-ship`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{BookmarkRecord, UpwellStructureRecord};
use eve_destiny::{DVec3, DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::{align_time_ticks, subwarp_velocity_at_time};
use eve_server::router::ServerContext;
use eve_server::system_actor::SystemHandle;

const DEMO_PILOT: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142;
const JITA_44_STATION: i64 = 60_003_760;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn rpc_bound(ctx: &ServerContext, sess: &mut SessionState, bid: u64, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, "beyonce", Some(bid), method, args, &HashMap::new(), None).await
}

async fn step_n(handle: &SystemHandle, n: usize) {
    for _ in 0..n {
        handle.step_manual(1.0).await.unwrap();
    }
}

async fn setup_pilot(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

fn set_corp_only(ctx: &ServerContext, sess: &mut SessionState, struct_id: i64, corp_only: bool) {
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    eve_server::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
        ctx, sess, struct_id, Some(corp_only), &[], &[], &[], &[],
    ).expect("configure_structure_access_profile");
    sess.role = 0;
}

#[tokio::test]
async fn test_navigation_undock_and_station_docking() {
    // parity: feat:navigation.undock
    // parity: feat:navigation.dock-station
    // parity: rpc:stationSvc.Undock
    // parity: rpc:stationSvc.Dock
    // parity: notify:OnSessionChanged
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 2101, DEMO_PILOT).await;
    assert_eq!(sess.station_id, Some(JITA_44_STATION));
    let ship_id = sess.ship_id.expect("active ship in station");

    // 1. Undock places ship at celestial undock point with velocity vector and 30s invulnerability
    ctx.advance_secs(12);
    let undock_rep = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("stationSvc.Undock");
    let PyRep::KeyVal(ukv) = undock_rep else { panic!("Expected KeyVal from Undock"); };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(ukv.get("shipID").unwrap().as_i64().unwrap(), ship_id);
    assert_eq!(ukv.get("undockInvulnerabilityTicks").unwrap().as_i64().unwrap(), 30);
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.solar_system_id, Some(JITA_SYS));

    let notifs = ctx.drain_notifications_for_char(DEMO_PILOT);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"), "Undock emits OnSessionChanged");

    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball");

    let st_rec = &ctx.sde.stations[&JITA_44_STATION];
    let st_pos = st_rec.position;
    let orbit_id = st_rec.orbit_id.expect("Jita 4-4 orbit_id");
    let orbit_cel = ctx.sde.celestials_in_system(JITA_SYS).into_iter().find(|c| c.celestial_id == orbit_id).unwrap();
    let expected_dir = (st_pos - orbit_cel.position).normalize();
    let st_radius = ctx.sde.types[&st_rec.station_type_id].radius;
    let ship_radius = ctx.sde.types[&ball.type_id].radius;
    let expected_offset = st_radius + ship_radius + 500.0;
    let expected_spawn = st_pos + expected_dir * expected_offset;

    assert!(ball.position.distance(expected_spawn) < 1.0, "Spawn position along undock vector");
    assert!((ball.velocity - expected_dir * (ball.max_velocity * 0.5)).length() < 1.0, "50% max speed");
    assert_eq!(ball.mode, MovementMode::GotoDirection { direction: expected_dir });

    let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs.undock_invulnerable_remaining_ticks, 30);
    let hp_before = cs.shield_hp;
    sys_handle.apply_damage(ship_id, DamageVector { em: 250.0, thermal: 250.0, kinetic: 250.0, explosive: 250.0 })
        .await.expect("apply damage during invulnerability");
    let cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after.shield_hp, hp_before, "Damage ignored while undock invulnerable");

    // Pilot action breaks undock invulnerability
    rpc_bound(&ctx, &mut sess, beyonce_id, "CmdSetSpeedFraction", &[PyRep::Real(0.75)]).await.expect("SetSpeed");
    let cs_broken = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_broken.undock_invulnerable_remaining_ticks, 0);

    // 2. Station Docking: perimeter checks & crimewatch restrictions
    sys_handle.set_ball_position(ship_id, st_pos + DVec3::new(150_000.0, 0.0, 0.0)).await;
    let far_err = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_44_STATION)]).await.unwrap_err();
    assert_eq!(far_err.code, "OutsideDockingPerimeter");

    sys_handle.set_ball_position(ship_id, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_weapon_timer(DEMO_PILOT, now);
    }
    let wpn_err = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_44_STATION)]).await.unwrap_err();
    assert_eq!(wpn_err.code, "WeaponsTimerActive");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(DEMO_PILOT);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 900;
    }
    let crim_err = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_44_STATION)]).await.unwrap_err();
    assert_eq!(crim_err.code, "CantDockGlobalCriminalFlag");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(DEMO_PILOT).criminal_flag_until_unix = 0;
    }

    ctx.advance_secs(12);
    let _ = ctx.drain_notifications_for_char(DEMO_PILOT);
    let dock_ok = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_44_STATION)]).await.expect("Dock");
    assert!(matches!(dock_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.station_id, Some(JITA_44_STATION));
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Docking removes ship");
    let dock_notifs = ctx.drain_notifications_for_char(DEMO_PILOT);
    assert!(dock_notifs.iter().any(|n| n.event == "OnSessionChanged"), "Docking emits OnSessionChanged");
}

#[tokio::test]
async fn test_navigation_structure_docking_and_tethering() {
    // parity: feat:navigation.dock-structure
    // parity: feat:navigation.tether
    // parity: rpc:structureDocking.Dock
    // parity: rpc:structureDocking.Undock
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 2201, DEMO_PILOT).await;
    let ship_id = sess.ship_id.unwrap();
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock from station");

    let struct_id = 1_040_000_000_200_i64;
    let struct_pos = DVec3::new(250_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35832, // Astrahus (Medium Citadel)
            solar_system_id: JITA_SYS,
            owner_corp_id: 98_000_099, // Hostile corp
            name: "Astrahus Parity Station".into(),
            position: struct_pos.to_array(),
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 2_500_000.0,
            armor_hp: 2_500_000.0,
            hull_hp: 2_500_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
    });

    set_corp_only(&ctx, &mut sess, struct_id, true);
    sys_handle.set_ball_position(ship_id, struct_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    // 1. Rejection via structure docking ACL
    let acl_err = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.unwrap_err();
    assert_eq!(acl_err.code, "StructureDockingAccessDenied");

    // Allow access to structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = 1_000_001;
    });
    set_corp_only(&ctx, &mut sess, struct_id, false);

    // 2. Rejection via ship hull size restriction (Carrier 23757 at Medium Citadel)
    let (_, mut cap_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let orig_type = cap_ball.type_id;
    cap_ball.type_id = 23757; // Archon
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) { it.type_id = 23757; }
    });
    sys_handle.spawn_ball(cap_ball.clone()).await.unwrap();

    let size_err = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.unwrap_err();
    assert_eq!(size_err.code, "ShipTooLargeForStructure");

    cap_ball.type_id = orig_type;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) { it.type_id = orig_type; }
    });
    sys_handle.spawn_ball(cap_ball).await.unwrap();

    // 3. Structure Dock & Undock cycle
    ctx.advance_secs(12);
    let s_dock = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.expect("structureDocking.Dock");
    assert!(matches!(s_dock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, Some(struct_id));
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    ctx.advance_secs(12);
    let s_undock = rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("structureDocking.Undock");
    assert!(matches!(s_undock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, None);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // 4. Structure Tethering: invulnerability, HP repair, and breaking rules
    sys_handle.set_ball_position(ship_id, struct_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    {
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.shield_hp = cs.max_shield_hp * 0.25;
        cs.armor_hp = cs.max_armor_hp * 0.35;
        cs.structure_hp = cs.max_structure_hp * 0.45;
        cs.undock_invulnerable_remaining_ticks = 0;
        cs.gate_cloak_remaining_ticks = 0;
        sys_handle.set_combat_state(ship_id, cs).await.unwrap();
    }

    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, DEMO_PILOT, JITA_SYS, ship_id).await;
    let cs_teth = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_teth.is_tethered, "Ship within range of friendly cored structure must tether");

    let (applied, _, _) = sys_handle.apply_damage(ship_id, DamageVector {
        em: 500.0, thermal: 500.0, kinetic: 500.0, explosive: 500.0,
    }).await.unwrap().unwrap();
    assert_eq!(applied, 0.0, "Tethered ship must be immune to incoming damage");

    step_n(&sys_handle, 1).await;
    let cs_repaired = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_repaired.shield_hp > cs_teth.shield_hp, "Tether must repair shields");
    assert!(cs_repaired.armor_hp > cs_teth.armor_hp, "Tether must repair armor");
    assert!(cs_repaired.structure_hp > cs_teth.structure_hp, "Tether must repair hull");

    // Tether breaks upon acquiring target lock
    sys_handle.add_target_lock(ship_id, TargetLock {
        target_id: JITA_44_STATION,
        lock_time_ms: 1000.0,
        remaining_lock_ms: 1000.0,
        locked: false,
        max_range_m: None,
    }).await.unwrap();
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, DEMO_PILOT, JITA_SYS, ship_id).await;
    assert!(!sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Target lock breaks tether");

    let (dmg_unlocked, _, _) = sys_handle.apply_damage(ship_id, DamageVector {
        em: 50.0, thermal: 50.0, kinetic: 50.0, explosive: 50.0,
    }).await.unwrap().unwrap();
    assert!(dmg_unlocked > 0.0, "Untethered ship takes normal damage");

    sys_handle.clear_target_locks(ship_id).await.unwrap();
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, DEMO_PILOT, JITA_SYS, ship_id).await;
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Tether re-engages after clear locks");

    // Weapons timer breaks tether
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_weapon_timer(DEMO_PILOT, ctx.now_unix());
    }
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, DEMO_PILOT, JITA_SYS, ship_id).await;
    assert!(!sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Weapons timer breaks tether");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(DEMO_PILOT).weapon_timer_until_unix = 0;
    }
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, DEMO_PILOT, JITA_SYS, ship_id).await;
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Tether restores after weapons timer ends");
}

#[tokio::test]
async fn test_navigation_flight_approach_orbit_and_keep_at_range() {
    // parity: feat:navigation.approach
    // parity: feat:navigation.orbit
    // parity: feat:navigation.keep-at-range
    // parity: feat:navigation.stop-ship
    // parity: rpc:beyonce.CmdFollowBall
    // parity: rpc:beyonce.CmdOrbit
    // parity: rpc:beyonce.CmdStop
    // parity: destiny:Stop
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 2301, DEMO_PILOT).await;
    let ship_id = sess.ship_id.unwrap();
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    let bid = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA_SYS)).expect("bind beyonce");

    let gate_id = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS).expect("Jita gate").stargate_id;
    let gate_pos = ctx.sde.jumps[&gate_id].position;

    // Position ship 30km from stargate with zero velocity and full speed fraction
    let (_, mut reset_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    reset_ball.position = gate_pos + DVec3::new(30_000.0, 0.0, 0.0);
    reset_ball.velocity = DVec3::ZERO;
    reset_ball.speed_fraction = 1.0;
    reset_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(reset_ball).await.unwrap();

    // 1. Approach: CmdFollowBall with range 0.0 accelerates toward target object
    rpc_bound(&ctx, &mut sess, bid, "CmdFollowBall", &[PyRep::Integer(gate_id), PyRep::Real(0.0)])
        .await.expect("CmdFollowBall approach");

    let (_, app_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(app_ball.mode, MovementMode::Follow { target_id: gate_id, range: 0.0 });

    let dist_before = app_ball.position.distance(gate_pos);
    step_n(&sys_handle, 4).await;
    let (_, app_stepped) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_after = app_stepped.position.distance(gate_pos);
    assert!(dist_after < dist_before, "Approach must move ship closer: before={dist_before}, after={dist_after}");
    assert!(app_stepped.velocity.length() > 0.0, "Ship must have non-zero velocity while approaching");

    // 2. Keep-at-Range: CmdFollowBall with designated standoff range
    rpc_bound(&ctx, &mut sess, bid, "CmdFollowBall", &[PyRep::Integer(gate_id), PyRep::Real(10_000.0)])
        .await.expect("CmdFollowBall keep-at-range");

    let (_, kar_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(kar_ball.mode, MovementMode::Follow { target_id: gate_id, range: 10_000.0 });

    // 3. Orbit: CmdOrbit sets orbital kinematics and maintains tangential velocity
    let (_, mut orb_init) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    orb_init.position = gate_pos + DVec3::new(3_000.0, 0.0, 0.0);
    sys_handle.spawn_ball(orb_init).await.unwrap();

    rpc_bound(&ctx, &mut sess, bid, "CmdOrbit", &[PyRep::Integer(gate_id), PyRep::Real(2_500.0)])
        .await.expect("CmdOrbit");

    let (_, orb_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(orb_ball.mode, MovementMode::Orbit { target_id: gate_id, range: 2_500.0 });

    step_n(&sys_handle, 6).await;
    let (_, orb_stepped) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let radial_dir = (gate_pos - orb_stepped.position).normalize();
    let v_perp = orb_stepped.velocity - radial_dir * orb_stepped.velocity.dot(radial_dir);
    assert!(v_perp.length() > 10.0, "Orbiting ship must establish tangential velocity: v_perp={}", v_perp.length());

    // 4. Stop Ship: CmdStop sets Stop mode and decelerates velocity toward 0
    let v_pre_stop = orb_stepped.velocity.length();
    rpc_bound(&ctx, &mut sess, bid, "CmdStop", &[]).await.expect("CmdStop");

    let (_, stop_mode_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(stop_mode_ball.mode, MovementMode::Stop);

    step_n(&sys_handle, 8).await;
    let (_, stopped_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(stopped_ball.velocity.length() < v_pre_stop * 0.25, "CmdStop decelerates: v_pre={v_pre_stop}, v_stopped={}", stopped_ball.velocity.length());
}

#[tokio::test]
async fn test_navigation_align_to_and_vector_steering() {
    // parity: feat:navigation.align-to
    // parity: feat:navigation.goto-point
    // parity: rpc:beyonce.CmdAlignTo
    // parity: rpc:beyonce.CmdGotoPoint
    // parity: rpc:beyonce.CmdGotoDirection
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 2401, DEMO_PILOT).await;
    let ship_id = sess.ship_id.unwrap();
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    let bid = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA_SYS)).expect("bind beyonce");

    let gate_id = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS).expect("Jita gate").stargate_id;
    let gate_pos = ctx.sde.jumps[&gate_id].position;

    // 1. Align-To: Turns toward target and accelerates to 75% max velocity (align time formula)
    let (_, mut init_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    init_ball.position = DVec3::new(10_000_000.0, 0.0, 0.0);
    init_ball.velocity = DVec3::ZERO;
    init_ball.speed_fraction = 0.0;
    init_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(init_ball).await.unwrap();

    rpc_bound(&ctx, &mut sess, bid, "CmdAlignTo", &[PyRep::Integer(gate_id)]).await.expect("CmdAlignTo");

    let (_, snap_align) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(snap_align.speed_fraction, 1.0);
    let expected_dir = (gate_pos - snap_align.position).normalize();
    if let MovementMode::GotoDirection { direction } = snap_align.mode {
        assert!((direction - expected_dir).length() < 1e-4, "Align-To direction points straight to target gate");
    } else {
        panic!("Expected GotoDirection mode from CmdAlignTo");
    }

    // Verify warp alignment equation: t_align = ceil(ln(4) * I * M / 1e6)
    let v_max = snap_align.max_velocity;
    let inertia = snap_align.inertia;
    let mass = snap_align.mass;
    let align_ticks = align_time_ticks(inertia, mass);
    assert!(align_ticks > 0);

    step_n(&sys_handle, align_ticks as usize).await;
    let (_, snap_aligned) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let v_actual = snap_aligned.velocity.length();
    let v_theory = subwarp_velocity_at_time(v_max, inertia, mass, align_ticks as f64);
    assert!(v_actual >= 0.75 * v_max - 0.5, "Velocity >= 75% max within align_ticks: v_act={v_actual}, 75%={}", 0.75 * v_max);
    assert!((v_actual - v_theory).abs() < 1.0, "Velocity matches subwarp_velocity_at_time formula");

    // 2. Align-To with Bookmark
    let bm_id = 888_999i64;
    let bm_pos = DVec3::new(100_000.0, 200_000.0, -150_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(bm_id, BookmarkRecord {
            bookmark_id: bm_id,
            owner_id: DEMO_PILOT,
            label: "Safe Spot Beta".into(),
            solar_system_id: JITA_SYS,
            x: bm_pos.x,
            y: bm_pos.y,
            z: bm_pos.z,
            item_id: None,
            memo: String::new(),
            folder_id: None,
            is_shared: false,
        });
    });

    let mut bm_kw = HashMap::new();
    bm_kw.insert("bookmarkID".to_string(), PyRep::Integer(bm_id));
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bid), "CmdAlignTo", &[], &bm_kw, None).await.expect("CmdAlignTo bookmark");

    let (_, snap_bm) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(snap_bm.speed_fraction, 1.0);
    let expected_bm_dir = (bm_pos - snap_bm.position).normalize();
    if let MovementMode::GotoDirection { direction } = snap_bm.mode {
        assert!((direction - expected_bm_dir).length() < 1e-4, "Align-To bookmark points to bookmark coords");
    } else {
        panic!("Expected GotoDirection mode from bookmark align");
    }

    // 3. Vector Steering: CmdGotoPoint and CmdGotoDirection
    let dest_point = DVec3::new(50_000.0, 100_000.0, -50_000.0);
    rpc_bound(&ctx, &mut sess, bid, "CmdGotoPoint", &[PyRep::Real(dest_point.x), PyRep::Real(dest_point.y), PyRep::Real(dest_point.z)])
        .await.expect("CmdGotoPoint");

    let (_, snap_pt) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(snap_pt.speed_fraction, 1.0);
    let expected_pt_dir = (dest_point - snap_pt.position).normalize();
    if let MovementMode::GotoDirection { direction } = snap_pt.mode {
        assert!((direction - expected_pt_dir).length() < 1e-4, "CmdGotoPoint steers toward 3D destination");
    } else {
        panic!("Expected GotoDirection mode from CmdGotoPoint");
    }

    let steer_vec = DVec3::new(0.0, 1.0, 0.0);
    rpc_bound(&ctx, &mut sess, bid, "CmdGotoDirection", &[PyRep::Real(steer_vec.x), PyRep::Real(steer_vec.y), PyRep::Real(steer_vec.z)])
        .await.expect("CmdGotoDirection");

    let (_, snap_dir) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    if let MovementMode::GotoDirection { direction } = snap_dir.mode {
        assert!((direction - steer_vec).length() < 1e-4, "CmdGotoDirection sets unit steering vector");
    } else {
        panic!("Expected GotoDirection mode from CmdGotoDirection");
    }
}
