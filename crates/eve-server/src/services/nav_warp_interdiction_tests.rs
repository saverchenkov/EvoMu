#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

#[tokio::test]
async fn nav_warp_interdiction_routes_waypoints_and_session_timer_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    assert!(!is_valid_waypoint_destination(&ctx, 0));
    assert!(!is_valid_waypoint_destination(&ctx, -5));
    assert!(is_valid_waypoint_destination(&ctx, 30000142));
    assert!(is_valid_waypoint_destination(&ctx, 60003760));
    assert_eq!(resolve_waypoint_solar_system(&ctx, 60003760), Some(30000142));

    // Sync waypoints from settings
    assert!(sync_waypoints_from_char_settings(
        &ctx,
        90_000_001,
        &PyRep::List(vec![PyRep::Integer(999_999_999)])
    )
    .is_err());

    sync_waypoints_from_char_settings(
        &ctx,
        90_000_001,
        &PyRep::List(vec![PyRep::Integer(30000142), PyRep::Integer(30000144)]),
    )
    .unwrap();
    sync_waypoints_on_login(&ctx, 90_000_001);
    advance_waypoints_on_arrival(&ctx, 90_000_001, 30000142);
    let rem_wps = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().autopilot_waypoints.clone());
    assert_eq!(rem_wps, vec![30000144]);
    sync_waypoints_to_char_settings(&ctx, 90_000_001, &[]);

    // Route calculation
    assert!(calculate_route_with_waypoints(&ctx, 999_999, &[], &[], "shortest").is_err());
    assert!(calculate_route_with_waypoints(&ctx, 30000142, &[999_999_999], &[], "shortest").is_err());
    assert_eq!(
        calculate_route_with_waypoints(&ctx, 30000142, &[], &[30000142], "shortest")
            .unwrap_err()
            .code,
        "OriginSystemInAvoidList"
    );
    assert_eq!(
        calculate_route_with_waypoints(&ctx, 30000142, &[], &[], "shortest").unwrap(),
        vec![30000142]
    );
    let route = calculate_route_with_waypoints(&ctx, 30000142, &[30000144], &[], "secure").unwrap();
    assert_eq!(route.first().copied(), Some(30000142));
    assert_eq!(route.last().copied(), Some(30000144));

    // Warp landing point resolution
    let ship_pos = DVec3::ZERO;
    let anchor_pos = DVec3::new(1_000_000.0, 0.0, 0.0);
    let station_landing = resolve_warp_landing_point(&ctx, 30000142, 60003760, ship_pos, anchor_pos);
    assert!(station_landing.x <= anchor_pos.x);

    // Ensure not in active warp
    assert!(ensure_not_in_active_warp(&MovementMode::Stop).is_ok());

    // handle_use_warp_vector while docked fails ShipDocked
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(100_000_101);
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // handle_set_session_check
    let timer_init = handle_set_session_check(&ctx, &mut sess, &[]).await.unwrap();
    assert_eq!(timer_init.dict_get("active"), Some(&PyRep::Bool(false)));

    assert!(handle_set_session_check(&ctx, &mut sess, &[PyRep::String("invalid_trigger".into())])
        .await
        .is_err());

    let set_timer = handle_set_session_check(&ctx, &mut sess, &[PyRep::String("dock".into())])
        .await
        .unwrap();
    assert_eq!(set_timer.dict_get("sessionChangeTimerSeconds"), Some(&PyRep::Integer(10)));
}

#[test]
fn check_warp_exit_approach_or_reject_enforces_phase_and_horizon() {
    use eve_destiny::{Ball, WarpPhase};

    let origin = DVec3::ZERO;
    let dest = DVec3::new(10_000_000.0, 0.0, 0.0);
    let make_ball = |pos: DVec3, mode: MovementMode| Ball {
        id: 100_001,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    };

    // Sub-warp and pre-warp Aligning are not in active warp -> Ok(false)
    let stopped = make_ball(origin, MovementMode::Stop);
    assert!(!check_warp_exit_approach_or_reject(&stopped, true).unwrap());
    assert!(!check_warp_exit_approach_or_reject(&stopped, false).unwrap());

    let aligning = make_ball(
        origin,
        MovementMode::Warp {
            origin,
            destination: dest,
            warp_speed_au_s: 3.0,
            total_distance: dest.x,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 1.0,
        },
    );
    assert!(!check_warp_exit_approach_or_reject(&aligning, true).unwrap());

    // Accelerating and Cruising always reject with ShipAlreadyInWarp
    for phase in [WarpPhase::Accelerating, WarpPhase::Cruising] {
        let mid_warp = make_ball(
            DVec3::new(5_000_000.0, 0.0, 0.0),
            MovementMode::Warp {
                origin,
                destination: dest,
                warp_speed_au_s: 3.0,
                total_distance: dest.x,
                phase,
                elapsed_warp_time: 5.0,
            },
        );
        assert_eq!(
            check_warp_exit_approach_or_reject(&mid_warp, true)
                .unwrap_err()
                .code,
            "ShipAlreadyInWarp"
        );
    }

    // Early Decelerating (> 4.0s remaining: 1_000_000 m at k_d=1.0 -> 18_315 m/s > 100 m/s) still rejects
    let early_decel = make_ball(
        dest - DVec3::new(1_000_000.0, 0.0, 0.0),
        MovementMode::Warp {
            origin,
            destination: dest,
            warp_speed_au_s: 3.0,
            total_distance: dest.x,
            phase: WarpPhase::Decelerating,
            elapsed_warp_time: 10.0,
        },
    );
    assert_eq!(
        check_warp_exit_approach_or_reject(&early_decel, true)
            .unwrap_err()
            .code,
        "ShipAlreadyInWarp"
    );

    // Final <= 4.0s Decelerating tail (300 m at k_d=1.0 -> 5.51 m/s < 100 m/s):
    // allow_warp_exit_noop=true -> Ok(true); allow_warp_exit_noop=false -> ShipAlreadyInWarp
    let tail_decel = make_ball(
        dest - DVec3::new(300.0, 0.0, 0.0),
        MovementMode::Warp {
            origin,
            destination: dest,
            warp_speed_au_s: 3.0,
            total_distance: dest.x,
            phase: WarpPhase::Decelerating,
            elapsed_warp_time: 18.0,
        },
    );
    assert!(check_warp_exit_approach_or_reject(&tail_decel, true).unwrap());
    assert_eq!(
        check_warp_exit_approach_or_reject(&tail_decel, false)
            .unwrap_err()
            .code,
        "ShipAlreadyInWarp"
    );
}

fn make_test_character(
    char_id: i64,
    name: &str,
    sys_id: u32,
    ship_id: i64,
) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        activity_status: None,
        character_id: char_id,
        user_id: char_id as u64,
        name: name.into(),
        corporation_id: 98_000_001,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id: sys_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: ship_id,
        balance: eve_db::IskCents(0),
        plex_balance: 0,
        loyalty_points: Default::default(),
        standings: Default::default(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn make_test_ball(
    ship_id: i64,
    pos: DVec3,
    max_velocity: f64,
    mode: MovementMode,
) -> eve_destiny::Ball {
    eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    }
}

fn arm_safe_logoff_and_location_invuln(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    crate::services::space_destiny::grant_location_change_invulnerability(
        ctx,
        char_id,
        ctx.now_unix() + 60,
    );
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            char_id,
            eve_db::SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: ctx.now_unix(),
                complete_at_unix: ctx.now_unix() + 30,
                completed: false,
            },
        );
    });
}

fn register_online_test_character(
    ctx: &ServerContext,
    char_id: i64,
    name: &str,
    sys_id: u32,
    ship_id: i64,
) {
    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("nav1384_{uid}"),
            eve_db::AccountRecord {
                user_id: uid,
                username: format!("nav1384_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters
            .insert(char_id, make_test_character(char_id, name, sys_id, ship_id));
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1_000, Some(char_id), true, ctx.now_unix())
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_subwarp_goto_point_failure_preserves_invuln_cloak_and_safe_logoff() {
    use eve_destiny::{CombatState, WarpPhase};

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_384_001_i64;
    let ship_id = 1_091_384_001_i64;
    let origin = DVec3::new(100_000.0, 0.0, 0.0);

    register_online_test_character(&ctx, char_id, "GotoFailPilot", sys_id, ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let stop_ball = make_test_ball(ship_id, origin, 300.0, MovementMode::Stop);
    let protected_cs = CombatState {
        is_cloaked: true,
        is_covert_cloak: false,
        gate_cloak_remaining_ticks: 30,
        undock_invulnerable_remaining_ticks: 20,
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..Default::default()
    };
    sys_handle
        .spawn_ball_with_combat(stop_ball.clone(), protected_cs.clone())
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Direct `Ballpark::goto_point` rejection (non-finite destination) must preserve all states.
    let nan_err = execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &stop_ball,
        DVec3::new(f64::NAN, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(nan_err.code, ERR_DESTINY_COMMAND_FAILED);

    let cs_after_nan = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_nan.is_cloaked);
    assert_eq!(cs_after_nan.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_after_nan.undock_invulnerable_remaining_ticks, 20);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let nan_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        nan_notifs
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );

    // 2. Live ball transitioned into active warp while caller holds stale `MovementMode::Stop` snapshot.
    let cruising_mode = MovementMode::Warp {
        origin,
        destination: origin + DVec3::new(10_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 10_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 5.0,
    };
    sys_handle
        .set_movement_mode(ship_id, cruising_mode)
        .await
        .unwrap();
    sys_handle
        .set_combat_state(ship_id, protected_cs)
        .await
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let warp_err = execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &stop_ball,
        origin + DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(warp_err.code, ERR_SHIP_ALREADY_IN_WARP);

    let cs_after_warp_err = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_warp_err.is_cloaked);
    assert_eq!(cs_after_warp_err.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_after_warp_err.undock_invulnerable_remaining_ticks, 20);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let warp_err_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        warp_err_notifs
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_initiation_failure_preserves_invuln_cloak_and_safe_logoff() {
    use eve_db::InventoryItemRecord;
    use eve_destiny::{CombatState, SlimItemMeta};

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_384_002_i64;
    let ship_id = 1_091_384_002_i64;
    let vec_id = 2_091_384_002_i64;

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    register_online_test_character(&ctx, char_id, "WarpFailPilot", sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut vec_item = InventoryItemRecord {
            item_id: vec_id,
            type_id: 77_825, // Jovian Symbolic Navigator (group 4599)
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 2,
            ..Default::default()
        };
        vec_item.dynamic_attributes.insert(5429, 5_000_000.0);
        vec_item.dynamic_attributes.insert(5430, 0.0);
        vec_item.dynamic_attributes.insert(5431, 0.0);
        vec_item.dynamic_attributes.insert(5432, 1e15);
        snap.items.insert(vec_id, vec_item);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // `SlimItemMeta { category_id: 22, .. }` causes `Ballpark::set_mode` to reject with
    // `StructureCannotNavigate`, exercising the exact post-validation `set_warp_movement_mode`
    // failure branch in both warp paths.
    let immobile_ball = make_test_ball(ship_id, star_pos, 300.0, MovementMode::Stop);
    sys_handle
        .spawn_ball_with_meta(
            immobile_ball.clone(),
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 30,
                undock_invulnerable_remaining_ticks: 25,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
            SlimItemMeta {
                category_id: 22,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. `execute_agency_content_warp` failure must preserve cloak, invuln, safe logoff, and capacitor.
    let agency_err = execute_agency_content_warp(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &immobile_ball,
        star_pos + DVec3::new(5_000_000.0, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(agency_err.code, ERR_DESTINY_COMMAND_FAILED);

    let cs_after_agency = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_agency.is_cloaked);
    assert_eq!(cs_after_agency.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_after_agency.undock_invulnerable_remaining_ticks, 25);
    assert!((cs_after_agency.capacitor - 1_000.0).abs() < 1e-6);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let agency_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        agency_notifs
            .iter()
            .filter(|n| {
                n.event == "OnInvulnCancelled"
                    || n.event == "OnJumpCloakCancelled"
                    || n.event == "OnCapacitorChange"
            })
            .count(),
        0
    );

    // 2. `handle_use_warp_vector` failure must roll back vector item + cooldown AND preserve all states.
    let mut sess = auth_session(char_id as u64, char_id, 98_000_001, sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let vec_err = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
        .await
        .unwrap_err();
    assert_eq!(vec_err.code, "WarpFailed");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&vec_id).map(|i| i.quantity), Some(2));
        assert!(!snap
            .cluster_and_runtime
            .warp_vector_cooldown_until
            .contains_key(&char_id));
        assert!(snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id));
    });
    let cs_after_vec = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_vec.is_cloaked);
    assert_eq!(cs_after_vec.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_after_vec.undock_invulnerable_remaining_ticks, 25);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    let vec_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        vec_notifs
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_subwarp_goto_point_uses_post_sync_max_velocity() {
    use eve_db::InventoryItemRecord;
    use eve_destiny::CombatState;

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_384_003_i64;
    let ship_id = 1_091_384_003_i64;
    let siege_mod_id = 2_091_384_003_i64;
    let origin = DVec3::new(200_000.0, 0.0, 0.0);

    let siege_attr = crate::services::space_navigation_advanced::find_attr_id_by_name(
        &ctx.sde,
        "siegeModeWarpStatus",
    )
    .expect("SDE must define siegeModeWarpStatus");
    let siege_mod_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| t.default_attributes.get(&siege_attr).copied().unwrap_or(0.0) != 0.0)
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .expect("SDE must contain a siege/bastion immobilizing module type");

    register_online_test_character(&ctx, char_id, "PostSyncVelPilot", sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siege_mod_id,
            InventoryItemRecord {
                item_id: siege_mod_id,
                type_id: siege_mod_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(siege_mod_id);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Live ball has `max_velocity = 0.0` (immobilized), while caller passes stale `max_velocity = 300.0`.
    let live_immobile = make_test_ball(ship_id, origin, 0.0, MovementMode::Stop);
    let stale_mobile = make_test_ball(ship_id, origin, 300.0, MovementMode::Stop);
    sys_handle
        .spawn_ball_with_combat(
            live_immobile.clone(),
            CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 25,
                undock_invulnerable_remaining_ticks: 15,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let err = execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &stale_mobile,
        origin + DVec3::new(30_000.0, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ERR_CANT_MOVE_IMMOBILIZED);

    let cs_immob = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_immob.is_cloaked);
    assert_eq!(cs_immob.gate_cloak_remaining_ticks, 25);
    assert_eq!(cs_immob.undock_invulnerable_remaining_ticks, 15);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert_eq!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );

    // Conversely, when the live post-sync ball has restored `max_velocity = 300.0`,
    // passing a stale `max_velocity = 0.0` snapshot must succeed using refreshed velocity.
    assert!(sys_handle.set_ball_max_velocity(ship_id, 300.0).await.unwrap());
    execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &live_immobile,
        origin + DVec3::new(30_000.0, 0.0, 0.0),
    )
    .await
    .unwrap();
    let (_, post_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(post_ball.mode, MovementMode::GotoDirection { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn movement_and_warp_commit_deduplicates_on_invuln_cancelled() {
    use eve_db::InventoryItemRecord;
    use eve_destiny::CombatState;

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_384_004_i64;
    let ship_id = 1_091_384_004_i64;
    let cloak_mod_id = 2_091_384_004_i64;
    let vec_id = 2_091_384_005_i64;

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    register_online_test_character(&ctx, char_id, "DedupInvulnPilot", sys_id, ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let start_ball = make_test_ball(ship_id, star_pos, 300.0, MovementMode::Stop);
    sys_handle
        .spawn_ball_with_combat(
            start_ball.clone(),
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 30,
                undock_invulnerable_remaining_ticks: 25,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Subwarp `execute_subwarp_goto_point` (gate cloak + undock invuln + location_change_invuln).
    execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &start_ball,
        star_pos + DVec3::new(25_000.0, 0.0, 0.0),
    )
    .await
    .unwrap();
    let cs_1 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_1.is_cloaked);
    assert_eq!(cs_1.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_1.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(!ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let notifs_1 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        notifs_1
            .iter()
            .filter(|n| n.event == "OnJumpCloakCancelled")
            .count(),
        1
    );
    assert_eq!(
        notifs_1
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled")
            .count(),
        1
    );

    // 2. Covert-cloaked `execute_subwarp_goto_point` (both invulns active, covert cloak preserved).
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11_578, // Covert Ops Cloaking Device II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .unwrap();
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: true,
                gate_cloak_remaining_ticks: 0,
                undock_invulnerable_remaining_ticks: 25,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    execute_subwarp_goto_point(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &start_ball,
        star_pos + DVec3::new(35_000.0, 0.0, 0.0),
    )
    .await
    .unwrap();
    let cs_2 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_2.is_cloaked && cs_2.is_covert_cloak);
    assert_eq!(cs_2.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    let notifs_2 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        notifs_2
            .iter()
            .filter(|n| n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );
    assert_eq!(
        notifs_2
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled")
            .count(),
        1
    );

    // 3. Covert-cloaked `execute_agency_content_warp` (both invulns active, covert cloak preserved).
    sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .unwrap();
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: true,
                gate_cloak_remaining_ticks: 0,
                undock_invulnerable_remaining_ticks: 25,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    execute_agency_content_warp(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &start_ball,
        star_pos + DVec3::new(5_000_000.0, 0.0, 0.0),
    )
    .await
    .unwrap();
    let cs_3 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_3.is_cloaked && cs_3.is_covert_cloak);
    assert_eq!(cs_3.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(!ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let notifs_3 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        notifs_3
            .iter()
            .filter(|n| n.event == "OnJumpCloakCancelled")
            .count(),
        0
    );
    assert_eq!(
        notifs_3
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled")
            .count(),
        1
    );

    // 4. `handle_use_warp_vector` (gate cloak + undock invuln + location_change_invuln -> 1 each).
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
        let mut vec_item = InventoryItemRecord {
            item_id: vec_id,
            type_id: 77_825,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        };
        vec_item.dynamic_attributes.insert(5429, 5_000_000.0);
        vec_item.dynamic_attributes.insert(5430, 0.0);
        vec_item.dynamic_attributes.insert(5431, 0.0);
        vec_item.dynamic_attributes.insert(5432, 1e15);
        snap.items.insert(vec_id, vec_item);
    });
    sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .unwrap();
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 30,
                undock_invulnerable_remaining_ticks: 25,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    arm_safe_logoff_and_location_invuln(&ctx, char_id, ship_id, sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut sess = auth_session(char_id as u64, char_id, 98_000_001, sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let vec_ok = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
        .await
        .unwrap();
    assert_eq!(vec_ok.dict_get("consumed"), Some(&PyRep::Bool(true)));
    let cs_4 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_4.is_cloaked);
    assert_eq!(cs_4.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_4.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(
        &ctx,
        char_id,
        ctx.now_unix(),
    ));
    assert!(!ctx
        .db
        .with_snapshot(|snap| snap.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let notifs_4 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        notifs_4
            .iter()
            .filter(|n| n.event == "OnJumpCloakCancelled")
            .count(),
        1
    );
    assert_eq!(
        notifs_4
            .iter()
            .filter(|n| n.event == "OnInvulnCancelled")
            .count(),
        1
    );
}
