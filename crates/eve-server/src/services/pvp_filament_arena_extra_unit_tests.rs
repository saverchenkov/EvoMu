//! Comprehensive branch-coverage and mutation-killing tests for `pvp_filament_arena.rs`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pvp_filament_arena::{
    are_opposing_in_match, credit_proving_grounds_match_rewards_in_snap, effective_match_teams,
    handle_pvp_end_gate_activation, match_contains_char, notify_pvp_stats_for_pilots_in_db,
    record_proving_grounds_combat_result_in_snap, spawn_matched_arena_instance,
    step_proving_grounds_arena_hazards_in_ballpark, PROVING_LOSS_POINTS, PROVING_WIN_POINTS,
};
use crate::services::pvp_filament_mgr::{
    load_pvp_store, save_pvp_store, PvpArenaMatchInstance, PvpQueueCharacterEntry,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[test]
fn multi_team_combat_resolution_queued_fallback_and_online_notifications() {
    let ctx = test_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let c3 = 97_000_003i64;
    let c4 = 97_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&c1].clone();
        for (cid, uid) in [(c3, 97003u64), (c4, 97004u64)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            snap.characters.insert(cid, ch);
        }
    });

    // 1. Both combatants queued (no active match) -> resolves queue duel & credits win/loss points
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        for cid in [c1, c2] {
            store.queue.insert(
                cid,
                PvpQueueCharacterEntry {
                    item_id: cid,
                    type_id: 56381,
                    location_id: 60_003_760,
                    flag: 4,
                    solar_system_id: 30_000_142,
                    joined_unix: 1_000,
                },
            );
        }
        save_pvp_store(snap, &store);

        let (ev_id, pilots) = record_proving_grounds_combat_result_in_snap(
            snap,
            Some(&ctx.sde),
            c1,
            c2,
            1_500.0,
            1_010,
        )
        .unwrap();
        assert_eq!(ev_id, 171);
        assert_eq!(pilots, vec![c1, c2]);

        let after = load_pvp_store(snap);
        assert!(after.queue.is_empty());
        assert_eq!(after.stats[&c1], (1, 0, PROVING_WIN_POINTS, 1, 1_500.0));
        assert_eq!(after.stats[&c2], (0, 1, PROVING_LOSS_POINTS, 0, 0.0));
    });

    // 2. 2v2 team match: first kill eliminates c3 (match stays active), second kill eliminates c4 (concludes match & credits both teammates c1 & c2!)
    let mid = 6001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.stats.clear();
        store.event_stats.clear();
        let m = PvpArenaMatchInstance {
            match_id: mid,
            event_id: 171,
            arena_solar_system_id: 32_000_001,
            end_gate_id: 70_601,
            team_a: vec![c1, c2],
            team_b: vec![c3, c4],
            origin_systems: HashMap::new(),
            exited_pilots: HashSet::new(),
            winner_char_id: None,
            active: true,
            started_unix: 2_000,
        };
        store
            .match_teams
            .insert(mid, vec![vec![c1, c2], vec![c3, c4]]);
        store.match_filament_types.insert(mid, 56381);
        assert_eq!(
            effective_match_teams(&store, &m),
            vec![vec![c1, c2], vec![c3, c4]]
        );
        assert!(match_contains_char(&store, &m, c4));
        assert!(!are_opposing_in_match(&store, &m, c1, c2)); // teammates
        assert!(are_opposing_in_match(&store, &m, c1, c3)); // opponents
        store.active_matches.insert(mid, m);
        save_pvp_store(snap, &store);

        // Kill 1: c1 kills c3 -> c4 is still alive on team B, so match stays active!
        let _ = record_proving_grounds_combat_result_in_snap(
            snap,
            Some(&ctx.sde),
            c1,
            c3,
            800.0,
            2_010,
        )
        .unwrap();
        let mid_store = load_pvp_store(snap);
        assert!(mid_store.active_matches[&mid].active);
        assert_eq!(mid_store.stats[&c1], (0, 0, 0, 1, 800.0));

        // Kill 2: c2 kills c4 -> team B eliminated, match concludes, c1 and c2 both get +100 win points!
        let (_, all_pilots) = record_proving_grounds_combat_result_in_snap(
            snap,
            Some(&ctx.sde),
            c2,
            c4,
            1_200.0,
            2_020,
        )
        .unwrap();
        assert_eq!(all_pilots.len(), 4);
        let final_store = load_pvp_store(snap);
        assert!(!final_store.active_matches[&mid].active);
        assert_eq!(final_store.stats[&c1], (1, 0, PROVING_WIN_POINTS, 1, 800.0));
        assert_eq!(
            final_store.stats[&c2],
            (1, 0, PROVING_WIN_POINTS, 1, 1_200.0)
        );
        assert_eq!(final_store.stats[&c3], (0, 1, PROVING_LOSS_POINTS, 0, 0.0));
        assert_eq!(final_store.stats[&c4], (0, 1, PROVING_LOSS_POINTS, 0, 0.0));
    });

    // 3. Test credit_proving_grounds_match_rewards_in_snap with sde_opt = None
    ctx.db.with_mut_snapshot(|snap| {
        let before = snap.characters[&c3].balance.0;
        credit_proving_grounds_match_rewards_in_snap(snap, None, 0, &[c3]);
        assert_eq!(snap.characters[&c3].balance.0, before + 100_000_000);
    });

    // 4. Test notify_pvp_stats_for_pilots_in_db with an online session registered for c1
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&c1].user_id);
    let _ = ctx.db.acquire_online_lock(uid1, 1, Some(c1), true, 2_000);
    notify_pvp_stats_for_pilots_in_db(
        &ctx.db,
        Some(&ctx.sde),
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        171,
        &[c1],
    );
    let q = ctx.pending_notifications.read().unwrap();
    assert!(q.get(&c1).is_some_and(|v| v.len() >= 2));
}

#[tokio::test]
async fn hazard_tick_recovery_pod_destruction_ffa_spawn_and_end_gate_errors() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(10_000)));
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let c3 = 97_100_003i64;
    let ship1 = 97_100_101i64;
    let ship2 = 97_100_102i64;
    let ship3 = 97_100_103i64;
    let mid = 7001i64;
    let gate_id = 70_701i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&c1].clone();
        let mut ch3 = base;
        ch3.character_id = c3;
        ch3.user_id = 97103;
        snap.characters.insert(c3, ch3);

        for (cid, sid) in [(c1, ship1), (c2, ship2), (c3, ship3)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
                ch.solar_system_id = 32_000_001;
                ch.home_station_id = 60_003_760;
            }
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
        let m = PvpArenaMatchInstance {
            match_id: mid,
            event_id: 171,
            arena_solar_system_id: 32_000_001,
            end_gate_id: gate_id,
            team_a: vec![c1],
            team_b: vec![c2],
            origin_systems: HashMap::from([
                (c1, 30_000_142),
                (c2, 30_000_142),
                (c3, 30_000_142),
            ]),
            exited_pilots: HashSet::new(),
            winner_char_id: None,
            active: true,
            started_unix: 9_500, // 500s elapsed (< 900s timeout)
        };
        // 3-team FFA exercises the circular TAU placement branch in spawn_matched_arena_instance
        store
            .match_teams
            .insert(mid, vec![vec![c1], vec![c2], vec![c3]]);
        store.match_arena_offsets.insert(mid, [0.0, 0.0, 0.0]);
        store.hazard_ticks.insert(ship1, 3); // pre-existing hazard ticks to test recovery inside sphere
        store.active_matches.insert(mid, m);
        save_pvp_store(snap, &store);
    });

    let mut sess1 = auth_session(1, c1, 98_000_001, 30_000_142);
    sess1.ship_id = Some(ship1);
    spawn_matched_arena_instance(
        &ctx,
        &mut sess1,
        mid,
        gate_id,
        [0.0, 0.0, 0.0],
        vec![vec![c1], vec![c2], vec![c3]],
        vec![
            (c1, ship1, 30_000_142),
            (c2, ship2, 30_000_142),
            (c3, ship3, 30_000_142),
        ],
    )
    .await;

    // 1. Step arena hazards in ballpark:
    // - ship1 is inside 75 km sphere -> clears its hazard_ticks!
    // - ship2 is outside 75 km sphere with high HP -> survives tick 1 (takes shield/armor damage)
    // - ship3 is already a Capsule (670) outside 75 km sphere with 10 HP -> pod destroyed & respawned in faction corvette!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship3).unwrap().type_id = 670;
    });
    let mut bp = Ballpark::new(32_000_001);
    for (sid, tid, pos, hp) in [
        (ship1, 587u32, DVec3::new(5_000.0, 0.0, 0.0), 1_000.0),
        (ship2, 587u32, DVec3::new(90_000.0, 0.0, 0.0), 1_000.0),
        (ship3, 670u32, DVec3::new(-90_000.0, 0.0, 0.0), 10.0),
    ] {
        bp.add_ball(Ball {
            id: sid,
            type_id: tid,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        bp.combat_states.insert(
            sid,
            CombatState {
                shield_hp: hp,
                max_shield_hp: hp,
                armor_hp: hp,
                max_armor_hp: hp,
                structure_hp: hp,
                max_structure_hp: hp,
                ..Default::default()
            },
        );
    }

    let destroyed = step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert_eq!(destroyed, vec![ship3]);
    assert!(bp.combat_states[&ship2].shield_hp < 1_000.0);
    ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        assert!(!store.hazard_ticks.contains_key(&ship1));
        assert_eq!(store.hazard_ticks.get(&ship2), Some(&1));
        assert_eq!(snap.characters[&c3].station_id, Some(60_003_760));
    });

    // 2. End gate activation error branches:
    // - NotInArena when session is in 30_000_142
    let mut outside_sess = sess1.clone();
    outside_sess.solar_system_id = Some(30_000_142);
    assert!(
        handle_pvp_end_gate_activation(&ctx, &mut outside_sess, c1, &[PyRep::Integer(gate_id)], 10_000)
            .await
            .is_err()
    );

    // - Draw timeout without winner (now_unix = 10_500 >= 9_500 + 900) MUST NOT unlock exit gate (#443)
    assert!(handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(gate_id)],
        10_500
    )
    .await
    .is_err());

    // Record winner_char_id = Some(c1) for the concluded match
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let m = store.active_matches.get_mut(&mid).unwrap();
        m.active = false;
        m.winner_char_id = Some(c1);
        save_pvp_store(snap, &store);
    });

    // - Wrong gate ID (including base 70_001 when end_gate_id is 70_044) -> TooFarFromGate
    assert!(handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(99_999)],
        10_500
    )
    .await
    .is_err());
    assert!(handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(70_001)],
        10_500
    )
    .await
    .is_err());

    // - Ship > 50 km from gate -> TooFarFromGate
    let arena = ctx.systems.get_or_spawn(32_000_001);
    let _ = arena
        .spawn_ball(Ball {
            id: ship1,
            type_id: 587,
            position: DVec3::new(65_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(gate_id)],
        10_500
    )
    .await
    .is_err());

    // - Move ship1 within 50 km and activate as winner -> succeeds!
    let _ = arena
        .spawn_ball(Ball {
            id: ship1,
            type_id: 587,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let ok_gate = handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(gate_id)],
        10_500,
    )
    .await
    .unwrap();
    assert_eq!(ok_gate.dict_get("activated"), Some(&PyRep::Bool(true)));

    // - Second activation by already-exited c1 -> NoActiveMatch
    sess1.solar_system_id = Some(32_000_001);
    assert!(handle_pvp_end_gate_activation(
        &ctx,
        &mut sess1,
        c1,
        &[PyRep::Integer(gate_id)],
        10_500
    )
    .await
    .is_err());
}
