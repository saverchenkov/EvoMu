//! Additional branch-coverage and mutation-killing unit tests for `nav_warp_interdiction.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::with_misc_state;
use crate::services::nav_warp_interdiction::{
    advance_waypoints_on_arrival, calculate_route_with_waypoints, ensure_not_in_active_warp,
    handle_set_session_check, handle_use_warp_vector, is_valid_waypoint_destination,
    resolve_warp_landing_point, resolve_waypoint_solar_system, sync_waypoints_from_char_settings,
    sync_waypoints_on_login, validate_pre_warp_common, WARP_VECTOR_COOLDOWN_SECS,
};
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx_with_warp_vector_type() -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = shared_sde();
    let reg = Arc::make_mut(&mut sde);
    let mut vec_type: InvType = reg
        .types
        .values()
        .next()
        .cloned()
        .expect("at least one type in SDE");
    vec_type.type_id = 77_825;
    vec_type.group_id = 4599;
    vec_type.name = "Jovian Symbolic Navigator".into();
    vec_type.default_attributes = HashMap::from([
        (5429, 500_000.0),
        (5430, 0.0),
        (5431, 0.0),
        (5432, 1_496_000_000_000.0),
    ]);
    reg.types.insert(77_825, vec_type);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[test]
fn waypoints_login_sync_structure_destinations_and_route_dijkstra_flags_kill_mutations() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let struct_id = 1_045_100_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "WaypointCitadel".into(),
                state: "Armor_Vulnerable".into(),
                position: [10_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });

    assert_eq!(resolve_waypoint_solar_system(&ctx, struct_id), Some(30_000_144));
    assert!(is_valid_waypoint_destination(&ctx, struct_id));

    // Destroyed structure is not a valid waypoint destination
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(resolve_waypoint_solar_system(&ctx, struct_id), None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "Armor_Vulnerable".into();
    });

    // sync_waypoints_from_char_settings with Tuple and non-list (clears waypoints)
    sync_waypoints_from_char_settings(
        &ctx,
        cid,
        &PyRep::Tuple(vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(struct_id),
            PyRep::Integer(60_003_760),
        ]),
    )
    .unwrap();
    // Arriving at middle waypoint (struct_id) drains 0..=1, leaving [60_003_760]
    advance_waypoints_on_arrival(&ctx, cid, struct_id);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid].autopilot_waypoints.clone()),
        vec![60_003_760]
    );
    // Arriving at non-queued destination is a no-op
    advance_waypoints_on_arrival(&ctx, cid, 30_002_187);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid].autopilot_waypoints.clone()),
        vec![60_003_760]
    );

    // Clear via PyRep::None, then test sync_waypoints_on_login from char_settings with mixed valid/invalid IDs
    sync_waypoints_from_char_settings(&ctx, cid, &PyRep::None).unwrap();
    assert!(ctx
        .db
        .with_snapshot(|s| s.characters[&cid].autopilot_waypoints.is_empty()));
    with_misc_state(&ctx, |st| {
        st.char_settings.entry(cid).or_default().insert(
            "waypoints".into(),
            PyRep::List(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(-99),
                PyRep::Integer(30_000_144),
            ]),
        );
    });
    sync_waypoints_on_login(&ctx, cid);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid].autopilot_waypoints.clone()),
        vec![30_000_142, 30_000_144]
    );

    // Route calculation: duplicate current_sys, WaypointInAvoidList, WaypointUnreachable, RouteBlockedByAvoidList, "insecure" flag
    let route_insecure = calculate_route_with_waypoints(
        &ctx,
        30_000_142,
        &[30_000_142, struct_id],
        &[],
        "insecure",
    )
    .unwrap();
    assert_eq!(route_insecure.first(), Some(&30_000_142));
    assert_eq!(route_insecure.last(), Some(&30_000_144));

    assert_eq!(
        calculate_route_with_waypoints(&ctx, 30_000_142, &[30_000_144], &[30_000_144], "shortest")
            .unwrap_err()
            .code,
        "WaypointInAvoidList"
    );

    // Wormhole / disconnected system -> WaypointUnreachable when avoid_systems is empty
    if let Some(&wh_sys) = ctx.sde.solar_systems.keys().find(|&&s| s >= 31_000_000) {
        assert_eq!(
            calculate_route_with_waypoints(&ctx, 30_000_142, &[wh_sys as i64], &[], "shortest")
                .unwrap_err()
                .code,
            "WaypointUnreachable"
        );
        assert_eq!(
            calculate_route_with_waypoints(
                &ctx,
                30_000_142,
                &[wh_sys as i64],
                &[30_002_187],
                "shortest"
            )
            .unwrap_err()
            .code,
            "RouteBlockedByAvoidList"
        );
    }
}

#[test]
fn resolve_warp_landing_point_for_gates_stations_structures_planets_moons_and_stars_kills_mutations() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let ship_pos = DVec3::ZERO;
    let anchor_pos = DVec3::new(1_000_000.0, 0.0, 0.0);

    // 1. Coincident ship_pos == anchor_pos returns anchor_pos directly
    assert_eq!(
        resolve_warp_landing_point(&ctx, sys_id, 60_003_760, anchor_pos, anchor_pos),
        anchor_pos
    );

    // 2. Stargate in sys_id lands at anchor_pos.x - gate_radius
    if let Some((&gate_id, _)) = ctx
        .sde
        .jumps
        .iter()
        .find(|(_, j)| j.from_solar_system_id == sys_id)
    {
        let gate_landing =
            resolve_warp_landing_point(&ctx, sys_id, gate_id, ship_pos, anchor_pos);
        assert!(gate_landing.x < anchor_pos.x);
        assert!((anchor_pos.x - gate_landing.x) >= 1_000.0);
    }

    // 3. Upwell Structure lands at anchor_pos.x - struct_radius
    let struct_id = 1_045_100_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "LandingCitadel".into(),
                state: "Armor_Vulnerable".into(),
                position: [1_000_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });
    let struct_landing =
        resolve_warp_landing_point(&ctx, sys_id, struct_id, ship_pos, anchor_pos);
    assert!(struct_landing.x <= anchor_pos.x);

    // 4. Star (group 6), Planet (group 7), Moon (group 8)
    let cels = ctx.sde.celestials_in_system(sys_id);
    if let Some(star) = cels.iter().find(|c| c.group_id == 6) {
        let p = resolve_warp_landing_point(&ctx, sys_id, star.celestial_id, ship_pos, anchor_pos);
        let expected_standoff = star.radius.max(0.0) + 100_000.0;
        assert!(((anchor_pos.x - p.x) - expected_standoff).abs() < 1e-3);
    }
    if let Some(planet) = cels.iter().find(|c| c.group_id == 7) {
        let p = resolve_warp_landing_point(&ctx, sys_id, planet.celestial_id, ship_pos, anchor_pos);
        let expected_standoff = planet.radius.max(0.0) + 2_500.0;
        assert!(((anchor_pos.x - p.x) - expected_standoff).abs() < 1e-3);
    }
    if let Some(moon) = cels.iter().find(|c| c.group_id == 8) {
        let p = resolve_warp_landing_point(&ctx, sys_id, moon.celestial_id, ship_pos, anchor_pos);
        let expected_standoff = moon.radius.max(0.0) + 2_500.0;
        assert!(((anchor_pos.x - p.x) - expected_standoff).abs() < 1e-3);
    }

    // 5. Unknown target returns exact anchor_pos
    assert_eq!(
        resolve_warp_landing_point(&ctx, sys_id, 999_999_999, ship_pos, anchor_pos),
        anchor_pos
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_vector_consumption_guards_cooldown_star_distance_and_session_check_kill_mutations() {
    let (ctx, clock) = test_ctx_with_warp_vector_type();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_101_i64;

    // 1. ensure_not_in_active_warp phases
    let mut warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e7, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = WarpPhase::Aligning;
    }
    assert!(ensure_not_in_active_warp(&warp_mode).is_ok());
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = WarpPhase::Cruising;
    }
    assert_eq!(
        ensure_not_in_active_warp(&warp_mode).unwrap_err().code,
        "ShipAlreadyInWarp"
    );

    // 2. validate_pre_warp_common in Abyssal space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert_eq!(
        validate_pre_warp_common(&ctx, cid, ship_id, 32_000_001, &sys_handle)
            .await
            .unwrap_err()
            .code,
        "CannotWarpInAbyssalSpace"
    );

    // 3. handle_use_warp_vector pre-flight guards
    let mut sess = auth_session(1, cid, corp_id, sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = None;
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(950_001)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball near system star
    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: star_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Cloaked without covert cloak -> CloakedCannotWarp
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 0,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(950_001)])
            .await
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    let _ = sys_handle
        .set_combat_state(ship_id, CombatState::default())
        .await;

    // Missing cargo item -> InvalidVectorItem; wrong flag -> InvalidVectorItem; wrong group -> InvalidVectorItem
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(950_001)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    let vec_iid = 950_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vec_iid,
            InventoryItemRecord {
                item_id: vec_iid,
                type_id: 34, // Tritanium, not group 4599
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    // Set type_id to 77_825 (group 4599), test max_star_dist failure, short distance failure, and success
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.type_id = 77_825;
        it.dynamic_attributes.insert(5432, -1.0); // forces star_dist > max_star_dist
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorTooFarFromStar"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.dynamic_attributes.insert(5432, 1e15);
        it.dynamic_attributes.insert(5429, 1_000.0); // < 150,000 m
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "WarpDistanceTooShort"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.dynamic_attributes.insert(5429, 500_000.0);
    });
    let ok_vec = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
        .await
        .unwrap();
    assert_eq!(ok_vec.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(ok_vec.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));

    // Immediate reactivation hits WarpVectorCooldown
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorCooldown"
    );
    clock.advance_secs(WARP_VECTOR_COOLDOWN_SECS + 1);

    // 4. handle_set_session_check dict mismatch, valid dict, active timer query, and SessionChangeInProgress
    let bad_dict = PyRep::keyval([("solarsystemid2", PyRep::Integer(30_000_144))]);
    assert_eq!(
        handle_set_session_check(&ctx, &mut sess, &[bad_dict])
            .await
            .unwrap_err()
            .code,
        "SessionCheckMismatch"
    );

    let good_dict = PyRep::keyval([("solarsystemid2", PyRep::Integer(sys_id as i64))]);
    let started = handle_set_session_check(&ctx, &mut sess, &[good_dict])
        .await
        .unwrap();
    assert_eq!(
        started.dict_get("trigger"),
        Some(&PyRep::String("session_change".into()))
    );

    let active_timer = handle_set_session_check(&ctx, &mut sess, &[]).await.unwrap();
    assert_eq!(active_timer.dict_get("active"), Some(&PyRep::Bool(true)));

    assert_eq!(
        handle_set_session_check(&ctx, &mut sess, &[PyRep::String("undock".into())])
            .await
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agency_warp_interdiction_bubbles_capacitor_tier_rollback_and_session_timer_expiry() {
    use crate::services::nav_warp_interdiction::{
        check_warp_interdiction_and_adjust_dest, execute_agency_content_warp, on_warp_initiated_hooks,
    };
    use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
    use eve_db::TrainedSkillRecord;

    let (ctx, clock) = test_ctx_with_warp_vector_type();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 970_000_101_i64;
    let shuttle_id = 970_000_102_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: cid,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.skills.entry(cid).or_default().insert(
            3455,
            TrainedSkillRecord {
                type_id: 3455,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let origin = DVec3::new(10_000_000.0, 0.0, 0.0);
    let far_dest = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.5,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(
            ship_ball.clone(),
            CombatState {
                capacitor: 250.0,
                max_capacitor: 250.0,
                gate_cloak_remaining_ticks: 10,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: shuttle_id,
                type_id: 672, // Caldari Shuttle (group 31 bubble immune)
                position: origin,
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                is_interdiction_nullified: true,
                ..Default::default()
            },
        )
        .await;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.ball_group_ids.insert(shuttle_id, 31);
    }

    // 1. check_warp_interdiction_and_adjust_dest: clear, destination bubble intercept, origin bubble scram, bubble-immune bypass
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest).unwrap(),
        far_dest
    );
    let mid_bubble = far_dest - DVec3::new(200_000.0, 0.0, 0.0);
    let _ = sys_handle.add_warp_bubble(970_100_001, mid_bubble, 20_000.0).await;
    let intercepted = check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest)
        .expect("intercepted inside path bubble");
    assert!(intercepted.x < far_dest.x);
    assert!((intercepted.distance(mid_bubble) - 20_000.0).abs() < 5_000.0);

    // Bubble-immune ship ignores both destination interception and origin scram bubble
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, shuttle_id, origin, far_dest).unwrap(),
        far_dest
    );

    let _ = sys_handle.add_warp_bubble(970_100_002, origin, 15_000.0).await;
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, shuttle_id, origin, far_dest).unwrap(),
        far_dest
    );
    let _ = sys_handle.remove_warp_bubble(970_100_002).await;

    // 2. execute_agency_content_warp: already in warp -> ShipAlreadyInWarp
    let mut cruising_ball = ship_ball.clone();
    cruising_ball.mode = MovementMode::new_warp(origin, far_dest, 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = cruising_ball.mode {
        *phase = WarpPhase::Cruising;
    }
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, cid, ship_id, sys_id, &cruising_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "ShipAlreadyInWarp"
    );

    // Low capacitor where affordable_dist_m < 150_000 -> NotEnoughCapacitor
    let _ = sys_handle.remove_warp_bubble(970_100_001).await;
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 0.000_001,
                max_capacitor: 400.0,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, cid, ship_id, sys_id, &ship_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // Partial cap short-warp (affordable >= 150_000 m, < requested) & gate cloak break + active_warps insertion
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 250.0,
                max_capacitor: 250.0,
                gate_cloak_remaining_ticks: 10,
                ..Default::default()
            },
        )
        .await;
    // Huge distance so 300-400 GJ capacitor yields short-warp (capacitor drained to 0.0)
    let huge_dest = origin + DVec3::new(100_000.0 * 149_597_870_700.0, 0.0, 0.0);
    execute_agency_content_warp(&ctx, &sys_handle, cid, ship_id, sys_id, &ship_ball, huge_dest)
        .await
        .expect("short warp succeeds with drained cap");
    let after_short = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(after_short.capacitor, 0.0);
    assert!(ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    let b_warping = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(b_warping.mode, MovementMode::Warp { .. }));

    // Full cap warp succeeds leaving remaining capacitor > 0 and calls on_warp_initiated_hooks
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 5_000.0,
                max_capacitor: 5_000.0,
                ..Default::default()
            },
        )
        .await;
    let near_dest = origin + DVec3::new(500_000.0, 0.0, 0.0);
    execute_agency_content_warp(&ctx, &sys_handle, cid, ship_id, sys_id, &ship_ball, near_dest)
        .await
        .expect("full cap warp succeeds");
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor > 4_900.0);
    on_warp_initiated_hooks(&ctx, &sys_handle, cid, ship_id, sys_id).await;

    // 3. handle_use_warp_vector edge validation, star-radius clamping, deadspace rollback (None + Some(old_cd)), and full consumption
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let mut sess = auth_session(1, cid, corp_id, sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: star_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let vec_iid = 970_200_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vec_iid,
            InventoryItemRecord {
                item_id: vec_iid,
                type_id: 77_825,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Hangar, // Wrong flag -> InvalidVectorItem
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    // Non-finite delta -> InvalidArgument
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.flag = InventoryFlag::Cargo;
    });
    ctx.db.corrupt_in_memory_snapshot_for_test(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.dynamic_attributes.insert(5429, f64::INFINITY);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    ctx.db.corrupt_in_memory_snapshot_for_test(|snap| {
        if let Some(it) = snap.items.get_mut(&vec_iid) {
            it.dynamic_attributes.remove(&5429);
        }
    });

    // Deadspace rollback with no prior cooldown removes cooldown entry and restores quantity
    let gated_pocket_site_pos = [star_pos.x, star_pos.y, star_pos.z];
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "GATED-TEST-DS".into(),
                solar_system_id: sys_id,
                total_rooms: 2,
                current_room: 2,
                cleared: false,
                position: gated_pocket_site_pos,
                ..Default::default()
            }],
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_iid).unwrap();
        it.dynamic_attributes.insert(5429, 500_000.0);
        it.dynamic_attributes.insert(5430, 0.0);
        it.dynamic_attributes.insert(5431, 0.0);
        snap.cluster_and_runtime.warp_vector_cooldown_until.remove(&cid);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "NaturalPhenomenaDisruptingWarp"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&vec_iid).map(|i| i.quantity), Some(1));
        assert!(!snap.cluster_and_runtime.warp_vector_cooldown_until.contains_key(&cid));
    });

    // Deadspace rollback when expired prior cooldown existed restores that exact timestamp!
    let expired_cd = clock.now_unix_secs() - 10;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime
            .warp_vector_cooldown_until
            .insert(cid, expired_cd);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
            .await
            .unwrap_err()
            .code,
        "NaturalPhenomenaDisruptingWarp"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.cluster_and_runtime.warp_vector_cooldown_until.get(&cid).copied(),
            Some(expired_cd)
        );
    });

    // Clear deadspace site, use huge delta (> sys.radius) -> clamped to system radius AND quantity == 0 deletes item
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.remove(&sys_id);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&vec_iid)
            .unwrap()
            .dynamic_attributes
            .insert(5429, 1e18);
    });
    let clamped_ok = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_iid)])
        .await
        .expect("clamped warp vector succeeds");
    assert_eq!(clamped_ok.dict_get("remainingQuantity"), Some(&PyRep::Integer(0)));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&vec_iid)));

    // 4. Session change timer expiry + solarsystemid check + unauthenticated rejection
    let mut unauth = eve_machonet::SessionState::default();
    assert!(handle_set_session_check(&ctx, &mut unauth, &[]).await.is_err());

    let mut sess_no_sys = auth_session(2, cid, corp_id, sys_id);
    sess_no_sys.solar_system_id = None;
    let bad_sys = PyRep::keyval([("solarsystemid", PyRep::Integer(30_000_144))]);
    assert_eq!(
        handle_set_session_check(&ctx, &mut sess_no_sys, &[bad_sys])
            .await
            .unwrap_err()
            .code,
        "SessionCheckMismatch"
    );
    let ok_trigger = handle_set_session_check(&ctx, &mut sess_no_sys, &[PyRep::String("wormhole_jump".into())])
        .await
        .unwrap();
    assert_eq!(ok_trigger.dict_get("trigger"), Some(&PyRep::String("wormhole_jump".into())));
    clock.advance_secs(11);
    let expired_state = handle_set_session_check(&ctx, &mut sess_no_sys, &[]).await.unwrap();
    assert_eq!(expired_state.dict_get("active"), Some(&PyRep::Bool(false)));
}

