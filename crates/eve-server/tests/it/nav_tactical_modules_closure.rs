//! End-to-End Integration Tests closing Navigation parity rows (Part 1):
//! `feat:navigation.route-waypoints`, `feat:navigation.warp-vector`,
//! `feat:navigation.align-server-tick-rounding`, `feat:navigation.collision-bumping`,
//! `feat:navigation.warp-landing-points`, and `feat:navigation.cloak-velocity-penalty`.

use crate::modules_advanced_tactical::*;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode, WarpPhase};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_waypoints_warp_vector_and_align_tick_rounding() {
    // parity: feat:navigation.route-waypoints, feat:navigation.warp-vector, feat:navigation.align-server-tick-rounding
    eve_testkit::attest(&[
        "feat:navigation.route-waypoints",
        "feat:navigation.warp-vector",
        "feat:navigation.align-server-tick-rounding",
    ]);

    let ctx = Arc::new(build_ctx());
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 94_001;
    let ship_id: i64 = 84_001;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);

    // 1. Route waypoints (`feat:navigation.route-waypoints`)
    // Fresh character has empty settings dict (no "waypoints" key)
    let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings initial");
    assert_eq!(init_settings, PyRep::Dict(Vec::new()));

    // Invalid destination ID is rejected
    let bad_wp = rpc(
        &ctx,
        &mut sess,
        "map",
        "SetWaypoint",
        vec![PyRep::Integer(999_999_999), PyRep::Bool(true), PyRep::Bool(false)],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_wp.code, "InvalidDestination");

    // Pick a Jita stargate jump destination so we can test live jump arrival advancement
    let jita_jump = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .expect("Jita stargate jump")
        .clone();
    let dest_sys_id = jita_jump.to_solar_system_id;

    // SetWaypoint (clear=true) -> SetWaypoint (clear=false) -> GetWaypoints -> charMgr.GetCharacterSettings sync
    rpc(
        &ctx,
        &mut sess,
        "map",
        "SetWaypoint",
        vec![
            PyRep::Integer(dest_sys_id as i64),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("SetWaypoint clear=true");
    rpc(
        &ctx,
        &mut sess,
        "map",
        "SetWaypoint",
        vec![
            PyRep::Integer(JITA_STATION),
            PyRep::Bool(false),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("SetWaypoint clear=false");
    let wps = rpc(&ctx, &mut sess, "map", "GetWaypoints", vec![])
        .await
        .expect("GetWaypoints");
    assert_eq!(
        wps,
        PyRep::List(vec![
            PyRep::Integer(dest_sys_id as i64),
            PyRep::Integer(JITA_STATION),
        ])
    );
    let settings_after_add = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after SetWaypoint");
    assert_eq!(
        settings_after_add.dict_get("waypoints"),
        Some(&PyRep::List(vec![
            PyRep::Integer(dest_sys_id as i64),
            PyRep::Integer(JITA_STATION),
        ]))
    );

    // SaveCharacterSetting("waypoints", ...) syncs back into CharacterRecord.autopilot_waypoints
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        vec![
            PyRep::String("waypoints".into()),
            PyRep::List(vec![
                PyRep::Integer(30_002_187),
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(JITA_STATION),
            ]),
        ],
    )
    .await
    .expect("SaveCharacterSetting waypoints");
    rpc(
        &ctx,
        &mut sess,
        "map",
        "RemoveWaypointIfFirst",
        vec![PyRep::Integer(30_002_187)],
    )
    .await
    .expect("RemoveWaypointIfFirst");

    // Stargate jump arrival pops matching front waypoint (`dest_sys_id`)
    sess.station_id = None;
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position).await;
    let b_jita = ctx
        .bind_object(&mut sess, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita),
        "CmdStargateJump",
        &[PyRep::Integer(jita_jump.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdStargateJump advances waypoint");
    let post_jump_wps = rpc(&ctx, &mut sess, "map", "GetWaypoints", vec![])
        .await
        .expect("GetWaypoints post jump");
    assert_eq!(post_jump_wps, PyRep::List(vec![PyRep::Integer(JITA_STATION)]));

    // Return to Jita and dock at Jita 4-4 Station -> pops final waypoint & clears "waypoints" setting
    ctx.session_change_timers.lock().unwrap().remove(&char_id);
    sess.solar_system_id = Some(jita_sys);
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, station_pos).await;
    ctx.systems.get_or_spawn(jita_sys).set_ball_position(ship_id, station_pos).await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita),
        "CmdDock",
        &[PyRep::Integer(JITA_STATION)],
        &kw,
        None,
    )
    .await
    .expect("CmdDock advances final waypoint");
    let post_dock_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings post dock");
    assert_eq!(post_dock_settings.dict_get("waypoints"), None);

    // ESI POST /v2/ui/autopilot/waypoint/ syncs into charMgr.GetCharacterSettings
    let uid = ctx
        .db
        .provision_player_account("nav_esi_pilot", "nav-esi-secret");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().user_id = uid;
    });
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        sess.session_id,
        Some(char_id),
        Some("nav_esi_hash".into()),
        true,
        ctx.now_unix(),
    );
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "nav_esi_pilot",
            "nav-esi-secret",
            char_id,
            "evomu-nav-test",
            "https://localhost/callback",
            vec!["esi-ui.write_waypoint.v1".into()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-nav-test",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token;
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let esi_resp = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(esi_resp.status_code, 204);
    let esi_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after ESI waypoint");
    assert_eq!(
        esi_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(30_002_187)]))
    );
    rpc(&ctx, &mut sess, "map", "ClearWaypoints", vec![])
        .await
        .expect("ClearWaypoints");

    // 2. Cargo Warp Vector Data & Space Vector Beacon (`feat:navigation.warp-vector`)
    sess.station_id = None;
    let star_cel = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .expect("Jita star");
    let vec_item_id: i64 = 74_001;
    give_item(&ctx, vec_item_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);

    // Out of star proximity (> 1,496,000,000,000 m = 10 AU) -> rejected
    let far_from_star = star_cel.position + DVec3::new(2_000_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, far_from_star).await;
    let out_of_range_err = rpc(
        &ctx,
        &mut sess,
        "warpVectorMgr",
        "UseVector",
        vec![PyRep::Integer(vec_item_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(out_of_range_err.code, "WarpVectorTooFarFromStar");

    // Within star proximity -> consumes 1 unit from cargo and warps to star_pos + (dx, dy, dz)
    let near_star = star_cel.position + DVec3::new(star_cel.radius + 500_000.0, 0.0, 0.0);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    sys.set_ball_position(ship_id, near_star).await;
    let vec_res = rpc(
        &ctx,
        &mut sess,
        "warpVectorMgr",
        "UseVector",
        vec![PyRep::Integer(vec_item_id)],
    )
    .await
    .expect("UseVector with cargo Warp Vector Data");
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let expected_vec_dest = near_star + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0);
    let (_, vec_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: actual_vec_dest, .. } = vec_ball.mode else {
        panic!("Expected MovementMode::Warp after cargo Warp Vector Data activation");
    };
    assert!(actual_vec_dest.distance(expected_vec_dest) < 10.0);

    // 3. Server-Tick Sub-Tick Align Rounding (`feat:navigation.align-server-tick-rounding`)
    // Exact align time = ln(4) * inertia * mass / 1e6.
    // Choose inertia = 2.3083120654223415, mass = 1_000_000.0 -> t_align = 3.200000 s -> ceil(3.2) = 4 ticks!
    let t_align_3_2_inertia = 3.2 / 4.0_f64.ln();
    let align_origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let align_dest = align_origin + DVec3::new(10_000_000.0, 0.0, 0.0);
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: align_origin,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: t_align_3_2_inertia,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::new_warp(align_origin, align_dest, 3.0),
    })
    .await
    .unwrap();

    // Ticks 1, 2, 3 (t = 1s, 2s, 3s): ship must still be Aligning (not yet entered warp at 3.2s)
    for tick_idx in 1..=3 {
        sys.step_manual(1.0).await.unwrap();
        let (_, b) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. }),
            "Ship with 3.2s align time must remain Aligning on tick {tick_idx}"
        );
    }
    // Tick 4 (t = 4.0s = ceil(3.2s)): ship enters Accelerating/Cruising/Decelerating warp phase!
    sys.step_manual(1.0).await.unwrap();
    let (_, b_t4) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(
            b_t4.mode,
            MovementMode::Warp {
                phase: WarpPhase::Accelerating | WarpPhase::Cruising | WarpPhase::Decelerating,
                ..
            }
        ),
        "Ship with 3.2s align time must enter warp on tick 4 (4.0s): got {:?}",
        b_t4.mode
    );
}

#[tokio::test]
async fn test_collision_bumping_warp_landing_points_and_cloak_velocity() {
    // parity: feat:navigation.collision-bumping, feat:navigation.warp-landing-points, feat:navigation.cloak-velocity-penalty
    eve_testkit::attest(&[
        "feat:navigation.collision-bumping",
        "feat:navigation.warp-landing-points",
        "feat:navigation.cloak-velocity-penalty",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_011;
    let ship_a: i64 = 84_011;
    let char_b: i64 = 94_012;
    let ship_b: i64 = 84_012;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 640, jita_sys, false);
    let _sess_b = seed_pilot(&ctx, char_b, ship_b, 640, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Ship-to-ship collision bumping & align timer reset (`feat:navigation.collision-bumping`)
    let bump_origin = DVec3::new(80_000_000_000.0, 0.0, 0.0);
    // Ship A (heavy Machariel-mass bumper moving +X at 800 m/s) collides with Ship B (aligning for warp)
    let mut cs_a = ctx.build_ship_combat_state(char_a, ship_a, 640);
    cs_a.undock_invulnerable_remaining_ticks = 0;
    let mut cs_b = ctx.build_ship_combat_state(char_b, ship_b, 640);
    cs_b.undock_invulnerable_remaining_ticks = 0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_a,
            type_id: 640,
            position: bump_origin,
            velocity: DVec3::new(800.0, 0.0, 0.0),
            max_velocity: 800.0,
            inertia: 0.5,
            mass: 100_000_000.0,
            radius: 300.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        },
        cs_a,
        ctx.build_ship_slim_meta(char_a, 640),
    )
    .await
    .unwrap();
    let mut align_b = MovementMode::new_warp(
        bump_origin + DVec3::new(900.0, 0.0, 0.0),
        bump_origin + DVec3::new(50_000_000.0, 0.0, 0.0),
        3.0,
    );
    if let MovementMode::Warp { ref mut elapsed_warp_time, .. } = align_b {
        *elapsed_warp_time = 6.0;
    }
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_b,
            type_id: 640,
            position: bump_origin + DVec3::new(900.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 300.0,
            speed_fraction: 0.0,
            mode: align_b,
        },
        cs_b,
        ctx.build_ship_slim_meta(char_b, 640),
    )
    .await
    .unwrap();

    let tick_b = sys.step_manual(1.0).await.unwrap();
    let (_, post_a) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert!(
        post_a.position.distance(post_b.position) >= post_a.radius + post_b.radius - 1e-3,
        "Colliding ships must be separated to at least sum of radii"
    );
    assert!(
        post_b.velocity.x > 100.0,
        "Bumped ship B must receive positive +X velocity impulse: got {:?}",
        post_b.velocity
    );
    assert!(
        tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
            && tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b)),
        "Collision bump must emit SetBallPosition and SetBallVelocity events"
    );
    if let MovementMode::Warp { phase, elapsed_warp_time, .. } = post_b.mode {
        assert_eq!(phase, WarpPhase::Aligning);
        assert_eq!(elapsed_warp_time, 0.0, "Collision bump must reset Aligning warp timer");
    } else {
        panic!("Expected ship B to remain in Aligning warp phase after bump");
    }

    // 2. Celestial, Stargate, Station, & Upwell Structure Warp Landing Points (`feat:navigation.warp-landing-points`)
    let b_jita = ctx
        .bind_object(&mut sess_a, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    let mut cs_warp = ctx.build_ship_combat_state(char_a, ship_a, 640);
    cs_warp.capacitor = 100_000.0;
    cs_warp.max_capacitor = 100_000.0;
    sys.set_combat_state(ship_a, cs_warp).await.unwrap();

    // 2a. Planet (group 7) -> radius + 2,500 m standoff
    let planet = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 7 && c.radius > 0.0)
        .expect("Jita planet");
    let approach_offset = DVec3::new(500_000_000.0, 0.0, 0.0);
    sys.spawn_ball(Ball {
        id: ship_a,
        type_id: 640,
        position: planet.position - approach_offset,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.1,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess_a,
        "beyonce",
        Some(b_jita),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("Warp to planet at 0m");
    let (_, w_planet) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_planet, .. } = w_planet.mode else {
        panic!("Expected Warp mode to planet");
    };
    let expected_planet_landing = planet.position - DVec3::new(planet.radius + 2_500.0, 0.0, 0.0);
    assert!(
        dest_planet.distance(expected_planet_landing) < 10.0,
        "Planet warp landing point must be radius + 2,500m from center: got {}, expected {}",
        dest_planet.distance(planet.position),
        planet.radius + 2_500.0
    );

    // 2b. Star (group 6) -> radius + 100,000 m standoff
    let star = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6 && c.radius > 0.0)
        .expect("Jita star");
    sys.spawn_ball(Ball {
        id: ship_a,
        type_id: 640,
        position: star.position - DVec3::new(2_000_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.1,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess_a,
        "beyonce",
        Some(b_jita),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(star.celestial_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("Warp to star at 0m");
    let (_, w_star) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_star, .. } = w_star.mode else {
        panic!("Expected Warp mode to star");
    };
    assert!(
        (dest_star.distance(star.position) - (star.radius + 100_000.0)).abs() < 10.0,
        "Star warp landing point must be radius + 100,000m"
    );

    // 2c. Stargate (`sde.jumps`) -> gate_radius standoff (0m from gate perimeter)
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .expect("Jita gate");
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    sys.spawn_ball(Ball {
        id: ship_a,
        type_id: 640,
        position: gate.position - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.1,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess_a,
        "beyonce",
        Some(b_jita),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(gate.stargate_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("Warp to stargate at 0m");
    let (_, w_gate) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_gate, .. } = w_gate.mode else {
        panic!("Expected Warp mode to gate");
    };
    assert!(
        (dest_gate.distance(gate.position) - gate_radius).abs() < 10.0,
        "Stargate warp landing point must be gate_radius ({gate_radius}) from center"
    );

    // 2d. Upwell Structure -> exact anchor_pos (0m from tether/dock center)
    let struct_id: i64 = 1_040_000_001;
    let struct_pos = [90_000_000_000.0, 0.0, 0.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id, type_id: 35832, owner_corp_id: 1_000_125, solar_system_id: jita_sys,
                name: "Test Astrahus".into(), position: struct_pos, state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: DVec3::from_array(struct_pos) - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(struct_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to Upwell structure at 0m");
    let (_, w_struct) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_struct, .. } = w_struct.mode else {
        panic!("Expected Warp mode to Upwell structure");
    };
    assert!(dest_struct.distance(DVec3::from_array(struct_pos)) < 10.0);

    // 3. Cloak Velocity Penalty & Restoration (`feat:navigation.cloak-velocity-penalty`)
    // Test Prototype Cloaking Device I (11370 -> 10% max_velocity), Improved II (11577 -> 25%), Covert Ops II (11578 -> 100%)
    for (cloak_tid, expected_mult) in [(11_370_u32, 0.10_f64), (11_577, 0.25), (11_578, 1.00)] {
        let hull_tid: u32 = if cloak_tid == 11_578 { 11_192 } else { 640 }; // Buzzard for Covert Ops Cloak II, Raven otherwise
        let cloak_mod_id: i64 = 74_100 + cloak_tid as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.location_id != ship_a);
            if let Some(s) = snap.items.get_mut(&ship_a) {
                s.type_id = hull_tid;
            }
            snap.module_online_states.clear();
            snap.module_active_states.clear();
        });
        give_item(&ctx, cloak_mod_id, cloak_tid, char_a, ship_a, InventoryFlag::HiSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cloak_mod_id, true); });
        ctx.invalidate_ship_fitting(ship_a);

        let pre_all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await.unwrap();
        let base_vel = attr_from_all_info(&pre_all_info, "shipInfo", 37);
        let mut clean_cs = ctx.build_ship_combat_state(char_a, ship_a, hull_tid);
        clean_cs.capacitor = 100_000.0;
        clean_cs.max_capacitor = 100_000.0;
        let _ = sys.remove_ball(ship_a).await;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: hull_tid, position: DVec3::new(95_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: base_vel, inertia: 1.0, mass: 10_000_000.0,
                radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            clean_cs,
            ctx.build_ship_slim_meta(char_a, hull_tid),
        ).await.unwrap();

        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod_id)])
            .await.expect("Activate cloaking device");
        let (_, cloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!(
            (cloaked_ball.max_velocity - base_vel * expected_mult).abs() < 1e-3,
            "Cloak type {cloak_tid} must scale max_velocity by {expected_mult}: got {}",
            cloaked_ball.max_velocity
        );
        let all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await.unwrap();
        assert!(
            (attr_from_all_info(&all_info, "shipInfo", 37) - base_vel * expected_mult).abs() < 1e-3,
            "GetAllInfo effectiveMaxVelocity must match cloaked max_velocity"
        );

        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(cloak_mod_id)])
            .await.expect("Deactivate cloaking device");
        let (_, decloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!(
            (decloaked_ball.max_velocity - base_vel).abs() < 1e-3,
            "Decloaking must restore original max_velocity ({base_vel}): got {}",
            decloaked_ball.max_velocity
        );
    }
}
