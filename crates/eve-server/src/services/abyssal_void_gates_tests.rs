use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use eve_destiny::{Ball, MovementMode};

#[tokio::test]
async fn test_abyssal_gate_and_end_gate_activation_and_void_space() {
    let (mut ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let void_fil_type = 950_001_u32;
    let void_gate_id = 777_001_i64;
    std::sync::Arc::make_mut(&mut ctx.sde).types.insert(
        void_fil_type,
        crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type(
            void_fil_type,
            4145,
            66,
            0.1,
            vec![],
        ),
    );

    // Ensure ship type is a valid cruiser (621 = Caracal) and seed a Void Space filament in cargo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
        snap.items.insert(
            void_gate_id,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item(
                void_gate_id,
                void_fil_type,
                char_id,
                ship_id,
                eve_db::InventoryFlag::Cargo,
                1,
            ),
        );
    });

    // 1. Spawn ship far from celestials in origin_sys and jump into Void Space via filament
    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let _ = origin_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::splat(500_000_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let vj = handle_void_space_player_jump(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(void_gate_id), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(
        vj.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(VOID_SPACE_SYSTEM_ID as i64)
    );

    let vexit = handle_void_space_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(888_001)],
    )
    .await
    .unwrap();
    assert_eq!(
        vexit.dict_get("exitedVoidSpace").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 2. Test Abyssal room transfer gate (room 1 -> 2 -> 3) and end gate exit
    let pid = 1_i64;
    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    session.solar_system_id = Some(abyss_sys);
    let now = ctx.now_unix();
    let r1_gate = 1_895_000_000 + pid * 10 + 1;
    let r3_gate = 1_895_000_000 + pid * 10 + 3;

    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0, 0.0, 0.0],
                grid_offset: [0.0, 0.0, 0.0],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: -30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [
                    AbyssalRoomRecord {
                        room_index: 1,
                        dungeon_id: 1,
                        dungeon_name: "Room 1".into(),
                        npc_ball_ids: Vec::new(),
                        cache_item_id: 1_896_000_011,
                        cleared: true,
                        gate_unlocked_at_sec: Some(now),
                    },
                    AbyssalRoomRecord {
                        room_index: 2,
                        dungeon_id: 2,
                        dungeon_name: "Room 2".into(),
                        npc_ball_ids: Vec::new(),
                        cache_item_id: 1_896_000_012,
                        cleared: true,
                        gate_unlocked_at_sec: Some(now),
                    },
                    AbyssalRoomRecord {
                        room_index: 3,
                        dungeon_id: 3,
                        dungeon_name: "Room 3".into(),
                        npc_ball_ids: Vec::new(),
                        cache_item_id: 1_896_000_013,
                        cleared: true,
                        gate_unlocked_at_sec: Some(now),
                    },
                ],
                started_at_sec: now,
                collapse_at_sec: now + 1_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 1)]),
                max_ships: 1,
            },
        );
    });

    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    for id in [ship_id, r1_gate, r3_gate] {
        let _ = abyss_handle
            .spawn_ball(Ball {
                id,
                type_id: 621,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let g1 = handle_abyssal_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(r1_gate)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(g1.dict_get("room").and_then(|v| v.as_i64()), Some(2));

    // Move pocket to room 3 and ship/gate to matching position, then activate end gate
    with_abyssal_state(&ctx, |st| {
        if let Some(p) = st.pockets.get_mut(&char_id) {
            p.current_room = 3;
            p.rooms[2].cleared = true;
            p.rooms[2].npc_ball_ids.clear();
        }
    });
    let _ = abyss_handle.set_ball_position(ship_id, DVec3::ZERO).await;
    let _ = abyss_handle.set_ball_position(r3_gate, DVec3::ZERO).await;

    let end_res = handle_abyssal_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(r3_gate)],
    )
    .await
    .unwrap();
    assert_eq!(
        end_res
            .dict_get("exitedAbyssalSpace")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}
