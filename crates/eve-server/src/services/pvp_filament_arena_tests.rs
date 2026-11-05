use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pvp_filament_mgr::{
    load_pvp_store, save_pvp_store, PvpArenaMatchInstance,
};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

#[tokio::test]
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
