use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_abyssal_pylons::step_abyssal_hazards_and_pylons_in_ballpark;
use crate::services::pve_special_sites_abyssal::{
    AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pvp_filament_arena::handle_pvp_end_gate_activation;
use crate::services::pvp_filament_mgr::{
    load_pvp_store, save_pvp_store, PvpArenaMatchInstance, PROVING_ARENA_MIN_X_M,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

fn test_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn test_combat_state(hp: f64) -> CombatState {
    CombatState {
        shield_hp: hp,
        max_shield_hp: hp,
        armor_hp: hp,
        max_armor_hp: hp,
        structure_hp: hp,
        max_structure_hp: hp,
        structure_resonance: [1.0; 4],
        ..Default::default()
    }
}

#[tokio::test]
async fn r7_tc_01_proving_grounds_exit_gate_requires_winner_and_rejects_draw_and_eliminated() {
    let ctx = ServerContext::new();
    let (pilot_a, pilot_b) = (90_000_001_i64, 90_000_002_i64);
    let (ship_a, ship_b) = (100_000_001_i64, 100_000_002_i64);
    let mut sess_a = SessionState {
        session_id: 101,
        user_id: Some(1),
        character_id: Some(pilot_a),
        solar_system_id: Some(32_000_001),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let mut sess_b = SessionState {
        session_id: 102,
        user_id: Some(2),
        character_id: Some(pilot_b),
        solar_system_id: Some(32_000_001),
        ship_id: Some(ship_b),
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(pilot_a, ship_a), (pilot_b, ship_b)] {
            snap.characters.entry(cid).and_modify(|c| {
                c.solar_system_id = 32_000_001;
                c.active_ship_id = sid;
                c.station_id = None;
                c.structure_id = None;
            });
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: 32_000_001,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let mut store = load_pvp_store(snap);
        store.active_matches.insert(
            1,
            PvpArenaMatchInstance {
                match_id: 1,
                event_id: 171,
                arena_solar_system_id: 32_000_001,
                team_a: vec![pilot_a],
                team_b: vec![pilot_b],
                origin_systems: HashMap::from([(pilot_a, 30_000_142), (pilot_b, 30_000_142)]),
                started_unix: 1_000,
                active: false,
                winner_char_id: Some(pilot_b),
                exited_pilots: HashSet::new(),
                end_gate_id: 70_001,
            },
        );
        store.match_eliminated.entry(1).or_default().insert(pilot_a);

        store.active_matches.insert(
            2,
            PvpArenaMatchInstance {
                match_id: 2,
                event_id: 171,
                arena_solar_system_id: 32_000_001,
                team_a: vec![pilot_a],
                team_b: vec![pilot_b],
                origin_systems: HashMap::from([(pilot_a, 30_000_142), (pilot_b, 30_000_142)]),
                started_unix: 5_000,
                active: true,
                winner_char_id: None,
                exited_pilots: HashSet::new(),
                end_gate_id: 70_002,
            },
        );
        store
            .match_arena_offsets
            .insert(2, [55_000_000.0, 0.0, 0.0]);
        save_pvp_store(snap, &store);
    });

    let arena = ctx.systems.get_or_spawn(32_000_001);
    let _ = arena
        .spawn_ball(test_ship_ball(
            70_002,
            32_400,
            DVec3::new(55_000_000.0, 0.0, 0.0),
        ))
        .await;
    let _ = arena
        .spawn_ball(test_ship_ball(
            ship_a,
            587,
            DVec3::new(55_001_000.0, 0.0, 0.0),
        ))
        .await;
    let _ = arena
        .spawn_ball(test_ship_ball(
            ship_b,
            670,
            DVec3::new(55_001_500.0, 0.0, 0.0),
        ))
        .await;

    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess_a, pilot_a, &[PyRep::Integer(70_001)], 5_100)
            .await
            .unwrap_err()
            .code,
        "MatchNotConcluded"
    );
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess_a, pilot_a, &[PyRep::Integer(70_002)], 5_100)
            .await
            .unwrap_err()
            .code,
        "MatchNotConcluded"
    );

    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess_a, pilot_a, &[PyRep::Integer(70_002)], 5_950)
            .await
            .unwrap_err()
            .code,
        "ArenaCollapsed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let m = store.active_matches.get_mut(&2).unwrap();
        m.active = false;
        m.winner_char_id = Some(pilot_a);
        store.match_eliminated.entry(2).or_default().insert(pilot_b);
        save_pvp_store(snap, &store);
    });

    assert!(
        handle_pvp_end_gate_activation(&ctx, &mut sess_b, pilot_b, &[PyRep::Integer(70_002)], 5_950)
            .await
            .is_err()
    );
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess_a, pilot_a, &[PyRep::Integer(70_001)], 5_950)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );
    let ok = handle_pvp_end_gate_activation(
        &ctx,
        &mut sess_a,
        pilot_a,
        &[PyRep::Integer(70_002)],
        5_950,
    )
    .await
    .unwrap();
    assert_eq!(ok.dict_get("activated"), Some(&PyRep::Bool(true)));
}

#[test]
fn r7_tc_02_abyssal_boundary_tick_preserves_pg_pods_and_concluded_winners() {
    let (mut ctx, _, base_cid, _, _) = make_fighter_ctx_and_session();
    let (winner, loser, abyss_char) = (base_cid, base_cid + 1, base_cid + 2);
    let (win_ship, loser_pod, abyss_ship) = (100_000_011_i64, 100_000_012_i64, 100_000_013_i64);

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, sid, tid) in [
            (winner, win_ship, 587_u32),
            (loser, loser_pod, 670_u32),
            (abyss_char, abyss_ship, 587_u32),
        ] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.solar_system_id = 32_000_001;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.home_station_id = 60_003_760;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: 32_000_001,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        let mut store = load_pvp_store(snap);
        store.active_matches.insert(
            7,
            PvpArenaMatchInstance {
                match_id: 7,
                event_id: 171,
                arena_solar_system_id: 32_000_001,
                team_a: vec![winner],
                team_b: vec![loser],
                origin_systems: HashMap::from([(winner, 30_000_142), (loser, 30_000_142)]),
                started_unix: 1_000,
                active: false,
                winner_char_id: Some(winner),
                exited_pilots: HashSet::new(),
                end_gate_id: 70_007,
            },
        );
        store.match_eliminated.entry(7).or_default().insert(loser);
        save_pvp_store(snap, &store);
    });

    let make_room = |idx: u8| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 2100 + u32::from(idx),
        dungeon_name: "Abyss Room".into(),
        npc_ball_ids: Vec::new(),
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.insert(
            abyss_char,
            AbyssalPocketSession {
                pocket_id: 1,
                character_id: abyss_char,
                ship_id: abyss_ship,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: 32_000_001,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [make_room(0), make_room(1), make_room(2)],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![abyss_ship],
                ship_room: HashMap::from([(abyss_ship, 1)]),
                max_ships: 1,
            },
        );
    });

    let mut bp = eve_destiny::Ballpark::new(32_000_001);
    bp.add_ball(test_ship_ball(
        win_ship,
        587,
        DVec3::new(PROVING_ARENA_MIN_X_M + 10_000.0, 0.0, 0.0),
    ));
    bp.combat_states.insert(win_ship, test_combat_state(1_000.0));
    bp.add_ball(test_ship_ball(
        loser_pod,
        670,
        DVec3::new(PROVING_ARENA_MIN_X_M + 2_000.0, 0.0, 0.0),
    ));
    bp.combat_states.insert(loser_pod, test_combat_state(10.0));
    bp.add_ball(test_ship_ball(
        abyss_ship,
        587,
        DVec3::new(90_000.0, 0.0, 0.0),
    ));
    bp.combat_states
        .insert(abyss_ship, test_combat_state(10_000.0));

    for _ in 0..5 {
        let _ = step_abyssal_hazards_and_pylons_in_ballpark(
            &mut bp,
            Some(&ctx.sde),
            Some(&ctx.db),
            32_000_001,
        );
    }

    assert_eq!(bp.combat_states[&win_ship].structure_hp, 1_000.0);
    assert_eq!(bp.combat_states[&loser_pod].structure_hp, 10.0);
    assert!(
        bp.combat_states
            .get(&abyss_ship)
            .is_none_or(|cs| cs.structure_hp < 10_000.0)
    );
    let _ = &mut ctx;
}
