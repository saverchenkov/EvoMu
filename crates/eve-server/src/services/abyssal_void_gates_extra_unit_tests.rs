//! Deep branch-coverage and mutation-killing unit tests for `abyssal_void_gates`.

#![forbid(unsafe_code)]

use crate::services::abyssal_void_gates::*;
use crate::services::abyssal_void_mgr::{ABYSSAL_SPACE_SYSTEM_ID, VOID_SPACE_SYSTEM_ID};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord,
    AbyssalWeatherKind, VoidSpaceSessionRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[cfg(test)]
#[path = "abyssal_void_end_gate_unit_tests.rs"]
mod abyssal_void_end_gate_unit_tests;

pub(super) fn make_pocket(
    pid: i64,
    char_id: i64,
    ship_id: i64,
    origin_sys: u32,
    abyss_sys: u32,
    curr_room: u8,
    collapse_at_sec: u64,
    tier: u8,
) -> AbyssalPocketSession {
    AbyssalPocketSession {
        pocket_id: pid,
        character_id: char_id,
        ship_id,
        origin_solar_system_id: origin_sys,
        abyssal_solar_system_id: abyss_sys,
        origin_position: [1_000.0, 2_000.0, 3_000.0],
        grid_offset: [0.0, 0.0, 0.0],
        filament_type_id: 47_762,
        tier,
        weather: AbyssalWeatherKind::Electrical,
        weather_penalty_pct: -30.0,
        weather_bonus_pct: 50.0,
        npc_difficulty_mult: 1.0,
        current_room: curr_room,
        rooms: [
            AbyssalRoomRecord {
                room_index: 1,
                dungeon_id: 1,
                dungeon_name: "R1".into(),
                npc_ball_ids: Vec::new(),
                cache_item_id: 1_896_000_000 + pid * 10 + 1,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
            AbyssalRoomRecord {
                room_index: 2,
                dungeon_id: 2,
                dungeon_name: "R2".into(),
                npc_ball_ids: Vec::new(),
                cache_item_id: 1_896_000_000 + pid * 10 + 2,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
            AbyssalRoomRecord {
                room_index: 3,
                dungeon_id: 3,
                dungeon_name: "R3".into(),
                npc_ball_ids: Vec::new(),
                cache_item_id: 1_896_000_000 + pid * 10 + 3,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
        ],
        started_at_sec: 1_000,
        collapse_at_sec,
        completed: false,
        collapsed_destroyed: false,
        trace_ball_id: 888_999,
        fleet_ship_ids: vec![ship_id],
        ship_room: HashMap::from([(ship_id, curr_room)]),
        max_ships: 1,
    }
}

#[tokio::test]
async fn abyssal_gate_activation_error_branches_coop_retention_and_collapse() {
    let (ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let pid = 7_i64;
    let abyss_sys = ABYSSAL_SPACE_SYSTEM_ID;
    let r1_gate = 1_895_000_000 + pid * 10 + 1;
    let alt_r1_gate = 1_895_000_001_i64;
    let npc_id = 910_001_i64;

    // 1. Missing solar_system_id / ship_id
    session.solar_system_id = None;
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    session.solar_system_id = Some(abyss_sys);
    session.ship_id = None;
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    session.ship_id = Some(ship_id);

    // 2. No active pocket -> NotInAbyssalSpace
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );

    // 3. Already in final room (room 3) -> AlreadyInFinalRoom
    let now = ctx.now_unix();
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            make_pocket(pid, char_id, ship_id, origin_sys, abyss_sys, 3, now + 600, 1),
        );
    });
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "AlreadyInFinalRoom"
    );

    // 4. Invalid gate ID -> InvalidGate
    with_abyssal_state(&ctx, |st| {
        let p = st.pockets.get_mut(&char_id).unwrap();
        p.current_room = 1;
        p.rooms[0].npc_ball_ids = vec![npc_id];
    });
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(0)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(999)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );

    // 5. Alive NPC in room -> GateLocked
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
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "GateLocked"
    );

    // 6. Remove NPC ball -> GateNotFound when ship/gate balls are absent
    abyss_handle.try_remove_ball(npc_id);
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kw)
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );

    // 7. Spawn ship ball and alt_r1_gate far apart (> 2500m surface dist) -> GateOutOfRange
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
            id: alt_r1_gate,
            type_id: 47_685,
            position: DVec3::new(3_000.0, 0.0, 0.0), // 3000 - 100 = 2900m > 2500m
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
        handle_abyssal_gate_activation(
            &ctx,
            &mut session,
            char_id,
            &[PyRep::Integer(alt_r1_gate)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "GateOutOfRange"
    );

    // 8. Move alt_r1_gate to exact 2500.0m surface dist (2600.0m center-to-center) and add unentered co-op trace slot
    let _ = abyss_handle
        .set_ball_position(alt_r1_gate, DVec3::new(2_600.0, 0.0, 0.0))
        .await;
    with_abyssal_state(&ctx, |st| {
        st.traces.insert(
            888_999,
            AbyssalTraceRecord {
                trace_ball_id: 888_999,
                char_id,
                filament_type_id: 47_762,
                origin_solar_system_id: origin_sys,
                position: [0.0, 0.0, 0.0],
                created_unix: now,
                expires_at_sec: now + 600,
                max_ships: 3,
                entered_char_ids: vec![char_id],
                pocket_id: Some(pid),
                fleet_id: None,
            },
        );
    });
    let ok_res = handle_abyssal_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(alt_r1_gate)],
        &kw,
    )
    .await
    .expect("exact 2500m boundary with alt gate and coop retention should succeed");
    assert_eq!(ok_res.dict_get("room").and_then(|v| v.as_i64()), Some(2));

    // 9. Expire pocket -> PocketCollapsed
    with_abyssal_state(&ctx, |st| {
        st.pockets.get_mut(&char_id).unwrap().collapse_at_sec = now.saturating_sub(1);
    });
    assert_eq!(
        handle_abyssal_gate_activation(
            &ctx,
            &mut session,
            char_id,
            &[PyRep::Integer(alt_r1_gate)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "PocketCollapsed"
    );
}
