#![allow(unused_imports)]
//! Semantic unit tests split from `abyssal_void_gates_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;

use crate::services::abyssal_void_gates::*;
use crate::services::abyssal_void_mgr::{ABYSSAL_SPACE_SYSTEM_ID, VOID_SPACE_SYSTEM_ID};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::pve_abyssal_hazards::ABYSSAL_TRACE_TYPE_ID;
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord,
    AbyssalWeatherKind, VoidSpaceSessionRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test]
async fn abyssal_end_gate_validation_suspect_flag_and_void_space_error_branches() {
    let (mut ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    let pid = 8_i64;
    let abyss_sys = ABYSSAL_SPACE_SYSTEM_ID;
    let r3_gate = 1_895_000_000 + pid * 10 + 3;
    let npc_id = 920_001_i64;
    let now = ctx.now_unix();

    // Docked check on end gate
    session.solar_system_id = Some(abyss_sys);
    session.ship_id = Some(ship_id);
    session.station_id = Some(60_003_760);
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    session.station_id = None;

    // NotInAbyssalSpace when no pocket
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );

    // NotInFinalRoom when in room 2
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            make_pocket(pid, char_id, ship_id, origin_sys, abyss_sys, 2, now + 600, 5),
        );
    });
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "NotInFinalRoom"
    );

    // Move to room 3, test InvalidGate, GateLocked, GateOutOfRange, and high-sec tier 5 suspect flag
    with_abyssal_state(&ctx, |st| {
        let p = st.pockets.get_mut(&char_id).unwrap();
        p.current_room = 3;
        p.rooms[2].npc_ball_ids = vec![npc_id];
    });
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(123)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );

    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_handle
        .spawn_ball(Ball {
            id: npc_id,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "GateLocked"
    );

    abyss_handle.try_remove_ball(npc_id);
    let _ = abyss_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = abyss_handle
        .spawn_ball(Ball {
            id: r3_gate,
            type_id: 47_686,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e7,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "GateOutOfRange"
    );

    let _ = abyss_handle.set_ball_position(r3_gate, DVec3::ZERO).await;
    if let Some(sys) = std::sync::Arc::make_mut(&mut ctx.sde).solar_systems.get_mut(&origin_sys) {
        sys.security_status = 0.80;
    }
    handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
        .await
        .expect("end gate activation should succeed");
    assert!(ctx.crimewatch.read().unwrap().is_suspect(char_id, ctx.now_unix()));

    // Clear suspect flag so Void Space jump is allowed
    ctx.crimewatch.write().unwrap().states.remove(&char_id);

    // Void Space jump error branches: docked, missing arg, cloaked, tethered, self-target, invalid gate type, out-of-range gate, valid in-space entrance gate
    session.station_id = Some(60_003_760);
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    session.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
    });
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_far = DVec3::splat(500_000_000.0);
    let _ = origin_handle.set_ball_position(ship_id, ship_far).await;
    let _ = origin_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(777)])
            .await
            .unwrap_err()
            .code,
        "ShipIsCloaked"
    );

    let _ = origin_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_tethered: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(777)])
            .await
            .unwrap_err()
            .code,
        "ShipIsTethered"
    );
    let _ = origin_handle
        .set_combat_state(ship_id, CombatState::default())
        .await;

    // Self-target -> InvalidTarget
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    // Abyssal trace ball in space cannot be used as Void Space gate -> InvalidTarget
    let trace_gate_id = 930_001_i64;
    let _ = origin_handle
        .spawn_ball(Ball {
            id: trace_gate_id,
            type_id: ABYSSAL_TRACE_TYPE_ID,
            position: ship_far,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(trace_gate_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    origin_handle.try_remove_ball(trace_gate_id);

    // Valid entrance gate type 60_770 at > 5000m -> OutOfRange, then within 5000m -> succeeds
    if let Some(t) = std::sync::Arc::make_mut(&mut ctx.sde).types.get_mut(&60_770) {
        t.category_id = 2;
        t.group_id = 4000;
    }
    let valid_gate_id = 930_002_i64;
    let _ = origin_handle
        .spawn_ball(Ball {
            id: valid_gate_id,
            type_id: 60_770,
            position: ship_far + DVec3::new(6_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(valid_gate_id)])
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    let _ = origin_handle
        .set_ball_position(valid_gate_id, ship_far + DVec3::new(4_000.0, 0.0, 0.0))
        .await;
    handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(valid_gate_id)])
        .await
        .expect("void space jump via in-space entrance gate should succeed");

    // Void Space end gate error branches: invalid gate_id <= 0, bad gate type, out-of-range, expired session
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    let bad_exit_id = 888_099_i64;
    let _ = void_handle
        .spawn_ball(Ball {
            id: bad_exit_id,
            type_id: 621, // not an exit gate
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(bad_exit_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );

    // Expire void session -> PocketCollapsed
    with_abyssal_state(&ctx, |st| {
        if let Some(vs) = st.void_space_sessions.get_mut(&char_id) {
            vs.expires_at_sec = now.saturating_sub(1);
        }
    });
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "PocketCollapsed"
    );
    let _ = (make_fighter_type, make_fighter_item, VoidSpaceSessionRecord::clone, &mut ctx);
}
