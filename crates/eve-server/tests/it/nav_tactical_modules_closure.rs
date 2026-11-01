//! End-to-End Integration Tests closing Navigation parity rows (Part 1):
//! `feat:navigation.route-waypoints`, `feat:navigation.warp-vector`,
//! `feat:navigation.align-server-tick-rounding`, `feat:navigation.collision-bumping`,
//! and `feat:navigation.cloak-velocity-penalty`.

use crate::modules_advanced_tactical::*;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode, WarpPhase};
use eve_machonet::SessionState;
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

    // 1. Route waypoints via real V24 surfaces (`charMgr` + ESI `/v2/ui/autopilot/waypoint/` + `beyonce`)
    let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![]).await.expect("GetCharacterSettings initial");
    assert_eq!(init_settings, PyRep::Dict(Vec::new()));

    // Invalid destination ID rejected on charMgr.SaveCharacterSetting("waypoints", ...)
    let bad_wp = rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
        PyRep::String("waypoints".into()),
        PyRep::List(vec![PyRep::Integer(999_999_999)]),
    ]).await.unwrap_err();
    assert_eq!(bad_wp.code, "InvalidDestination");

    // Pick a Jita stargate jump destination so we can test live autopilot warp + jump arrival advancement
    let jita_jump = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .expect("Jita stargate jump")
        .clone();
    let dest_sys_id = jita_jump.to_solar_system_id;

    // SaveCharacterSetting("waypoints", ...) persists to DB & survives fresh session reconnect
    rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
        PyRep::String("waypoints".into()),
        PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]),
    ]).await.expect("SaveCharacterSetting waypoints");

    let mut reconnect_sess = SessionState::new(994_001);
    reconnect_sess.user_id = sess.user_id;
    reconnect_sess.character_id = Some(char_id);
    let reconnect_settings = rpc(&ctx, &mut reconnect_sess, "charMgr", "GetCharacterSettings", vec![])
        .await.expect("GetCharacterSettings on reconnected session");
    assert_eq!(
        reconnect_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]))
    );

    // Undock in Jita and verify beyonce.CmdWarpToStuffAutopilot uses 15 km default landing perimeter
    sess.station_id = None;
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    spawn_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        jita_sys,
        jita_jump.position - DVec3::new(10_000_000.0, 0.0, 0.0),
    )
    .await;
    let b_jita = ctx
        .bind_object(&mut sess, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita),
        "CmdWarpToStuffAutopilot",
        &[PyRep::Integer(jita_jump.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuffAutopilot to stargate");
    let (_, ap_ball) = ctx
        .systems
        .get_or_spawn(jita_sys)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let MovementMode::Warp { destination: ap_dest, .. } = ap_ball.mode else {
        panic!("Expected Warp mode from CmdWarpToStuffAutopilot");
    };
    assert!(
        (ap_dest.distance(jita_jump.position) - (gate_radius + 15_000.0)).abs() < 10.0,
        "Autopilot warp must land at gate_radius + 15,000m"
    );

    // Stargate jump arrival pops matching front waypoint (`dest_sys_id`)
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position).await;
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
    let post_jump_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings post jump");
    assert_eq!(
        post_jump_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(JITA_STATION)]))
    );

    // Return to Jita and dock at Jita 4-4 Station -> pops final waypoint & removes "waypoints" setting
    ctx.session_change_timers.lock().unwrap().remove(&char_id);
    sess.solar_system_id = Some(jita_sys);
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, station_pos).await;
    ctx.systems
        .get_or_spawn(jita_sys)
        .set_ball_position(ship_id, station_pos)
        .await;
    let b_jita_dock = ctx
        .bind_object(&mut sess, "beyonce", jita_sys as i64)
        .expect("rebind beyonce Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita_dock),
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

    // ESI POST /v2/ui/autopilot/waypoint/ (clear_other_waypoints, append, add_to_beginning, invalid 400)
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
    let bad_esi = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(bad_esi.status_code, 404);

    for url in [
        "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true".to_string(),
        format!("/v2/ui/autopilot/waypoint/?destination_id={JITA_STATION}&add_to_beginning=false&clear_other_waypoints=false"),
        format!("/v2/ui/autopilot/waypoint/?destination_id={dest_sys_id}&add_to_beginning=true&clear_other_waypoints=false"),
    ] {
        let resp = esi.handle_request(&EsiHttpRequest::new("POST", &url).with_bearer_token(&token));
        assert_eq!(resp.status_code, 204);
    }
    let esi_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after ESI waypoints");
    assert_eq!(
        esi_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![
            PyRep::Integer(dest_sys_id as i64),
            PyRep::Integer(30_002_187),
            PyRep::Integer(JITA_STATION),
        ]))
    );
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        vec![PyRep::String("waypoints".into())],
    )
    .await
    .expect("DeleteCharacterSetting waypoints");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone()),
        Vec::<i64>::new()
    );

    // 2. Cargo Warp Vector Data (`feat:navigation.warp-vector`): star-centered coordinates + 300s cooldown
    sess.station_id = None;
    let star_cel = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .expect("Jita star");
    let vec_item_id: i64 = 74_001;
    give_item(&ctx, vec_item_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);

    // Non-inventory space ball IDs (e.g., stargate or container ball) are rejected with InvalidVectorItem (no beacon fallback)
    let far_from_star = star_cel.position + DVec3::new(2_000_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, far_from_star).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(jita_jump.stargate_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    // Out of star proximity (> 1,496,000,000,000 m = 10 AU) -> rejected
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorTooFarFromStar"
    );

    // Place ship at a non-zero 3D offset from star so star_pos + delta != ship_pos + delta!
    let near_star = star_cel.position + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    sys.set_ball_position(ship_id, near_star).await;
    let vec_res = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .expect("UseVector with cargo Warp Vector Data");
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let expected_vec_dest = star_cel.position + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0);
    let (_, vec_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: actual_vec_dest, .. } = vec_ball.mode else {
        panic!("Expected MovementMode::Warp after cargo Warp Vector Data activation");
    };
    assert!(
        actual_vec_dest.distance(expected_vec_dest) < 10.0,
        "Warp Vector Data must resolve relative to solar-system star coordinates: got {actual_vec_dest:?} vs {expected_vec_dest:?}"
    );
    assert!(
        actual_vec_dest.distance(near_star + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0)) > 100_000_000.0,
        "Warp Vector destination must NOT be ship-relative"
    );

    // 300s per-character cooldown enforced on immediate 2nd activation, clears after 301s
    sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    sys.set_ball_position(ship_id, near_star).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorCooldown"
    );
    ctx.advance_secs(301);
    // Vector destination < 150,000 m from ship is rejected without consuming item
    sys.set_ball_position(ship_id, expected_vec_dest - DVec3::new(100_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpDistanceTooShort"
    );
    sys.set_ball_position(ship_id, near_star).await;
    rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .expect("UseVector succeeds after 300s cooldown");

    // 3. Server-Tick Sub-Tick Align Rounding (`feat:navigation.align-server-tick-rounding`)
    // Exact align time = ln(4) * inertia * mass / 1e6 = 3.200000 s -> ceil(3.2) = 4 ticks!
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

    for tick_idx in 1..=3 {
        sys.step_manual(1.0).await.unwrap();
        let (_, b) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. }),
            "Ship with 3.2s align time must remain Aligning on tick {tick_idx}"
        );
    }
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
    // parity: feat:navigation.collision-bumping, feat:navigation.cloak-velocity-penalty
    eve_testkit::attest(&[
        "feat:navigation.collision-bumping",
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
    assert!(post_a.position.distance(post_b.position) >= post_a.radius + post_b.radius - 1e-3);
    assert!(post_b.velocity.x > 100.0);
    assert!(
        tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
            && tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b))
    );
    if let MovementMode::Warp { phase, elapsed_warp_time, .. } = post_b.mode {
        assert_eq!(phase, WarpPhase::Aligning);
        assert_eq!(elapsed_warp_time, 0.0);
    } else {
        panic!("Expected ship B to remain in Aligning warp phase after bump");
    }

    // 1b. Ship-vs-Station, Ship-vs-Upwell Structure, Ship-vs-Stargate, and Ship-vs-Planet static hull collision bumping
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    let (_, sta_ball) = sys.get_ball_snapshot(JITA_STATION).await.unwrap().unwrap();
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
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, jita_sys, struct_id).await;
    let (_, struct_ball) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
    let (_, gate_ball) = sys.get_ball_snapshot(gate.stargate_id).await.unwrap().unwrap();
    let planet = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 7 && c.radius > 0.0)
        .expect("Jita planet");
    for (static_id, static_pos, core_rad, label) in [
        (JITA_STATION, station_pos, sta_ball.radius, "NPC station"),
        (struct_id, DVec3::from_array(struct_pos), struct_ball.radius, "Upwell structure"),
        (gate.stargate_id, gate.position, gate_ball.radius, "stargate"),
    ] {
        let mut cs_coll = ctx.build_ship_combat_state(char_a, ship_a, 640);
        cs_coll.undock_invulnerable_remaining_ticks = 0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: 640, position: static_pos + DVec3::new(core_rad - 50.0, 0.0, 0.0),
                velocity: DVec3::new(-500.0, 0.0, 0.0), max_velocity: 500.0, inertia: 0.5,
                mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs_coll,
            ctx.build_ship_slim_meta(char_a, 640),
        ).await.unwrap();
        let tick_s = sys.step_manual(1.0).await.unwrap();
        let (_, post_s) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!(
            post_s.position.distance(static_pos) >= core_rad + post_s.radius - 1e-3,
            "Ship must not interpenetrate {label} hull"
        );
        assert!(post_s.velocity.x > 0.0, "Ship must bounce off {label} hull");
        assert!(tick_s.events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if (*ball_a == ship_a && *ball_b == static_id) || (*ball_a == static_id && *ball_b == ship_a))));
    }
    let (_, post_struct) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
    assert!(post_struct.position.distance(DVec3::from_array(struct_pos)) < 1e-6, "Anchored Upwell structure must remain stationary");

    // 2. Celestial, Stargate, Station, & Upwell Structure Warp Landing Points
    let b_jita = ctx
        .bind_object(&mut sess_a, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    let mut cs_warp = ctx.build_ship_combat_state(char_a, ship_a, 640);
    cs_warp.capacitor = 100_000.0;
    cs_warp.max_capacitor = 100_000.0;
    sys.set_combat_state(ship_a, cs_warp).await.unwrap();

    // 2a. Planet (group 7) at 0m -> anchor_pos; at 50km -> 50,000 m; <150km effective -> WarpDistanceTooShort; NaN/Inf -> InvalidArgument
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: planet.position - DVec3::new(planet.radius + 500_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    for bad_range in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let nan_err = ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(bad_range)], &kw, None,
        ).await.unwrap_err();
        assert_eq!(nan_err.code, "InvalidArgument", "Non-finite warp range {bad_range} must be rejected");
    }
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to planet at 0m");
    let (_, w_planet) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_planet, .. } = w_planet.mode else { panic!("Expected Warp"); };
    assert!(dest_planet.distance(planet.position) < 10.0);

    sys.set_movement_mode(ship_a, MovementMode::Stop).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
    ).await.expect("Warp to planet at 50km");
    let (_, w_planet_50) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_planet_50, .. } = w_planet_50.mode else { panic!("Expected Warp"); };
    assert!((dest_planet_50.distance(planet.position) - 50_000.0).abs() < 10.0);

    // 180 km from planet anchor with minRange=50 km -> effective warp distance 130 km < 150 km -> WarpDistanceTooShort!
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: planet.position - DVec3::new(180_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    let short_err = ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
    ).await.unwrap_err();
    assert_eq!(short_err.code, "WarpDistanceTooShort");

    // 2b. Star (group 6) -> radius + 100,000 m standoff
    let star = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6 && c.radius > 0.0)
        .expect("Jita star");
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: star.position - DVec3::new(star.radius + 2_000_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(star.celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to star at 0m");
    let (_, w_star) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_star, .. } = w_star.mode else { panic!("Expected Warp"); };
    assert!((dest_star.distance(star.position) - (star.radius + 100_000.0)).abs() < 10.0);

    // 2c. Stargate (`sde.jumps`) -> gate_radius standoff (0m from gate perimeter)
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: gate.position - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(gate.stargate_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to stargate at 0m");
    let (_, w_gate) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_gate, .. } = w_gate.mode else { panic!("Expected Warp"); };
    assert!((dest_gate.distance(gate.position) - gate_radius).abs() < 10.0);

    // 2d. NPC Station & Upwell Structure -> 2,000 m docking ring perimeter (>0m from center, inside <=2,500m CmdDock perimeter)
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: station_pos - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to NPC station at 0m");
    let (_, w_sta) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_sta, .. } = w_sta.mode else { panic!("Expected Warp"); };
    assert!((dest_sta.distance(station_pos) - (sta_ball.radius + 2_000.0)).abs() < 10.0, "NPC station landing must be on Zero Point Envelope (radius + 2,000m)");

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
    let MovementMode::Warp { destination: dest_struct, .. } = w_struct.mode else { panic!("Expected Warp"); };
    assert!((dest_struct.distance(DVec3::from_array(struct_pos)) - (struct_ball.radius + 2_000.0)).abs() < 10.0, "Structure landing must be on Zero Point Envelope (radius + 2,000m)");

    // 3. Cloak Velocity Penalty & Restoration (`feat:navigation.cloak-velocity-penalty`)
    for (cloak_tid, expected_mult) in [(11_370_u32, 0.10_f64), (11_577, 0.25), (11_578, 1.00)] {
        let hull_tid: u32 = if cloak_tid == 11_578 { 11_192 } else { 640 };
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
        assert!((cloaked_ball.max_velocity - base_vel * expected_mult).abs() < 1e-3);
        let all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await.unwrap();
        assert!((attr_from_all_info(&all_info, "shipInfo", 37) - base_vel * expected_mult).abs() < 1e-3);

        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(cloak_mod_id)])
            .await.expect("Deactivate cloaking device");
        let (_, decloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!((decloaked_ball.max_velocity - base_vel).abs() < 1e-3);
    }
}
