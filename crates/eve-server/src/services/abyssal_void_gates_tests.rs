use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use eve_destiny::{Ball, MovementMode};

#[tokio::test]
async fn test_abyssal_gate_and_end_gate_activation_and_void_space() {
    let (mut ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    let kw: HashMap<String, PyRep> = HashMap::new();
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

#[tokio::test]
async fn test_abyssal_gates_and_void_space_all_error_and_fleet_cleanup_branches() {
    use crate::services::pve_special_sites::{
        with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord,
        AbyssalWeatherKind, VoidSpaceSessionRecord,
    };
    use eve_db::InventoryFlag;

    let (mut ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    ctx.set_clock(std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let kw: HashMap<String, PyRep> = HashMap::new();
    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let void_fil_type = 950_002_u32;
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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
    });

    // 1. handle_void_space_player_jump error paths: docked, no_sys, no_ship, target_id <= 0, cloaked, target_id == ship_id, missing target, invalid target type, out of range
    let mut s_docked = session.clone();
    s_docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut s_docked, char_id, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    let mut s_nosys = session.clone();
    s_nosys.solar_system_id = None;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut s_nosys, char_id, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut s_noship = session.clone();
    s_noship.ship_id = None;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut s_noship, char_id, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );

    let origin_h = ctx.systems.get_or_spawn(origin_sys);
    let far_pos = DVec3::splat(500_000_000.0);
    let _ = origin_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: far_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(99_999)])
            .await
            .unwrap_err()
            .code,
        "ShipIsCloaked"
    );
    let _ = origin_h
        .set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;

    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(888_999_001)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    // Non-gate ball in space -> InvalidTarget; valid gate ball >5000m -> OutOfRange
    let bad_gate = 888_999_002_i64;
    let good_gate = 888_999_003_i64;
    let _ = origin_h
        .spawn_ball(Ball {
            id: bad_gate,
            type_id: 587,
            position: far_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = origin_h
        .spawn_ball(Ball {
            id: good_gate,
            type_id: 60_770,
            position: far_pos + DVec3::new(10_000.0, 0.0, 0.0),
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
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(bad_gate)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_void_space_player_jump(&ctx, &mut session, char_id, &[PyRep::Integer(good_gate)])
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );

    // 2. handle_void_space_end_gate_activation error branches
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "NotInVoidSpace"
    );
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut s_docked, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "NotInVoidSpace"
    );
    s_docked.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut s_docked, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    s_noship.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut s_noship, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    // No VoidSpaceSessionRecord -> NotInVoidSpace
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "NotInVoidSpace"
    );

    let now = ctx.now_unix();
    // NotInVoidSpace before entering void system, then expired VoidSpaceSessionRecord -> PocketCollapsed
    session.station_id = None;
    session.structure_id = None;
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "NotInVoidSpace"
    );
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    with_abyssal_state(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [0.0; 3],
                target_id: 1,
                entered_unix: now.saturating_sub(2000),
                expires_at_sec: now.saturating_sub(10),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_001)])
            .await
            .unwrap_err()
            .code,
        "PocketCollapsed"
    );

    // Valid void session -> test missing gate, wrong gate type, out-of-range gate, warp scrambled
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    session.station_id = None;
    session.structure_id = None;
    with_abyssal_state(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [0.0; 3],
                target_id: 1,
                entered_unix: now,
                expires_at_sec: now + 600,
                ..Default::default()
            },
        );
    });
    let void_h = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    let _ = void_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_777)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    let _ = void_h
        .spawn_ball(Ball {
            id: 888_778,
            type_id: 587,
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
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_778)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    let _ = void_h
        .spawn_ball(Ball {
            id: 888_779,
            type_id: 47_686,
            position: DVec3::new(10_000.0, 0.0, 0.0),
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
        handle_void_space_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(888_779)])
            .await
            .unwrap_err()
            .code,
        "GateOutOfRange"
    );

    // 3. handle_abyssal_end_gate_activation & handle_abyssal_gate_activation error & fleet cleanup
    session.solar_system_id = Some(abyss_sys);
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );

    let pid = 2_i64;
    let r3_gate = 1_895_000_000 + pid * 10 + 3;
    let r3_npc = 60_000_201_i64;
    let trace_id = 1_897_000_222_i64;
    let mate_id = 90_000_888_i64;
    let mate_ship = 100_000_888_i64;
    let make_r = |idx: u8, npcs: Vec<i64>| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: idx as u32,
        dungeon_name: "R".into(),
        npc_ball_ids: npcs,
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    with_abyssal_state(&ctx, |reg| {
        reg.traces.insert(
            trace_id,
            AbyssalTraceRecord {
                trace_ball_id: trace_id,
                char_id,
                filament_type_id: 47_762,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: now,
                expires_at_sec: now + 1200,
                max_ships: 2,
                entered_char_ids: vec![char_id, mate_id],
                pocket_id: Some(pid),
                fleet_id: None,
            },
        );
        reg.pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: -30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 3,
                rooms: [make_r(1, vec![]), make_r(2, vec![]), make_r(3, vec![r3_npc])],
                started_at_sec: now,
                collapse_at_sec: now + 1200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: trace_id,
                fleet_ship_ids: vec![ship_id, mate_ship],
                ship_room: HashMap::from([(ship_id, 3), (mate_ship, 3)]),
                max_ships: 2,
            },
        );
        reg.pockets.insert(
            mate_id,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: mate_id,
                ship_id: mate_ship,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: -30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 3,
                rooms: [make_r(1, vec![]), make_r(2, vec![]), make_r(3, vec![r3_npc])],
                started_at_sec: now,
                collapse_at_sec: now + 1200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: trace_id,
                fleet_ship_ids: vec![ship_id, mate_ship],
                ship_room: HashMap::from([(ship_id, 3), (mate_ship, 3)]),
                max_ships: 2,
            },
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut mate_ch) = snap.characters.get(&char_id).cloned() {
            mate_ch.character_id = mate_id;
            mate_ch.active_ship_id = mate_ship;
            mate_ch.solar_system_id = abyss_sys;
            mate_ch.station_id = None;
            mate_ch.structure_id = None;
            snap.characters.insert(mate_id, mate_ch);
        }
        snap.items.insert(
            mate_ship,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item(
                mate_ship,
                621,
                mate_id,
                abyss_sys as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });

    let abyss_h = ctx.systems.get_or_spawn(abyss_sys);
    for (bid, pos) in [
        (ship_id, DVec3::ZERO),
        (mate_ship, DVec3::ZERO),
        (r3_npc, DVec3::new(1000.0, 0.0, 0.0)),
    ] {
        let _ = abyss_h
            .spawn_ball(Ball {
                id: bid,
                type_id: 621,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // InvalidGate ID
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(999_111)])
            .await
            .unwrap_err()
            .code,
        "InvalidGate"
    );
    // GateLocked due to alive r3_npc
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "GateLocked"
    );
    let _ = abyss_h.remove_ball(r3_npc).await;
    // GateNotFound (r3_gate not spawned yet)
    assert_eq!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );
    // Spawn r3_gate far away -> GateOutOfRange
    let _ = abyss_h
        .spawn_ball(Ball {
            id: r3_gate,
            type_id: 47_686,
            position: DVec3::new(10_000.0, 0.0, 0.0),
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
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .unwrap_err()
            .code,
        "GateOutOfRange"
    );

    // Move r3_gate within range: first pilot exits (mate remains), then mate exits (trace cleaned up!)
    let _ = abyss_h.set_ball_position(r3_gate, DVec3::ZERO).await;
    assert!(
        handle_abyssal_end_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r3_gate)])
            .await
            .is_ok()
    );
    assert!(with_abyssal_state(&ctx, |reg| reg.traces.contains_key(&trace_id)));

    let mut mate_sess = session.clone();
    mate_sess.character_id = Some(mate_id);
    mate_sess.ship_id = Some(mate_ship);
    mate_sess.solar_system_id = Some(abyss_sys);
    assert!(
        handle_abyssal_end_gate_activation(&ctx, &mut mate_sess, mate_id, &[PyRep::Integer(r3_gate)])
            .await
            .is_ok()
    );
    assert!(!with_abyssal_state(&ctx, |reg| reg.traces.contains_key(&trace_id)));

    // SessionChangeInProgress on VoidSpaceExitGate + Expired PocketCollapsed on transfer gate & end gate
    let mut sess2 = mate_sess.clone();
    sess2.character_id = Some(char_id);
    sess2.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    sess2.station_id = None;
    sess2.structure_id = None;
    sess2.ship_id = Some(ship_id);
    with_abyssal_state(&ctx, |reg| {
        reg.void_space_sessions.insert(char_id, crate::services::pve_special_sites::VoidSpaceSessionRecord {
            char_id, ship_id, origin_solar_system_id: 30_000_142, origin_position: [0.0, 0.0, 0.0],
            target_id: 777, entered_unix: now, expires_at_sec: now + 600, ..Default::default()
        });
    });
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, now, VOID_SPACE_SYSTEM_ID, 0, "jump");
    assert_eq!(handle_void_space_end_gate_activation(&ctx, &mut sess2, char_id, &[PyRep::Integer(999)]).await.unwrap_err().code, "SessionChangeInProgress");

    let mk_room = |idx: u8| crate::services::pve_special_sites::AbyssalRoomRecord {
        room_index: idx, dungeon_id: 0, dungeon_name: format!("R{idx}"), npc_ball_ids: vec![], cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None,
    };
    with_abyssal_state(&ctx, |reg| {
        reg.pockets.insert(char_id, crate::services::pve_special_sites::AbyssalPocketSession {
            pocket_id: 55,
            trace_ball_id: 0,
            character_id: char_id,
            ship_id,
            fleet_ship_ids: vec![ship_id],
            filament_type_id: 47_762,
            tier: 1,
            origin_solar_system_id: 30_000_142,
            origin_position: [0.0; 3],
            grid_offset: [0.0; 3],
            abyssal_solar_system_id: abyss_sys,
            weather: crate::services::pve_special_sites::AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 30.0,
            weather_bonus_pct: 50.0,
            npc_difficulty_mult: 1.0,
            started_at_sec: now.saturating_sub(2_000),
            collapse_at_sec: now.saturating_sub(10),
            current_room: 1,
            completed: false,
            collapsed_destroyed: false,
            ship_room: Default::default(),
            max_ships: 1,
            rooms: [mk_room(1), mk_room(2), mk_room(3)],
        });
    });
    sess2.solar_system_id = Some(abyss_sys);
    assert_eq!(handle_abyssal_gate_activation(&ctx, &mut sess2, char_id, &[PyRep::Integer(1)], &kw).await.unwrap_err().code, "PocketCollapsed");

    with_abyssal_state(&ctx, |reg| {
        reg.pockets.insert(char_id, crate::services::pve_special_sites::AbyssalPocketSession {
            pocket_id: 56,
            trace_ball_id: 0,
            character_id: char_id,
            ship_id,
            fleet_ship_ids: vec![ship_id],
            filament_type_id: 47_762,
            tier: 1,
            origin_solar_system_id: 30_000_142,
            origin_position: [0.0; 3],
            grid_offset: [0.0; 3],
            abyssal_solar_system_id: abyss_sys,
            weather: crate::services::pve_special_sites::AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 30.0,
            weather_bonus_pct: 50.0,
            npc_difficulty_mult: 1.0,
            started_at_sec: now.saturating_sub(2_000),
            collapse_at_sec: now.saturating_sub(10),
            current_room: 3,
            completed: false,
            collapsed_destroyed: false,
            ship_room: Default::default(),
            max_ships: 1,
            rooms: [mk_room(1), mk_room(2), mk_room(3)],
        });
    });
    sess2.station_id = None;
    sess2.structure_id = None;
    sess2.solar_system_id = Some(abyss_sys);
    assert_eq!(handle_abyssal_end_gate_activation(&ctx, &mut sess2, char_id, &[PyRep::Integer(1)]).await.unwrap_err().code, "PocketCollapsed");
    let _ = InventoryFlag::Cargo;

    // Direct unit checks for `is_void_space_filament_type`, `is_valid_void_entrance_gate`, and `is_valid_void_exit_gate`
    assert!(super::is_void_space_filament_type(&ctx, void_fil_type));
    assert!(!super::is_void_space_filament_type(&ctx, 587));
    assert!(!super::is_void_space_filament_type(&ctx, 999_999_999));

    assert!(super::is_valid_void_entrance_gate(&ctx, 500_001, 60_770));
    assert!(super::is_valid_void_entrance_gate(&ctx, 500_001, 82_139));
    assert!(!super::is_valid_void_entrance_gate(
        &ctx,
        500_001,
        crate::services::pve_special_sites::ABYSSAL_TRACE_TYPE_ID
    ));
    assert!(!super::is_valid_void_entrance_gate(&ctx, 500_001, 587));

    assert!(super::is_valid_void_exit_gate(47_686));
    assert!(super::is_valid_void_exit_gate(60_608));
    assert!(super::is_valid_void_exit_gate(80_944));
    assert!(super::is_valid_void_exit_gate(82_139));
    assert!(!super::is_valid_void_exit_gate(587));
}

#[tokio::test]
async fn test_abyssal_and_void_gate_handoffs_gate_missing_ball_and_restore_on_db_err() {
    use crate::services::pve_special_sites::{
        with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
        VoidSpaceSessionRecord,
    };
    use eve_db::InventoryFlag;
    use eve_destiny::CombatState;

    // 1. handle_void_space_player_jump: absent ship ball leaves cargo filament & DB untouched;
    //    DB failure after take_ball restores ship ball + combat state in origin system.
    let (mut ctx, mut session, char_id, ship_id, origin_sys) = make_fighter_ctx_and_session();
    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let void_fil_type = 950_009_u32;
    let void_fil_id = 777_009_i64;
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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
        snap.items.insert(
            void_fil_id,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item(
                void_fil_id,
                void_fil_type,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                2,
            ),
        );
        snap.item_positions.insert(ship_id, [500_000_000.0; 3]);
    });

    // 1a. Absent ball -> rejects before consuming cargo filament or creating void session
    let missing_err = handle_void_space_player_jump(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(void_fil_id), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert!(!missing_err.code.is_empty());
    assert_eq!(
        ctx.db.with_snapshot(|s| (
            s.items.get(&void_fil_id).map(|i| i.quantity),
            s.characters.get(&char_id).map(|c| c.solar_system_id),
            s.abyssal_state.void_space_content.contains_key(&char_id),
        )),
        (Some(2), Some(origin_sys), false)
    );
    assert!(with_abyssal_state(&ctx, |st| st.void_space_sessions.is_empty()));

    // 1b. Spawn ball with custom CombatState, then provoke DB snapshot rejection (stale solar system in DB)
    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let mut custom_cs = CombatState::new();
    custom_cs.shield_hp = 123.0;
    custom_cs.capacitor = 77.0;
    let _ = origin_handle
        .spawn_ball_with_combat(
            Ball {
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
            },
            custom_cs,
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_002_187; // concurrent move mismatch
        }
    });
    let db_err = handle_void_space_player_jump(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(void_fil_id), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(db_err.code, "SystemTransitionFailed");
    assert!(origin_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());
    let restored_cs = origin_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(restored_cs.shield_hp, 123.0);
    assert_eq!(restored_cs.capacitor, 77.0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&void_fil_id).map(|i| i.quantity)),
        Some(2)
    );
    assert!(with_abyssal_state(&ctx, |st| st.void_space_sessions.is_empty()));

    // 2. handle_void_space_end_gate_activation: absent ball preserves session; DB failure restores ball
    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    let _ = void_handle
        .spawn_ball(Ball {
            id: 888_001,
            type_id: 47_686,
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
    let now = ctx.now_unix();
    with_abyssal_state(&ctx, |st| {
        st.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [0.0; 3],
                target_id: 777,
                entered_unix: now,
                expires_at_sec: now + 600,
                ..Default::default()
            },
        );
    });
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    session.station_id = None;
    session.structure_id = None;
    let void_missing_err = handle_void_space_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(888_001)],
    )
    .await
    .unwrap_err();
    assert!(!void_missing_err.code.is_empty());
    assert!(with_abyssal_state(&ctx, |st| st.void_space_sessions.contains_key(&char_id)));

    let mut void_cs = CombatState::new();
    void_cs.shield_hp = 234.0;
    void_cs.capacitor = 88.0;
    let _ = void_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            void_cs,
        )
        .await;
    let saved_ch = ctx.db.with_mut_snapshot(|snap| snap.characters.remove(&char_id));
    let void_db_err = handle_void_space_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(888_001)],
    )
    .await
    .unwrap_err();
    assert_eq!(void_db_err.code, "SystemTransitionFailed");
    assert!(void_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());
    let void_restored = void_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(void_restored.shield_hp, 234.0);
    assert_eq!(void_restored.capacitor, 88.0);
    assert!(with_abyssal_state(&ctx, |st| st.void_space_sessions.contains_key(&char_id)));

    // 3. handle_abyssal_end_gate_activation: absent ball preserves pocket; DB failure restores ball
    let pid = 42_i64;
    let r3_gate = 1_895_000_000 + pid * 10 + 3;
    let make_r = |idx: u8| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: idx as u32,
        dungeon_name: "R".into(),
        npc_ball_ids: vec![],
        cache_item_id: 0,
        cleared: true,
        gate_unlocked_at_sec: Some(now),
    };
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: pid,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: -30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 3,
                rooms: [make_r(1), make_r(2), make_r(3)],
                started_at_sec: now,
                collapse_at_sec: now + 600,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 3)]),
                max_ships: 1,
            },
        );
    });
    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_handle
        .spawn_ball(Ball {
            id: r3_gate,
            type_id: 47_686,
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
    session.solar_system_id = Some(abyss_sys);
    let ab_missing = handle_abyssal_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(r3_gate)],
    )
    .await
    .unwrap_err();
    assert_eq!(ab_missing.code, "GateNotFound");
    assert!(with_abyssal_state(&ctx, |st| st.pockets.contains_key(&char_id)));

    let mut ab_cs = CombatState::new();
    ab_cs.shield_hp = 345.0;
    ab_cs.capacitor = 99.0;
    let _ = abyss_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ab_cs,
        )
        .await;
    let ab_db_err = handle_abyssal_end_gate_activation(
        &ctx,
        &mut session,
        char_id,
        &[PyRep::Integer(r3_gate)],
    )
    .await
    .unwrap_err();
    assert_eq!(ab_db_err.code, "SystemTransitionFailed");
    assert!(abyss_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());
    let ab_restored = abyss_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(ab_restored.shield_hp, 345.0);
    assert_eq!(ab_restored.capacitor, 99.0);
    assert!(with_abyssal_state(&ctx, |st| st.pockets.contains_key(&char_id)));
    if let Some(ch) = saved_ch {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(char_id, ch);
        });
    }
}
