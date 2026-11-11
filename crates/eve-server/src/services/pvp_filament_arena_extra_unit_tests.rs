//! Comprehensive branch-coverage and mutation-killing tests for `pvp_filament_arena.rs`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pvp_filament_arena::{
    are_opposing_in_match, credit_proving_grounds_match_rewards_in_snap, effective_match_teams,
    handle_pvp_end_gate_activation, match_contains_char, notify_pvp_stats_for_pilots,
    notify_pvp_stats_for_pilots_in_db, record_proving_grounds_combat_result_in_snap,
    spawn_matched_arena_instance, step_proving_grounds_arena_hazards_in_ballpark,
    PROVING_LOSS_POINTS, PROVING_WIN_POINTS,
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

#[test]
fn real_sde_rewards_home_station_fallback_3way_ffa_custom_events_and_notif_caps() {
    let ctx = test_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let c3 = 97_100_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters[&c1].clone();
        ch3.character_id = c3;
        ch3.user_id = 97_100_003;
        ch3.home_station_id = 0; // triggers 60_000_004 fallback
        snap.characters.insert(c3, ch3);
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.home_station_id = 60_003_760;
        }
    });

    // 1. Find a filament type from SDE with rewards (if any) and verify sde_opt = Some vs None & home_station_id == 0 fallback
    let sde_reward_fil = ctx
        .sde
        .load_cfsd_nested("res:/staticdata/pvpfilamentmatchtypes.fsdbinary")
        .ok()
        .and_then(|map| {
            map.into_iter().find_map(|(_, val)| {
                let fid = val.get("filamentTypeID").and_then(|v| v.as_u64())? as u32;
                let has_r = val
                    .get("rewards")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty());
                if fid > 0 && has_r {
                    Some(fid)
                } else {
                    None
                }
            })
        })
        .unwrap_or(56381);

    ctx.db.with_mut_snapshot(|snap| {
        let before_items = snap.items.len();
        let c3_bal_before = snap.characters[&c3].balance.0;
        credit_proving_grounds_match_rewards_in_snap(snap, Some(&ctx.sde), sde_reward_fil, &[c1]);
        // With sde_opt = None & filament_type_id = 0, falls back to 1,000,000 ISK (type 52996) & 60_000_004 home station resolution
        credit_proving_grounds_match_rewards_in_snap(snap, None, 0, &[c3]);
        assert!(snap.items.len() >= before_items);
        assert_eq!(
            snap.characters[&c3].balance.0,
            c3_bal_before + eve_db::IskCents::from_isk(1_000_000).0
        );
        // Empty winners slice is a safe no-op
        credit_proving_grounds_match_rewards_in_snap(snap, Some(&ctx.sde), sde_reward_fil, &[]);
    });

    // 2. effective_match_teams falls back to [team_a, team_b] when match_teams is empty, and are_opposing_in_match returns false for outsiders
    ctx.db.with_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let m = PvpArenaMatchInstance {
            match_id: 7101,
            event_id: 202,
            arena_solar_system_id: 32_000_001,
            team_a: vec![c1],
            team_b: vec![c2],
            origin_systems: HashMap::new(),
            started_unix: 100,
            active: true,
            winner_char_id: None,
            exited_pilots: HashSet::new(),
            end_gate_id: 0,
        };
        assert_eq!(effective_match_teams(&store, &m), vec![vec![c1], vec![c2]]);
        assert!(are_opposing_in_match(&store, &m, c1, c2));
        assert!(!are_opposing_in_match(&store, &m, c1, c3));
        assert!(!match_contains_char(&store, &m, c3));
        store.queue.clear();
    });

    // 3. One-queued combatant -> NotInPvpQueue; pre-populated store.events picks custom event_id; 3-way FFA concludes only after 2nd elimination
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.queue.insert(
            c1,
            PvpQueueCharacterEntry {
                item_id: c1,
                type_id: 56381,
                location_id: 60_003_760,
                flag: 4,
                solar_system_id: 30_000_142,
                joined_unix: 100,
            },
        );
        save_pvp_store(snap, &store);
        let err = record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), c1, c2, 100.0, 200)
            .expect_err("only one queued");
        assert_eq!(err.code, "NotInPvpQueue");

        store.queue.clear();
        store.events.push(crate::services::pvp_filament_mgr::PvpEventSchedule {
            event_id: 987,
            schedule_id: 1,
            event_name: "CustomEvent".into(),
            description: "desc".into(),
            match_type_id: 1,
            start_unix: 0,
            end_unix: 99_999,
            type_ids: vec![56381],
        });
        let mid = 7200i64;
        store.active_matches.insert(
            mid,
            PvpArenaMatchInstance {
                match_id: mid,
                event_id: 987,
                arena_solar_system_id: 32_000_001,
                team_a: vec![c1],
                team_b: vec![c2, c3],
                origin_systems: HashMap::new(),
                started_unix: 100,
                active: true,
                winner_char_id: None,
                exited_pilots: HashSet::new(),
                end_gate_id: 0,
            },
        );
        store.match_teams.insert(mid, vec![vec![c1], vec![c2], vec![c3]]);
        save_pvp_store(snap, &store);

        let (ev1, _) = record_proving_grounds_combat_result_in_snap(snap, None, c1, c2, 500.0, 210).unwrap();
        assert_eq!(ev1, 987);
        assert!(load_pvp_store(snap).active_matches[&mid].active);
        let (_, _) = record_proving_grounds_combat_result_in_snap(snap, None, c1, c3, 600.0, 220).unwrap();
        let after = load_pvp_store(snap);
        assert!(!after.active_matches[&mid].active);
        assert_eq!(after.active_matches[&mid].winner_char_id, Some(c1));
    });

    // 4. Notification capping, pending_opt = None, sde_opt = None, and empty pilots slice
    notify_pvp_stats_for_pilots(&ctx, 987, &[]);
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&c1].user_id);
    let _ = ctx.db.acquire_online_lock(uid1, 910_001, Some(c1), true, 2_000);
    notify_pvp_stats_for_pilots_in_db(&ctx.db, None, None, None, 987, &[c1]);

    // Fill c1 pending queue beyond MAX_PENDING_NOTIFICATIONS_PER_CHAR via notify_pvp_stats_for_pilots_in_db
    for _ in 0..=crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        notify_pvp_stats_for_pilots_in_db(
            &ctx.db,
            None,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
            987,
            &[c1, c3, 999_999_999],
        );
    }
    let pending = ctx.pending_notifications.read().unwrap();
    let q_len = pending[&c1].len();
    assert!(q_len > 0 && q_len <= crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(!pending.contains_key(&c3));
    drop(pending);
}

#[tokio::test]
async fn arena_hazards_timer_ramp_ejection_end_gate_distances_and_cleanup_branches() {
    let ctx = test_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let s1 = 801_001i64;
    let s2 = 801_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.alliance_id = Some(99_000_001);
            ch.active_ship_id = s1;
            ch.solar_system_id = 32_000_001;
        }
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.active_ship_id = s2;
            ch.solar_system_id = 32_000_001;
        }
        for (sid, oid) in [(s1, c1), (s2, c2)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: oid,
                    location_id: 32_000_001,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..InventoryItemRecord::default()
                },
            );
        }
    });

    // 1. db_opt = None and non-32_000_001 active matches safely return empty vec
    let mut bp_other = Ballpark::new(30_000_142);
    assert!(step_proving_grounds_arena_hazards_in_ballpark(&mut bp_other, None, None, 32_000_001, None, None).is_empty());

    let mut bp = Ballpark::new(32_000_001);
    let db_now = ctx.db.clock().now_unix_secs();
    let mid = 8100i64;
    let gate_id = 70_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.active_matches.clear();
        store.active_matches.insert(
            mid,
            PvpArenaMatchInstance {
                match_id: mid,
                event_id: 171,
                arena_solar_system_id: 32_000_001,
                team_a: vec![c1],
                team_b: vec![c2],
                origin_systems: HashMap::from([(c1, 30_000_142), (c2, 30_000_142)]),
                started_unix: db_now - 895, // not exceeded at db_now
                active: true,
                winner_char_id: None,
                exited_pilots: HashSet::new(),
                end_gate_id: gate_id,
            },
        );
        store.match_teams.insert(mid, vec![vec![c1], vec![c2]]);
        store.match_eliminated.insert(mid, HashSet::new());
        store.hazard_ticks.clear();
        // Deliberately leave match_arena_offsets empty so fallback checks gate_id ball position!
        store.match_arena_offsets.clear();
        save_pvp_store(snap, &store);
    });

    let center = DVec3::new(1_000_000.0, 0.0, 0.0);
    bp.balls.insert(
        gate_id,
        Ball {
            id: gate_id,
            type_id: 16,
            position: center,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    for (sid, hp) in [(s1, 10_000.0), (s2, 100.0)] {
        bp.balls.insert(
            sid,
            Ball {
                id: sid,
                type_id: 587,
                position: center + DVec3::new(1_000.0, 0.0, 0.0), // inside safe sphere!
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        );
        bp.combat_states.insert(
            sid,
            CombatState {
                shield_hp: hp,
                max_shield_hp: hp,
                armor_hp: hp,
                max_armor_hp: hp,
                structure_hp: hp,
                max_structure_hp: hp,
                ..CombatState::default()
            },
        );
    }

    // Before started_unix + 900, timer has NOT exceeded -> no damage inside sphere
    step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        None,
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert!((bp.combat_states[&s1].shield_hp - 10_000.0).abs() < 1e-6);

    // Move started_unix past +900s (db_now > started_unix + 900) -> timer exceeded! Tick 1 = 250, Tick 2 = 375, Tick 3 = 500.
    // s2 (total 300 HP) gets destroyed across ticks 1-2, ejecting pod & concluding match in favor of c1!
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.active_matches.get_mut(&mid).unwrap().started_unix = db_now - 905;
        save_pvp_store(snap, &store);
    });
    let s1_before = bp.combat_states[&s1].shield_hp;
    step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        None,
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert!((s1_before - bp.combat_states[&s1].shield_hp - 250.0).abs() < 1e-6);

    // Tick 2 kills s2 (only 50 total HP left vs 375 dmg) with sde_opt = None & defer = false -> ejects c2 pod & concludes match!
    let destroyed = step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        None,
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert!(destroyed.contains(&s2));
    assert!((s1_before - 250.0 - 375.0 - bp.combat_states[&s1].shield_hp).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|snap| !load_pvp_store(snap).active_matches[&mid].active));
    let c2_pod_id = ctx.db.with_snapshot(|snap| snap.characters[&c2].active_ship_id);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items[&c2_pod_id].type_id), 670);

    // Tick 3: 500 damage hits s1 -> remaining shield decreases by exact 500.0
    step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        None,
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert!((s1_before - 250.0 - 375.0 - 500.0 - bp.combat_states[&s1].shield_hp).abs() < 1e-6);

    // 2. End-gate distance & timer error codes + gate cleanup (> 70_001 removed, == 0 preserved)
    let arena_sys = ctx.systems.get_or_spawn(32_000_001);
    arena_sys.spawn_ball(bp.balls[&gate_id].clone()).await.unwrap();
    // Place c1 ship 50_001 m beyond gate surface -> TooFarFromGate
    let mut far_ship = bp.balls[&s1].clone();
    far_ship.position = center + DVec3::new(50_501.0, 0.0, 0.0);
    arena_sys.spawn_ball(far_ship).await.unwrap();

    let mut s1_sess = auth_session(90_000_001, c1, 1_000_009, 32_000_001);
    s1_sess.solar_system_id = Some(32_000_001);
    s1_sess.ship_id = Some(s1);
    let too_far = handle_pvp_end_gate_activation(&ctx, &mut s1_sess, c1, &[PyRep::Integer(gate_id)], 2_000)
        .await
        .expect_err("50001m from gate surface");
    assert_eq!(too_far.code, "TooFarFromGate");

    // Eliminated loser c2 tries to take end gate on active/concluded match -> AccessDenied if active or loser check
    arena_sys.try_set_ball_position(s1, center + DVec3::new(100.0, 0.0, 0.0));
    let c2_pod_ball = Ball {
        id: c2_pod_id,
        type_id: 670,
        position: center + DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    arena_sys.spawn_ball(c2_pod_ball).await.unwrap();

    // Check active match timer error boundary before concluding cleanup:
    // Reset started_unix = 1_000 and test MatchNotConcluded (started + 899), ArenaCollapsed (started + 900), and AccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let m = store.active_matches.get_mut(&mid).unwrap();
        m.started_unix = 1_000;
        m.active = true;
        m.winner_char_id = None;
        save_pvp_store(snap, &store);
    });
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut s1_sess, c1, &[PyRep::Integer(gate_id)], 1_899)
            .await
            .unwrap_err()
            .code,
        "MatchNotConcluded"
    );
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut s1_sess, c1, &[PyRep::Integer(gate_id)], 1_900)
            .await
            .unwrap_err()
            .code,
        "ArenaCollapsed"
    );

    let mut s2_sess = auth_session(90_000_002, c2, 1_000_009, 32_000_001);
    s2_sess.solar_system_id = Some(32_000_001);
    s2_sess.ship_id = Some(c2_pod_id);
    // Mark match concluded with c1 winner -> c2 (not on winning team) gets AccessDenied!
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.match_eliminated.remove(&mid);
        let m = store.active_matches.get_mut(&mid).unwrap();
        m.active = false;
        m.winner_char_id = Some(c1);
        store.match_arena_offsets.insert(mid, [1_000_000.0, 0.0, 0.0]);
        save_pvp_store(snap, &store);
    });
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut s2_sess, c2, &[PyRep::Integer(gate_id)], 1_950)
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Winner c1 exits cleanly and cleans up gate > 70_001 & match_arena_offsets!
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.active_matches.get_mut(&mid).unwrap().exited_pilots.insert(c2);
        save_pvp_store(snap, &store);
    });
    handle_pvp_end_gate_activation(&ctx, &mut s1_sess, c1, &[PyRep::Integer(gate_id)], 1_950)
        .await
        .expect("winner exits");
    assert!(arena_sys.get_ball_snapshot(gate_id).await.unwrap().is_none());
    assert!(ctx.db.with_snapshot(|snap| !load_pvp_store(snap).match_arena_offsets.contains_key(&mid)));
}
