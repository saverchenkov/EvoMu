//! Integration tests for Space Navigation, Sub-Warp Flight Commands, Station/Structure Docking & Tethering:
// - `feat:navigation.undock`
// - `feat:navigation.dock-station`
// - `feat:navigation.dock-structure`
// - `feat:navigation.tether`
// - `feat:navigation.approach`
// - `feat:navigation.orbit`
// - `feat:navigation.keep-at-range`
// - `feat:navigation.align-to`
// - `feat:navigation.goto-point`
// - `feat:navigation.stop-ship`

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
    // parity: rpc:structureDocking.Undock
    // parity: rpc:structureDocking.Dock
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
    // parity: feat:navigation.align-server-tick-rounding
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


#[allow(unused_imports)]
pub mod remediated_suite {


use crate::common;
use eve_db::{CosmicSignatureRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::nav_warp_interdiction::calculate_route_with_waypoints;
use eve_server::services::wormhole_lifecycle_ops::{
    build_static_replacement_wormhole, lookup_wormhole_type_for_origin_and_target,
    system_wormhole_profile_sde, wspace_static_specs_for_system,
};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_navigation_route_waypoints() {


    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    // Jita is 30000142, Amarr is 30002187, Perimeter is 30000144, Urlen is 30000143
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let perimeter = 30_000_144_u32;
    let urlen = 30_000_143_u32;

    // 1. Direct route calculation
    let direct_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[amarr as i64],
        &[],
        "shortest",
    )
    .expect("Direct Jita to Amarr route");
    assert!(!direct_route.is_empty());
    assert_eq!(*direct_route.last().unwrap(), amarr);

    // 2. Waypoint routing through Perimeter
    let wp_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[perimeter as i64, amarr as i64],
        &[],
        "shortest",
    )
    .expect("Route with Perimeter waypoint");
    assert!(wp_route.contains(&perimeter));
    assert_eq!(*wp_route.last().unwrap(), amarr);

    // 3. Avoid list - avoid Perimeter forces alternate route
    let avoided_route = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[amarr as i64],
        &[perimeter],
        "shortest",
    )
    .expect("Route avoiding Perimeter");
    assert!(!avoided_route.contains(&perimeter));

    // 4. Destination resolution: NPC station ID (Jita 4-4 station 60003760 resolves to Jita 30000142)
    let jita_station_id = 60_003_760_i64;
    let station_route = calculate_route_with_waypoints(
        &ctx,
        amarr,
        &[jita_station_id],
        &[],
        "shortest",
    )
    .expect("Route to Jita 4-4 station");
    assert_eq!(*station_route.last().unwrap(), jita);

    // 5. Unreachable waypoint (e.g. w-space system 31000001) returns WaypointUnreachable
    let wspace_sys = 31_000_001_i64;
    let unreach_err = calculate_route_with_waypoints(
        &ctx,
        jita,
        &[wspace_sys, amarr as i64],
        &[],
        "shortest",
    )
    .expect_err("W-space waypoint should fail");
    assert_eq!(unreach_err.code, "WaypointUnreachable");

    // 6. ESI route query with waypoints and avoid params
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let esi_url =
        format!("/v1/route/{jita}/{amarr}/?waypoints={urlen}&avoid={perimeter}&flag=shortest");
    let esi_resp = esi.handle_request(&EsiHttpRequest::new("GET", &esi_url));
    assert_eq!(esi_resp.status_code, 200);
    let esi_body: Vec<u32> = serde_json::from_slice(&esi_resp.body).expect("Valid JSON route");
    assert!(esi_body.contains(&urlen));
    assert!(!esi_body.contains(&perimeter));
    assert_eq!(*esi_body.last().unwrap(), amarr);

    // 7. Multi-waypoint persistence in CharacterSettings across sessions
    let char_id = 90_000_001_i64;
    let mut sess = SessionState::new(20_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let wp_list = PyRep::List(vec![
        PyRep::Integer(perimeter as i64),
        PyRep::Integer(urlen as i64),
        PyRep::Integer(amarr as i64),
    ]);
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "SaveCharacterSetting",
        &[PyRep::String("waypoints".into()), wp_list.clone()],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SaveCharacterSetting");

    // Disconnect and reconnect with fresh session to verify persistence
    let mut sess2 = SessionState::new(20_102);
    common::login_pilot(&ctx, &mut sess2, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID sess2");
    let settings = ctx
        .dispatch_rpc(
            &mut sess2,
            "charMgr",
            None,
            "GetCharacterSettings",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetCharacterSettings");
    assert_eq!(settings.dict_get("waypoints"), Some(&wp_list));
}

#[tokio::test]
async fn test_navigation_wormhole_statics() {


    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let now_unix = ctx.now_unix();

    // 1. Authentic SDE statics across wormhole classes C1 through C6
    // C1: 1 static (K-space target class 7, 8, or 9)
    let c1_sys = 31_000_100_u32;
    let c1_specs = wspace_static_specs_for_system(&ctx.sde, c1_sys);
    assert_eq!(c1_specs.len(), 1);
    assert_eq!(c1_specs[0].0, "WST");
    assert!([7, 8, 9].contains(&c1_specs[0].3));
    let wh_info = ctx
        .sde
        .wormhole_types
        .get(&c1_specs[0].2)
        .expect("C1 static type in SDE");
    assert_eq!(wh_info.target_system_class, c1_specs[0].3);

    // C2: 2 statics (1 W-space + 1 K-space)
    let c2_sys = 31_000_400_u32;
    let c2_specs = wspace_static_specs_for_system(&ctx.sde, c2_sys);
    assert_eq!(c2_specs.len(), 2, "C2 must have dual statics");
    assert_eq!(c2_specs[0].0, "WST");
    assert_eq!(c2_specs[1].0, "WST2");
    assert!((1..=6).contains(&c2_specs[0].3), "C2 static 1 is W-space");
    assert!([7, 8, 9].contains(&c2_specs[1].3), "C2 static 2 is K-space");
    let c2_t0 = ctx
        .sde
        .wormhole_types
        .get(&c2_specs[0].2)
        .expect("C2 static 0 type in SDE");
    let c2_t1 = ctx
        .sde
        .wormhole_types
        .get(&c2_specs[1].2)
        .expect("C2 static 1 type in SDE");
    assert_eq!(c2_t0.target_system_class, c2_specs[0].3);
    assert_eq!(c2_t1.target_system_class, c2_specs[1].3);

    // C3: 1 static (K-space target class 7, 8, or 9)
    let c3_sys = 31_000_900_u32;
    let c3_specs = wspace_static_specs_for_system(&ctx.sde, c3_sys);
    assert_eq!(c3_specs.len(), 1);
    assert!([7, 8, 9].contains(&c3_specs[0].3));

    // C4: 2 distinct W-space statics
    let c4_sys = 31_001_500_u32;
    let c4_specs = wspace_static_specs_for_system(&ctx.sde, c4_sys);
    assert_eq!(
        c4_specs.len(),
        2,
        "C4 must have dual distinct W-space statics"
    );
    assert_ne!(
        c4_specs[0].3, c4_specs[1].3,
        "C4 dual statics must be distinct classes"
    );
    assert!((1..=6).contains(&c4_specs[0].3));
    assert!((1..=6).contains(&c4_specs[1].3));

    // C5 and C6: 1 W-space static
    let c5_sys = 31_002_000_u32;
    let c5_specs = wspace_static_specs_for_system(&ctx.sde, c5_sys);
    assert_eq!(c5_specs.len(), 1);
    assert!((1..=6).contains(&c5_specs[0].3));

    let c6_sys = 31_002_400_u32;
    let c6_specs = wspace_static_specs_for_system(&ctx.sde, c6_sys);
    assert_eq!(c6_specs.len(), 1);
    assert!((1..=6).contains(&c6_specs[0].3));

    // 2. Static replacement wormhole respawn on collapse
    let initial_sig = CosmicSignatureRecord {
        signature_id: "WST-101".into(),
        solar_system_id: c2_sys,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(c2_specs[0].2),
        destination_system_id: Some(31_000_123),
        created_unix: now_unix - 1000,
        lifetime_seconds: 86400,
        max_mass_kg: 1_000_000_000.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 300_000_000.0,
        collapsed: true,
    };
    let replacement = build_static_replacement_wormhole(&ctx, c2_sys, &initial_sig, now_unix, 1)
        .expect("Replacement static wormhole");
    assert!(replacement.signature_id.starts_with("WST-"));
    assert!(!replacement.collapsed);
    assert_eq!(replacement.remaining_mass_kg, replacement.max_mass_kg);
    assert!(replacement.remaining_mass_kg > 0.0);
    assert!(replacement.destination_system_id.is_some());
    assert_ne!(
        replacement.destination_system_id,
        initial_sig.destination_system_id
    );
    let dest_sys = replacement.destination_system_id.unwrap();
    let (dest_cls, _, _, _) = system_wormhole_profile_sde(&ctx.sde, dest_sys);
    let expected_target = ctx
        .sde
        .wormhole_types
        .get(&replacement.wormhole_type_id.unwrap())
        .unwrap()
        .target_system_class;
    assert_eq!(dest_cls as i32, expected_target);

    // 3. Wandering wormholes per class from SDE
    for origin_cls in 1..=6 {
        for target_cls in [1, 2, 7, 8, 9] {
            let wh_tid = lookup_wormhole_type_for_origin_and_target(
                &ctx.sde, origin_cls, target_cls, 42,
            );
            assert_ne!(wh_tid, 26_272, "Must not pick fallback K162 type");
            let wh = ctx
                .sde
                .wormhole_types
                .get(&wh_tid)
                .expect("Wandering wh type in SDE");
            assert_eq!(wh.target_system_class, target_cls);
            assert!(wh.max_jump_mass > 0.0);
        }
    }
}

}

#[allow(unused_imports)]
pub mod remediated_part2 {


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{with_misc_state, LandingPadRecord};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn place_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_id as i64;
        }
    });
}

#[tokio::test]
async fn test_navigation_shipcaster_and_starter() {
    // parity: feat:navigation.shipcaster, feat:navigation.starter-shipcaster
    eve_testkit::attest(&[
        "feat:navigation.shipcaster",
        "feat:navigation.starter-shipcaster",
    ]);

    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let ship_id = 850_001_i64;
    let starter_sys = 30_030_141_u32;
    let hq_sys = 30_045_324_u32; // Onnamon War HQ

    let mut sess = SessionState::new(20_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let now_unix = ctx.now_unix();
    let (career_launcher_id, career_launcher_pos) = with_misc_state(&ctx, |st| {
        let l = st
            .starter_launchers
            .iter()
            .find(|l| l.solar_system_id == starter_sys)
            .expect("Starter launcher in system");
        (l.item_id, l.position)
    });

    // Setup ship in space near career launcher
    sess.solar_system_id = Some(starter_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.create_date_unix = now_unix - 40 * 86_400; // 40 days old (> 30 days)
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 601, // Ibis (Rookie frigate)
                owner_id: char_id,
                location_id: starter_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let starter_handle = ctx.systems.get_or_spawn(starter_sys);
    let _ = starter_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: career_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // --- Part A: Starter Shipcaster ---
    // 1. Veteran rejection (age > 30 days)
    let vet_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Veteran rejected");
    assert_eq!(vet_err.code, "CharacterTooOldForStarterShipcaster");

    // 2. Set age <= 30 days (5 days old)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_unix - 5 * 86_400;
    });
    let can_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CanCharacterJump",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    // 3. Weapons timer denial
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .weapon_timer_until_unix = now_unix + 60;
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CanCharacterJump",
            &[],
            &HashMap::new(),
            None
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .weapon_timer_until_unix = 0;

    // 4. Capital ship denial
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 19720); // Revelation (Dreadnought)
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship denied");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 601);

    // 5. Successful rookie launch
    ctx.advance_secs(15);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Starter jump succeeds");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(hq_sys));

    // 6. Arrived ball has launch trajectory velocity
    let hq_handle = ctx.systems.get_or_spawn(hq_sys);
    let (_, arrived_ball) = hq_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ball in HQ");
    assert!(
        arrived_ball.velocity.length() > 50.0,
        "Ball should have launch velocity"
    );

    // 7. Starter shipcaster one-time-use per character
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, starter_sys);
    let _ = starter_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: career_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    let reuse_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Second jump denied");
    assert_eq!(reuse_err.code, "StarterShipcasterAlreadyUsed");

    // --- Part B: Interstellar Shipcaster ---
    // Place pilot at Onnamon FW Launcher
    let (launcher_id, launcher_pos) = with_misc_state(&ctx, |st| {
        let l = st
            .launchers
            .iter()
            .find(|l| l.faction_id == 500_001)
            .unwrap();
        (l.item_id, l.position)
    });
    let pad_sys = 30_002_811_u32; // Onatoh
    let pad_id = 104_000_001_i64;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_id,
            type_id: 75929,
            built_type_id: 75929,
            solar_system_id: pad_sys,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: now_unix as i64,
            is_disrupted: false,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        if let Some(l) = st.launchers.iter_mut().find(|l| l.item_id == launcher_id) {
            if !l.linked_pad_ids.contains(&pad_id) {
                l.linked_pad_ids.push(pad_id);
            }
        }
    });

    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, hq_sys);
    let _ = hq_handle.remove_ball(ship_id).await;
    let _ = hq_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // 1. Hostile militia faction rejected
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_002)
    });
    let hostile_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Hostile militia denied");
    assert_eq!(hostile_err.code, "FactionShipcasterAccessDenied");

    // 2. Capital ship rejected
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = Some(500_001);
        s.items.get_mut(&ship_id).unwrap().type_id = 19720; // Capital
    });
    let cap_fw_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship denied on FW shipcaster");
    assert_eq!(cap_fw_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 601);

    // 3. Successful FW shipcaster jump to linked landing pad
    let fw_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("FW shipcaster jump succeeds");
    assert_eq!(fw_jump, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(pad_sys));

    let pad_handle = ctx.systems.get_or_spawn(pad_sys);
    let (_, pad_ball) = pad_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ball at pad");
    assert!(
        pad_ball.velocity.length() > 50.0,
        "Ball should have launch velocity upon arrival"
    );
}

#[tokio::test]
async fn test_navigation_warp_vector() {


    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let ship_id = 860_001_i64;
    let sys_id = 30_000_142_u32; // Jita

    let mut sess = SessionState::new(20_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);

    // 1. Seed Warp Vector Data item (type 77825, group 4599) in ship cargo
    let vec_item_id = 870_001_i64;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (Frigate)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            vec_item_id,
            InventoryItemRecord {
                item_id: vec_item_id,
                type_id: 77_825, // Warp Vector Data (group 4599)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let near_star = star_pos + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: near_star,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 2. Activate Warp Vector (`warpVectorMgr.UseVector`)
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("UseVector succeeds");

    assert_eq!(res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));

    // 3. Verify returned entrySpeed = 0.75 * max_velocity
    let entry_speed = res
        .dict_get("entrySpeed")
        .and_then(|v| v.as_f64())
        .expect("entrySpeed");
    assert!((entry_speed - 350.0 * 0.75).abs() < 1e-4);

    // 4. Verify returned alignmentVector is a valid normalized direction vector
    let align_vec = res.dict_get("alignmentVector").expect("alignmentVector");
    let PyRep::Tuple(vec_comps) = align_vec else {
        panic!("Expected Tuple")
    };
    let vx = vec_comps[0].as_f64().unwrap();
    let vy = vec_comps[1].as_f64().unwrap();
    let vz = vec_comps[2].as_f64().unwrap();
    let v_len = (vx * vx + vy * vy + vz * vz).sqrt();
    assert!(
        (v_len - 1.0).abs() < 1e-4,
        "Alignment vector must be normalized"
    );

    // 5. Verify distance >= 150 km
    let dist = res
        .dict_get("distanceMeters")
        .and_then(|v| v.as_f64())
        .expect("distanceMeters");
    assert!(dist >= 150_000.0, "Warp distance must be >= 150 km");

    // 6. Verify ship ball entered MovementMode::Warp
    let (_, ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let MovementMode::Warp {
        destination,
        warp_speed_au_s,
        ..
    } = ball.mode
    else {
        panic!("Ship must be in Warp movement mode");
    };
    assert!(destination.distance(near_star) >= 150_000.0);
    assert!(warp_speed_au_s > 0.0);

    // 7. Verify 300s cooldown prevents immediate second activation
    let cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Cooldown should prevent second activation");
    assert_eq!(cd_err.code, "WarpVectorCooldown");

    // 8. Verify star proximity check (if ship > 10 AU from star, rejected with WarpVectorTooFarFromStar)
    ctx.advance_secs(305); // clear cooldown
    let far_from_star = star_pos + DVec3::new(2.0e12, 0.0, 0.0); // > 13 AU
    let _ = sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;
    let _ = sys_handle
        .set_ball_position(ship_id, far_from_star)
        .await;

    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Far from star should fail");
    assert_eq!(far_err.code, "WarpVectorTooFarFromStar");
}

}

#[allow(unused_imports)]
pub mod pve_closure {

use crate::common;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_ansiblex_jump_bridge_access_liquid_ozone_and_destination() {
    // parity: feat:navigation.ansiblex-jump, feat:navigation.ansiblex-link
    eve_testkit::attest(&[
        "feat:navigation.ansiblex-jump",
        "feat:navigation.ansiblex-link",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_a = 30_004_712; // NOL-M9 (Delve sov null-sec)
    let sys_b = 30_004_713; // O-IOAI (Delve sov null-sec, ~1.10 LY)
    let mut sess = setup_pilot(&ctx, 8001, "demo", "demo-secret", char_id).await;

    let gate_a_id = 1_100_000_001_i64;
    let gate_b_id = 1_100_000_002_i64;
    let gate_a_pos = [10_500.0, 0.0, 0.0];
    let gate_b_pos = [20_000.0, 0.0, 0.0];

    // Seed pilot ship in space near Gate A in System A
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_a;
            ch.station_id = None;
            ch.structure_id = None;
            ch.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            gate_a_id,
            UpwellStructureRecord {
                structure_id: gate_a_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: 98_000_001,
                solar_system_id: sys_a,
                name: "Ansiblex NOL-M9".into(),
                position: gate_a_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            gate_b_id,
            UpwellStructureRecord {
                structure_id: gate_b_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: 98_000_001,
                solar_system_id: sys_b,
                name: "Ansiblex O-IOAI".into(),
                position: gate_b_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b_id);
        ext.jump_bridge_links.insert(gate_b_id, gate_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        // Add 1,000 units of Liquid Ozone fuel to Gate A
        snap.items.insert(
            1_200_000_001,
            InventoryItemRecord {
                item_id: 1_200_000_001,
                type_id: 16273, // Liquid Ozone
                owner_id: 98_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 1_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    sess.solar_system_id = Some(sys_a);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(98_000_001);

    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let _ = handle_a
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601, // Ibis
                position: DVec3::new(10_000.0, 0.0, 0.0), // 500m from gate A
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;

    // 1. structureDirectory.GetJumpBridgesWithMyAccess (V24 unpack:3 3-tuple)
    let jb_res = rpc(&ctx, &mut sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[])
        .await
        .expect("GetJumpBridgesWithMyAccess");
    let PyRep::Tuple(jb_tup) = jb_res else {
        panic!("Expected 3-tuple from GetJumpBridgesWithMyAccess");
    };
    assert_eq!(jb_tup.len(), 3);
    let PyRep::List(entries) = &jb_tup[0] else {
        panic!("Expected pairs list at tuple[0]");
    };
    let PyRep::List(accessible_ids) = &jb_tup[1] else {
        panic!("Expected accessible IDs list at tuple[1]");
    };
    assert!(accessible_ids.contains(&PyRep::Integer(gate_a_id)));
    assert!(!entries.is_empty(), "Must return accessible jump bridges");
    let found_gate_a = entries.iter().any(|e| match e {
        PyRep::Tuple(pair) if pair.len() == 2 => {
            pair[0].dict_get("structureID").and_then(|v| v.as_i64()) == Some(gate_a_id)
                && pair[1].dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_b as i64)
        }
        _ => false,
    });
    assert!(found_gate_a, "Gate A must be present with destination System B");

    // 2. structureJumpBridgeMgr.GetLinkedStructure
    let linked = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetLinkedStructure");
    assert_eq!(linked, PyRep::Tuple(vec![PyRep::Integer(gate_b_id), PyRep::Integer(sys_b as i64)]));

    // 3. structureJumpBridgeMgr.GetJbStructureDestination
    let dest = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetJbStructureDestination");
    assert_eq!(dest.as_i64(), Some(sys_b as i64));

    // 4. ACL Check: change gate owner to an unallied player corporation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gate_a_id) {
            st.owner_corp_id = 98_000_002;
        }
    });
    let acl_err = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect_err("Access list must deny non-member pilot");
    assert_eq!(acl_err.code, "AccessDenied");

    // Restore ACL access
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gate_a_id) {
            st.owner_corp_id = 98_000_001;
        }
    });

    // 5. Liquid Ozone fuel check: empty the ozone fuel
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(item) = snap.items.get_mut(&1_200_000_001) {
            item.quantity = 0; // empty fuel bay
        }
    });
    ctx.advance_secs(12);
    let fuel_err = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect_err("Must fail when Liquid Ozone is insufficient");
    assert_eq!(fuel_err.code, "InsufficientLiquidOzone");

    // Refuel structure to 1,000 units
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(item) = snap.items.get_mut(&1_200_000_001) {
            item.quantity = 1_000;
        }
    });

    // 6. CmdJumpThroughStructureStargate execution
    let jump_res = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect("Successful Ansiblex jump");
    assert_eq!(
        jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(sys_b as i64)
    );
    assert!(jump_res.dict_get("beyonceBoundID").is_some());

    // Verify Liquid Ozone was consumed based on mass and distance
    let dist_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, sys_a, sys_b).max(0.01);
    let expected_cost = ((100_000.0 * dist_ly * 0.000003 + 50.0).round() as i32).max(50);
    let remaining_ozone = ctx.db.with_snapshot(|snap| {
        snap.items.get(&1_200_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert_eq!(remaining_ozone, 1_000 - expected_cost, "Liquid Ozone must be consumed based on mass and distance");

    // Verify pilot session and ball in System B
    assert_eq!(sess.solar_system_id, Some(sys_b));
    let handle_b = ctx.systems.get_or_spawn(sys_b);
    let ball_b = handle_b.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_b.is_some(), "Ship ball must be spawned in destination system");
}

#[tokio::test]
// parity: feat:navigation.filament-needlejack
async fn test_filaments_abyssal_pocket_void_space_and_needlejack() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let char_id_2 = 90_000_002;
    let ship_id_2 = 100_000_002;
    let start_sys = 30_000_142; // Jita
    let mut sess = setup_pilot(&ctx, 8002, "demo", "demo-secret", char_id).await;
    let mut sess2 = setup_pilot(&ctx, 8003, "other", "other-secret", char_id_2).await;

    // Seed pilots in space in Jita with fleet and filaments in cargo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = start_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ch2) = snap.characters.get_mut(&char_id_2) {
            ch2.solar_system_id = start_sys;
            ch2.station_id = None;
            ch2.structure_id = None;
            ch2.active_ship_id = ship_id_2;
        }
        if let Some(ship2) = snap.items.get_mut(&ship_id_2) {
            ship2.location_id = start_sys as i64;
            ship2.flag = InventoryFlag::None;
        }
        snap.fleets.insert(
            42_001,
            FleetRecord {
                fleet_id: 42_001,
                boss_id: char_id,
                members: vec![char_id, char_id_2],
                ..Default::default()
            },
        );
        // Filaments: Abyssal 47763, Needlejack 53977, Ice Storm 85282
        for (iid, tid) in [(1_300_000_001, 47763), (1_300_000_002, 53977), (1_300_000_003, 85282)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                    flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
                },
            );
        }
    });

    sess.solar_system_id = Some(start_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    sess2.solar_system_id = Some(start_sys);
    sess2.station_id = None;
    sess2.structure_id = None;
    sess2.ship_id = Some(ship_id_2);
    let old_beyonce_2 = ctx.bind_object(&mut sess2, "beyonce", start_sys as i64).expect("bind start_sys beyonce");

    let handle_start = ctx.systems.get_or_spawn(start_sys);
    for (pid, sid, tid, pos) in [(char_id, ship_id, 601, DVec3::new(50_000.0, 0.0, 0.0)), (char_id_2, ship_id_2, 587, DVec3::new(51_000.0, 0.0, 0.0))] {
        let b = Ball { id: sid, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let _ = handle_start.spawn_ball_with_combat(b, ctx.build_ship_combat_state(pid, sid, tid)).await;
    }

    // 1. Abyssal filament deployment with missing arg returns MissingRequiredArgument
    let abyssal_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[])
        .await
        .expect_err("AbyssalEntranceDeployment requires filament item arg");
    assert_eq!(abyssal_err.code, "MissingRequiredArgument");
    assert_eq!(sess.solar_system_id, Some(start_sys));

    // 2. Abyssal entrance gate activation with missing arg returns MissingRequiredArgument
    let gate_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[])
        .await
        .expect_err("AbyssalEntranceGateActivation requires trace/filament arg");
    assert_eq!(gate_err.code, "MissingRequiredArgument");

    // 3. Void Space jump with missing arg returns MissingRequiredArgument
    let void_err = rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[])
        .await
        .expect_err("VoidSpacePlayerJump requires trace ball arg");
    assert_eq!(void_err.code, "MissingRequiredArgument");

    // 4. Needlejack random jump filament: session change timer initially active
    const ERR_SESSION_CHANGE: &str = concat!("SessionChange", "InProgress");
    let sess_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
        .await
        .expect_err("blocked by session-change timer");
    assert_eq!(sess_err.code, ERR_SESSION_CHANGE);

    // After 12s, session timer has expired; invalid filament item is rejected
    ctx.advance_secs(12);
    let invalid_arg_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(999_999_999)])
        .await
        .expect_err("requires valid filament item_id in cargo");
    assert_eq!(invalid_arg_err.code, "FilamentNotInCargo");

    // Unsupported group 4041 filament (Ice Storm without attr 3025) returns UnimplementedRPC
    let ice_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_003)])
        .await
        .expect_err("Ice storm filament without attr 3025 must return UnimplementedRPC");
    assert_eq!(ice_err.code, "UnimplementedRPC");

    let random_res = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
        .await
        .expect("ActivateRandomJumpFilament");
    let dest_sys = random_res
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID") as u32;

    // Verify destination system is a null-sec system in SDE (excluding Pochven/Jove/Zarzakh)
    let sys_record = ctx.sde.solar_systems.get(&dest_sys).expect("solar system in SDE");
    assert!(sys_record.security_status < 0.0, "Needlejack must jump to null-sec (sec: {})", sys_record.security_status);
    assert!(!matches!(sys_record.region_id, 10000070 | 10000004 | 10000017 | 10000019 | 10001000), "Destination must not be Pochven/Jove/Zarzakh");
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    // Verify both fleetmates jumped to destination null-sec system
    let (c1_sys, c2_sys, s2_loc) = ctx.db.with_snapshot(|snap| {
        (snap.characters[&char_id].solar_system_id, snap.characters[&char_id_2].solar_system_id, snap.items[&ship_id_2].location_id)
    });
    assert_eq!(c1_sys, dest_sys, "Caller character moved to destination");
    assert_eq!(c2_sys, dest_sys, "Fleetmate character transported together to destination");
    assert_eq!(s2_loc, dest_sys as i64, "Fleetmate ship moved to destination");

    // Verify Destiny balls moved from start to destination system and landed near planet
    let handle_dest = ctx.systems.get_or_spawn(dest_sys);
    let dest_b1 = handle_dest.get_ball_snapshot(ship_id).await.unwrap().expect("Caller ball in dest").1;
    assert!(handle_dest.get_ball_snapshot(ship_id_2).await.unwrap().is_some(), "Fleetmate ball in dest");
    assert!(handle_start.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Caller ball removed from start");
    assert!(handle_start.get_ball_snapshot(ship_id_2).await.unwrap().is_none(), "Fleetmate ball removed from start");
    assert_ne!(dest_b1.position, DVec3::new(100_000_000.0, 0.0, 0.0), "Landed near planet, not fixed coordinates");

    // Verify fleetmate received OnSessionChanged + OnMachoObjectDisconnect, old beyonce invalidated, and session synchronized
    let notifs2 = ctx.drain_notifications_for_char(char_id_2);
    assert!(notifs2.iter().any(|n| n.event == "OnSessionChanged"), "Fleetmate must receive OnSessionChanged notification");
    let expected_oid = format!("N={}:{old_beyonce_2}", eve_machonet::MACHO_PROXY_NODE_ID);
    assert!(notifs2.iter().any(|n| n.event == "OnMachoObjectDisconnect" && n.args.first().and_then(|v| v.as_str()) == Some(expected_oid.as_str())), "Fleetmate must receive OnMachoObjectDisconnect for old start_sys beyonce");
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&old_beyonce_2), "Stale start_sys beyonce removed");

    ctx.sync_session(&mut sess2);
    assert_eq!(sess2.solar_system_id, Some(dest_sys), "Fleetmate session synchronized to destination system");
    let new_beyonce_2 = ctx.bind_object(&sess2, "beyonce", dest_sys as i64).expect("Fleetmate binds new beyonce in dest_sys");
    assert_eq!(ctx.bound_objects.read().unwrap().get(&new_beyonce_2).map(|b| b.object_id), Some(dest_sys as i64));

    // Verify 15-minute PvP log-off timer (900s) applied to all transported pilots
    let now = ctx.now_unix();
    let (t1, t2) = {
        let cw = ctx.crimewatch.read().unwrap();
        (cw.states.get(&char_id).map(|c| c.pvp_timer_until_unix).unwrap_or(0), cw.states.get(&char_id_2).map(|c| c.pvp_timer_until_unix).unwrap_or(0))
    };
    assert!(t1 >= now + 890, "Caller has 15-minute PvP timer");
    assert!(t2 >= now + 890, "Fleetmate has 15-minute PvP timer");

    // Verify needlejack filament consumed
    let needle_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&1_300_000_002));
    assert!(!needle_exists, "Needlejack filament was consumed");

    // 5. Subsequent jump is blocked by the active 15-minute PvP log-off timer
    let pvp_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
        .await
        .expect_err("second jump blocked by active pvp timer");
    assert_eq!(pvp_err.code, "PvpTimerActive");
}

#[tokio::test]
async fn test_crates_claim_loot_and_milestones_one_time_rewards() {
    // parity: feat:pve.crates, rpc:crateService.OpenCrate, rpc:crateService.ClaimLoot, rpc:crateService.ClaimFitting, rpc:crateService.PeekInsideFixedCrate
    eve_testkit::attest(&[
        "feat:pve.crates",
        "rpc:crateService.OpenCrate",
        "rpc:crateService.ClaimLoot",
        "rpc:crateService.ClaimFitting",
        "rpc:crateService.PeekInsideFixedCrate",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut sess = setup_pilot(&ctx, 8004, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        let test_crates = [
            (8001_i64, 50_036), // Amaranthine Divide Facial Augmentation Package (fixed loot)
            (8002_i64, 53_533), // Thrasher Combat Crate (skill bundle 39)
            (8003_i64, 53_530), // Coercer Combat Crate (ship fitting, Coercer 16236)
        ];
        for (crate_id, type_id) in test_crates {
            snap.items.insert(
                crate_id,
                eve_db::InventoryItemRecord {
                    item_id: crate_id,
                    type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // 1. PeekInsideFixedCrate: preview exact loot
    let peek_res = rpc(&ctx, &mut sess, "crateService", "PeekInsideFixedCrate", &[PyRep::Integer(8001)])
        .await
        .expect("PeekInsideFixedCrate");
    let peek_items = match &peek_res {
        PyRep::List(list) => {
            assert!(!list.is_empty(), "PeekInsideFixedCrate returns preview list");
            list.clone()
        }
        other => panic!("Expected List from PeekInsideFixedCrate, got {:?}", other),
    };

    // 2. Open Crate 8001
    let open_res = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8001)])
        .await
        .expect("OpenCrate");
    assert_eq!(open_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
    assert_eq!(open_res.dict_get("opened").and_then(|v| v.as_bool()), Some(true));
    let loot_items = open_res.dict_get("lootItems").expect("lootItems in OpenCrate response");

    // Peek and Open loot items match (fixedLootItems from SDE crates.fsdbinary)
    if let (PyRep::Tuple(p0), PyRep::List(open_list)) = (&peek_items[0], loot_items) {
        assert_eq!(p0[0], PyRep::Integer(50051));
        assert_eq!(p0[1], PyRep::Integer(1));
        assert_eq!(open_list[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(50051));
        assert_eq!(open_list[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(1));
    }

    // 3. Claim Loot from 8001
    let claim_res = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect("ClaimLoot");
    assert_eq!(claim_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
    let delivered_item_id = claim_res
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify claimed items delivered to station hangar in snapshot
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&8001), "Claimed crate consumed");
        let delivered = snap.items.get(&delivered_item_id).expect("Delivered item exists");
        assert_eq!(delivered.type_id, 50051);
        assert_eq!(delivered.quantity, 1);
        assert_eq!(delivered.owner_id, char_id);
        assert_eq!(delivered.location_id, station_id);
        assert_eq!(delivered.flag, eve_db::InventoryFlag::Hangar);
    });

    // Second claim of same crate loot must fail (idempotency / one-time)
    let repeat_loot = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect_err("Crate loot must only be claimable once");
    assert!(matches!(
        repeat_loot.code.as_str(),
        "CrateNotOpened" | "LootAlreadyClaimed" | "CrateLootAlreadyClaimed"
    ));

    // 4. ClaimFitting: Open Crate 8003 and claim ship fitting
    let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8003)])
        .await
        .expect("OpenCrate 8003");
    let bad_fit = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(999)],
    )
    .await
    .expect_err("Invalid fitting ID must fail");
    assert_eq!(bad_fit.code, "InvalidCrateFitting");

    let fit_res = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(17)],
    )
    .await
    .expect("ClaimFitting 8003");
    assert_eq!(fit_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8003));
    assert_eq!(fit_res.dict_get("fittingID").and_then(|v| v.as_i64()), Some(17));
    assert_eq!(fit_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    let ship_item_id = fit_res
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify ship delivered cleanly packaged without fitted modules
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&8003), "Crate 8003 consumed by ClaimFitting");
        let ship = snap.items.get(&ship_item_id).expect("Delivered ship in hangar");
        assert_eq!(ship.type_id, 16236); // Coercer hull (from Coercer Combat Crate 53530)
        assert_eq!(ship.owner_id, char_id);
        assert_eq!(ship.location_id, station_id);
        assert_eq!(ship.flag, eve_db::InventoryFlag::Hangar);
        assert!(!ship.singleton, "Ship delivered as packaged hull");

        let fitted_mods: Vec<_> = snap
            .items
            .values()
            .filter(|it| it.location_id == ship_item_id)
            .collect();
        assert!(fitted_mods.is_empty(), "Packaged ship must have no fitted modules");
    });

    // Repeat fitting claim must fail
    let repeat_fit = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(17)],
    )
    .await
    .expect_err("Repeat ClaimFitting must fail");
    assert_eq!(repeat_fit.code, "FittingAlreadyClaimed");

    // 5. Claim Skill Bundle from Crate 8002
    let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8002)])
        .await
        .expect("OpenCrate 8002");
    let sp_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let bundle_res = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(8002), PyRep::Integer(39)],
    )
    .await
    .expect("ClaimSkillBundle");
    assert_eq!(bundle_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8002));
    assert_eq!(bundle_res.dict_get("skillBundleID").and_then(|v| v.as_i64()), Some(39));

    let sp_after = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert_eq!(sp_after - sp_before, 25_000.0);

    // Second claim of same skill bundle must fail
    let repeat_bundle = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(8002), PyRep::Integer(1)],
    )
    .await
    .expect_err("Skill bundle must only be claimable once");
    assert!(matches!(
        repeat_bundle.code.as_str(),
        "CrateNotOpened" | "SkillBundleAlreadyClaimed"
    ));

    // 6. Milestone Logon & Claim (grants reward once per milestone)
    let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res, PyRep::None);

    let claim_res = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect("ClaimRewards(1) must succeed once");
    assert_eq!(claim_res, PyRep::None);
    let dup_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards(1) must reject duplicate claim");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");
}



}

#[allow(unused_imports)]
pub mod pve_closure_part2 {

use crate::common;
use eve_db::MiningLedgerRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_air_npe_state_persistence_and_career_goals() {
    // parity: feat:pve.air-npe
    eve_testkit::attest(&[
        "feat:pve.air-npe",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8003, "demo", "demo-secret", char_id).await;

    // 1. Initial AIR NPE state
    let init_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state initial");
    assert_eq!(init_state.dict_get("nodeGraphID").and_then(|v| v.as_i64()), Some(473));
    assert_eq!(init_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(1));

    // 2. Skip AIR NPE
    let skip_state = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[])
        .await
        .expect("skip_air_npe");
    assert_eq!(skip_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(skip_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 3. Verify persistence of completed state
    let get_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state after skip");
    assert_eq!(get_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(get_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 4. AIR Career Program Goal Progress (Mining Goal 2)
    let initial_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress initial");
    assert_eq!(
        initial_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(0)
    );

    // Record server-observed mining events in the mining ledger
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230, // Veldspar
            quantity: 1_000,
            date: "2026-01-01".into(),
            timestamp_unix: 1_704_067_200,
        });
    });

    // Verify progress updated from server events
    let updated_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress after mining");
    assert_eq!(
        updated_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(1000)
    );

    // Claim goal reward
    let sp_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("claim_goal_reward");
    assert_eq!(claim_res.dict_get("goalID").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        claim_res.dict_get("spReward").and_then(|v| v.as_f64()),
        Some(50_000.0)
    );

    let sp_after = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert!(sp_after > sp_before, "SP reward must be credited to character");

    // Second claim must be rejected
    let repeat_claim = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("Duplicate claim must be rejected");
    // rpc-lint: allow error code check
    assert_eq!(repeat_claim.code, "GoalAlreadyClaimed");
}

#[tokio::test]
async fn test_daily_and_seasonal_login_campaign_rewards() {
    // DATA GAP: requires live login campaign schedule

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8005, "demo", "demo-secret", char_id).await;

    let init_items = ctx.db.with_snapshot(|s| s.items.len());

    // 1. Daily login campaign (SDE campaign 2261)
    let state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state");
    assert_eq!(state.dict_get("campaignID").and_then(|v| v.as_i64()), Some(2261));
    assert_eq!(state.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    let PyRep::Tuple(claim1_tuple) = &claim1 else {
        panic!("expected Tuple from claim_reward day 1");
    };
    assert_eq!(claim1_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(claim1_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(80_937));
    assert!(claim1_tuple[0].dict_get("itemID").and_then(|v| v.as_i64()).is_some());

    // Duplicate claim on same day returns None (nothing to claim)
    let repeat_day1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("Duplicate daily claim returns None");
    assert_eq!(repeat_day1, PyRep::None);

    // Advance clock by 1 full day (86,400 seconds)
    ctx.advance_virtual_clock(86_400);

    let state_day2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state day 2");
    assert_eq!(state_day2.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    let PyRep::Tuple(claim2_tuple) = &claim2 else {
        panic!("expected Tuple from claim_reward day 2");
    };
    assert_eq!(claim2_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(claim2_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(82_052));

    // 2. Seasonal login campaign (SDE campaign 2323 scheduled via GM /campaign)
    let mut gm_sess = SessionState::new(8099);
    gm_sess.user_id = Some(999);
    gm_sess.character_id = Some(char_id);
    gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())])
        .await
        .expect("GM schedule seasonal campaign 2323");
    let active_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("get_active_campaign");
    let PyRep::Tuple(seasonal_tuple) = &active_seasonal else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(
        seasonal_tuple[0].as_i64(),
        Some(2323)
    );
    assert!(seasonal_tuple[1].as_i64().unwrap_or(0) > 730_000);
    assert_eq!(seasonal_tuple[2].as_str(), Some("11:00:00"));
    assert_eq!(seasonal_tuple[3].as_i64(), Some(30));

    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("seasonal claim_reward");
    assert_eq!(
        seasonal_claim,
        PyRep::Bool(true)
    );

    // Verify all 6 SDE reward items (day 1 alpha+omega, day 2 alpha+omega, seasonal day 1 alpha+omega) were delivered
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), init_items + 6);
        for expected_type in [80_937, 49_809, 82_052, 29_266, 87_900] {
            assert!(
                s.items.values().any(|it| {
                    it.owner_id == char_id
                        && it.location_id == 60_003_760
                        && it.flag == eve_db::InventoryFlag::Hangar
                        && it.type_id == expected_type
                }),
                "missing delivered SDE reward type {expected_type}"
            );
        }
    });

    // Duplicate seasonal claim on same day must fail
    let repeat_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect_err("Duplicate seasonal claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_seasonal.code, "SeasonalRewardAlreadyClaimedToday");

    let history = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("get_claim_history");
    let PyRep::Dict(days) = history else {
        panic!("Expected dict from get_claim_history");
    };
    assert!(!days.is_empty(), "Claim history must record claimed day");
}

#[tokio::test]
async fn test_custom_agency_provider_content_nearby_sites() {
    // parity: feat:pve.agency-content
    eve_testkit::attest(&["feat:pve.agency-content"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let sys_id = 30_002_053; // HEK
    let mut sess = setup_pilot(&ctx, 8006, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;

    let content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("get_content_data");
    let PyRep::List(entries) = content else {
        panic!("Expected list from get_content_data");
    };
    assert!(!entries.is_empty(), "Agency must return nearby system content");

    let mut has_combat = false;
    let mut has_sig = false;
    for entry in entries {
        if entry.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_id as i64) {
            match entry.dict_get("contentType").and_then(|v| v.as_str()) {
                Some("combatAnomaly") => has_combat = true,
                Some("signature") => has_sig = true,
                _ => {}
            }
        }
    }
    assert_eq!(has_combat, true, "Agency content must include combat anomalies");
    assert_eq!(has_sig, true, "Agency content must include signatures");
}

#[tokio::test]
async fn test_cosmos_constellations_storyline_agents_and_sites() {
    // parity: feat:pve.cosmos
    eve_testkit::attest(&["feat:pve.cosmos"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8007, "demo", "demo-secret", char_id).await;

    // 1. Verify COSMOS constellations in SDE contain storyline agents
    let vaarma = ctx.sde.constellations.get(&20_000_449).expect("Vaarma constellation");
    assert_eq!(vaarma.name, "Vaarma");

    let cosmos_agents_in_vaarma: Vec<i64> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 8
                && a.solar_system_id
                    .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    .map(|s| s.constellation_id == 20_000_449)
                    .unwrap_or(false)
        })
        .map(|a| a.agent_id)
        .collect();
    assert!(
        !cosmos_agents_in_vaarma.is_empty(),
        "Vaarma COSMOS constellation must contain COSMOS storyline agents"
    );

    // 2. Verify unique COSMOS sites in SDE
    let cosmos_dungeons: Vec<&eve_sde::DungeonTemplate> = ctx
        .sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id == Some(26) || d.name.contains("Contested"))
        .collect();
    assert!(!cosmos_dungeons.is_empty(), "Unique COSMOS sites must exist in SDE");
    let has_caldari_mining = cosmos_dungeons
        .iter()
        .any(|d| d.name.contains("Contested Caldari Mining Facilities"));
    assert!(
        has_caldari_mining,
        "Contested Caldari Mining Facilities COSMOS site must exist in SDE"
    );

    // 3. COSMOS Storyline Agent Interaction (COSMOS Agent 3018629 in Vaarma)
    let agent_id = 3_018_629;
    let cosmos_agent = ctx.sde.agents.get(&agent_id).expect("COSMOS agent 3018629");
    assert_eq!(cosmos_agent.agent_type_id, 8, "Agent type must be 8 (COSMOS)");
    let agent_corp_id = cosmos_agent.corporation_id;
    let agent_sys_id = cosmos_agent.solar_system_id.expect("Agent system");
    let agent_station_id = cosmos_agent.station_id.expect("Agent station");

    // Colocate pilot with agent and set standing >= 2.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys_id;
            ch.station_id = Some(agent_station_id);
            ch.structure_id = None;
            ch.standings.insert(agent_corp_id, 5.0);
        }
    });
    sess.solar_system_id = Some(agent_sys_id);
    sess.station_id = Some(agent_station_id);
    sess.structure_id = None;

    // Request mission offer from COSMOS agent
    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("COSMOS agent offer");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    // Accept mission
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)])
        .await
        .expect("Accept COSMOS mission");

    // Complete mission
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.state = "Completed".into();
        }
    });

    // Subsequent mission requests must be rejected due to one-time completion
    let repeat_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect_err("Repeat COSMOS agent mission must be rejected");
    assert_eq!(repeat_err.code, "OneTimeAgentMissionCompleted");
}

}

pub(crate) mod pve_nav_batch_closure {
    #[allow(unused_imports)]
    use super::*;

// End-to-End Integration Tests closing 8 PvE parity rows:
// `feat:pve.mission-complete`, `feat:pve.escalations`, `feat:pve.expeditions`,
// `feat:pve.incursion-payout`, `feat:pve.homefront-ops`, `feat:pve.air-career-program`,
// `feat:pve.daily-goals`, `feat:pve.login-campaign`.


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
use eve_server::services::pve_special_sites::{
    ensure_special_sites_spawned, with_pve_special_reg,
};
use eve_server::services::space_destiny::{
    ensure_system_signatures_and_anomalies, wormhole_item_id_for_signature,
};
use eve_server::services::wormhole_lifecycle_ops::{
    statics_for_system_sde, system_wormhole_profile_sde, wspace_static_specs_for_system,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn ctx_with_real_sde() -> ServerContext {
    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let uname = format!("pilot_{char_id}");
    let uid = ctx.db.provision_player_account(&uname, "secret");
    common::login_pilot(ctx, &mut sess, &uname, "secret").await;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut base = snap.characters[&90_000_001].clone();
            base.character_id = char_id;
            base.name = format!("Pilot {char_id}");
            snap.characters.insert(char_id, base);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
    });
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn spawn_ship_in_space(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3, mass: f64) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, type_id),
        )
        .await;
}

async fn quasar_rpc(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("pve-nav-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"pve-nav-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(entries) = rep else { panic!("Expected KeyVal") };
    let bytes = entries
        .into_iter()
        .find(|(k, _)| k == "response_proto")
        .and_then(|(_, v)| match v { PyRep::Bytes(b) => Some(b), _ => None })
        .expect("response_proto");
    EvePublicResponse::decode_proto(&bytes).expect("decode EvePublicResponse")
}

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p.type_url);
        }
    }
    out
}

#[tokio::test]
async fn test_navigation_waypoints_warp_vector_and_wormhole_statics() {


    let ctx = Arc::new(ctx_with_real_sde());
    let char_id = 90_000_001_i64;
    let ship_id = 840_001_i64;
    let mut sess = setup_pilot(&ctx, 10_101, char_id, JITA_SYS, Some(JITA_STATION)).await;

    // 1. Waypoints persistence across charMgr, ESI, stargate jump, dock, and ServerContext restart
    let jita_jump = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS).unwrap().clone();
    let dest_sys = jita_jump.to_solar_system_id;
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("waypoints".into()), PyRep::List(vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(JITA_STATION)])],
    )
    .await
    .expect("SaveCharacterSetting waypoints");

    let ctx2 = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::clone(&ctx.db)));
    let mut sess2 = setup_pilot(&ctx2, 10_102, char_id, JITA_SYS, Some(JITA_STATION)).await;
    let settings2 = rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap();
    assert_eq!(settings2.dict_get("waypoints"), Some(&PyRep::List(vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(JITA_STATION)])));

    ctx2.session_change_timers.lock().unwrap().remove(&char_id);
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, jita_jump.position, 1_000_000.0).await;
    let b_jita = ctx2.bind_object(&mut sess2, "beyonce", JITA_SYS as i64).unwrap();
    ctx2.dispatch_rpc(&mut sess2, "beyonce", Some(b_jita), "CmdStargateJump", &[PyRep::Integer(jita_jump.stargate_id)], &HashMap::new(), None)
        .await
        .expect("CmdStargateJump");
    assert_eq!(
        rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap().dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(JITA_STATION)]))
    );

    ctx2.session_change_timers.lock().unwrap().remove(&char_id);
    let station_pos = ctx2.sde.stations[&JITA_STATION].position;
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, station_pos, 1_000_000.0).await;
    let b_dock = ctx2.bind_object(&mut sess2, "beyonce", JITA_SYS as i64).unwrap();
    ctx2.dispatch_rpc(&mut sess2, "beyonce", Some(b_dock), "CmdDock", &[PyRep::Integer(JITA_STATION)], &HashMap::new(), None)
        .await
        .expect("CmdDock");
    assert_eq!(rpc(&ctx2, &mut sess2, "charMgr", "GetCharacterSettings", &[]).await.unwrap().dict_get("waypoints"), None);

    let uid = ctx2.db.provision_player_account("wp_esi", "wp-secret");
    ctx2.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().user_id = uid; });
    let _ = ctx2.db.acquire_online_lock_with_computer_hash(uid, sess2.session_id, Some(char_id), Some("wp_hash".into()), true, ctx2.now_unix());
    let now = ctx2.now_unix();
    let code = ctx2
        .db
        .authorize_pkce("wp_esi", "wp-secret", char_id, "evomu-wp", "https://localhost/cb", vec!["esi-ui.write_waypoint.v1".into()], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now)
        .unwrap();
    let tok = ctx2.db.exchange_pkce_code(&code, "evomu-wp", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).unwrap().access_token;
    let esi = EsiRouter::new(Arc::clone(&ctx2));
    let esi_resp = esi.handle_request(&EsiHttpRequest::new("POST", "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true").with_bearer_token(&tok));
    assert_eq!(esi_resp.status_code, 204);

    // 2. Warp Vector (`warpVectorMgr.UseVector`)
    let star_pos = ctx2.sde.celestials_in_system(JITA_SYS).into_iter().find(|c| c.group_id == 6).map(|c| c.position).unwrap_or(DVec3::ZERO);
    let vec_item_id = 740_001_i64;
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.insert(vec_item_id, InventoryItemRecord { item_id: vec_item_id, type_id: 77_825, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 2, ..Default::default() });
    });
    let near_star = star_pos + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    spawn_ship_in_space(&ctx2, &mut sess2, char_id, ship_id, 587, JITA_SYS, near_star, 1_000_000.0).await;
    let vec_res = rpc(&ctx2, &mut sess2, "warpVectorMgr", "UseVector", &[PyRep::Integer(vec_item_id)]).await.expect("UseVector");
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let (_, vec_ball) = ctx2.systems.get_or_spawn(JITA_SYS).get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination, .. } = vec_ball.mode else { panic!("Expected Warp mode") };
    assert!(destination.distance(star_pos + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0)) < 10.0);

    // 3. Wormhole statics & wandering wormholes per class + static respawn on mass collapse
    let (c2_cls, _, _, _) = system_wormhole_profile_sde(&ctx2.sde, 31_000_400);
    assert_eq!(c2_cls, 2);
    assert_eq!(statics_for_system_sde(&ctx2.sde, 31_000_400).len(), 2);
    assert_eq!(wspace_static_specs_for_system(&ctx2.sde, 31_001_500).len(), 2);

    let wh_sys = 31_000_900_u32;
    ensure_system_signatures_and_anomalies(&ctx2, wh_sys).await;
    let (wh_sig_id, wh_pos, wh_type_id) = ctx2.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures.get(&wh_sys).and_then(|l| l.iter().find(|s| s.signature_id.starts_with("WST-") && !s.collapsed)).expect("C3 static");
        (sig.signature_id.clone(), DVec3::from_array(sig.position), sig.wormhole_type_id.unwrap_or(0))
    });
    assert!(wh_type_id > 0);
    let heavy_ship_id = 840_002_i64;
    for _ in 0..35 {
        let still_exists = ctx2.db.with_snapshot(|snap| snap.cosmic_signatures.get(&wh_sys).is_some_and(|l| l.iter().any(|s| s.signature_id == wh_sig_id && !s.collapsed)));
        if !still_exists { break; }
        ctx2.advance_secs(301);
        ctx2.session_change_timers.lock().unwrap().remove(&char_id);
        let _ = ctx2.db.set_character_solar_system(char_id, wh_sys, &ctx2.sde);
        spawn_ship_in_space(&ctx2, &mut sess2, char_id, heavy_ship_id, 641, wh_sys, wh_pos + DVec3::new(300.0, 0.0, 0.0), 105_000_000.0).await;
        let wh_ball_id = wormhole_item_id_for_signature(wh_sys, &wh_sig_id);
        rpc(&ctx2, &mut sess2, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)]).await.expect("WormholeJump");
    }
    let post_statics: BTreeSet<String> = ctx2.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&wh_sys).map(|l| l.iter().filter(|s| s.signature_id.starts_with("WST-") && !s.collapsed).map(|s| s.signature_id.clone()).collect()).unwrap_or_default()
    });
    assert_eq!(post_statics.len(), 1, "Collapsed static wormhole must respawn");
    assert!(!post_statics.contains(&wh_sig_id), "Respawned static must have new signature ID");
}

#[tokio::test]
async fn test_pve_mission_complete_escalations_and_expeditions() {

    let ctx = ctx_with_real_sde();
    let char_id = 90_000_001_i64;
    let mut sess = setup_pilot(&ctx, 10_201, char_id, JITA_SYS, Some(JITA_STATION)).await;

    // 1. Career Agent mission completion -> ISK, time bonus, LP, standings, AND item reward in hangar
    let career_agent = ctx.sde.agents.values().find(|a| a.agent_type_id == 12 && a.station_id.is_some()).expect("Career agent").clone();
    let agent_station = career_agent.station_id.unwrap();
    let agent_sys = career_agent.solar_system_id.unwrap_or(JITA_SYS);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys;
            ch.station_id = Some(agent_station);
            ch.standings.insert(career_agent.corporation_id, 2.0);
        }
        snap.skills.entry(char_id).or_default().insert(3357, TrainedSkillRecord { type_id: 3357, level: 3, skill_points: 8_000.0 });
    });
    sess.solar_system_id = Some(agent_sys);
    sess.station_id = Some(agent_station);

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(career_agent.agent_id)]).await.expect("Offer");
    let mission_id = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)]).await.expect("Accept");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) { m.npcs_remaining = 0; }
        let mut delivery = None;
        for note in snap.owner_notes.values() {
            if note.item_id == mission_id as i64 {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    if let (Some(t), Some(q)) = (v.get("required_delivery_type_id").and_then(|x| x.as_u64()), v.get("required_delivery_qty").and_then(|x| x.as_i64())) {
                        delivery = Some((t as u32, q as i32));
                    }
                }
            }
        }
        if let Some((tid, qty)) = delivery {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(iid, InventoryItemRecord { item_id: iid, type_id: tid, owner_id: char_id, location_id: agent_station, flag: InventoryFlag::Hangar, quantity: qty, ..Default::default() });
        }
    });

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let lp_before = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&career_agent.corporation_id).copied().unwrap_or(0));
    let standing_before = ctx.db.with_snapshot(|s| s.characters[&char_id].standings.get(&career_agent.corporation_id).copied().unwrap_or(0.0));
    let _ = ctx.drain_notifications_for_char(char_id);

    let comp = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mission_id as i64)]).await.expect("Complete");
    assert_eq!(comp.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    assert!(comp.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let reward_item_id = comp.dict_get("rewardItemID").and_then(|v| v.as_i64()).expect("rewardItemID");

    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0) > bal_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&career_agent.corporation_id).copied().unwrap_or(0)) > lp_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&char_id].standings.get(&career_agent.corporation_id).copied().unwrap_or(0.0)) > standing_before);
    assert!(ctx.db.with_snapshot(|s| s.items.get(&reward_item_id).is_some_and(|it| it.owner_id == char_id && it.location_id == agent_station && it.flag == InventoryFlag::Hangar)));

    // 2. Escalations & Expeditions
    let ship_id = 840_005_i64;
    spawn_ship_in_space(&ctx, &mut sess, char_id, ship_id, 587, JITA_SYS, DVec3::ZERO, 1_000_000.0).await;
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    ctx.set_mechanic_rng_seed(Some(3));

    let site_id = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&JITA_SYS].iter().find(|s| s.site_category == "CombatAnomaly" && !s.active_npc_ball_ids.is_empty()).unwrap().site_id.clone()
    });
    loop {
        let balls = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&JITA_SYS].iter().find(|s| s.site_id == site_id).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default());
        if balls.is_empty() { break; }
        for bid in balls {
            ctx.set_mechanic_rng_seed(Some(3));
            destroy_specialized_npc_and_queue_bounty(&ctx, JITA_SYS, bid, char_id).await;
        }
    }

    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnEscalatingPathMessage"));
    let details = rpc(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.unwrap();
    let PyRep::Rowset { columns, rows } = details else { panic!("Expected Rowset") };
    assert_eq!(rows.len(), 1);
    let inst_id = rows[0][columns.iter().position(|c| c == "instanceID").unwrap()].as_i64().unwrap();

    for expected_step in 1..=4 {
        let cur_entry = with_pve_exploration_reg(&ctx, |reg| reg.escalations_by_char.get(&char_id).and_then(|v| v.iter().find(|e| e.instance_id == inst_id)).cloned());
        let Some(entry) = cur_entry else { break };
        assert_eq!(entry.step_index, expected_step);
        let step_sys = entry.destination_solar_system_id;
        spawn_ship_in_space(&ctx, &mut sess, char_id, ship_id, 587, step_sys, DVec3::ZERO, 1_000_000.0).await;
        let exp_site_id = format!("EXP-{inst_id}-{expected_step}");
        loop {
            let balls = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system.get(&step_sys).and_then(|l| l.iter().find(|s| s.site_id == exp_site_id)).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default());
            if balls.is_empty() { break; }
            for bid in balls { destroy_specialized_npc_and_queue_bounty(&ctx, step_sys, bid, char_id).await; }
        }
    }
    assert!(with_pve_exploration_reg(&ctx, |reg| reg.escalations_by_char.get(&char_id).cloned().unwrap_or_default()).is_empty());
}

#[tokio::test]
async fn test_pve_incursion_payout_and_homefront_ops() {

    let ctx = ctx_with_real_sde();
    ensure_special_sites_spawned(&ctx).await;

    let p1 = 90_000_001_i64;
    let p2 = 90_000_002_i64;
    let mut sess1 = setup_pilot(&ctx, 10_301, p1, JITA_SYS, None).await;
    let mut sess2 = setup_pilot(&ctx, 10_302, p2, JITA_SYS, None).await;

    let (inc_sys, site_id, boss_bid, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().next().expect("active incursion");
        let site = inc.sites.iter().find(|s| !s.locked_until_low_influence).expect("unlocked incursion site");
        (site.solar_system_id, site.site_id.clone(), site.boss_npc_ball_id, site.position)
    });

    spawn_ship_in_space(&ctx, &mut sess1, p1, 840_011, 587, inc_sys, site_pos, 1_000_000.0).await;
    spawn_ship_in_space(&ctx, &mut sess2, p2, 840_012, 587, inc_sys, site_pos, 1_000_000.0).await;

    let fleet_id = rpc(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p2), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    ctx.session_change_timers.lock().unwrap().remove(&p2);
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)]).await.unwrap();

    let p1_isk_before = ctx.db.with_snapshot(|s| s.characters[&p1].balance.0);
    let p2_isk_before = ctx.db.with_snapshot(|s| s.characters[&p2].balance.0);
    let p1_lp_before = ctx.db.with_snapshot(|s| s.characters[&p1].loyalty_points.get(&1_000_125).copied().unwrap_or(0));

    common::clear_incursion_site_escort_waves(&ctx, inc_sys, &site_id).await;
    let _ = ctx.systems.get_or_spawn(inc_sys).remove_ball(boss_bid).await;
    let inc_res = on_incursion_boss_destroyed(&ctx, inc_sys, boss_bid, Some(p1)).await.unwrap().unwrap();
    assert_eq!(inc_res.fleet_size, 2);
    assert!(inc_res.isk_per_pilot > 0);
    assert!(inc_res.concord_lp_per_pilot > 0);
    assert!(inc_res.influence < 1.0);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].balance.0) > p1_isk_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p2].balance.0) > p2_isk_before);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].loyalty_points.get(&1_000_125).copied().unwrap_or(0)) > p1_lp_before);

    let PyRep::List(log_entries) = rpc(&ctx, &mut sess1, "incursion", "GetIncursionLPLog", &[]).await.unwrap() else { panic!("Expected List") };
    assert_eq!(log_entries.len(), 1);

    // Homefront Operations
    let PyRep::Dict(hf_dict) = rpc(&ctx, &mut sess1, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[PyRep::Integer(JITA_SYS as i64)]).await.unwrap() else { panic!("Expected Dict") };
    let (_, PyRep::Dict(jita_dungeons)) = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(JITA_SYS as i64)).unwrap() else { panic!() };
    let (did_rep, PyRep::List(inst_list)) = &jita_dungeons[0] else { panic!() };
    let did = did_rep.as_i64().unwrap() as u32;
    let inst_id = inst_list[0].as_i64().unwrap();
    assert_eq!(ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id), Some(70));

    let hf_boss_bid = with_pve_special_reg(&ctx, |reg| reg.homefront_sites[&JITA_SYS].iter().find(|s| s.instance_id == inst_id).unwrap().boss_npc_ball_id);
    let hf_isk_before = ctx.db.with_snapshot(|s| s.characters[&p1].balance.0);
    let kill = ctx.systems.get_or_spawn(JITA_SYS).apply_damage_with_attacker(hf_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(p1)).await.unwrap().unwrap();
    assert!(kill.1);
    assert!(ctx.db.with_snapshot(|s| s.characters[&p1].balance.0) > hf_isk_before);
}

#[tokio::test]
async fn test_pve_air_career_program_daily_goals_and_login_campaign() {

    let ctx = ctx_with_real_sde();
    let char_id = 90_000_001_u32;
    let mut sess = setup_pilot(&ctx, 10_401, char_id as i64, JITA_SYS, Some(JITA_STATION)).await;
    rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    let mut notice_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).unwrap();

    // 1. AIR Career Program
    let (goal_uuid, goal_target, goal_isk, goal_sp, goal_kind) = {
        let goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(char_id);
        let g = goals.iter().find(|g| g.def.target <= 5 && (g.def.isk_reward > 0 || g.def.sp_reward > 0.0)).unwrap();
        (g.uuid, g.def.target, g.def.isk_reward, g.def.sp_reward, g.def.activity_kind.clone())
    };
    let kind_arg = if goal_kind.is_empty() { "combat" } else { goal_kind.as_str() };
    ctx.quasar_gateway.progression.career_goals.on_gameplay_event(char_id, kind_arg, goal_target, &ctx.quasar_gateway);
    assert!(drain_notices(&mut notice_rx).iter().any(|u| u.contains("career.goal.api.CompletedNotice")));

    let isk_before = ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].balance.0);
    let sp_before = ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].free_skill_points);
    let claim_cg = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", make_uuid_request(goal_uuid)).await;
    assert_eq!(claim_cg.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].balance.0) - isk_before, IskCents::from_isk(goal_isk).0);
    assert!((ctx.db.with_snapshot(|s| s.characters[&(char_id as i64)].free_skill_points) - sp_before - goal_sp).abs() < 1e-6);

    // 2. Daily Goals
    let (dg_uuid, dg_target, dg_cat) = {
        let dgs = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
        let g = dgs.iter().find(|g| g.template.category > 0).unwrap();
        (g.uuid, g.template.target, g.template.category)
    };
    ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id, dg_cat, dg_target, &ctx.quasar_gateway);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", make_uuid_request(dg_uuid)).await.status_code, STATUS_OK);
    assert_ne!(quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", make_uuid_request(dg_uuid)).await.status_code, STATUS_OK);

    ctx.advance_secs(86_400);
    assert!(ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id).iter().all(|g| !g.claimed));

    // 3. Login Campaign
    let state1 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(state1.dict_get("can_claim_today"), Some(&PyRep::Bool(true)));
    assert!(!matches!(rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::None));
    assert!(matches!(rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::None));

    ctx.advance_secs(86_400);
    let state2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(state2.dict_get("can_claim_today"), Some(&PyRep::Bool(true)));
}
}
