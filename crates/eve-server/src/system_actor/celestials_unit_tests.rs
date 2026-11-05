//! Unit and mutation-killing tests for `system_actor::celestials`.

use super::*;
use crate::router::ServerContext;
use crate::system_actor::{ActiveModuleCycle, ActiveNaniteRepair, AsteroidState};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta, TargetLock};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: hull,
        max_structure_hp: hull,
        capacitor: cap,
        max_capacitor: cap,
        ..CombatState::default()
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::new(10.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn tidi_factor_load_asteroids_and_sync_ball_helpers_cover_boundaries() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);

    // TiDi factor clamping and non-finite fallback
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);
    let prev = sys.set_tidi_factor_raw(0.25);
    assert!((prev - 1.0).abs() < 1e-9);
    assert!((sys.tidi_factor() - 0.25).abs() < 1e-9);

    let prev2 = sys.set_tidi_factor_raw(0.01); // clamps to 0.10
    assert!((prev2 - 0.25).abs() < 1e-9);
    assert!((sys.tidi_factor() - 0.10).abs() < 1e-9);

    let _ = sys.set_tidi_factor_raw(f64::NAN); // non-finite -> 1.0
    assert!((sys.tidi_factor() - 1.0).abs() < 1e-9);

    // Spawn ball with meta & combat, stop ball, set position, update combat state
    let ship_id = 701_001_i64;
    let pos1 = DVec3::new(100_000.0, 200_000.0, 300_000.0);
    sys.try_spawn_ball_with_meta(
        make_ball(ship_id, 606, pos1),
        make_combat_state(500.0, 500.0, 500.0, 250.0),
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: 90_000_001,
            char_id: Some(90_000_001),
            ..Default::default()
        },
    );
    assert_eq!(sys.get_ball_position_sync(ship_id), Some(pos1));
    assert!(!sys.is_ball_scrambled_sync(ship_id));

    let pos2 = DVec3::new(150_000.0, 200_000.0, 300_000.0);
    sys.try_set_ball_position(ship_id, pos2);
    assert_eq!(sys.get_ball_position_sync(ship_id), Some(pos2));

    sys.try_stop_ball(ship_id);
    {
        let bp = sys.ballpark.read().unwrap();
        let b = bp.balls.get(&ship_id).unwrap();
        assert_eq!(b.speed_fraction, 0.0);
        assert_eq!(b.velocity, DVec3::ZERO);
        assert!(matches!(b.mode, MovementMode::Stop));
    }

    assert!(sys.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 2));
    assert!(!sys.try_update_combat_state(999_999_999, |_cs| {}));

    // Mobile warp disruptor bubble scrambles ship in range
    let bubble_id = 701_050_i64;
    sys.try_add_mobile_warp_disruptor(bubble_id, pos2, 20_000.0);
    assert!(sys.is_ball_scrambled_sync(ship_id));
    sys.try_remove_warp_bubble(bubble_id);

    // Asteroid spawning and belt-depletion respawn_at_tick scheduling
    let belt_id = 40_009_077_i64;
    let ast1 = 702_001_i64;
    let ast2 = 702_002_i64;
    for aid in [ast1, ast2] {
        sys.try_spawn_asteroid(
            AsteroidState {
                asteroid_id: aid,
                belt_id,
                type_id: 1230,
                initial_units: 1_000,
                remaining_units: 1_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(aid, 1230, pos2 + DVec3::new(1_000.0, 0.0, 0.0)),
        );
    }
    sys.sync_asteroid_quantity(ast1, 0, 55_555);
    {
        let st = sys.actor_state.read().unwrap();
        assert!(st.asteroids[&ast1].depleted);
        assert_eq!(st.asteroids[&ast1].respawn_at_tick, None); // ast2 still has ore!
    }
    sys.sync_asteroid_quantity(ast2, 0, 55_555);
    {
        let st = sys.actor_state.read().unwrap();
        assert_eq!(st.asteroids[&ast1].respawn_at_tick, Some(55_555));
        assert_eq!(st.asteroids[&ast2].respawn_at_tick, Some(55_555));
    }
    assert!(sys.respawn_depleted_belts_on_downtime() >= 2);
}

#[tokio::test]
async fn module_cycle_queries_nanite_repairs_and_try_remove_ball_cleanup_work() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let char_id = 90_000_001_i64;
    let ship_id = 703_001_i64;
    let mod_adc = 703_101_i64;
    let mod_rah = 703_102_i64;
    let mod_mjd = 703_103_i64;
    let mod_ind = 703_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 606, char_id, 30_000_142, InventoryFlag::None, 1),
        );
        for mid in [mod_adc, mod_rah, mod_mjd, mod_ind] {
            let mut it = make_item(mid, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1);
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 40.0);
            snap.items.insert(mid, it);
            snap.module_active_states.insert(mid);
            snap.module_active_charges.insert(mid, 240);
        }
        snap.transient_item_attributes
            .entry(ship_id)
            .or_default()
            .insert(267, 0.5);
        snap.transient_item_attributes
            .entry(mod_rah)
            .or_default()
            .insert(84, 1.5);
        snap.transient_item_attributes
            .entry(mod_rah)
            .or_default()
            .insert(268, 0.3);
    });

    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 606, DVec3::new(200_000.0, 0.0, 0.0)),
        make_combat_state(500.0, 500.0, 500.0, 250.0),
    );

    {
        let mut st = sys.actor_state.write().unwrap();
        let c_adc = ActiveModuleCycle {
            ship_id,
            module_id: mod_adc,
            module_type_id: 2048,
            duration_s: 10.0,
            repeat: 0,
            adc_saved_resists: Some(([0.0; 4], [0.0; 4], [0.0; 4])),
            is_overloaded: true,
            ..Default::default()
        };
        st.active_module_cycles.push(c_adc);

        let c_rah = ActiveModuleCycle {
            ship_id,
            module_id: mod_rah,
            module_type_id: 2048,
            duration_s: 10.0,
            repeat: 1,
            is_rah: true,
            mutadaptive_spool_step: 0.05,
            ..Default::default()
        };
        st.active_module_cycles.push(c_rah);

        let c_mjd = ActiveModuleCycle {
            ship_id,
            module_id: mod_mjd,
            module_type_id: 4383,
            duration_s: 10.0,
            repeat: 0,
            mjd_jump_distance_m: Some(100_000.0),
            ..Default::default()
        };
        st.active_module_cycles.push(c_mjd);

        let c_ind = ActiveModuleCycle {
            ship_id,
            module_id: mod_ind,
            module_type_id: 28583,
            duration_s: 60.0,
            repeat: 1,
            ..Default::default()
        };
        st.active_module_cycles.push(c_ind);

        st.active_nanite_repairs.push(ActiveNaniteRepair {
            ship_id,
            char_id,
            module_id: mod_adc,
            initial_damage_hp: 40.0,
            duration_s: 20.0,
            elapsed_s: 10.0, // 50% done -> 20 dmg remaining, 2 paste used, 2 refunded
            paste_total_needed: 4,
            paste_consumed_so_far: 4,
        });
    }

    assert!(sys.has_active_module_cycle_sync(ship_id, mod_adc));
    assert!(sys.has_active_mjd_or_mjfg_spool_sync(ship_id));
    assert!(sys.is_module_mjd_or_mjfg_spooling_sync(ship_id, mod_mjd));
    assert!(sys.has_active_overloaded_cycle_sync(ship_id));
    assert!(sys.has_active_industrial_core_sync(ship_id));
    assert!(sys.has_active_nanite_repair_sync(ship_id, mod_adc));
    assert!(sys.has_any_active_nanite_repair_sync(ship_id));
    assert!(sys.is_module_being_nanite_repaired_sync(ship_id, mod_adc));
    assert!(sys.active_entity_and_cycle_load() >= 10);

    // Cancel nanite repairs via try_remove_ball and verify DB state cleanup
    sys.try_remove_ball(ship_id);
    assert!(!sys.has_any_active_nanite_repair_sync(ship_id));
    assert!(!sys.has_active_module_cycle_sync(ship_id, mod_adc));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_adc));
        assert!(!snap.module_active_charges.contains_key(&mod_adc));
        let dmg = snap.items[&mod_adc]
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0);
        assert!((dmg - 20.0).abs() < 1e-6);
        assert!(!snap.transient_item_attributes[&ship_id].contains_key(&267));
        assert!(!snap.transient_item_attributes[&mod_rah].contains_key(&84));
        assert!(!snap.transient_item_attributes[&mod_rah].contains_key(&268));
    });

    // Also test cancel_nanite_repairs_for_ship_sync and persist_landed_warps_in_db + try_remove_ball_for_db
    let ship2 = 703_002_i64;
    let landing_pos = DVec3::new(999_000.0, 888_000.0, 777_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship2,
            make_item(ship2, 606, char_id, 30_000_142, InventoryFlag::None, 1),
        );
    });
    sys.try_spawn_ball_with_combat(
        make_ball(ship2, 606, landing_pos),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    {
        let bp = sys.ballpark.read().unwrap();
        persist_landed_warps_in_db(
            &bp,
            &[DestinyEvent::OnDeactivatingWarp { ball_id: ship2 }],
            &Some(Arc::clone(&ctx.db)),
        );
    }
    let saved_pos = ctx.db.with_snapshot(|s| s.item_positions.get(&ship2).copied());
    assert_eq!(saved_pos, Some(landing_pos.to_array()));

    try_remove_ball_for_db(&ctx.db, 30_000_142, ship2);
    assert_eq!(sys.get_ball_position_sync(ship2), None);
}

#[tokio::test]
async fn async_system_handle_commands_execute_roundtrip() {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let b1 = 704_001_i64;
    let b2 = 704_002_i64;
    let p1 = DVec3::new(50_000.0, 0.0, 0.0);
    let p2 = DVec3::new(55_000.0, 0.0, 0.0);

    let _ = sys
        .spawn_ball_with_meta(
            make_ball(b1, 606, p1),
            make_combat_state(500.0, 500.0, 500.0, 200.0),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = sys
        .spawn_ball_with_combat(
            make_ball(b2, 606, p2),
            make_combat_state(500.0, 500.0, 500.0, 200.0),
        )
        .await
        .unwrap();

    assert_eq!(sys.get_ball_group_id(b1).await.unwrap(), Some(25));
    assert!(sys.get_ball_slim_item(b1).await.unwrap().is_some());
    assert!(sys.set_ball_wormhole_stages(b1, 1, 0.5).await.unwrap());
    assert!(
        sys.change_slim_item(
            b1,
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                security_status: 2.5,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    );

    let _ = sys.set_speed_fraction(b1, 0.5).await.unwrap();
    let _ = sys.goto_point(b1, p2).await.unwrap();
    assert!(sys.set_ball_position(b1, p1 + DVec3::new(100.0, 0.0, 0.0)).await);
    assert!(
        sys.emit_destiny_event(
            b1,
            DestinyEvent::OnSpecialFX {
                ball_id: b1,
                module_id: None,
                guid: "effects.WarpScramble".into(),
                is_active: true,
            }
        )
        .await
    );
    assert!(sys.emit_packaged_actions(b1, vec![]).await);

    // Locks, capacitor, combat state, scramble, interdiction nullification, MJD
    sys.add_target_lock(
        b1,
        TargetLock {
            target_id: b2,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(sys.get_target_locks(b1).await.unwrap().len(), 1);
    assert!(sys.remove_target_lock(b1, b2).await.unwrap());
    assert_eq!(sys.clear_target_locks(b1).await.unwrap(), 0);

    sys.set_capacitor(b1, 123.0).await.unwrap();
    assert!((sys.get_combat_state(b1).await.unwrap().unwrap().capacitor - 123.0).abs() < 1e-6);

    let _ = sys
        .apply_damage(
            b1,
            DamageVector {
                em: 50.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
        )
        .await
        .unwrap();
    let healed = sys.heal_combat_state(b1).await.unwrap();
    assert!((healed.shield_hp - 500.0).abs() < 1e-6);

    let _ = sys.set_warp_scramble_strength(b1, 1).await.unwrap();
    assert!(sys.is_ball_scrambled(b1).await.unwrap());
    let _ = sys.set_warp_core_strength(b1, 2).await.unwrap();
    assert!(!sys.is_ball_scrambled(b1).await.unwrap());
    assert!(sys.set_interdiction_nullified(b1, true).await.unwrap());

    let jumped_pos = sys.micro_jump(b1, 100_000.0).await.unwrap();
    assert!(jumped_pos.distance(p1) > 50_000.0);

    let _ = sys.push_set_state(b1).await.unwrap();
    let (_tick, _state_rep, _updates_rep) = sys.get_set_state(b1).await.unwrap();
    let _ = sys.drain_observer_updates(b1).await.unwrap();
    assert!(sys.get_bubble_id_for_ball(b1).await.unwrap().is_some());
    assert!(!sys.list_balls_in_bubble(b1).await.unwrap().is_empty());
    assert!(!sys.list_dynamic_balls_in_bubble(b1).await.unwrap().is_empty());

    sys.try_step_manual(0.5);
    assert!(sys.sim_elapsed_secs() >= 0.5);

    assert!(sys.remove_ball(b2).await.is_some());
    assert!(sys.accept_docking(b1, 60_003_760, DVec3::ZERO).await.is_some());
}
