//! Deep edge, error, boundary, and state-transition tests for `pvp_filament_arena`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pvp_filament_mgr::{
    get_pvp_filament_match_spec, load_pvp_store, save_pvp_store, PvpArenaMatchInstance,
    PvpEventSchedule, PvpQueueCharacterEntry,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[test]
fn reward_fallbacks_empty_team_override_and_three_team_combat_transitions() {
    let ctx = test_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let c3 = 97_200_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters[&c1].clone();
        ch3.character_id = c3;
        ch3.user_id = 97203;
        ch3.home_station_id = 0; // force 60_000_004 fallback when sde_opt is None
        ch3.balance = eve_db::IskCents(0);
        snap.characters.insert(c3, ch3);

        // Find any SDE filament with an item reward (non-52996) if present, else verify ISK + journal
        let item_reward_fid = ctx
            .sde
            .types
            .keys()
            .copied()
            .find(|&tid| {
                let spec = get_pvp_filament_match_spec(&ctx.sde, tid);
                spec.rewards.iter().any(|&(rt, amt)| rt != 52_996 && amt > 0)
            });
        if let Some(fid) = item_reward_fid {
            let before_items = snap.items.len();
            credit_proving_grounds_match_rewards_in_snap(snap, Some(&ctx.sde), fid, &[c1]);
            assert!(snap.items.len() > before_items);
        }

        // Fallback ISK credit + journal entry when sde_opt is None
        credit_proving_grounds_match_rewards_in_snap(snap, None, 0, &[c3]);
        assert_eq!(snap.characters[&c3].balance.0, 100_000_000);
        assert!(snap
            .wallet_journal
            .iter()
            .any(|e| e.character_id == c3 && e.entry_type == "ProvingGroundsReward"));
    });

    // effective_match_teams falls back to [team_a, team_b] when store.match_teams entry is empty vec
    let mut store = crate::services::pvp_filament_mgr::PvpFilamentStore::default();
    let mid = 8100i64;
    let m = PvpArenaMatchInstance {
        match_id: mid,
        event_id: 299,
        arena_solar_system_id: 32_000_001,
        end_gate_id: 70_810,
        team_a: vec![c1],
        team_b: vec![c2],
        origin_systems: HashMap::new(),
        exited_pilots: HashSet::new(),
        winner_char_id: None,
        active: true,
        started_unix: 100,
    };
    store.match_teams.insert(mid, Vec::new());
    assert_eq!(effective_match_teams(&store, &m), vec![vec![c1], vec![c2]]);

    // 3-team FFA match: teammates c1/c1 rejected, kill 1 leaves 2 teams active, kill 2 concludes match
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_pvp_store(snap);
        let m3 = PvpArenaMatchInstance {
            match_id: mid,
            event_id: 299,
            arena_solar_system_id: 32_000_001,
            end_gate_id: 70_810,
            team_a: vec![c1],
            team_b: vec![c2],
            origin_systems: HashMap::new(),
            exited_pilots: HashSet::new(),
            winner_char_id: None,
            active: true,
            started_unix: 100,
        };
        st.match_teams
            .insert(mid, vec![vec![c1], vec![c2], vec![c3]]);
        st.active_matches.insert(mid, m3);
        save_pvp_store(snap, &st);

        // Same team / non-opposing error
        let same_team =
            record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), c1, c1, 10.0, 200);
        assert_eq!(same_team.unwrap_err().code, "InvalidArgument");

        // Kill 1: c1 kills c3 -> teams 0 and 1 remain active
        let (ev1, _) =
            record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), c1, c3, 500.0, 201)
                .unwrap();
        assert_eq!(ev1, 299);
        assert!(load_pvp_store(snap).active_matches[&mid].active);

        // Kill 2: c1 kills c2 -> only team 0 remains, match concludes
        let (_, affected) =
            record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), c1, c2, 900.0, 202)
                .unwrap();
        assert_eq!(affected.len(), 3);
        let final_st = load_pvp_store(snap);
        assert!(!final_st.active_matches[&mid].active);
        assert_eq!(final_st.active_matches[&mid].winner_char_id, Some(c1));

        // Queued duel uses store.events.last().event_id
        st.active_matches.clear();
        let mut st2 = load_pvp_store(snap);
        st2.active_matches.clear();
        st2.events.push(PvpEventSchedule {
            event_id: 777,
            schedule_id: 779,
            event_name: "Custom Event".into(),
            description: String::new(),
            match_type_id: 778,
            start_unix: 0,
            end_unix: 9_999_999,
            type_ids: vec![52_996],
        });
        for cid in [c1, c2] {
            st2.queue.insert(
                cid,
                PvpQueueCharacterEntry {
                    item_id: cid,
                    type_id: 56381,
                    location_id: 60_003_760,
                    flag: 4,
                    solar_system_id: 30_000_142,
                    joined_unix: 300,
                },
            );
        }
        save_pvp_store(snap, &st2);
        let (q_ev, _) =
            record_proving_grounds_combat_result_in_snap(snap, None, c1, c2, 250.0, 310).unwrap();
        assert_eq!(q_ev, 777);
    });
}

#[test]
fn notification_guards_offline_skip_queue_cap_and_empty_pilots() {
    let ctx = test_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;

    // Early returns: empty pilots & pending_opt = None
    notify_pvp_stats_for_pilots(&ctx, 171, &[]);
    notify_pvp_stats_for_pilots_in_db(&ctx.db, Some(&ctx.sde), None, None, 171, &[c1]);
    notify_pvp_stats_for_pilots_in_db(
        &ctx.db,
        Some(&ctx.sde),
        Some(&ctx.pending_notifications),
        None,
        171,
        &[],
    );
    assert!(ctx.pending_notifications.read().unwrap().is_empty());

    // Offline pilot receives 0 notifications
    notify_pvp_stats_for_pilots(&ctx, 999_111, &[c2]);
    assert!(!ctx.pending_notifications.read().unwrap().contains_key(&c2));

    // Online pilot c1: pre-fill pending_notifications to MAX_PENDING_NOTIFICATIONS_PER_CHAR -> caps & evicts oldest
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&c1].user_id);
    let _ = ctx.db.acquire_online_lock(uid1, 1, Some(c1), true, 1_000);
    {
        let mut qmap = ctx.pending_notifications.write().unwrap();
        let q = qmap.entry(c1).or_default();
        for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            let (pkt, mpkt) = ServerContext::build_macho_notification_packet(
                c1,
                Some(uid1),
                &format!("OldEvent{i}"),
                "charid",
                vec![],
            );
            q.push(crate::router::PendingMachoNotification { seq_id: 0,
                recipient_char_id: c1,
                event: format!("OldEvent{i}"),
                id_type: "charid".into(),
                args: vec![],
                packet: pkt,
                marshaled_packet: mpkt,
                created_filetime: i as i64,
            });
        }
    }
    notify_pvp_stats_for_pilots_in_db(
        &ctx.db,
        Some(&ctx.sde),
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        999_111,
        &[c1],
    );
    let qmap = ctx.pending_notifications.read().unwrap();
    let q = &qmap[&c1];
    assert_eq!(q.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!(q[0].event, "OldEvent2");
    assert_eq!(q[q.len() - 2].event, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(q[q.len() - 1].event, "OnPVPFilamentsLeaderboard");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hazard_collapse_timer_ship_death_capsule_ejection_and_end_gate_edge_cases() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(10_000)));
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let c3 = 97_300_003i64; // already eliminated non-pod
    let c4 = 97_300_004i64; // already exited
    let ship1 = 97_300_101i64;
    let ship2 = 97_300_102i64;
    let ship2_cargo = 97_300_105i64;
    let ship3 = 97_300_103i64;
    let ship4 = 97_300_104i64;
    let mid = 8201i64;

    // db_opt = None & empty active_matches return empty
    let mut bp = Ballpark::new(32_000_001);
    assert!(
        step_proving_grounds_arena_hazards_in_ballpark(&mut bp, None, None, 32_000_001, None, None)
            .is_empty()
    );
    assert!(step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        32_000_001,
        None,
        None,
    )
    .is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&c1].clone();
        for (cid, uid) in [(c3, 97303u64), (c4, 97304u64)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            snap.characters.insert(cid, ch);
        }
        for (cid, sid) in [(c1, ship1), (c2, ship2), (c3, ship3), (c4, ship4)] {
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
        snap.items.insert(
            ship2_cargo,
            InventoryItemRecord {
                item_id: ship2_cargo,
                type_id: 34,
                owner_id: c2,
                location_id: ship2,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let mut store = load_pvp_store(snap);
        let m = PvpArenaMatchInstance {
            match_id: mid,
            event_id: 171,
            arena_solar_system_id: 32_000_001,
            end_gate_id: 0, // tests fallback target_gate_id = 70_001
            team_a: vec![c1, c4],
            team_b: vec![c2, c3],
            origin_systems: HashMap::from([
                (c1, 30_000_142),
                (c2, 30_000_142),
                (c3, 30_000_142),
                (c4, 30_000_142),
            ]),
            exited_pilots: HashSet::from([c4]),
            winner_char_id: None,
            active: true,
            // Started at 9_000 -> at now = 10_000, elapsed 1000s > PROVING_ARENA_DURATION_SECS (900s)
            started_unix: 9_000,
        };
        store.origin_positions.insert(c1, [1_234.0, 5_678.0, 0.0]);
        store.match_eliminated.insert(mid, HashSet::from([c3]));
        store.active_matches.insert(mid, m);
        save_pvp_store(snap, &store);
    });

    // Spawn all ships INSIDE 75 km sphere -> collapse timer still hits c1 and c2, skips c3 (eliminated) and c4 (exited)
    for (sid, sh, ar, hu) in [
        (ship1, 1000.0, 1000.0, 1000.0),
        (ship2, 100.0, 100.0, 200.0), // total 400 HP: survives tick 1 (250 dmg -> 150 hull left), dies tick 2 (375 dmg)
        (ship3, 50.0, 50.0, 50.0),
        (ship4, 50.0, 50.0, 50.0),
    ] {
        bp.add_ball(Ball {
            id: sid,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
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
                shield_hp: sh,
                max_shield_hp: sh,
                armor_hp: ar,
                max_armor_hp: ar,
                structure_hp: hu,
                max_structure_hp: hu,
                ..Default::default()
            },
        );
    }

    // Tick 1: 250 dmg -> ship1 shield=750, ship2 shield=0 armor=0 structure=150
    let d1 = step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert!(d1.is_empty());
    assert!((bp.combat_states[&ship1].shield_hp - 750.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship2].structure_hp - 150.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship3].shield_hp - 50.0).abs() < 1e-6);
    assert!((bp.combat_states[&ship4].shield_hp - 50.0).abs() < 1e-6);

    // Tick 2: 375 dmg -> ship2 destroyed, cargo removed, killmail recorded, capsule spawned, match concluded with c1 winner!
    let d2 = step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert_eq!(d2, vec![ship2]);
    assert!((bp.combat_states[&ship1].shield_hp - 375.0).abs() < 1e-6);

    let c2_pod_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship2));
        assert!(!snap.items.contains_key(&ship2_cargo));
        let pod_id = snap.characters[&c2].active_ship_id;
        assert_ne!(pod_id, ship2);
        assert_eq!(snap.items[&pod_id].type_id, 670);
        assert!(snap
            .killmails
            .values()
            .any(|km| km.victim_character_id == c2 && km.attacker_corp_id == 1_000_298));
        let store = load_pvp_store(snap);
        assert!(!store.active_matches[&mid].active);
        assert_eq!(store.active_matches[&mid].winner_char_id, Some(c1));
        pod_id
    });
    assert!(bp.balls.contains_key(&c2_pod_id));

    // End gate errors:
    // - Eliminated c2 (even though active match has winner) -> NoActiveMatch
    let mut sess2 = auth_session(2, c2, 98_000_001, 32_000_001);
    sess2.ship_id = Some(c2_pod_id);
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess2, c2, &[PyRep::Integer(70_001)], 10_010)
            .await
            .unwrap_err()
            .code,
        "NoActiveMatch"
    );

    // - Already exited c4 -> NoActiveMatch
    let mut sess4 = auth_session(4, c4, 98_000_001, 32_000_001);
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess4, c4, &[PyRep::Integer(70_001)], 10_010)
            .await
            .unwrap_err()
            .code,
        "NoActiveMatch"
    );

    // - Uneliminated losing team pilot (clear c3 from match_eliminated temporarily) -> AccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.match_eliminated.get_mut(&mid).unwrap().remove(&c3);
        save_pvp_store(snap, &store);
    });
    let mut sess3 = auth_session(3, c3, 98_000_001, 32_000_001);
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess3, c3, &[PyRep::Integer(70_001)], 10_010)
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.match_eliminated.get_mut(&mid).unwrap().insert(c3);
        save_pvp_store(snap, &store);
    });

    // - Winner c1 with missing gate ball in arena -> TooFarFromGate
    let mut sess1 = auth_session(1, c1, 98_000_001, 32_000_001);
    sess1.solar_system_id = None; // tests DB fallback for solar_system_id
    sess1.ship_id = Some(ship1);
    assert_eq!(
        handle_pvp_end_gate_activation(&ctx, &mut sess1, c1, &[PyRep::Integer(70_001)], 10_010)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );

    // Spawn gate 70_001 and ship1 within range, set timers to verify they get cleared on exit
    let arena = ctx.systems.get_or_spawn(32_000_001);
    for (id, tid) in [(70_001i64, 47459u32), (ship1, 587u32)] {
        let _ = arena
            .spawn_ball(Ball {
                id,
                type_id: tid,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(c1).or_default();
        st.pvp_timer_until_unix = 20_000;
        st.weapon_timer_until_unix = 20_000;
    }
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
        st.pvp_timers_until.insert(c1, 20_000);
        st.weapon_timers_until.insert(c1, 20_000);
    });

    let exit_res = handle_pvp_end_gate_activation(&ctx, &mut sess1, c1, &[PyRep::Integer(70_001)], 10_010)
        .await
        .unwrap();
    assert_eq!(exit_res.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(exit_res.dict_get("gateID"), Some(&PyRep::Integer(70_001)));
    assert_eq!(
        exit_res.dict_get("originSystemID"),
        Some(&PyRep::Integer(30_000_142))
    );
    // Base gate 70_001 is NOT removed from arena
    assert!(arena.get_ball_snapshot(70_001).await.unwrap().is_some());
    // Timers cleared and ship placed at origin_positions
    {
        let cw = ctx.crimewatch.read().unwrap();
        let st = &cw.states[&c1];
        assert_eq!(st.pvp_timer_until_unix, 0);
        assert_eq!(st.weapon_timer_until_unix, 0);
    }
    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(!st.pvp_timers_until.contains_key(&c1));
        assert!(!st.weapon_timers_until.contains_key(&c1));
    });
    assert_eq!(
        ctx.db.with_snapshot(|s| s.item_positions.get(&ship1).copied()),
        Some([1_234.0, 5_678.0, 0.0])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutants_cov_for_tick_pod_death_killmail_combat_and_end_gate_edges() {
    // 1. `step_proving_grounds_arena_hazards_in_ballpark_for_tick`: defer=true with only c_ship in DB
    // preserves snap.items when ship is present, and still falls back to inline kill when ship item is absent.
    {
        let ctx = test_ctx();
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(10_000)));
        let c_a = 97_400_001i64;
        let c_b = 97_400_002i64;
        let ship_a = 97_400_101i64;
        let ship_b = 97_400_102i64;
        let mid = 8291i64;

        ctx.db.with_mut_snapshot(|snap| {
            let template = snap.characters.values().next().cloned().expect("demo char");
            snap.characters.clear();
            for (uid, cid, sid) in [(97_401u64, c_a, ship_a), (97_402u64, c_b, ship_b)] {
                let mut ch = template.clone();
                ch.character_id = cid;
                ch.user_id = uid;
                ch.active_ship_id = sid;
                ch.solar_system_id = 32_000_001;
                ch.home_station_id = 60_003_760;
                snap.characters.insert(cid, ch);
            }
            snap.items.insert(
                ship_a,
                InventoryItemRecord {
                    item_id: ship_a,
                    type_id: 587,
                    owner_id: c_a,
                    location_id: 32_000_001,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            // Deliberately omit ship_b from snap.items so its death runs inline even in for_tick!
            let mut store = load_pvp_store(snap);
            store.active_matches.insert(
                mid,
                PvpArenaMatchInstance {
                    match_id: mid,
                    event_id: 171,
                    arena_solar_system_id: 32_000_001,
                    end_gate_id: 70_291,
                    team_a: vec![c_a],
                    team_b: vec![c_b],
                    origin_systems: HashMap::from([(c_a, 30_000_142), (c_b, 30_000_142)]),
                    exited_pilots: HashSet::new(),
                    winner_char_id: None,
                    active: true,
                    started_unix: 9_000,
                },
            );
            save_pvp_store(snap, &store);
        });

        let mut bp = Ballpark::new(32_000_001);
        for (sid, hp) in [(ship_a, 50.0), (ship_b, 50.0)] {
            bp.add_ball(Ball {
                id: sid,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
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
                    armor_hp: 50.0,
                    max_armor_hp: 50.0,
                    structure_hp: 50.0,
                    max_structure_hp: 50.0,
                    ..Default::default()
                },
            );
        }

        let dead = step_proving_grounds_arena_hazards_in_ballpark_for_tick(
            &mut bp,
            Some(&ctx.sde),
            Some(&ctx.db),
            32_000_001,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
        );
        assert_eq!(dead, vec![ship_a, ship_b]);
        ctx.db.with_snapshot(|snap| {
            // ship_a deferred to step hook -> item still exists & no killmail for c_a
            assert!(snap.items.contains_key(&ship_a));
            assert!(!snap.killmails.values().any(|km| km.victim_character_id == c_a));
            // ship_b absent from snap.items -> will_run_step_hook == false -> inline killmail & pod created
            assert!(snap.killmails.values().any(|km| km.victim_character_id == c_b));
        });
    }

    // 2. Full inline KillmailRecord + Capsule InventoryItemRecord + Capsule CombatState + Pod destruction + notifications + spawn_matched_arena_instance + end gate boundary
    {
        let ctx = test_ctx();
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(10_000)));
        let mut rx = ctx.notify_tx.subscribe();
        let c1 = 90_000_001i64;
        let c2 = 90_000_002i64;
        let ship1 = 97_400_201i64;
        let ship2 = 97_400_202i64;
        let mid = 8292i64;

        let (uid1, uid2) = ctx.db.with_snapshot(|s| (s.characters[&c1].user_id, s.characters[&c2].user_id));
        let _ = ctx.db.acquire_online_lock(uid1, 1, Some(c1), true, 1_000);
        let _ = ctx.db.acquire_online_lock(uid2, 2, Some(c2), true, 1_000);

        let expected_price = ctx.sde.types.get(&587).map(|t| t.base_price).unwrap_or(250_000.0);
        let base_eid = ctx.db.with_mut_snapshot(|snap| {
            let eid0 = snap.next_entity_id;
            for (cid, sid) in [(c1, ship1), (c2, ship2)] {
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
            store.active_matches.insert(
                mid,
                PvpArenaMatchInstance {
                    match_id: mid,
                    event_id: 171,
                    arena_solar_system_id: 32_000_001,
                    end_gate_id: 70_292,
                    team_a: vec![c1],
                    team_b: vec![c2],
                    origin_systems: HashMap::from([(c1, 30_000_142), (c2, 30_000_142)]),
                    exited_pilots: HashSet::new(),
                    winner_char_id: None,
                    active: true,
                    started_unix: 9_000,
                },
            );
            save_pvp_store(snap, &store);
            eid0
        });

        let mut bp = Ballpark::new(32_000_001);
        for (sid, hp) in [(ship1, 5_000.0), (ship2, 50.0)] {
            bp.add_ball(Ball {
                id: sid,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
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
                    armor_hp: 50.0,
                    max_armor_hp: 50.0,
                    structure_hp: 50.0,
                    max_structure_hp: 50.0,
                    ..Default::default()
                },
            );
        }

        // Exact timeout boundary checks on end gate before conclusion (winner_char_id == None)
        let mut sess1 = auth_session(1, c1, 98_000_001, 32_000_001);
        sess1.ship_id = Some(ship1);
        assert_eq!(
            handle_pvp_end_gate_activation(
                &ctx,
                &mut sess1,
                c1,
                &[PyRep::Integer(70_292)],
                9_000 + PROVING_ARENA_DURATION_SECS - 1,
            )
            .await
            .unwrap_err()
            .code,
            "MatchNotConcluded"
        );
        assert_eq!(
            handle_pvp_end_gate_activation(
                &ctx,
                &mut sess1,
                c1,
                &[PyRep::Integer(70_292)],
                9_000 + PROVING_ARENA_DURATION_SECS,
            )
            .await
            .unwrap_err()
            .code,
            "ArenaCollapsed"
        );

        let dead = step_proving_grounds_arena_hazards_in_ballpark(
            &mut bp,
            Some(&ctx.sde),
            Some(&ctx.db),
            32_000_001,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
        );
        assert_eq!(dead, vec![ship2]);

        let c2_pod_id = ctx.db.with_snapshot(|snap| {
            let km = snap
                .killmails
                .values()
                .find(|k| k.victim_character_id == c2)
                .expect("killmail for c2");
            assert_eq!(km.kill_id, base_eid + 1);
            assert_eq!(km.attacker_character_id, 0);
            assert_eq!(km.attacker_corp_id, 1_000_298);
            assert_eq!(km.solar_system_id, 32_000_001);
            assert_eq!(km.ship_type_id, 587);
            assert!((km.total_value_isk - expected_price).abs() < 1e-6);

            let pod_id = snap.characters[&c2].active_ship_id;
            assert_eq!(pod_id, base_eid + 2);
            let pod = snap.items.get(&pod_id).expect("pod item");
            assert_eq!(pod.item_id, pod_id);
            assert_eq!(pod.type_id, 670);
            assert_eq!(pod.owner_id, c2);
            assert_eq!(pod.location_id, 32_000_001);
            assert_eq!(pod.flag, InventoryFlag::None);
            assert_eq!(pod.quantity, 1);
            assert!(pod.singleton);
            pod_id
        });

        let pod_cs = &bp.combat_states[&c2_pod_id];
        assert!(pod_cs.shield_hp > 0.0);
        assert_eq!(pod_cs.max_shield_hp, pod_cs.shield_hp);
        assert!(pod_cs.armor_hp > 0.0);
        assert_eq!(pod_cs.max_armor_hp, pod_cs.armor_hp);
        assert!(pod_cs.structure_hp > 0.0);
        assert_eq!(pod_cs.max_structure_hp, pod_cs.structure_hp);
        assert!(pod_cs.signature_radius > 0.0);
        assert_eq!(pod_cs.base_signature_radius, Some(pod_cs.signature_radius));

        let mut tx_events = Vec::new();
        while let Ok((_cid, notif)) = rx.try_recv() {
            tx_events.push(notif.event);
        }
        assert!(tx_events.iter().any(|e| e == "OnSessionChanged"));
        assert!(tx_events.iter().any(|e| e == "OnPVPFilamentsCharacterStatistics"));
        assert!(tx_events.iter().any(|e| e == "OnPVPFilamentsLeaderboard"));

        // Pod death path (ship_type_id == 670): already_eliminated c2 loses pod -> records pod killmail, respawns corvette at home station
        bp.combat_states.get_mut(&c2_pod_id).unwrap().shield_hp = 10.0;
        bp.combat_states.get_mut(&c2_pod_id).unwrap().armor_hp = 10.0;
        bp.combat_states.get_mut(&c2_pod_id).unwrap().structure_hp = 10.0;
        let km_count_before = ctx.db.with_snapshot(|s| s.killmails.len());
        let pod_dead = step_proving_grounds_arena_hazards_in_ballpark(
            &mut bp,
            Some(&ctx.sde),
            Some(&ctx.db),
            32_000_001,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
        );
        assert_eq!(pod_dead, vec![c2_pod_id]);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.killmails.len(), km_count_before + 1);
            let newest_km = snap.killmails.values().max_by_key(|k| k.kill_id).expect("pod killmail");
            assert_eq!(newest_km.victim_character_id, c2);
            assert_eq!(newest_km.ship_type_id, 670);
            let corv_id = snap.characters[&c2].active_ship_id;
            assert_ne!(corv_id, c2_pod_id);
            assert_eq!(snap.items[&corv_id].location_id, 60_003_760);
        });
        let c2_notifs = ctx.drain_notifications_for_char(c2);
        assert!(c2_notifs.iter().any(|n| n.event == "OnSessionChanged"));

        // spawn_matched_arena_instance forces solar_system_id = 32_000_001 and clears station_id/structure_id
        let mut spawn_sess = auth_session(1, c1, 98_000_001, 30_000_142);
        spawn_sess.station_id = Some(60_003_760);
        spawn_sess.structure_id = Some(1_000_999_001);
        spawn_sess.ship_id = Some(ship1);
        spawn_matched_arena_instance(
            &ctx,
            &mut spawn_sess,
            mid,
            70_292,
            [500.0, 0.0, 0.0],
            vec![vec![c1], vec![c2]],
            vec![(c1, ship1, 587)],
        )
        .await;
        assert_eq!(spawn_sess.solar_system_id, Some(32_000_001));
        assert!(spawn_sess.station_id.is_none());
        assert!(spawn_sess.structure_id.is_none());
        let arena_handle = ctx.systems.get_or_spawn(32_000_001);
        assert!(arena_handle.get_ball_snapshot(70_292).await.unwrap().is_some());

        // Winner exits end gate -> active_ship_id > 0 preserved across transition
        let exit_ok = handle_pvp_end_gate_activation(
            &ctx,
            &mut spawn_sess,
            c1,
            &[PyRep::Integer(70_292)],
            10_050,
        )
        .await
        .unwrap();
        assert_eq!(exit_ok.dict_get("activated"), Some(&PyRep::Bool(true)));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&c1].active_ship_id), ship1);
    }
}
