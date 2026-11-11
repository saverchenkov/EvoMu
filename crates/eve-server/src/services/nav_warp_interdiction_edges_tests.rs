//! Edge-case behavioral unit tests for `nav_warp_interdiction`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, ServerClock,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::sync::Arc;

fn make_char(cid: i64, name: &str, sys_id: u32, ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id: cid,
        user_id: cid as u64,
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

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode,
    }
}

#[test]
fn waypoint_sync_and_landing_point_edge_cases() {
    let ctx = test_ctx();
    let char_id = 90_400_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, "WaypointEdge", sys_id, 0));
        snap.structures.insert(
            1_040_000_000_777,
            UpwellStructureRecord {
                structure_id: 1_040_000_000_777,
                type_id: 35832, // Astrahus
                name: "TestAstra".into(),
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                ..UpwellStructureRecord::default()
            },
        );
    });

    // Empty sync clears waypoints key when previously present
    sync_waypoints_to_char_settings(&ctx, char_id, &[30_000_142]);
    assert!(with_misc_state(&ctx, |st| {
        st.char_settings
            .get(&char_id)
            .and_then(|m| m.get("waypoints"))
            .is_some()
    }));
    sync_waypoints_to_char_settings(&ctx, char_id, &[]);
    assert!(with_misc_state(&ctx, |st| {
        st.char_settings
            .get(&char_id)
            .and_then(|m| m.get("waypoints"))
            .is_none()
    }));

    // Invalid entry inside List/Tuple returns InvalidDestination
    assert_eq!(
        sync_waypoints_from_char_settings(
            &ctx,
            char_id,
            &PyRep::Tuple(vec![PyRep::Integer(999_999_999)]),
        )
        .unwrap_err()
        .code,
        "InvalidDestination"
    );

    // Login sync with Tuple in char_settings filters invalid entries
    with_misc_state(&ctx, |st| {
        st.char_settings.entry(char_id).or_default().insert(
            "waypoints".into(),
            PyRep::Tuple(vec![PyRep::Integer(999_999_999), PyRep::Integer(30_000_144)]),
        );
    });
    sync_waypoints_on_login(&ctx, char_id);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).unwrap().autopilot_waypoints.clone()),
        vec![30_000_144]
    );

    // Non-matching arrival & missing character arrival are no-ops
    advance_waypoints_on_arrival(&ctx, char_id, 30_000_142);
    advance_waypoints_on_arrival(&ctx, 99_999_888, 30_000_144);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).unwrap().autopilot_waypoints.clone()),
        vec![30_000_144]
    );

    // Non-list settings on login with empty db waypoints does nothing
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().autopilot_waypoints.clear();
    });
    with_misc_state(&ctx, |st| {
        st.char_settings
            .entry(char_id)
            .or_default()
            .insert("waypoints".into(), PyRep::Integer(123));
    });
    sync_waypoints_on_login(&ctx, char_id);
    assert!(ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().autopilot_waypoints.is_empty()));

    // resolve_warp_landing_point: zero-vector when ship_pos == anchor_pos
    let anchor = DVec3::new(100_000.0, 200_000.0, 0.0);
    assert_eq!(
        resolve_warp_landing_point(&ctx, sys_id, 30_000_142, anchor, anchor),
        anchor
    );
    // Upwell structure standoff along approach vector
    let struct_radius = ctx.sde.types.get(&35832).map(|t| t.radius).unwrap_or(0.0);
    let lp_struct = resolve_warp_landing_point(
        &ctx,
        sys_id,
        1_040_000_000_777,
        DVec3::ZERO,
        DVec3::new(1_000_000.0, 0.0, 0.0),
    );
    assert!((lp_struct.x - (1_000_000.0 - struct_radius)).abs() < 1e-3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interdiction_bubbles_and_execute_agency_content_warp_paths() {
    let ctx = test_ctx();
    let char_id = 90_400_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_000_400_001_i64;

    // Find a ship type in SDE whose max_capacitor (attr 482) > 250.0 and warp_cap_need (attr 153) > 0.0
    let (cap_ship_tid, sde_max_cap) = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.category_id == 6
                && t.default_attributes.get(&482).copied().unwrap_or(0.0) > 600.0
                && t.default_attributes.get(&153).copied().unwrap_or(0.0) > 1e-8
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, t)| (tid, t.default_attributes[&482]))
        .expect("SDE ship with max capacitor > 600");

    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("nav_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("nav_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(char_id, make_char(char_id, "AgencyWarper", sys_id, ship_id));
        snap.skills.entry(char_id).or_default().insert(
            3455, // Warp Drive Operation lvl 5
            TrainedSkillRecord {
                type_id: 3455,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1000, Some(char_id), true, ctx.now_unix())
        .unwrap();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let origin = DVec3::ZERO;
    let far_dest = DVec3::new(15_000_000_000.0, 0.0, 0.0); // 15M km (~0.1 AU)
    let start_ball = make_ship_ball(ship_id, cap_ship_tid, origin, MovementMode::Stop);
    sys_handle
        .spawn_ball_with_combat(
            start_ball.clone(),
            CombatState {
                capacitor: 250.0,
                max_capacitor: 250.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 1. validate_pre_warp_common fails in abyssal space
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, 32_000_001, &start_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "CannotWarpInAbyssalSpace"
    );

    // 2. ShipAlreadyInWarp when ball mode is Cruise
    let cruise_ball = make_ship_ball(
        ship_id,
        cap_ship_tid,
        origin,
        MovementMode::Warp {
            origin,
            destination: far_dest,
            warp_speed_au_s: 3.0,
            total_distance: (far_dest - origin).length(),
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 10.0,
        },
    );
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &cruise_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "ShipAlreadyInWarp"
    );

    // 3. WarpScrambled
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                warp_scramble_strength: 2,
                warp_core_strength: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &start_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    // 4. CloakedCannotWarp (non-covert cloak fitted + active in HiSlot0)
    let is_covert_cloak_type = |effect_ids: &[u16]| {
        effect_ids.iter().any(|&eid| {
            ctx.sde
                .effects
                .get(&eid)
                .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
        })
    };
    let non_covert_cloak_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| t.group_id == 330 && !is_covert_cloak_type(&t.effect_ids))
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_577);
    let covert_cloak_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| t.group_id == 330 && is_covert_cloak_type(&t.effect_ids))
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_578);
    let cloak_mod_id = 2_000_400_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: non_covert_cloak_tid,
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
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &start_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // 5. NotEnoughCapacitor (< 150 km affordable)
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 0.0,
                max_capacitor: sde_max_cap,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &start_ball, far_dest)
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // 6. check_warp_interdiction_and_adjust_dest: catch bubble within 500km of destination -> adjusted intercept_pos
    let bubble_id = 777_001_i64;
    sys_handle
        .add_warp_bubble(bubble_id, far_dest - DVec3::new(100_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    let adjusted = check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest).unwrap();
    assert_ne!(adjusted, far_dest);
    assert!(adjusted.x < far_dest.x);

    // Origin bubble -> WarpScrambled; bubble immune -> returns raw dest_pos
    let origin_bubble = 777_002_i64;
    sys_handle.add_warp_bubble(origin_bubble, origin, 20_000.0).await.unwrap();
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        assert!(bp.set_interdiction_nullified(ship_id, true));
    }
    assert_eq!(
        check_warp_interdiction_and_adjust_dest(&sys_handle, ship_id, origin, far_dest).unwrap(),
        far_dest
    );
    let _ = sys_handle.remove_warp_bubble(origin_bubble).await;
    let _ = sys_handle.remove_warp_bubble(bubble_id).await;

    // 7. Partial capacitor warp shortening (>= 150 km, < requested) + 250.0 hydration + covert cloak retained
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: covert_cloak_tid,
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
    // Use 250.0/250.0 initial hydration across huge distance 1e16 m so cap < req_cap OR set partial cap:
    let super_far = DVec3::new(1e15, 0.0, 0.0);
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 250.0,
                max_capacitor: 250.0,
                is_cloaked: true,
                is_covert_cloak: true,
                gate_cloak_remaining_ticks: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = ctx.db.acquire_online_lock(1001, 1, Some(char_id), false, ctx.now_unix());
    execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &start_ball, super_far)
        .await
        .unwrap();
    let cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after.capacitor, 0.0);
    assert!(cs_after.is_cloaked);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnCapacitorChange"));
    assert!(!notifs.iter().any(|n| n.event == "OnWarpStarted"));
    let warped_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(warped_ball.mode, MovementMode::Warp { .. }));
    assert_eq!(
        ctx.active_warps.lock().unwrap().get(&ship_id).copied(),
        Some(char_id)
    );

    // 8. Full capacitor normal warp (200 km) deducts < max_cap and retains remainder
    let short_dest = DVec3::new(200_000.0, 0.0, 0.0);
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: sde_max_cap,
                max_capacitor: sde_max_cap,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    execute_agency_content_warp(&ctx, &sys_handle, char_id, ship_id, sys_id, &start_ball, short_dest)
        .await
        .unwrap();
    let cs_full = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_full.capacitor > 0.0 && cs_full.capacitor < sde_max_cap);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn use_warp_vector_rollback_and_session_check_edges() {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(test_ctx().sde.clone(), db);
    ctx.set_clock(clock.clone());

    let char_id = 90_400_003_i64;
    let ship_id = 1_000_400_003_i64;
    // Find a K-space nullsec system (sec <= 0.0 and < 31_000_000) so ESS check runs
    let null_sys = *ctx
        .sde
        .solar_systems
        .iter()
        .find(|(&id, s)| id < 31_000_000 && s.security_status <= 0.0)
        .map(|(id, _)| id)
        .expect("SDE must have a nullsec k-space system");

    // 1. handle_use_warp_vector early guards
    let mut unauth = SessionState::default();
    assert!(handle_use_warp_vector(&ctx, &mut unauth, &[]).await.is_err());
    assert!(handle_set_session_check(&ctx, &mut unauth, &[]).await.is_err());
    assert!(handle_set_session_check(&ctx, &mut unauth, &[PyRep::String("undock".into())])
        .await
        .is_err());

    let mut sess = auth_session(1, char_id, 98_000_001, null_sys);
    sess.ship_id = Some(ship_id);
    sess.structure_id = Some(1_040_000_000_111);
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.structure_id = None;
    sess.solar_system_id = None;
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Abyssal system rejected by validate_pre_warp_common
    sess.solar_system_id = Some(32_000_005);
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(999)])
            .await
            .unwrap_err()
            .code,
        "CannotWarpInAbyssalSpace"
    );

    // Restore nullsec sys, spawn ship + ESS beacon
    sess.solar_system_id = Some(null_sys);
    let star_pos = ctx
        .sde
        .celestials_in_system(null_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 587, star_pos, MovementMode::Stop),
            CombatState {
                warp_scramble_strength: 2,
                warp_core_strength: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // WarpScrambled guard
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(999)])
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys_handle
        .set_combat_state(ship_id, CombatState::default())
        .await
        .unwrap();

    // Wrong cargo ownership/qty == 0 -> InvalidVectorItem; non-finite delta -> InvalidArgument
    let vec_id = 960_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, "VecTester", null_sys, ship_id));
        snap.items.insert(
            vec_id,
            InventoryItemRecord {
                item_id: vec_id,
                type_id: 77_825,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 0,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_id).unwrap();
        it.quantity = 1;
        it.dynamic_attributes.insert(5429, f64::NAN);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // Spawn ESS beacon at star_pos + (400_000, 0, 0) and point vector right onto it ->
    // EssIntrusionBubbleWarpBlocked triggers rollback_warp_vector_reservation!
    let ess_pos = star_pos + DVec3::new(400_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball(make_ship_ball(820_000_000 + i64::from(null_sys), 32882, ess_pos, MovementMode::Stop))
        .await
        .unwrap();
    let old_cd = clock.now_unix_secs().saturating_sub(5);
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_id).unwrap();
        it.dynamic_attributes.insert(5429, 400_000.0);
        it.dynamic_attributes.insert(5430, 0.0);
        it.dynamic_attributes.insert(5431, 0.0);
        it.dynamic_attributes.insert(5432, 1e15);
        snap.cluster_and_runtime.warp_vector_cooldown_until = HashMap::from([(char_id, old_cd)]);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    // Verify item quantity=1 and old_cd were restored by rollback_warp_vector_reservation
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&vec_id).unwrap().quantity, 1);
        let cds = &snap.cluster_and_runtime.warp_vector_cooldown_until;
        assert_eq!(cds.get(&char_id).copied(), Some(old_cd));
    });

    // Clear ESS rollback target delta far from ESS AND huge (> sys_radius) so sys_radius clamps it, qty 1->0 removes item!
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&vec_id).unwrap();
        it.dynamic_attributes.insert(5429, 0.0);
        it.dynamic_attributes.insert(5430, 1e20);
        snap.cluster_and_runtime.warp_vector_cooldown_until.clear();
    });
    let ok_res = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
        .await
        .unwrap();
    assert_eq!(ok_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(0)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&vec_id)));

    // handle_set_session_check edges: ship_id == None, solarsystemid field, invalid trigger
    sess.ship_id = None;
    let dict_sys = PyRep::keyval([("solarsystemid", PyRep::Integer(i64::from(null_sys)))]);
    assert_eq!(
        handle_set_session_check(&ctx, &mut sess, &[PyRep::String("not_a_trigger".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidSessionChangeTrigger"
    );
    let ok_timer = handle_set_session_check(&ctx, &mut sess, &[dict_sys])
        .await
        .unwrap();
    assert_eq!(ok_timer.dict_get("sessionChangeTimerSeconds"), Some(&PyRep::Integer(10)));
}

#[tokio::test]
async fn use_warp_vector_gate_cloak_vs_covert_cloak_rollback_none_and_agency_warp_mutants() {
    let ctx = test_ctx();
    let char_id = 90_400_555_i64;
    let ship_id = 1_000_400_555_i64;
    let null_sys = *ctx
        .sde
        .solar_systems
        .iter()
        .find(|(&id, s)| id < 31_000_000 && s.security_status <= 0.0)
        .map(|(id, _)| id)
        .expect("SDE must have a nullsec k-space system");

    let mut sess = auth_session(1, char_id, 98_000_001, null_sys);
    sess.ship_id = Some(ship_id);
    let star_pos = ctx
        .sde
        .celestials_in_system(null_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let gate_cloak = CombatState {
        is_cloaked: true,
        is_covert_cloak: false,
        gate_cloak_remaining_ticks: 30,
        ..Default::default()
    };
    sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 587, star_pos, MovementMode::Stop),
            gate_cloak.clone(),
        )
        .await
        .unwrap();

    // Verify validate_pre_warp_common & execute_agency_content_warp return Err when scrambled
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                warp_scramble_strength: 2,
                warp_core_strength: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id: null_sys },
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    let ball_snap = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(
        execute_agency_content_warp(
            &ctx,
            &sys_handle,
            char_id,
            ship_id,
            null_sys,
            &ball_snap,
            star_pos + DVec3::new(1_000_000.0, 0.0, 0.0),
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );

    // Clear scramble and test gate cloak -> MUST break cloak on use_warp_vector (`!validated.is_covert_cloaked`)
    sys_handle
        .set_combat_state(ship_id, gate_cloak)
        .await
        .unwrap();

    // ESS bubble triggers rollback_warp_vector_reservation when prev_cd_until was None
    let ess_pos = star_pos + DVec3::new(400_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball(make_ship_ball(
            820_100_000 + i64::from(null_sys),
            32882,
            ess_pos,
            MovementMode::Stop,
        ))
        .await
        .unwrap();

    let vec_id = 960_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(char_id, make_char(char_id, "CloakVecTester", null_sys, ship_id));
        let mut vec_item = InventoryItemRecord {
            item_id: vec_id,
            type_id: 77_825,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 2,
            ..Default::default()
        };
        vec_item.dynamic_attributes.insert(5429, 400_000.0);
        vec_item.dynamic_attributes.insert(5430, 0.0);
        vec_item.dynamic_attributes.insert(5431, 0.0);
        vec_item.dynamic_attributes.insert(5432, 1e15);
        snap.items.insert(vec_id, vec_item);
        snap.cluster_and_runtime.warp_vector_cooldown_until.remove(&char_id);
    });
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&vec_id].quantity, 2);
        assert!(!snap
            .cluster_and_runtime
            .warp_vector_cooldown_until
            .contains_key(&char_id));
    });

    // Clear ESS block with distant target -> succeeds and BREAKS gate cloak (`!validated.is_covert_cloaked`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&vec_id)
            .unwrap()
            .dynamic_attributes
            .insert(5429, 5_000_000.0);
    });
    let res1 = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
        .await
        .unwrap();
    assert_eq!(res1.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let cs_after_gate = sys_handle.ballpark.read().unwrap().combat_states[&ship_id].clone();
    assert!(!cs_after_gate.is_cloaked);
    assert_eq!(cs_after_gate.gate_cloak_remaining_ticks, 0);

    // Covert-cloaked ship keeps cloak across handle_use_warp_vector
    sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.warp_vector_cooldown_until.remove(&char_id);
    });
    let res2 = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(vec_id)])
        .await
        .unwrap();
    assert_eq!(res2.dict_get("remainingQuantity"), Some(&PyRep::Integer(0)));
    let cs_after_cov = sys_handle.ballpark.read().unwrap().combat_states[&ship_id].clone();
    assert!(cs_after_cov.is_cloaked);
}
