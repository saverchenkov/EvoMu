//! Comprehensive unit tests covering edge cases for `services::combat_crimewatch::combat_npcs_drones_cycle_ops`.

use super::*;
use crate::router::ServerContext;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_sde::SdeRegistry;
use glam::DVec3;
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
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
    mut cs: CombatState,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
        }
    });
    if cs.max_structure_hp <= 0.0 {
        cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

fn make_drone_state(
    ball_id: i64,
    drone_item_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    sys_id: u32,
    target_id: Option<i64>,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id,
        drone_item_id,
        type_id,
        group_id: 100,
        owner_char_id,
        original_owner_char_id: owner_char_id,
        parent_ship_id,
        solar_system_id: sys_id,
        target_id,
        activity_state: if target_id.is_some() { 1 } else { 0 },
        damage_vector: DamageVector::new(25.0, 25.0, 0.0, 0.0),
        optimal_range: 5_000.0,
        max_velocity: 800.0,
        is_abandoned: false,
        is_sentry: false,
        assist_char_id: None,
        guard_char_id: None,
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
        bandwidth_used: 5.0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ewar_immunity_checks_structure_sde_cycle_siege_and_fitting() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Structure in snap.structures -> immune
    let struct_id = 1_000_600_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                solar_system_id: sys_id,
                owner_corp_id: 98_000_001,
                ..Default::default()
            },
        );
    });
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, struct_id, 35_832).await);

    // 2. SDE category 65 / group 30 (Titan) / attr 1045 > 0
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut t = sde_mut.types[&587].clone();
        t.type_id = 997_001;
        t.group_id = 30;
        sde_mut.types.insert(997_001, t);

        let mut t2 = sde_mut.types[&587].clone();
        t2.type_id = 997_002;
        t2.default_attributes.insert(1045, 1.0);
        sde_mut.types.insert(997_002, t2);
    }
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, 100_600_002, 997_001).await);
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, 100_600_003, 997_002).await);

    // 3. Active module cycle with self_ewar_immune = true
    let ship_cycle = 100_600_010;
    let _ = sys
        .add_module_cycle(ActiveModuleCycle {
            ship_id: ship_cycle,
            module_id: 700_001,
            module_type_id: 33400,
            self_ewar_immune: true,
            ..Default::default()
        })
        .await;
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, ship_cycle, 587).await);

    // 4. Active Bastion module (33400) in snap.module_active_states
    let ship_bastion = 100_600_011;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_002,
            InventoryItemRecord {
                item_id: 700_002,
                type_id: 33400,
                owner_id: 90_000_001,
                location_id: ship_bastion,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(700_002);
    });
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, ship_bastion, 587).await);

    // 5. Normal player ship (587) -> not immune
    let normal_ship = 100_600_012;
    spawn_ship(&ctx, sys_id, normal_ship, 90_000_001, 587, DVec3::ZERO, CombatState::default());
    assert!(!is_target_immune_to_drone_ewar(&ctx, &sys, normal_ship, 587).await);

    // 6. Fitting with attr 1045 > 0 -> immune
    let mut fit = ctx.get_or_rebuild_ship_fitting(90_000_001, normal_ship).unwrap();
    fit.ship.base_attributes.insert(1045, 1.0);
    ctx.store_ship_fitting(normal_ship, fit);
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, normal_ship, 587).await);

    // 7. Fitting with attr 2045 == 0.0 -> immune
    let mut fit2 = ctx.get_or_rebuild_ship_fitting(90_000_001, normal_ship).unwrap();
    fit2.ship.base_attributes.remove(&1045);
    fit2.ship.base_attributes.insert(2045, 0.0);
    ctx.store_ship_fitting(normal_ship, fit2);
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys, normal_ship, 587).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_ecm_jam_taunt_rule_immunity_and_timer_renewal() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let attacker_char = 90_000_001;
    let victim_char = 90_000_002;
    let attacker_ship = 100_610_001;
    let target_ship = 100_610_002;
    let drone_ball_id = 100_610_010;

    spawn_ship(&ctx, sys_id, attacker_ship, attacker_char, 587, DVec3::ZERO, CombatState::default());
    spawn_ship(
        &ctx,
        sys_id,
        target_ship,
        victim_char,
        587,
        DVec3::new(1_000.0, 0.0, 0.0),
        CombatState::default(),
    );

    let drone_ball = Ball {
        id: drone_ball_id,
        type_id: 23707, // Vespa EC-600
        position: DVec3::new(900.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 800.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let target_ball = sys.get_ball_snapshot(target_ship).await.unwrap().unwrap().1;

    // 1. Zero ecm_strength -> false
    let mut d = make_drone_state(drone_ball_id, 500_001, 23707, attacker_char, attacker_ship, sys_id, Some(target_ship));
    d.damage_vector = DamageVector::default();
    d.ecm_strength = 0.0;
    assert!(!try_apply_drone_ecm_jam(&ctx, &sys, &d, &drone_ball, target_ship, &target_ball).await);

    // 2. Target tethered -> false
    d.ecm_strength = 500.0;
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&target_ship).unwrap().is_tethered = true;
    }
    assert!(!try_apply_drone_ecm_jam(&ctx, &sys, &d, &drone_ball, target_ship, &target_ball).await);
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&target_ship).unwrap().is_tethered = false;
    }

    // 3. Successful ECM jam preserves taunt lock on drone_ball_id while clearing other locks
    let _ = sys
        .add_target_lock(
            target_ship,
            TargetLock {
                target_id: drone_ball_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            target_ship,
            TargetLock {
                target_id: attacker_ship,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    assert!(try_apply_drone_ecm_jam(&ctx, &sys, &d, &drone_ball, target_ship, &target_ball).await);
    let locks = sys.get_target_locks(target_ship).await.unwrap();
    assert!(locks.iter().any(|l| l.target_id == drone_ball_id));
    assert!(!locks.iter().any(|l| l.target_id == attacker_ship));

    // resolve_victim_character_for_ball & renew_drone_aggression_timers
    assert_eq!(resolve_victim_character_for_ball(&ctx, target_ship), Some(victim_char));
    assert_eq!(resolve_victim_character_for_ball(&ctx, 999_999_111), None);
    renew_drone_aggression_timers(&ctx, attacker_char, Some(victim_char), 5_000);
    renew_drone_aggression_timers(&ctx, attacker_char, None, 5_050);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_engaged_drones_and_auto_aggression_assist_guard_destruction() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let owner_char = 90_000_001;
    let victim_char = 90_000_002;
    let parent_ship = 100_620_001;
    let target_ship = 100_620_002;
    let cloaked_target = 100_620_003;

    spawn_ship(&ctx, sys_id, parent_ship, owner_char, 587, DVec3::ZERO, CombatState::default());
    spawn_ship(
        &ctx,
        sys_id,
        target_ship,
        victim_char,
        587,
        DVec3::new(1_000.0, 0.0, 0.0),
        CombatState::default(),
    );
    let mut cloaked_cs = ctx.build_ship_combat_state(victim_char, cloaked_target, 587);
    cloaked_cs.is_cloaked = true;
    spawn_ship(&ctx, sys_id, cloaked_target, victim_char, 587, DVec3::new(1_200.0, 0.0, 0.0), cloaked_cs);

    // Spawn 3 drones: damage drone, cloaked-target drone, and ECM drone
    let d_dmg = 100_620_101;
    let d_clk_tgt = 100_620_102;
    let d_ecm = 100_620_103;
    for did in [d_dmg, d_clk_tgt, d_ecm] {
        sys.try_spawn_ball_with_combat(
            Ball {
                id: did,
                type_id: 2454,
                position: DVec3::new(900.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 800.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
    }

    let mut dmg_state = make_drone_state(d_dmg, 500_101, 2454, owner_char, parent_ship, sys_id, Some(target_ship));
    dmg_state.damage_vector = DamageVector::new(50_000.0, 50_000.0, 50_000.0, 50_000.0);
    let clk_state = make_drone_state(d_clk_tgt, 500_102, 2454, owner_char, parent_ship, sys_id, Some(cloaked_target));
    let mut ecm_state = make_drone_state(d_ecm, 500_103, 23707, owner_char, parent_ship, sys_id, Some(target_ship));
    ecm_state.damage_vector = DamageVector::default();
    ecm_state.ecm_strength = 500.0;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(d_dmg, dmg_state);
        rt.active_drones_in_space.insert(d_clk_tgt, clk_state);
        rt.active_drones_in_space.insert(d_ecm, ecm_state);
        rt.drone_settings.insert(
            owner_char,
            DroneBehaviorSettings {
                aggressive: true,
                focus_fire: true,
            },
        );
    });

    // step_engaged_drones: destroys target_ship, disengages cloaked-target drone, sets drone_next_ready_unix
    step_engaged_drones(&ctx, 10_000).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&d_clk_tgt].activity_state, 0);
        assert_eq!(rt.active_drones_in_space[&d_clk_tgt].target_id, None);
        assert_eq!(rt.active_drones_in_space[&d_dmg].target_id, None);
        assert!(rt.drone_next_ready_unix.contains_key(&d_dmg));
    });

    // Test step_drone_auto_aggression_and_assist with assist, guard, and aggressive fallback
    let target2 = 100_620_004;
    spawn_ship(&ctx, sys_id, target2, victim_char, 587, DVec3::new(2_000.0, 0.0, 0.0), CombatState::default());
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(d) = rt.active_drones_in_space.get_mut(&d_dmg) {
            d.assist_char_id = Some(90_000_003);
            d.damage_vector = DamageVector::new(10.0, 0.0, 0.0, 0.0);
        }
        if let Some(d) = rt.active_drones_in_space.get_mut(&d_clk_tgt) {
            d.guard_char_id = Some(90_000_004);
            d.is_sentry = true;
            d.damage_vector = DamageVector::new(10.0, 0.0, 0.0, 0.0);
        }
    });
    let engaged = step_drone_auto_aggression_and_assist(
        &ctx,
        sys_id,
        owner_char,
        Some(target2),
        Some((90_000_003, target2)),
        Some((90_000_004, target2)),
    )
    .await;
    assert_eq!(engaged.len(), 3);

    // Tethered parent ship skips auto-aggression and disengages in step_engaged_drones
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&parent_ship).unwrap().is_tethered = true;
    }
    let engaged_tethered = step_drone_auto_aggression_and_assist(
        &ctx,
        sys_id,
        owner_char,
        Some(target2),
        None,
        None,
    )
    .await;
    assert!(engaged_tethered.is_empty());

    with_mut_combat_runtime(&ctx, |rt| {
        rt.drone_next_ready_unix.clear();
    });
    step_engaged_drones(&ctx, 10_100).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&d_dmg].activity_state, 0);
    });
}
