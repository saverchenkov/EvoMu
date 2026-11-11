//! Edge-case behavioral unit tests for `pve_special_sites_abyssal`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::clock::VirtualServerClock;
use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::Ballpark;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::AttributeId;
use std::collections::HashMap;
use std::sync::Arc;

fn make_abyssal_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(test_ctx().sde.clone(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000)));
    ctx
}

fn make_room(room_index: u8) -> AbyssalRoomRecord {
    AbyssalRoomRecord {
        room_index,
        dungeon_id: 0,
        dungeon_name: String::new(),
        npc_ball_ids: Vec::new(),
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    }
}

fn make_pocket(pocket_id: i64, character_id: CharacterId, abyssal_solar_system_id: SolarSystemId) -> AbyssalPocketSession {
    AbyssalPocketSession {
        pocket_id,
        character_id,
        ship_id: 0,
        origin_solar_system_id: 30_000_142,
        abyssal_solar_system_id,
        origin_position: [0.0; 3],
        grid_offset: [0.0; 3],
        filament_type_id: 47_736,
        tier: 1,
        weather: AbyssalWeatherKind::Electrical,
        weather_penalty_pct: 50.0,
        weather_bonus_pct: 50.0,
        npc_difficulty_mult: 1.0,
        current_room: 1,
        rooms: [make_room(1), make_room(2), make_room(3)],
        started_at_sec: 1_785_000_000,
        collapse_at_sec: 1_785_001_200,
        completed: false,
        collapsed_destroyed: false,
        trace_ball_id: 0,
        fleet_ship_ids: Vec::new(),
        ship_room: HashMap::new(),
        max_ships: 1,
    }
}

fn make_char(cid: i64, origin_sys: u32, ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id: cid,
        user_id: cid as u64,
        name: "AbyssEdge".into(),
        corporation_id: 98_000_001,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id: origin_sys,
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

fn first_filament_tid(ctx: &ServerContext) -> TypeId {
    (47_730..=47_900)
        .chain(56_100..=56_200)
        .find(|&tid| classify_abyssal_filament(tid).is_some() && ctx.sde.types.contains_key(&tid))
        .unwrap_or(47_736)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_room_balls_spawn_tiers_and_npc_cache_destruction_edges() {
    let ctx = make_abyssal_ctx();
    let filament_tid = first_filament_tid(&ctx);
    let abyss_sys = 32_000_001_u32;
    let sys_handle = ctx.systems.get_or_spawn(abyss_sys);

    // 1. ensure_abyssal_room_balls_for_ctx missing / completed / collapsed -> no balls spawned
    ensure_abyssal_room_balls_for_ctx(&ctx, 90_700_001).await;
    with_abyssal_state(&ctx, |reg| {
        let mut p1 = make_pocket(9001, 90_700_002, abyss_sys);
        p1.completed = true;
        reg.pockets.insert(90_700_002, p1);
        let mut p2 = make_pocket(9002, 90_700_003, abyss_sys);
        p2.collapsed_destroyed = true;
        reg.pockets.insert(90_700_003, p2);
    });
    ensure_abyssal_room_balls_for_ctx(&ctx, 90_700_002).await;
    ensure_abyssal_room_balls_for_ctx(&ctx, 90_700_003).await;
    assert!(sys_handle.get_ball_snapshot(1_895_000_000 + 9001 * 10 + 1).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(1_895_000_000 + 9002 * 10 + 1).await.unwrap().is_none());

    // 2. Uncleared active pocket without balls -> spawns gate + npcs; cleared without balls -> spawns empty npcs; cleared with ball -> SetBallInteractive
    let cid = 90_700_010_i64;
    let pid = 9010_i64;
    let npc1 = 910_001_i64;
    let npc2 = 910_002_i64;
    let cache_id = 1_896_090_011_i64;
    with_abyssal_state(&ctx, |reg| {
        let mut s = make_pocket(pid, cid, abyss_sys);
        s.filament_type_id = filament_tid;
        s.tier = 3;
        s.weather = AbyssalWeatherKind::Electrical;
        s.rooms[0].npc_ball_ids = vec![npc1, npc2];
        s.rooms[0].cache_item_id = cache_id;
        s.rooms[2].cache_item_id = cache_id + 2;
        s.rooms[2].cleared = true;
        reg.pockets.insert(cid, s);
    });
    ensure_abyssal_room_balls_for_ctx(&ctx, cid).await;
    let r1_gate = 1_895_000_000 + pid * 10 + 1;
    assert!(sys_handle.get_ball_snapshot(r1_gate).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(npc1).await.unwrap().is_some());
    // Tier 3 cache has 5x HP and Extraction Subnode (cache_id + 500_000) was spawned
    let cache_cs = sys_handle.get_combat_state(cache_id).await.unwrap().unwrap();
    assert!(cache_cs.max_structure_hp > 0.0);
    assert!(sys_handle.get_ball_snapshot(cache_id + 500_000).await.unwrap().is_some());

    // 3. on_abyssal_room_npc_destroyed_core guards & unlock
    // K-space early return false
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, 30_000_142, npc1).await);
    // Unknown NPC returns false
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, 9_999_888).await);
    // Sibling NPC (npc2) still alive -> returns false
    let _ = sys_handle.remove_ball(npc1).await;
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, npc1).await);
    // Final NPC destroyed -> unlocks room & returns true; repeat returns false
    let _ = sys_handle.remove_ball(npc2).await;
    assert!(on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, npc2).await);
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, npc2).await);

    // ensure_abyssal_room_balls_for_ctx on already-spawned cleared room updates interactivity; and room 3 cleared without balls spawns gate 47686 without NPCs
    ensure_abyssal_room_balls_for_ctx(&ctx, cid).await;
    with_abyssal_state(&ctx, |reg| {
        reg.pockets.get_mut(&cid).unwrap().current_room = 3;
    });
    ensure_abyssal_room_balls_for_ctx(&ctx, cid).await;
    let r3_gate = 1_895_000_000 + pid * 10 + 3;
    let (_, r3_ball) = sys_handle.get_ball_snapshot(r3_gate).await.unwrap().unwrap();
    assert_eq!(r3_ball.type_id, 47686);

    // 4. spawn_abyssal_room_contents tier 0 (no subnode) & tier 5 (25x cache HP)
    let pid_t0 = 9020_i64;
    let c0 = 1_896_090_020_i64;
    spawn_abyssal_room_contents(&ctx, &sys_handle, pid_t0, 1, &[], c0, 0, filament_tid, 1.0, 0.0, 1).await;
    assert!(sys_handle.get_ball_snapshot(c0 + 500_000).await.unwrap().is_none());

    let pid_t5 = 9025_i64;
    let c5 = 1_896_090_025_i64;
    spawn_abyssal_room_contents(&ctx, &sys_handle, pid_t5, 1, &[], c5, 5, filament_tid, 6.0, 0.0, 1).await;
    let cs5 = sys_handle.get_combat_state(c5).await.unwrap().unwrap();
    let cs0 = sys_handle.get_combat_state(c0).await.unwrap().unwrap();
    assert!((cache_cs.max_structure_hp - cs0.max_structure_hp * 5.0).abs() < 1e-3);
    assert!((cs5.max_structure_hp - cs0.max_structure_hp * 25.0).abs() < 1e-3);

    // 5. on_abyssal_room_npc_destroyed_in_ballpark edges (unknown, cleared, sibling present, final)
    let mut bp = Ballpark::new(abyss_sys);
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, abyss_sys, 9_999_777));
    let pid_bp = 9030_i64;
    let bp_n1 = 930_001_i64;
    let bp_n2 = 930_002_i64;
    with_abyssal_state(&ctx, |reg| {
        let mut s = make_pocket(pid_bp, 90_700_030, abyss_sys);
        s.rooms[0].npc_ball_ids = vec![bp_n1, bp_n2];
        reg.pockets.insert(90_700_030, s);
    });
    let bp_gate = 1_895_000_000 + pid_bp * 10 + 1;
    bp.add_ball(make_site_npc_ball(bp_gate, 47685, glam::DVec3::ZERO));
    bp.add_ball(make_site_npc_ball(bp_n1, 48090, glam::DVec3::ZERO));
    bp.add_ball(make_site_npc_ball(bp_n2, 48090, glam::DVec3::ZERO));
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, abyss_sys, bp_n1));
    bp.remove_ball(bp_n1);
    assert!(on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, abyss_sys, bp_n2));
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, abyss_sys, bp_n2));

    // 6. on_abyssal_cache_destroyed_core: non-cache no-op, and transfers loot to wreck for 49_661
    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, 9_999_555, None).await;
    let custom_node = 1_896_999_001_i64;
    let loot_child = 1_896_999_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            custom_node,
            InventoryItemRecord {
                item_id: custom_node,
                type_id: 49_661,
                owner_id: 500_026,
                location_id: i64::from(abyss_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(custom_node, [100.0, 200.0, 300.0]);
        snap.items.insert(
            loot_child,
            InventoryItemRecord {
                item_id: loot_child,
                type_id: TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID,
                owner_id: 500_026,
                location_id: custom_node,
                flag: InventoryFlag::None,
                quantity: 7,
                ..Default::default()
            },
        );
    });
    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, custom_node, None).await;
    let wreck_loc = ctx.db.with_snapshot(|s| s.items.get(&loot_child).unwrap().location_id);
    assert_ne!(wreck_loc, custom_node);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&wreck_loc).unwrap().type_id),
        BIOADAPTIVE_CACHE_WRECK_TYPE_ID
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_pocket_prior_dogma_trace_gate_respawn_and_deployment_guards() {
    let ctx = make_abyssal_ctx();
    let filament_tid = first_filament_tid(&ctx);
    let mut sess = SessionState::default();

    // handle_abyssal_entrance_deployment guards
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess, &[]).await.is_err());

    let cid = 90_700_100_i64;
    let ship_id = 1_000_700_100_i64;
    let origin_sys = 30_000_142_u32;
    let mut auth = auth_session(1, cid, 98_000_001, origin_sys);
    auth.station_id = Some(60_003_760);
    assert_eq!(
        handle_abyssal_entrance_deployment(&ctx, &mut auth, &[]).await.unwrap_err().code,
        "ShipDocked"
    );
    auth.station_id = None;
    auth.ship_id = None;
    assert_eq!(
        handle_abyssal_entrance_deployment(&ctx, &mut auth, &[]).await.unwrap_err().code,
        "NoActiveShip"
    );
    auth.ship_id = Some(ship_id);
    auth.solar_system_id = None;
    assert_eq!(
        handle_abyssal_entrance_deployment(&ctx, &mut auth, &[]).await.unwrap_err().code,
        "NoSolarSystem"
    );
    auth.solar_system_id = Some(origin_sys);
    assert_eq!(
        handle_abyssal_entrance_deployment(&ctx, &mut auth, &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );

    // Prior pocket with saved dogma attrs + ship_id == None fallback path & distinct grid slot allocation
    let shield_em_attr: AttributeId = 271;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(cid, make_char(cid, origin_sys, ship_id));
        let mut ship_rec = InventoryItemRecord {
            item_id: ship_id,
            type_id: 587,
            owner_id: cid,
            location_id: i64::from(origin_sys),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        ship_rec.dynamic_attributes.insert(shield_em_attr, 0.45);
        snap.items.insert(ship_id, ship_rec);
    });
    with_abyssal_state(&ctx, |reg| {
        let mut p = make_pocket(88_001, cid, 32_000_001);
        p.ship_id = ship_id;
        reg.pockets.insert(cid, p);
    });

    // Enter new pocket for cid -> skips occupied slot 0 -> gets slot 1!
    let (pid_a, _) = enter_abyssal_pocket_for_character(&ctx, &mut auth, cid, filament_tid, 1, origin_sys).await;
    let offset_a = pocket_grid_offset(&ctx, pid_a);
    assert_eq!(offset_a[1], 10_000_000.0);

    // Re-enter via existing trace when r1_gate was removed from Ballpark -> respawns r1_gate
    let trace_id = 1_897_888_001_i64;
    let abyss_handle = ctx.systems.get_or_spawn(32_000_001);
    let r1_gate = 1_895_000_000 + pid_a * 10 + 1;
    let _ = abyss_handle.remove_ball(r1_gate).await;
    with_abyssal_state(&ctx, |reg| {
        reg.traces.insert(
            trace_id,
            AbyssalTraceRecord {
                trace_ball_id: trace_id,
                char_id: cid,
                filament_type_id: filament_tid,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: ctx.now_unix(),
                expires_at_sec: ctx.now_unix() + 1_200,
                max_ships: 3,
                entered_char_ids: vec![cid],
                pocket_id: Some(pid_a),
                fleet_id: None,
            },
        );
    });
    let mut mate_sess = auth_session(2, cid + 5, 98_000_001, origin_sys);
    mate_sess.ship_id = None; // exercises fallback unwrap_or(100_000_002)
    let (joined_pid, _) =
        enter_abyssal_pocket_with_trace(&ctx, &mut mate_sess, cid + 5, filament_tid, 1, origin_sys, Some(trace_id)).await;
    assert_eq!(joined_pid, pid_a);
    assert!(abyss_handle.get_ball_snapshot(r1_gate).await.unwrap().is_some());
}
