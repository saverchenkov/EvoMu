use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pvp_filament_mgr::{
    load_pvp_store, save_pvp_store, PvpArenaMatchInstance,
};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[path = "pvp_filament_arena_cov_edges_tests.rs"]
mod pvp_filament_arena_cov_edges_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_pvp_filament_arena_rewards_combat_hazards_and_end_gate() {
    let ctx = test_ctx();
    let mut sess_a = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut sess_b = auth_session(2, 90_000_002, 98_000_001, 30_000_142);
    let cid_a = 90_000_001;
    let cid_b = 90_000_002;

    // Give ships to both pilots
    let (ship_a, ship_b) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 2;
        let sa = snap.next_entity_id - 1;
        let sb = snap.next_entity_id;
        for (sid, cid) in [(sa, cid_a), (sb, cid_b)] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587, // Rifter
                    owner_id: cid,
                    location_id: 30_000_142,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
                ch.solar_system_id = 30_000_142;
            }
        }
        (sa, sb)
    });
    sess_a.ship_id = Some(ship_a);
    sess_b.ship_id = Some(ship_b);

    // 1. Test credit_proving_grounds_match_rewards_in_snap & combat result on queued pilots
    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&cid_a).unwrap().balance.0);
    ctx.db.with_mut_snapshot(|snap| {
        credit_proving_grounds_match_rewards_in_snap(snap, Some(&ctx.sde), 0, &[]);
        credit_proving_grounds_match_rewards_in_snap(snap, Some(&ctx.sde), 0, &[cid_a]);
    });
    let bal_after = ctx.db.with_snapshot(|s| s.characters.get(&cid_a).unwrap().balance.0);
    assert!(bal_after > bal_before);

    // Error branches for record_proving_grounds_combat_result_in_snap
    ctx.db.with_mut_snapshot(|snap| {
        assert!(record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), cid_a, cid_a, 100.0, 1000).is_err());
        assert!(record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), cid_a, cid_b, 100.0, 1000).is_err());
    });

    // Active match setup & helper checks
    let mid = 5001;
    let gate_id = 70_501;
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        let m = PvpArenaMatchInstance {
            match_id: mid,
            event_id: 171,
            arena_solar_system_id: 32_000_001,
            end_gate_id: gate_id,
            team_a: vec![cid_a],
            team_b: vec![cid_b],
            origin_systems: HashMap::from([(cid_a, 30_000_142), (cid_b, 30_000_142)]),
            exited_pilots: HashSet::new(),
            winner_char_id: None,
            active: true,
            started_unix: 1_000,
        };
        assert_eq!(effective_match_teams(&store, &m), vec![vec![cid_a], vec![cid_b]]);
        assert!(match_contains_char(&store, &m, cid_a));
        assert!(are_opposing_in_match(&store, &m, cid_a, cid_b));
        assert!(!are_opposing_in_match(&store, &m, cid_a, cid_a));
        store.active_matches.insert(mid, m);
        store.match_arena_offsets.insert(mid, [0.0, 0.0, 0.0]);
        save_pvp_store(snap, &store);
    });

    // Spawn arena instance
    spawn_matched_arena_instance(
        &ctx,
        &mut sess_a,
        mid,
        gate_id,
        [0.0, 0.0, 0.0],
        vec![vec![cid_a], vec![cid_b]],
        vec![(cid_a, ship_a, 30_000_142), (cid_b, ship_b, 30_000_142)],
    )
    .await;
    assert_eq!(sess_a.solar_system_id, Some(32_000_001));

    // Match not concluded yet -> handle_pvp_end_gate_activation returns MatchNotConcluded
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&cid_a) {
            c.solar_system_id = 32_000_001;
        }
    });
    assert!(handle_pvp_end_gate_activation(&ctx, &mut sess_a, cid_a, &[PyRep::Integer(gate_id)], 1_010)
        .await
        .is_err());

    // Record combat result concluding match
    let (ev_id, affected) = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            record_proving_grounds_combat_result_in_snap(snap, Some(&ctx.sde), cid_a, cid_b, 2500.0, 1_020)
        })
        .unwrap();
    assert_eq!(ev_id, 171);
    assert!(affected.contains(&cid_a) && affected.contains(&cid_b));
    notify_pvp_stats_for_pilots(&ctx, ev_id, &affected);

    // End gate activation succeeds now that winner is recorded
    let gate_res = handle_pvp_end_gate_activation(&ctx, &mut sess_a, cid_a, &[PyRep::Integer(gate_id)], 1_030)
        .await
        .unwrap();
    assert_eq!(gate_res.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(sess_a.solar_system_id, Some(30_000_142));

    // 2. Test step_proving_grounds_arena_hazards_in_ballpark boundary hazard kill
    let mut bp = Ballpark::new(32_000_001);
    assert!(step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        30_000_142,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx)
    )
    .is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&cid_b) {
            c.solar_system_id = 32_000_001;
            c.active_ship_id = ship_b;
        }
        let mut store = load_pvp_store(snap);
        if let Some(m) = store.active_matches.get_mut(&mid) {
            m.active = true;
            m.exited_pilots.remove(&cid_b);
        }
        store.match_eliminated.remove(&mid);
        save_pvp_store(snap, &store);
    });
    bp.add_ball(Ball {
        id: ship_b,
        type_id: 587,
        position: DVec3::new(100_000.0, 0.0, 0.0), // outside 75km boundary
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_b,
        CombatState {
            shield_hp: 10.0,
            max_shield_hp: 100.0,
            armor_hp: 10.0,
            max_armor_hp: 100.0,
            structure_hp: 10.0,
            max_structure_hp: 100.0,
            ..Default::default()
        },
    );
    let destroyed = step_proving_grounds_arena_hazards_in_ballpark(
        &mut bp,
        Some(&ctx.sde),
        Some(&ctx.db),
        32_000_001,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    assert_eq!(destroyed, vec![ship_b]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arch_585_arena_hazard_enforces_invuln_preserves_events_and_step_hook_pays_insurance() {
    let ctx = test_ctx();
    let cid_a = 90_000_001_i64;
    let cid_b = 90_000_002_i64;
    let arena_oob_x = PROVING_ARENA_BOUNDARY_RADIUS_M + 20_000.0;

    let (ship_a, ship_b, start_isk_b) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 2;
        let sa = snap.next_entity_id - 1;
        let sb = snap.next_entity_id;
        for (sid, cid) in [(sa, cid_a), (sb, cid_b)] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: 32_000_001,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(sid, [0.0, 0.0, 0.0]);
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
                ch.solar_system_id = 32_000_001;
                ch.station_id = None;
            }
        }
        let mut store = load_pvp_store(snap);
        store.active_matches.insert(
            77_702,
            PvpArenaMatchInstance {
                match_id: 77_702,
                event_id: 171,
                arena_solar_system_id: 32_000_001,
                end_gate_id: 1_900_990_002,
                team_a: vec![cid_a],
                team_b: vec![cid_b],
                origin_systems: HashMap::from([(cid_a, 30_000_142), (cid_b, 30_000_142)]),
                exited_pilots: HashSet::new(),
                winner_char_id: None,
                active: true,
                started_unix: ctx.now_unix(),
            },
        );
        save_pvp_store(snap, &store);
        let bal = snap.characters.get(&cid_b).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
        (sa, sb, bal)
    });

    // Insure ship_b so non-self-destruct hazard death pays out via execute_death_pipeline_sync.
    let now_u = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_b,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: ship_b,
                ship_type_id: 587,
                owner_id: cid_b,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 750_000.0,
                start_unix: now_u.saturating_sub(10),
                end_unix: now_u + 86_400,
            },
        );
    });

    // Subcase A: Ballpark-level invuln guard & observer_events preservation on lethal boundary damage.
    {
        let ctx_a = test_ctx();
        let ship_a_test = 1_900_880_001_i64;
        ctx_a.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ship_a_test,
                eve_db::InventoryItemRecord {
                    item_id: ship_a_test,
                    type_id: 587,
                    owner_id: cid_b,
                    location_id: 32_000_001,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&cid_b) {
                ch.active_ship_id = ship_a_test;
                ch.solar_system_id = 32_000_001;
                ch.station_id = None;
            }
            let mut store = load_pvp_store(snap);
            store.active_matches.insert(
                77_701,
                PvpArenaMatchInstance {
                    match_id: 77_701,
                    event_id: 171,
                    arena_solar_system_id: 32_000_001,
                    end_gate_id: 1_900_990_001,
                    team_a: vec![cid_a],
                    team_b: vec![cid_b],
                    origin_systems: HashMap::from([(cid_a, 30_000_142), (cid_b, 30_000_142)]),
                    exited_pilots: HashSet::new(),
                    winner_char_id: None,
                    active: true,
                    started_unix: ctx_a.now_unix(),
                },
            );
            save_pvp_store(snap, &store);
        });
        let mut bp = Ballpark::new(32_000_001);
        bp.add_ball(Ball {
            id: ship_a_test,
            type_id: 587,
            position: DVec3::new(arena_oob_x, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        bp.combat_states.insert(
            ship_a_test,
            CombatState {
                shield_hp: 10.0,
                max_shield_hp: 100.0,
                armor_hp: 10.0,
                max_armor_hp: 100.0,
                structure_hp: 10.0,
                max_structure_hp: 100.0,
                is_tethered: true,
                ..Default::default()
            },
        );
        let shielded = step_proving_grounds_arena_hazards_in_ballpark_for_tick(
            &mut bp,
            Some(&ctx_a.sde),
            Some(&ctx_a.db),
            32_000_001,
            Some(&ctx_a.pending_notifications),
            Some(&ctx_a.notify_tx),
        );
        assert!(shielded.is_empty());
        assert!((bp.combat_states[&ship_a_test].shield_hp - 10.0).abs() < 1e-6);

        bp.combat_states.get_mut(&ship_a_test).unwrap().is_tethered = false;
        let killed = step_proving_grounds_arena_hazards_in_ballpark_for_tick(
            &mut bp,
            Some(&ctx_a.sde),
            Some(&ctx_a.db),
            32_000_001,
            Some(&ctx_a.pending_notifications),
            Some(&ctx_a.notify_tx),
        );
        assert_eq!(killed, vec![ship_a_test]);
        let obs = bp.drain_observer_events(ship_a_test);
        assert!(obs
            .iter()
            .any(|ev| matches!(ev, &eve_destiny::DestinyEvent::TerminalExplosion { ball_id, .. } if ball_id == ship_a_test)));
    }

    // Subcase B: Full live SystemActor tick routes deferred ship kill through execute_death_pipeline_sync.
    let sys = ctx.systems.get_or_spawn(32_000_001);
    for (sid, pos) in [
        (ship_a, DVec3::ZERO),
        (ship_b, DVec3::new(arena_oob_x, 0.0, 0.0)),
    ] {
        let _ = sys
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    let _ = sys
        .set_combat_state(
            ship_b,
            CombatState {
                shield_hp: 10.0,
                max_shield_hp: 100.0,
                armor_hp: 10.0,
                max_armor_hp: 100.0,
                structure_hp: 10.0,
                max_structure_hp: 100.0,
                ..Default::default()
            },
        )
        .await;

    let _ = sys.step_manual(1.2).await;

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_b));
        let ch = snap.characters.get(&cid_b).unwrap();
        assert_ne!(ch.active_ship_id, ship_b);
        assert_eq!(snap.items[&ch.active_ship_id].type_id, 670);
        assert!(
            ch.balance.to_wire_f64() > start_isk_b,
            "insured arena boundary kill must pay insurance via execute_death_pipeline_sync"
        );
        assert!(snap.killmails.values().any(|k| k.victim_character_id == cid_b));
        assert!(snap
            .items
            .values()
            .any(|it| it.owner_id == cid_b && it.location_id == 32_000_001 && it.type_id != 670));
    });
}
