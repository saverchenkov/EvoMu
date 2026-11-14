//! Regression security & mechanics tests for Round-7 Track TB P1A (`#437`, `#438`, `#441`).

#![forbid(unsafe_code)]

use super::combat_reconnect_npc_concord_security_tests::make_p1a_ctx_and_session;
use crate::combat::death_pipeline::execute_death_pipeline_sync_inner;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use crate::sp6_lifecycle::handle_cmd_return_bay;
use crate::system_actor::{
    apply_overload_transition_to_cycle, cleanup_removed_module_cycle, handle_add_module_cycle,
    handle_update_module_overload, reconcile_tackle_and_bubbles, ActiveModuleCycle,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, CombatState, DVec3, DamageVector, MovementMode};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[tokio::test]
async fn r7_tb_10_drone_recall_atomic_claim_prevents_resurrection_or_bay_overfill() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_p1a_ctx_and_session();
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();

    // Ship has 25 m3 drone bay capacity; spawn two 25 m3 drones in range + 1 destroyed ghost entry
    let d1 = 881_001_i64;
    let d2 = 881_002_i64;
    let ghost = 881_003_i64;
    for did in [d1, d2] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                did,
                make_fighter_item(did, 2456, char_id, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.item_positions.insert(did, [500.0, 0.0, 0.0]);
        });
        sys.spawn_ball(Ball {
            id: did,
            type_id: 2456,
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1_500.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 5.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                did,
                DroneSpaceState {
                    ball_id: did,
                    drone_item_id: did,
                    type_id: 2456,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1_500.0,
                    optimal_range: 1_500.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 1.0,
                    neut_amount: 0.0,
                    tp_sig_bonus_pct: 0.0,
                    ecm_strength: 0.0,
                },
            );
        });
    }
    // Destroyed ghost drone exists only in snap.items at bay-bypassed state, NOT in active_drones_in_space
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ghost,
            make_fighter_item(ghost, 2456, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    let _ = handle_cmd_return_bay(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![
            PyRep::Integer(d1),
            PyRep::Integer(d2),
            PyRep::Integer(ghost),
        ])],
        &HashMap::new(),
    )
    .await
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        let in_bay_count = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::DroneBay)
            .count();
        assert_eq!(in_bay_count, 1, "Drone bay (25 m3 cap) must never overfill beyond 1 drone");
        assert!(
            !snap.items.get(&ghost).is_some_and(|i| i.flag == InventoryFlag::DroneBay),
            "Unlaunched ghost drone must never be recalled into bay"
        );
        assert_eq!(snap.items[&d1].flag, InventoryFlag::DroneBay);
        assert!(!snap.item_positions.contains_key(&d1), "Scooped drone item_positions entry must be cleaned");
    });
}

#[test]
fn r7_tb_11_overlapping_propulsion_wdfg_overload_and_webs_preserve_exact_base_velocity() {
    let mut bp = Ballpark::new(30_000_142);
    let ship_a = 500_001_i64;
    let ship_b = 500_002_i64;
    bp.add_ball(Ball {
        id: ship_a,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.add_ball(Ball {
        id: ship_b,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(ship_a, CombatState::default());
    bp.combat_states.insert(ship_b, CombatState::default());

    let mut cycles: Vec<ActiveModuleCycle> = Vec::new();
    let scram = HashMap::new();
    let core = HashMap::new();

    // 1. Activate MWD (+500% -> 2400.0 m/s)
    let mut mwd = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 1,
        module_type_id: 434,
        effect_name: "moduleBonusMicrowarpdrive".into(),
        duration_s: 10.0,
        repeat: 1,
        is_mwd_or_mjd: true,
        speed_boost_factor: 5.0,
        ..ActiveModuleCycle::default()
    };
    handle_add_module_cycle(&mut bp, &mut cycles, &scram, &core, &None, &None, None, mwd.clone()).unwrap();
    assert!((bp.balls[&ship_a].max_velocity - 2_400.0).abs() < 1e-6);

    // 2. Overlap HIC WDFG (*0.25 -> 600.0 m/s)
    let mut wdfg = ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 2,
        module_type_id: 24283,
        effect_name: "warpDisruptSphere".into(),
        duration_s: 10.0,
        repeat: 1,
        hic_velocity_multiplier: Some(0.25),
        ..ActiveModuleCycle::default()
    };
    handle_add_module_cycle(&mut bp, &mut cycles, &scram, &core, &None, &None, None, wdfg.clone()).unwrap();
    assert!((bp.balls[&ship_a].max_velocity - 600.0).abs() < 1e-6);

    // 3. Overlap 60% Stasis Webifier from ship_b (*0.40 -> 240.0 m/s)
    let mut web = ActiveModuleCycle {
        ship_id: ship_b,
        module_id: 3,
        module_type_id: 526,
        target_id: Some(ship_a),
        effect_name: "decreaseTargetSpeed".into(),
        duration_s: 10.0,
        repeat: 1,
        target_velocity_multiplier: Some(0.40),
        ..ActiveModuleCycle::default()
    };
    handle_add_module_cycle(&mut bp, &mut cycles, &scram, &core, &None, &None, None, web.clone()).unwrap();
    assert!((bp.balls[&ship_a].max_velocity - 240.0).abs() < 1e-6);

    // 4. Overload MWD while WDFG + Web are active (400 * 8.5 * 0.25 * 0.40 = 340.0 m/s)
    let _ = handle_update_module_overload(
        &mut bp, &mut cycles, &scram, &core, &None, &None, None, ship_a, 1, true,
    );
    apply_overload_transition_to_cycle(&mut cycles[0], true, &mut bp, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram, &core, &None, None);
    assert!((bp.balls[&ship_a].max_velocity - 340.0).abs() < 1e-6);

    // 5. FIFO deactivation: remove MWD first (400 * 0.25 * 0.40 = 40.0 m/s)
    let rem_mwd = cycles.remove(0);
    cleanup_removed_module_cycle(&mut bp, &rem_mwd, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram, &core, &None, None);
    assert!((bp.balls[&ship_a].max_velocity - 40.0).abs() < 1e-6);

    // 6. Remove WDFG second (400 * 0.40 = 160.0 m/s)
    wdfg = cycles.remove(0);
    cleanup_removed_module_cycle(&mut bp, &wdfg, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram, &core, &None, None);
    assert!((bp.balls[&ship_a].max_velocity - 160.0).abs() < 1e-6);

    // 7. Remove Web last -> exact 400.0 m/s unboosted base velocity restored
    web = cycles.remove(0);
    cleanup_removed_module_cycle(&mut bp, &web, &None);
    reconcile_tackle_and_bubbles(&mut bp, &mut cycles, &scram, &core, &None, None);
    let _ = &mut mwd;
    assert!((bp.balls[&ship_a].max_velocity - 400.0).abs() < 1e-6);
    assert!(bp
        .combat_states
        .get(&ship_a)
        .is_some_and(|cs| (cs.web_velocity_multiplier - 1.0).abs() < 1e-9));
}

#[tokio::test]
async fn r7_tb_14_death_pipeline_mixes_csprng_entropy_and_clears_boosters_on_pod_death() {
    let (ctx, _sess, victim, _ship_id, sys_id) = make_p1a_ctx_and_session();
    let attacker = 90_000_002_i64;
    let fixed_timestamp_seed = 1_750_000_000_u64;

    // 1. Identical fixed timestamp seed across kills must not produce deterministic loot outcomes
    let mut dropped_sets: HashSet<usize> = HashSet::new();
    for trial in 0..16_i64 {
        let test_ship = 910_000 + trial * 100;
        let mod_ids: Vec<i64> = (1..=8).map(|i| test_ship + i).collect();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                test_ship,
                make_fighter_item(test_ship, 587, victim, sys_id as i64, InventoryFlag::None, 1),
            );
            for &mid in &mod_ids {
                snap.items.insert(
                    mid,
                    make_fighter_item(mid, 434, victim, test_ship, InventoryFlag::Cargo, 1),
                );
            }
        });
        let res = execute_death_pipeline_sync_inner(
            &ctx,
            sys_id,
            victim,
            test_ship,
            attacker,
            DVec3::ZERO,
            fixed_timestamp_seed,
            false,
        )
        .unwrap();
        let bitmask = mod_ids.iter().enumerate().fold(0_usize, |acc, (idx, mid)| {
            if res.loot_dropped_item_ids.contains(mid) {
                acc | (1 << idx)
            } else {
                acc
            }
        });
        dropped_sets.insert(bitmask);
    }
    assert!(
        dropped_sets.len() > 1,
        "Loot drops must mix server CSPRNG entropy rather than depending deterministically on timestamp"
    );

    // 2. Pod death removes all active combat boosters in addition to implants
    let pod_id = 950_001_i64;
    let booster_id = 950_002_i64;
    let implant_id = 950_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.active_ship_id = pod_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, victim, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            booster_id,
            InventoryItemRecord {
                item_id: booster_id,
                type_id: 28672,
                owner_id: victim,
                location_id: victim,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            implant_id,
            InventoryItemRecord {
                item_id: implant_id,
                type_id: 19540,
                owner_id: victim,
                location_id: victim,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.active_implants.insert(victim, vec![19540]);
    });

    execute_death_pipeline_sync_inner(
        &ctx,
        sys_id,
        victim,
        pod_id,
        attacker,
        DVec3::ZERO,
        fixed_timestamp_seed,
        false,
    )
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&booster_id), "Pod death must destroy active booster items");
        assert!(!snap.items.contains_key(&implant_id), "Pod death must destroy active implant items");
        assert!(!snap.active_implants.contains_key(&victim), "Pod death must clear active_implants");
    });
}
