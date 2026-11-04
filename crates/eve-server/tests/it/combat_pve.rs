//! Integration tests for Phase 5: Combat, Damage & PvE (`C-1, C-2, C-3, C-4`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, DestinyEvent, MovementMode};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::combat::npc_ai::{NpcAiState, NpcSpawnManager};
use eve_server::formulas::{
    missile_damage_factor, turret_angular_velocity,
    turret_chance_to_hit, turret_chance_to_hit_with_res, turret_shot_damage,
};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_c1_turret_tracking_missile_damage_and_resistances() {
    // parity: plan:C-1
    // Turret tracking formula (angular velocity, signature resolution vs target signature radius, optimal + falloff)
    // Missile damage formula (explosion radius, explosion velocity, target velocity)
    // Target resistance reduction (EM, Therm, Kin, Exp) across Shield -> Armor -> Structure

    // 1. Angular Velocity calculation
    let pos_a = DVec3::new(0.0, 0.0, 0.0);
    let vel_a = DVec3::ZERO;
    let pos_b = DVec3::new(10_000.0, 0.0, 0.0); // 10 km away along X
    let vel_parallel = DVec3::new(500.0, 0.0, 0.0); // moving directly away
    let vel_perp = DVec3::new(0.0, 500.0, 0.0); // moving tangentially at 500 m/s

    let omega_parallel = turret_angular_velocity(pos_a, vel_a, pos_b, vel_parallel);
    assert_eq!(omega_parallel, 0.0, "Parallel motion has zero angular velocity");

    let omega_perp = turret_angular_velocity(pos_a, vel_a, pos_b, vel_perp);
    assert!(
        (omega_perp - 0.05).abs() < 1e-4,
        "Perpendicular 500 m/s at 10,000 m should yield 0.05 rad/s: {omega_perp}"
    );

    // 2. Turret ChanceToHit with Signature Resolution and Falloff
    let tracking = 150.0;
    let optimal = 10_000.0;
    let falloff = 5_000.0;
    let sig_res = 40_000.0;

    // At optimal range with zero angular velocity -> 100% hit chance
    let p_hit_perfect = turret_chance_to_hit_with_res(
        0.0, tracking, sig_res, 40.0, 10_000.0, optimal, falloff,
    );
    assert!((p_hit_perfect - 1.0).abs() < 1e-6);

    // Target signature radius scaling: larger target signature directly increases hit chance against angular velocity
    let p_hit_small_target = turret_chance_to_hit_with_res(
        0.02, tracking, sig_res, 35.0, 10_000.0, optimal, falloff,
    );
    let p_hit_large_target = turret_chance_to_hit_with_res(
        0.02, tracking, sig_res, 350.0, 10_000.0, optimal, falloff,
    );
    assert!(
        p_hit_large_target > p_hit_small_target,
        "Larger signature radius (350 vs 35) must dramatically increase hit chance: {p_hit_large_target} vs {p_hit_small_target}"
    );

    // Falloff scaling: at optimal + falloff (15,000m) with zero angular velocity, hit chance is exactly 0.50
    let p_hit_falloff_1 = turret_chance_to_hit(0.0, tracking, 40.0, 15_000.0, optimal, falloff);
    assert!(
        (p_hit_falloff_1 - 0.50).abs() < 1e-4,
        "Hit chance at optimal + falloff must be 0.50: {p_hit_falloff_1}"
    );

    // At optimal + 2x falloff (20,000m), hit chance is 0.5^4 = 0.0625
    let p_hit_falloff_2 = turret_chance_to_hit(0.0, tracking, 40.0, 20_000.0, optimal, falloff);
    assert!(
        (p_hit_falloff_2 - 0.0625).abs() < 1e-4,
        "Hit chance at optimal + 2x falloff must be 0.0625: {p_hit_falloff_2}"
    );

    // 3. Turret Shot Damage resolution: Miss, Wrecking Shot (3.0x), and Quality (0.50..1.49)
    let base_dmg = DamageVector {
        em: 100.0,
        thermal: 80.0,
        kinetic: 60.0,
        explosive: 40.0,
    };
    let total_base = base_dmg.total(); // 280.0

    // Roll >= chance -> Miss (0 damage)
    let (miss_dmg, is_wrecking_miss) = turret_shot_damage(base_dmg, 0.70, 0.75);
    assert_eq!(miss_dmg.total(), 0.0);
    assert!(!is_wrecking_miss);

    // Roll < 0.01 -> Wrecking Shot (3.0x damage)
    let (wreck_dmg, is_wrecking) = turret_shot_damage(base_dmg, 0.70, 0.005);
    assert!(is_wrecking);
    assert!((wreck_dmg.total() - total_base * 3.0).abs() < 1e-4);

    // Normal hit: Quality Q = 0.49 + roll
    let (hit_dmg, is_wrecking_normal) = turret_shot_damage(base_dmg, 0.70, 0.50);
    assert!(!is_wrecking_normal);
    let expected_mult = 0.49 + 0.50; // 0.99
    assert!((hit_dmg.total() - total_base * expected_mult).abs() < 1e-4);

    // 4. Missile Damage Application formula
    let exp_radius = 125.0; // Heavy missile explosion radius
    let exp_velocity = 140.0; // Explosion velocity
    let drf = 0.604; // Damage reduction factor

    // Large stationary target (sig 200, vel 0) takes 100% damage
    let m_factor_ideal = missile_damage_factor(200.0, 0.0, exp_radius, exp_velocity, drf);
    assert_eq!(m_factor_ideal, 1.0);

    // Small fast target (sig 35, vel 1200) takes significantly reduced damage
    let m_factor_fast_frigate =
        missile_damage_factor(35.0, 1_200.0, exp_radius, exp_velocity, drf);
    assert!(
        m_factor_fast_frigate < 0.20,
        "Fast small frigate should take < 20% missile damage: {m_factor_fast_frigate}"
    );

    // 5. Target Resistance Reduction across layers (Shield -> Armor -> Structure)
    let mut combat_state = eve_destiny::CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 400.0,
        max_armor_hp: 400.0,
        structure_hp: 300.0,
        max_structure_hp: 300.0,
        // Shield resistances: 50% EM (0.50 res), 40% Therm (0.60 res), 20% Kin (0.80 res), 0% Exp (1.00 res)
        shield_resonance: [0.50, 0.60, 0.80, 1.00],
        // Armor resistances: 60% EM (0.40 res), 50% Therm (0.50 res), 30% Kin (0.70 res), 10% Exp (0.90 res)
        armor_resonance: [0.40, 0.50, 0.70, 0.90],
        // Structure: 33% all (0.67 res)
        structure_resonance: [0.67, 0.67, 0.67, 0.67],
        ..Default::default()
    };

    // Apply pure EM damage of 200: with 50% EM shield resist, shield takes 100 damage
    let (applied_1, destroyed_1) = combat_state.apply_damage(DamageVector {
        em: 200.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    });
    assert!(!destroyed_1);
    assert_eq!(applied_1, 100.0);
    assert_eq!(combat_state.shield_hp, 400.0);

    // Massive hit breaks shield and bleeds into armor with armor resistances
    let (_applied_2, destroyed_2) = combat_state.apply_damage(DamageVector {
        em: 1_200.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    });
    assert!(!destroyed_2);
    assert_eq!(combat_state.shield_hp, 0.0, "Shield must be depleted");
    assert!(combat_state.armor_hp < 400.0, "Armor must take remaining damage");
    assert!(combat_state.structure_hp > 0.0, "Structure intact");
}

#[tokio::test]
async fn test_c2_hp_from_dogma_recharge_and_damage_notifications() {
    // parity: plan:C-2
    // HP from ship Dogma attributes (shield, armor, hull), recharge simulation,
    // and OnDamageStateChange notification emission.

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let ship_type_id = 587; // Rifter

    // 1. Initialize CombatState from ship attributes
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    assert!(combat_state.shield_hp > 0.0);
    assert!(combat_state.armor_hp > 0.0);
    assert!(combat_state.structure_hp > 0.0);
    assert!(combat_state.capacitor > 0.0);
    assert!(combat_state.shield_recharge_time_ms > 0.0);
    assert!(combat_state.capacitor_recharge_time_ms > 0.0);

    // 2. Analytical Passive Recharge simulation for Shield & Capacitor
    let mut park = eve_destiny::Ballpark::new(30_000_142);
    let ship_ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship_ball);

    // Set shield to 25% (peak recharge point) and capacitor to 20%
    if let Some(cs) = park.combat_states.get_mut(&ship_id) {
        cs.shield_hp = cs.max_shield_hp * 0.25;
        cs.capacitor = cs.max_capacitor * 0.20;
    }

    let initial_shield = park.combat_states[&ship_id].shield_hp;
    let initial_cap = park.combat_states[&ship_id].capacitor;

    // Step 5 seconds of simulation
    for _ in 0..5 {
        park.step(1.0);
    }

    let recharged_shield = park.combat_states[&ship_id].shield_hp;
    let recharged_cap = park.combat_states[&ship_id].capacitor;
    assert!(
        recharged_shield > initial_shield,
        "Shield must recharge passively: {recharged_shield} > {initial_shield}"
    );
    assert!(
        recharged_cap > initial_cap,
        "Capacitor must recharge passively: {recharged_cap} > {initial_cap}"
    );

    // 3. OnDamageStateChange emission on taking damage
    let observer_id = 100_000_002;
    let observer_ball = Ball {
        id: observer_id,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0), // in same grid
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(observer_ball);
    // Restore shield to full so damage stays on shield layer
    if let Some(cs) = park.combat_states.get_mut(&ship_id) {
        cs.shield_hp = cs.max_shield_hp;
    }
    let _ = park.drain_all_observer_events();

    // Apply damage to ship_id
    let dmg = DamageVector {
        em: 50.0,
        thermal: 50.0,
        kinetic: 50.0,
        explosive: 50.0,
    };
    let res = park.apply_damage_to_ball(ship_id, dmg);
    assert!(res.is_some());

    // Verify observer received OnDamageStateChange
    let obs_events = park.drain_observer_events(observer_id);
    let damage_event = obs_events.iter().find(|e| {
        matches!(
            e,
            DestinyEvent::OnDamageStateChange {
                ball_id: id,
                ..
            } if *id == ship_id
        )
    });
    assert!(
        damage_event.is_some(),
        "Observer must receive OnDamageStateChange when target ship takes damage"
    );

    if let Some(DestinyEvent::OnDamageStateChange {
        shield_ratio,
        armor_ratio,
        structure_ratio,
        ..
    }) = damage_event
    {
        assert!(*shield_ratio < 1.0, "Shield ratio must reflect taken damage");
        assert_eq!(*armor_ratio, 1.0, "Armor intact");
        assert_eq!(*structure_ratio, 1.0, "Structure intact");
    }
}

#[tokio::test]
async fn test_c3_death_pipeline_explosion_wreck_loot_pod_insurance_killmail() {
    // parity: plan:C-3
    // Death pipeline: ship explosion, wreck entity creation with SDE wreck type,
    // 50% loot drop / destroyed roll, capsule/pod ejection with session notification,
    // insurance payout calculation, killmail generation and persistence.

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let victim_ship_id = 100_000_001;
    let attacker_char_id = 90_000_002;
    let sys_id = 30_000_142;
    let death_pos = DVec3::new(10_000.0, 500.0, -2_000.0);

    // Seed victim character with known balance and active ship
    let initial_balance = IskCents::from_isk(5_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = initial_balance;
            ch.active_ship_id = victim_ship_id;
        }

        // Insure the ship with explicit insured payout
        if let Some(ship) = snap.items.get_mut(&victim_ship_id) {
            ship.insured_payout_isk = 180_000.0;
        }

        // Seed 10 cargo/fitted items inside victim ship to test 50% Loot Fairy roll
        for i in 1..=10 {
            let item_id = 880_000 + i;
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 34, // Tritanium
                    owner_id: char_id,
                    location_id: victim_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1_000,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });

    let initial_ship_items_count = ctx
        .db
        .with_snapshot(|snap| snap.items.values().filter(|it| it.location_id == victim_ship_id).count());

    // Execute the canonical death pipeline
    let result = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        victim_ship_id,
        attacker_char_id,
        death_pos,
        12345, // seed
    )
    .await
    .expect("execute_death_pipeline");

    // 1. SDE wreck type resolution
    let expected_wreck_type = ctx.sde.wrecks.get(&587).copied().unwrap_or(26557);
    assert_eq!(result.wreck_type_id, expected_wreck_type);

    // 2. 50% Loot Fairy split: items are divided between dropped and destroyed
    assert!(
        !result.loot_dropped_item_ids.is_empty(),
        "Loot Fairy must drop surviving items"
    );
    assert!(
        !result.loot_destroyed_item_ids.is_empty(),
        "Loot Fairy must destroy items"
    );
    assert_eq!(
        result.loot_dropped_item_ids.len() + result.loot_destroyed_item_ids.len(),
        initial_ship_items_count
    );

    // Verify surviving items are moved to the wreck item
    ctx.db.with_snapshot(|snap| {
        for &dropped_id in &result.loot_dropped_item_ids {
            let item = snap.items.get(&dropped_id).expect("dropped item in db");
            assert_eq!(item.location_id, result.wreck_item_id);
            assert_eq!(item.flag, InventoryFlag::Cargo);
        }
        for &destroyed_id in &result.loot_destroyed_item_ids {
            assert!(
                snap.items.get(&destroyed_id).is_none(),
                "Destroyed items must be removed"
            );
        }
    });

    // 3. Capsule ejection
    assert!(result.ejected_capsule_id.is_some());
    let cap_id = result.ejected_capsule_id.unwrap();
    ctx.db.with_snapshot(|snap| {
        let cap_item = snap.items.get(&cap_id).expect("capsule item in db");
        assert_eq!(cap_item.type_id, 670); // Capsule
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.active_ship_id, cap_id, "Character active ship updated to capsule");
    });

    // 4. Insurance payout credited to victim balance
    assert_eq!(result.insurance_payout_isk, 180_000.0);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        let expected_bal = initial_balance.checked_add(IskCents::from_isk(180_000)).unwrap();
        assert_eq!(ch.balance, expected_bal);

        let journal_entry = snap
            .wallet_journal
            .iter()
            .find(|j| j.character_id == char_id && j.entry_type == "insurance_payout");
        assert!(journal_entry.is_some(), "Wallet journal insurance payout recorded");
    });

    // 5. Killmail persistence
    ctx.db.with_snapshot(|snap| {
        let km = snap.killmails.get(&result.killmail_id).expect("killmail in db");
        assert_eq!(km.victim_character_id, char_id);
        assert_eq!(km.attacker_character_id, attacker_char_id);
        assert_eq!(km.solar_system_id, sys_id);
        assert_eq!(km.ship_type_id, 587);
    });

    // 6. Verify Wreck and Capsule balls spawned in Ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let wreck_ball = sys_handle
        .get_ball_snapshot(result.wreck_item_id)
        .await
        .unwrap()
        .expect("wreck ball in ballpark");
    assert_eq!(wreck_ball.1.type_id, expected_wreck_type);
    assert_eq!(wreck_ball.1.position, death_pos);

    let cap_ball = sys_handle
        .get_ball_snapshot(cap_id)
        .await
        .unwrap()
        .expect("capsule ball in ballpark");
    assert_eq!(cap_ball.1.type_id, 670);
}

#[tokio::test]
async fn test_c4_npc_spawns_ai_state_machine_and_dynamic_bounties() {
    // parity: plan:C-4
    // NPC belt/gate spawns from sde_npc_spawn_groups,
    // NPC AI state machine (idle -> chase -> orbit -> attack),
    // dynamic bounty payout based on NPC dogma bounty attribute.

    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let killer_char_id = 90_000_001;
    let belt_pos = DVec3::new(100_000.0, 0.0, 50_000.0);

    let mut spawn_mgr = NpcSpawnManager::new();

    // 1. Belt/gate spawn from SDE spawn group (e.g. spawn_group_id = 1)
    let spawned_ids = spawn_mgr
        .spawn_group(&ctx, sys_id, 1, belt_pos)
        .await
        .expect("spawn_group");
    assert!(!spawned_ids.is_empty(), "Must spawn NPC ships from SDE group 1");

    let npc_ball_id = spawned_ids[0];
    let npc_type_id = spawn_mgr.entities[&npc_ball_id].type_id;

    // Verify NPC ball exists in Ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let npc_ball = sys_handle
        .get_ball_snapshot(npc_ball_id)
        .await
        .unwrap()
        .expect("npc ball in ballpark");
    assert_eq!(npc_ball.1.type_id, npc_type_id);

    // 2. AI State Machine verification:
    // Initial state: Idle (no player nearby)
    assert_eq!(spawn_mgr.entities[&npc_ball_id].state, NpcAiState::Idle);

    // Spawn a player ship at 30 km (within 50 km aggro range)
    let player_ship_id = 700_001;
    let player_pos = belt_pos + DVec3::new(30_000.0, 0.0, 0.0);
    let player_ball = Ball {
        id: player_ship_id,
        type_id: 587,
        position: player_pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(player_ball, ctx.build_type_combat_state(587))
        .await;

    // Step AI: NPC detects player and transitions to Chase
    spawn_mgr.step_ai(&ctx, sys_id, 1, 1.0).await;
    assert_eq!(
        spawn_mgr.entities[&npc_ball_id].state,
        NpcAiState::Chase,
        "NPC must switch to Chase when hostile player is within 50 km"
    );

    // Move player ship into orbit/attack range (5 km)
    let close_pos = belt_pos + DVec3::new(5_000.0, 0.0, 0.0);
    sys_handle.set_ball_position(player_ship_id, close_pos).await;

    // Step AI: NPC transitions to Orbit and Attack
    spawn_mgr.step_ai(&ctx, sys_id, 3, 1.0).await;
    assert!(
        matches!(
            spawn_mgr.entities[&npc_ball_id].state,
            NpcAiState::Orbit | NpcAiState::Attack
        ),
        "NPC must transition to Orbit/Attack when player is in close range"
    );

    // 3. Dynamic Bounty payout based on SDE dogma attribute (ENTITY_KILL_BOUNTY 277)
    let sde_bounty = ctx
        .sde
        .types
        .get(&npc_type_id)
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY).copied())
        .unwrap_or(25_000.0);

    let initial_killer_balance = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_char_id].balance);

    let awarded_bounty = spawn_mgr
        .handle_npc_destroyed(&ctx, sys_id, npc_ball_id, killer_char_id, 1_700_000_000)
        .expect("handle_npc_destroyed");

    assert_eq!(
        awarded_bounty, sde_bounty,
        "Awarded bounty must match SDE attribute 277, not a hardcoded 100k"
    );

    let final_killer_balance = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_char_id].balance);
    assert_eq!(
        final_killer_balance,
        initial_killer_balance
            .checked_add(IskCents::from_f64_isk(sde_bounty))
            .unwrap()
    );

    // Verify 20-minute respawn cooldown was logged
    assert!(spawn_mgr.respawn_timers.contains_key(&(sys_id, 1)));
}



pub(crate) mod pve_ext_2 {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for Phase 5 (Part 2): Salvage, Tractor Beam, Drones & CrimeWatch (`C-7, C-8`).


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_server::combat::crimewatch::{AggressionEvaluation, CrimeWatchDirector};
use eve_server::combat::drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, DroneController,
    SalvageOutcome,
};
use eve_server::formulas::concord_response_delay_sec;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_c7_salvage_mechanics_tractor_beam_and_combat_drones() {
    // parity: plan:C-7
    // Salvage mechanics (salvager chance), tractor beam (pull wreck toward ship),
    // combat drone bandwidth & control (launch, recall, engage).

    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Set ship type to 593 (Tristan) which has 25 Mbit/s drone bandwidth in SDE
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 593;
        }
    });

    // Spawn player ship in space
    let ship_pos = DVec3::new(0.0, 0.0, 0.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 593,
        position: ship_pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(593))
        .await;

    // 1. Salvage mechanics
    let wreck_item_id = 991_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_item_id,
            InventoryItemRecord {
                item_id: wreck_item_id,
                type_id: 26557,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let wreck_pos = DVec3::new(2_000.0, 0.0, 0.0); // 2 km away (within 5 km salvager range)
    let wreck_ball = Ball {
        id: wreck_item_id,
        type_id: 26557,
        position: wreck_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(wreck_ball, ctx.build_type_combat_state(26557))
        .await;

    // Salvage attempt with failing roll (0.90 >= chance) -> wreck remains intact
    let fail_res =
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_item_id, 0.30, 0.90).await;
    assert_eq!(
        fail_res.unwrap(),
        SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials"
        }
    );
    assert!(sys_handle.get_ball_snapshot(wreck_item_id).await.unwrap().is_some());

    // Salvage attempt with succeeding roll (0.10 < chance) -> yields materials and consumes wreck
    let success_res =
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_item_id, 0.30, 0.10).await;
    match success_res.unwrap() {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert!(!yielded_salvage_type_ids.is_empty());
            assert!(!yielded_quantities.is_empty());
        }
        _ => panic!("Expected salvage success"),
    }
    assert!(
        sys_handle.get_ball_snapshot(wreck_item_id).await.unwrap().is_none(),
        "Wreck ball must be removed on successful salvage"
    );

    // 2. Tractor Beam pull simulation
    let far_wreck_pos = DVec3::new(15_000.0, 0.0, 0.0); // 15 km away
    let (pos_after_5s, arrived_5s) =
        step_tractor_beam(far_wreck_pos, ship_pos, 500.0, 5.0); // pulled 2500 m closer
    assert!(!arrived_5s);
    assert!((pos_after_5s.length() - 12_500.0).abs() < 1.0);

    let (pos_arrived, arrived) =
        step_tractor_beam(pos_after_5s, ship_pos, 500.0, 30.0); // arrives at stop distance
    assert!(arrived);
    assert!(pos_arrived.length() <= 1_201.0);

    // 3. Combat Drone Bandwidth & Control
    let valid_light_drones = vec![2454, 2454, 2454, 2454, 2454]; // 5x Hobgoblin I
    let bw_res = validate_drone_bandwidth(&ctx, ship_id, &valid_light_drones, &[]);
    assert!(bw_res.is_ok());

    let too_many_drones = vec![2454, 2454, 2454, 2454, 2454, 2454]; // 6 drones
    let count_err = validate_drone_bandwidth(&ctx, ship_id, &too_many_drones, &[]);
    assert!(count_err.is_err());

    let heavy_drones = vec![2444, 2444]; // 2x Ogre I (25 Mbit/s each = 50 Mbit/s > 25 Mbit/s limit)
    let bw_err = validate_drone_bandwidth(&ctx, ship_id, &heavy_drones, &[]);
    assert!(bw_err.is_err());

    // Drone Launch, Engage, and Recall
    let mut drone_ctrl = DroneController::new();
    let d1 = 890_001;
    let d2 = 890_002;
    ctx.db.with_mut_snapshot(|snap| {
        for &did in &[d1, d2] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454, // Hobgoblin I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });

    let launched_ids = drone_ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[d1, d2])
        .await
        .expect("launch_drones");
    assert_eq!(launched_ids.len(), 2);
    assert_eq!(drone_ctrl.active_drones.len(), 2);

    // Engage target
    let target_ball_id = 999_001;
    let target_ball = Ball {
        id: target_ball_id,
        type_id: 587,
        position: DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(target_ball, ctx.build_type_combat_state(587))
        .await;

    let engaged_count = drone_ctrl
        .engage_target(&ctx, sys_id, target_ball_id)
        .await
        .expect("engage_target");
    assert_eq!(engaged_count, 2);

    // Recall drones back to bay
    let recalled = drone_ctrl
        .recall_drones(&ctx, sys_id, ship_id)
        .await
        .expect("recall_drones");
    assert_eq!(recalled.len(), 2);
    assert!(drone_ctrl.active_drones.is_empty());
    ctx.db.with_snapshot(|snap| {
        for &did in &[d1, d2] {
            assert_eq!(snap.items[&did].location_id, ship_id);
            assert_eq!(snap.items[&did].flag, InventoryFlag::DroneBay);
        }
    });
}

#[tokio::test]
async fn test_c8_crimewatch_state_machine_timers_flags_and_concord() {
    // parity: plan:C-8
    // CrimeWatch state machine (weapon timer, PvP aggression timer,
    // suspect/criminal flags, CONCORD response timer by system security status).

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut cw_director = CrimeWatchDirector::new();
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142; // Jita 0.95
    let now = 1_700_000_000;

    // 1. Weapon timer: 60s
    cw_director.trigger_weapon_timer(char_id, now);
    let state = cw_director.get_or_create(char_id);
    assert!(state.is_weapon_timer_active(now + 30));
    assert!(!state.is_weapon_timer_active(now + 61));

    // 2. CONCORD response timer mapping by security status
    assert_eq!(concord_response_delay_sec(1.0), 6.0);
    assert_eq!(concord_response_delay_sec(0.9), 6.0);
    assert_eq!(concord_response_delay_sec(0.8), 7.0);
    assert_eq!(concord_response_delay_sec(0.7), 10.0);
    assert_eq!(concord_response_delay_sec(0.6), 14.0);
    assert_eq!(concord_response_delay_sec(0.5), 19.0);
    assert!(concord_response_delay_sec(0.4).is_infinite());

    // 3. Low-sec unprovoked attack -> Suspect flag (900s), no CONCORD
    let lowsec_eval = cw_director.handle_combat_aggression(
        char_id,
        victim_char_id,
        ship_id,
        30_002_813, // Tama 0.3
        0.3,
        false,
        now,
    );
    assert_eq!(lowsec_eval, AggressionEvaluation::SuspectFlagTriggered);
    assert!(cw_director.get_or_create(char_id).is_suspect(now + 300));
    assert!(!cw_director.get_or_create(char_id).is_criminal(now));

    // 4. High-sec unprovoked attack -> Criminal flag + CONCORD response
    let highsec_eval = cw_director.handle_combat_aggression(
        char_id,
        victim_char_id,
        ship_id,
        sys_id,
        0.95,
        false,
        now,
    );
    match highsec_eval {
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec,
            arrival_unix,
        } => {
            assert_eq!(concord_delay_sec, 6.0);
            assert_eq!(arrival_unix, now + 6);
        }
        _ => panic!("Expected CriminalFlagTriggered"),
    }
    assert!(cw_director.get_or_create(char_id).is_criminal(now + 300));

    // 5. CONCORD arrival after security-based delay:
    // Spawns real CONCORD ships and executes criminal through the canonical death pipeline (C-3)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let crim_pos = DVec3::new(50_000.0, 0.0, 0.0);
    let crim_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: crim_pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(crim_ball, ctx.build_type_combat_state(587))
        .await;

    // Advance clock to CONCORD arrival (now + 6s)
    let reports = cw_director.step_concord(&ctx, now + 6, 9999).await;
    assert_eq!(reports.len(), 1);

    let report = &reports[0];
    assert_eq!(report.criminal_char_id, char_id);
    assert_eq!(report.victim_char_id, victim_char_id);

    // Verify real CONCORD Police Battleship was spawned in space
    let concord_ball = sys_handle
        .get_ball_snapshot(report.concord_ball_id)
        .await
        .unwrap()
        .expect("concord ball in ballpark");
    assert_eq!(concord_ball.1.type_id, 11957); // CONCORD Police Commander

    // Verify death pipeline was executed: criminal ship destroyed, wreck created, killmail persisted
    let death = report.death_result.as_ref().expect("death pipeline result");
    assert!(death.wreck_item_id > 0);
    assert!(death.killmail_id > 0);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    // Verify 30-day Kill Right granted to victim
    ctx.db.with_snapshot(|snap| {
        let kr = snap.kill_rights.get(&report.kill_right_id).expect("kill right in db");
        assert_eq!(kr.owner_id, victim_char_id);
        assert_eq!(kr.target_id, char_id);
        assert_eq!(kr.activation_fee, IskCents(0));
        assert!(!kr.is_active_suspect);
    });
}
}

#[allow(dead_code, unused_imports)]
pub(crate) mod combat_c3_c4_escalations {
    use super::*;

//  Integration tests closing `plan:C-3`, `plan:C-4`, and their 8 combat/lifecycle/NPC parity rows.
// 
// parity: plan:C-3, plan:C-4
// parity: feat:lifecycle.death-session-change, feat:lifecycle.wreck-owner-victim
// parity: feat:lifecycle.pod-death-pipeline-corpse, feat:lifecycle.wreck-abandon
// parity: feat:lifecycle.jetcan-empty, feat:lifecycle.courier-package-destroyed
// parity: feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns


use std::collections::HashMap;
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::{execute_death_pipeline, execute_death_pipeline_sync};
use eve_server::combat::npc_ai::{
    clone_soldier_type_for_security, clone_soldier_type_for_security_and_faction, regional_pirate_faction_id,
};
use eve_server::router::{BoundObjectEntry, ServerContext};
use eve_server::services::combat_crimewatch::combat_npc_ai::{
    destroy_specialized_npc_and_queue_bounty, record_npc_threat_event, spawn_specialized_npc_entity, step_extended_npc_ai_sync,
};
use eve_server::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction;
use eve_server::services::combat_crimewatch::{
    step_combat_system, with_combat_runtime, with_mut_combat_runtime, InFlightMissileState,
};
use glam::DVec3;

fn new_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn new_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.5, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }
}

#[tokio::test]
async fn test_c3_ship_death_session_change_timer_and_wreck_owner_victim() {
    // parity: feat:lifecycle.death-session-change, feat:lifecycle.wreck-owner-victim, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.death-session-change",
        "feat:lifecycle.wreck-owner-victim",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let (victim_id, attacker_id, looter_id) = (90_000_001i64, 90_000_002i64, 90_000_003i64);
    let (victim_ship_id, looter_ship_id) = (610_001_001i64, 610_001_003i64);

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, corp_id) in &[
            (victim_id, victim_ship_id, 2_000_101i64),
            (attacker_id, 610_001_002i64, 2_000_102i64),
            (looter_id, looter_ship_id, 2_000_103i64),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.corporation_id = corp_id;
            }
            snap.items.insert(sid, new_item(sid, 587, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        for i in 0..20 {
            let iid = 610_002_000 + i;
            snap.items.insert(iid, new_item(iid, 34, victim_id, victim_ship_id, InventoryFlag::Cargo, 100, false));
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &sid in &[victim_ship_id, looter_ship_id] {
        let _ = sys_handle
            .spawn_ball_with_combat(new_ball(sid, 587, DVec3::new(1_000.0, 0.0, 0.0)), ctx.build_type_combat_state(587))
            .await;
    }

    let mut victim_sess = SessionState::default();
    victim_sess.session_id = 1;
    victim_sess.user_id = Some(1);
    victim_sess.character_id = Some(victim_id);
    victim_sess.solar_system_id = Some(sys_id);
    victim_sess.ship_id = Some(victim_ship_id);

    let mut notify_rx2 = ctx.notify_tx.subscribe();
    let now_unix = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, 1, Some(victim_id), true, now_unix);
    let _ = ctx.db.acquire_online_lock(2, 1, Some(victim_id), true, now_unix);
    let outcome = execute_death_pipeline(
        &ctx,
        sys_id,
        victim_id,
        victim_ship_id,
        attacker_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        now_unix,
    )
    .await
    .expect("ship death pipeline must succeed");

    let capsule_id = outcome.ejected_capsule_id.expect("Rifter destruction must eject a capsule");
    assert_ne!(capsule_id, victim_ship_id);

    // 1. OnSessionChanged notification must reflect shipid -> capsule_id across both session streams
    let notifs = ctx.drain_notifications_for_char(victim_id);
    let saw_session_change = notifs.iter().any(|n| {
        n.event == "OnSessionChanged"
            && format!("{:?}", n.args).contains("shipid")
            && format!("{:?}", n.args).contains(&capsule_id.to_string())
    });
    assert!(saw_session_change, "OnSessionChanged must be emitted for ship death -> capsule");
    let mut saw_rx2 = false;
    for _ in 0..64 {
        let Ok((cid, n)) = notify_rx2.try_recv() else { break };
        if cid == victim_id && n.event == "OnSessionChanged" && format!("{:?}", n.args).contains(&capsule_id.to_string()) {
            saw_rx2 = true;
        }
    }
    assert!(saw_rx2, "Second session stream must also receive OnSessionChanged");

    // 2. Session change timer (10s) must block immediate CmdDock even if session still has old ship_id
    let bid = ctx.bind_object(&mut victim_sess, "beyonce", sys_id as i64).expect("bind beyonce");
    let dock_err = ctx
        .dispatch_rpc(&mut victim_sess, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(60_003_760), PyRep::Integer(capsule_id)], &HashMap::new(), None)
        .await
        .expect_err("CmdDock during 10s death sessionChangeTimer must fail");
    assert_eq!(dock_err.code, "SessionChangeInProgress");

    // 3. Wreck owner must be the VICTIM (`feat:lifecycle.wreck-owner-victim`), not the attacker
    let (wreck_owner, dropped_owners_all_victim) = ctx.db.with_snapshot(|snap| {
        let w = snap.items.get(&outcome.wreck_item_id).expect("wreck item exists");
        let children_ok = snap.items.values().filter(|i| i.location_id == outcome.wreck_item_id).all(|i| i.owner_id == victim_id);
        (w.owner_id, children_ok)
    });
    assert_eq!(wreck_owner, victim_id, "Player ship wreck must be owned by victim");
    assert!(dropped_owners_all_victim, "Dropped items in victim wreck must be owned by victim");

    let slim_owner = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ball_slim_meta
        .get(&outcome.wreck_item_id)
        .expect("wreck slim meta present")
        .owner_id;
    assert_eq!(slim_owner, victim_id, "Wreck SlimItem owner_id must equal victim_id");

    // 4. Third-party looter taking from victim's wreck triggers Crimewatch Suspect flag
    let mut looter_sess = SessionState::default();
    looter_sess.user_id = Some(3);
    looter_sess.character_id = Some(looter_id);
    looter_sess.solar_system_id = Some(sys_id);
    looter_sess.ship_id = Some(looter_ship_id);

    let loot_res = ctx
        .dispatch_rpc(&mut looter_sess, "invbroker", None, "LootAll", &[PyRep::Integer(outcome.wreck_item_id)], &HashMap::new(), None)
        .await
        .expect("LootAll by third party with Red safety must succeed");
    assert_eq!(
        loot_res.dict_get("suspectFlagTriggered").and_then(|v| v.as_bool()),
        Some(true),
        "Looting a victim-owned player wreck must trigger Suspect flag"
    );
    let is_suspect = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(looter_id).is_suspect(now_unix);
    assert!(is_suspect, "Looter must hold an active Crimewatch Suspect timer");

    // 5. Verify inline handle_ship_or_npc_destruction also sets wreck owner to victim (`P1-3`)
    //    and non-ship balls (wreck/drone) do NOT trigger capsuleer death pipeline (`P0-1`)
    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&victim_id).unwrap().balance);
    assert!(execute_death_pipeline_sync(&ctx, sys_id, victim_id, outcome.wreck_item_id, attacker_id, DVec3::ZERO, now_unix).is_err());
    let (_, wreck_ball) = sys_handle.get_ball_snapshot(outcome.wreck_item_id).await.unwrap().unwrap();
    let (_, inline_w_id, _, _) = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_001_002, 587, 2905, outcome.wreck_item_id, &wreck_ball, Some((victim_id, outcome.wreck_item_id)), sys_id, 500.0, now_unix).await;
    assert_eq!(inline_w_id, 0, "Destroying a wreck must not spawn another wreck or pod the owner");
    assert_eq!(ctx.db.with_snapshot(|s| (s.characters.get(&victim_id).unwrap().active_ship_id, s.characters.get(&victim_id).unwrap().balance)), (capsule_id, bal_before));
    let ship2_id = 610_001_099i64;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = ship2_id;
        s.items.insert(ship2_id, new_item(ship2_id, 587, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let ship2_ball = new_ball(ship2_id, 587, DVec3::new(1_200.0, 0.0, 0.0));
    let _ = sys_handle.spawn_ball_with_combat(ship2_ball.clone(), ctx.build_type_combat_state(587)).await;
    let (_, inline_ship_wreck_id, _, _) = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_001_002, 587, 2905, ship2_id, &ship2_ball, Some((victim_id, ship2_id)), sys_id, 1000.0, now_unix).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&inline_ship_wreck_id).unwrap().owner_id), victim_id);
}

#[tokio::test]
async fn test_c3_pod_death_corpse_implants_and_home_station_respawn() {
    // parity: feat:lifecycle.pod-death-pipeline-corpse, feat:lifecycle.death-session-change, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.pod-death-pipeline-corpse",
        "feat:lifecycle.death-session-change",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let home_station_id = 60_003_760i64;
    let (victim_id, attacker_id) = (90_000_001i64, 90_000_002i64);
    let (pod_id, implant_item_id) = (610_010_001i64, 610_010_099i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = pod_id;
            ch.home_station_id = home_station_id;
        }
        snap.items.insert(pod_id, new_item(pod_id, 670, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.active_implants.insert(victim_id, vec![9899]);
        snap.items.insert(implant_item_id, new_item(implant_item_id, 10208, victim_id, victim_id, InventoryFlag::Implant, 1, true));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(new_ball(pod_id, 670, DVec3::new(2_500.0, 500.0, -250.0)), ctx.build_type_combat_state(670))
        .await;

    {
        let mut bo = ctx.bound_objects.write().unwrap_or_else(|e| e.into_inner());
        bo.insert(
            777_001,
            BoundObjectEntry { bound_id: 777_001, session_id: 1, user_id: 1, character_id: victim_id, service: "beyonce".into(), object_id: sys_id as i64, oid_str: "N=888444:777001".into(), ref_id: 133_000_000_000_000_000 },
        );
        bo.insert(
            777_002,
            BoundObjectEntry { bound_id: 777_002, session_id: 1, user_id: 1, character_id: victim_id, service: "scanMgr".into(), object_id: sys_id as i64, oid_str: "N=888444:777002".into(), ref_id: 133_000_000_000_000_000 },
        );
    }

    let now_unix = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, 1, Some(victim_id), true, now_unix);
    let outcome = execute_death_pipeline(&ctx, sys_id, victim_id, pod_id, attacker_id, DVec3::new(2_500.0, 500.0, -250.0), now_unix)
        .await
        .expect("pod death pipeline must succeed");

    assert!(outcome.ejected_capsule_id.is_none() && outcome.wreck_type_id == 25);

    // 1. Corpse (type_id 25) spawned in space and Ballpark; NO 26557 wreck spawned
    let (corpse_rec, any_26557_wreck, implants_cleared) = ctx.db.with_snapshot(|snap| {
        let corpse = snap.items.get(&outcome.wreck_item_id).cloned().expect("corpse item exists");
        let has_wreck = snap.items.values().any(|i| i.item_id == outcome.wreck_item_id && i.type_id == 26557);
        let imp_cleared = !snap.active_implants.contains_key(&victim_id) && !snap.items.contains_key(&implant_item_id);
        (corpse, has_wreck, imp_cleared)
    });
    assert_eq!(corpse_rec.type_id, 25, "Pod death must spawn a Corpse (type_id 25)");
    assert_eq!(corpse_rec.flag, InventoryFlag::None);
    assert_eq!(corpse_rec.location_id, sys_id as i64);
    assert!(!any_26557_wreck, "Pod death must not spawn a 26557 ship wreck");
    assert!(implants_cleared, "Pod death must destroy all plugged-in implants");

    let dkm = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&outcome.killmail_id).cloned())
        .expect("detailed killmail recorded");
    assert!(
        dkm.destroyed_items.iter().any(|i| i.type_id == 9899)
            && dkm.destroyed_items.iter().any(|i| i.type_id == 10208),
        "Pod killmail must list destroyed implants"
    );

    let (_, corpse_ball) = sys_handle.get_ball_snapshot(outcome.wreck_item_id).await.unwrap().expect("corpse ball in ballpark");
    assert_eq!(corpse_ball.type_id, 25);
    let corpse_slim = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ball_slim_meta
        .get(&outcome.wreck_item_id)
        .cloned()
        .expect("corpse slim meta");
    assert_eq!((corpse_slim.group_id, corpse_slim.category_id), (14, 2));

    // 2. Victim respawned at home_station_id in a fresh Capsule (670) and space bound objects released
    let (station_after, respawn_ship_id, respawn_ship_type) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&victim_id).expect("victim exists");
        let stype = snap.items.get(&ch.active_ship_id).map(|i| i.type_id).unwrap_or(0);
        (ch.station_id, ch.active_ship_id, stype)
    });
    assert_eq!(station_after, Some(home_station_id));
    assert_eq!(respawn_ship_type, 670);
    {
        let bo = ctx.bound_objects.read().unwrap_or_else(|e| e.into_inner());
        assert!(!bo.contains_key(&777_001) && !bo.contains_key(&777_002));
    }

    let notifs = ctx.drain_notifications_for_char(victim_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnMachoObjectDisconnect" && n.args.len() == 3 && n.args[1] == PyRep::Integer(1_000_000_000_001))
            && notifs.iter().any(|n| {
                let s = format!("{:?}", n.args);
                n.event == "OnSessionChanged" && s.contains("stationid") && s.contains(&home_station_id.to_string()) && s.contains("solarsystemid2") && s.contains(&respawn_ship_id.to_string())
            }),
        "Pod death must emit 3-arg OnMachoObjectDisconnect and OnSessionChanged with stationid/solarsystemid2/shipid"
    );
}

#[tokio::test]
async fn test_c3_wreck_abandon_and_empty_jetcan_despawn() {
    // parity: feat:lifecycle.wreck-abandon, feat:lifecycle.jetcan-empty, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.wreck-abandon",
        "feat:lifecycle.jetcan-empty",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let (owner_id, other_id) = (90_000_001i64, 90_000_002i64);
    let (owner_ship_id, other_ship_id) = (610_020_001i64, 610_020_002i64);
    let (wreck_id, wreck_loot_id) = (610_020_100i64, 610_020_101i64);
    let (jetcan_a_item, jetcan_b_item) = (610_020_201i64, 610_020_202i64);

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, corp_id) in &[(owner_id, owner_ship_id, 2_000_201i64), (other_id, other_ship_id, 2_000_202i64)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.corporation_id = corp_id;
            }
            snap.items.insert(sid, new_item(sid, 587, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        snap.items.insert(wreck_id, new_item(wreck_id, 26557, owner_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(wreck_loot_id, new_item(wreck_loot_id, 34, owner_id, wreck_id, InventoryFlag::Cargo, 50, false));
        snap.items.insert(jetcan_a_item, new_item(jetcan_a_item, 35, owner_id, owner_ship_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(jetcan_b_item, new_item(jetcan_b_item, 35, owner_id, owner_ship_id, InventoryFlag::Cargo, 20, false));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(bid, tid) in &[(owner_ship_id, 587u32), (other_ship_id, 587u32), (wreck_id, 26557u32)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(bid, tid, DVec3::ZERO), ctx.build_type_combat_state(tid)).await;
    }

    let _ = ctx.db.acquire_online_lock(1, 1, Some(owner_id), true, ctx.now_unix());
    let mut owner_sess = SessionState::default();
    owner_sess.session_id = 1;
    owner_sess.user_id = Some(1);
    owner_sess.character_id = Some(owner_id);
    owner_sess.solar_system_id = Some(sys_id);
    owner_sess.ship_id = Some(owner_ship_id);

    let mut other_sess = SessionState::default();
    other_sess.user_id = Some(2);
    other_sess.character_id = Some(other_id);
    other_sess.solar_system_id = Some(sys_id);
    other_sess.ship_id = Some(other_ship_id);

    // 1. Non-owner cannot abandon wreck
    let non_owner_err = ctx
        .dispatch_rpc(&mut other_sess, "beyonce", None, "CmdAbandonLoot", &[PyRep::List(vec![PyRep::Integer(wreck_id)])], &HashMap::new(), None)
        .await
        .expect_err("non-owner CmdAbandonLoot must fail");
    assert_eq!(non_owner_err.code, "AccessDenied");

    // 2. Owner abandons wreck via beyonce.CmdAbandonLoot (list argument) -> OnSlimItemChange & no Suspect on loot
    ctx.dispatch_rpc(&mut owner_sess, "beyonce", None, "CmdAbandonLoot", &[PyRep::List(vec![PyRep::Integer(wreck_id)])], &HashMap::new(), None)
        .await
        .expect("owner CmdAbandonLoot must succeed");

    let notifs = ctx.drain_notifications_for_char(owner_id);
    let slim_notif = notifs.iter().find(|n| n.event == "OnSlimItemChange").expect("CmdAbandonLoot must emit OnSlimItemChange");
    assert_eq!(slim_notif.args.len(), 2, "OnSlimItemChange must have (oldSlim, newSlim)");
    assert_eq!(slim_notif.args[0].dict_get("ownerID"), Some(&PyRep::Integer(owner_id)));
    assert_eq!(slim_notif.args[1].dict_get("ownerID"), Some(&PyRep::Integer(0)));
    assert_eq!(slim_notif.args[1].dict_get("typeID"), Some(&PyRep::Integer(26557)));

    // Other player loots abandoned wreck via invbroker.Add without Suspect penalty
    let add_res = ctx
        .dispatch_rpc(
            &mut other_sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(wreck_loot_id), PyRep::Integer(other_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("looting abandoned wreck via invbroker.Add must succeed");
    assert_eq!(add_res.dict_get("suspectFlagged").and_then(|v| v.as_bool()), Some(false));

    // 3. Jettison two items into a jetcan, partial loot keeps can, emptying last item despawns can immediately
    let jet_res = ctx
        .dispatch_rpc(&mut owner_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(jetcan_a_item), PyRep::Integer(jetcan_b_item)])], &HashMap::new(), None)
        .await
        .expect("ship.Jettison must succeed");
    let can_id = match &jet_res {
        PyRep::Tuple(t) => match t.first() {
            Some(PyRep::List(l)) => l.first().and_then(|v| v.as_i64()).expect("jetcan id in list"),
            _ => panic!("unexpected Jettison tuple: {jet_res:?}"),
        },
        _ => panic!("unexpected Jettison response: {jet_res:?}"),
    };

    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_some());
    for _ in 0..3 {
        let _ = sys_handle.step_manual(1.0).await;
    }

    // Move first item out -> jetcan still has 1 item, so it must remain
    ctx.dispatch_rpc(&mut owner_sess, "invbroker", None, "Add", &[PyRep::Integer(jetcan_a_item), PyRep::Integer(owner_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::new(), None)
        .await
        .expect("partial loot from jetcan must succeed");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&can_id)));
    let (expected_tick, _) = sys_handle.get_ball_snapshot(can_id).await.unwrap().unwrap();
    assert!(expected_tick >= 3);
    let _ = ctx.drain_notifications_for_char(owner_id);

    // Move last item out -> jetcan is now empty and must despawn immediately from DB and Ballpark with real tick stamp
    ctx.dispatch_rpc(&mut owner_sess, "invbroker", None, "Add", &[PyRep::Integer(jetcan_b_item), PyRep::Integer(owner_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::new(), None)
        .await
        .expect("looting final item from jetcan must succeed");

    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&can_id)));
    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_none());
    let jet_notifs = ctx.drain_notifications_for_char(owner_id);
    assert!(jet_notifs.iter().any(|n| n.event == "DoDestinyUpdate" && format!("{:?}", n.args).contains("RemoveBall") && format!("{:?}", n.args).contains(&format!("Integer({expected_tick})"))));
}

#[tokio::test]
async fn test_c3_courier_package_destroyed_and_missile_death_pipeline() {
    // parity: feat:lifecycle.courier-package-destroyed, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.courier-package-destroyed",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let start_station_id = 60_003_760i64;
    let (issuer_id, hauler_id, attacker_id) = (90_000_001i64, 90_000_002i64, 90_000_003i64);
    let (hauler_ship_id, wrap_item_id, wrapped_cargo_id) = (610_030_001i64, 610_030_100i64, 610_030_101i64);
    let (contract_id, zero_collat_contract_id, zero_collat_item_id) = (710_001i64, 710_002i64, 610_030_200i64);
    let (jet_can_id, jet_wrap_id, jet_cargo_id, jet_contract_id) = (610_030_300i64, 610_030_301i64, 610_030_302i64, 710_003i64);

    let issuer_initial_bal = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&hauler_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = hauler_ship_id;
        }
        let ibal = snap.characters.get(&issuer_id).map(|c| c.balance).unwrap_or(IskCents(0));
        snap.items.insert(hauler_ship_id, new_item(hauler_ship_id, 587, hauler_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(wrap_item_id, new_item(wrap_item_id, 3468, hauler_id, hauler_ship_id, InventoryFlag::Cargo, 1, true));
        snap.items.insert(wrapped_cargo_id, new_item(wrapped_cargo_id, 34, issuer_id, wrap_item_id, InventoryFlag::Locked, 500, false));
        snap.contracts.insert(contract_id, ContractRecord {
            contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(100_000.0), collateral: IskCents::from_f64_isk(5_000_000.0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![wrapped_cargo_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        snap.items.insert(jet_can_id, new_item(jet_can_id, 23, hauler_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(jet_wrap_id, new_item(jet_wrap_id, 3468, hauler_id, jet_can_id, InventoryFlag::Cargo, 1, true));
        snap.items.insert(jet_cargo_id, new_item(jet_cargo_id, 34, issuer_id, jet_wrap_id, InventoryFlag::Locked, 100, false));
        snap.contracts.insert(jet_contract_id, ContractRecord {
            contract_id: jet_contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(50_000.0), collateral: IskCents::from_f64_isk(1_000_000.0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![jet_cargo_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        snap.items.insert(zero_collat_item_id, new_item(zero_collat_item_id, 35, issuer_id, 60_008_494, InventoryFlag::Locked, 250, false));
        snap.contracts.insert(zero_collat_contract_id, ContractRecord {
            contract_id: zero_collat_contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(50_000.0), collateral: IskCents(0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![zero_collat_item_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        ibal
    });

    // 1. Test missile impact destroying hauler's ship, a jettisoned courier container, a drone, and an NPC
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut low_hp_cs = ctx.build_type_combat_state(587);
    low_hp_cs.shield_hp = 0.0;
    low_hp_cs.armor_hp = 0.0;
    low_hp_cs.structure_hp = 10.0;
    let _ = sys_handle.spawn_ball_with_combat(new_ball(hauler_ship_id, 587, DVec3::new(500.0, 0.0, 0.0)), low_hp_cs.clone()).await;
    let _ = sys_handle.spawn_ball_with_combat(new_ball(jet_can_id, 23, DVec3::new(700.0, 0.0, 0.0)), low_hp_cs.clone()).await;
    let npc_target_id = spawn_specialized_npc_entity(&ctx, sys_id, 33132, "CloneSoldier", 500_010, DVec3::new(600.0, 0.0, 0.0)).await;
    let _ = sys_handle.set_combat_state(npc_target_id, low_hp_cs.clone()).await;

    let (missile_id, drone_id, drone_missile_id, can_missile_id, npc_missile_id) = (699_001_001i64, 610_030_900i64, 699_001_002i64, 699_001_003i64, 699_001_004i64);
    ctx.db.with_mut_snapshot(|s| { s.items.insert(drone_id, new_item(drone_id, 2454, issuer_id, sys_id as i64, InventoryFlag::None, 1, true)); });
    let _ = sys_handle.spawn_ball_with_combat(new_ball(drone_id, 2454, DVec3::new(900.0, 0.0, 0.0)), low_hp_cs).await;
    for (mid, pos) in [(missile_id, 400.0), (drone_missile_id, 850.0), (can_missile_id, 650.0), (npc_missile_id, 550.0)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(mid, 204, DVec3::new(pos, 0.0, 0.0)), CombatState::default()).await;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        for (mid, tid) in [(missile_id, hauler_ship_id), (drone_missile_id, drone_id), (can_missile_id, jet_can_id), (npc_missile_id, npc_target_id)] {
            rt.in_flight_missiles.insert(mid, InFlightMissileState {
                missile_ball_id: mid, solar_system_id: sys_id, attacker_char_id: attacker_id, attacker_ship_id: 610_030_003,
                launcher_module_id: 610_030_004, launcher_type_id: 2410, missile_type_id: 204, target_ball_id: tid,
                max_velocity: 1_000.0, flight_time_s: 5.0, remaining_flight_s: 5.0, hp: 70.0, max_hp: 70.0,
                base_damage: DamageVector::new(100.0, 100.0, 100.0, 100.0), explosion_radius: 40.0, explosion_velocity: 150.0,
                drf: 0.6, is_fof: false, is_breacher_pod: false, breacher_dot_duration_s: 0.0, breacher_flat_cap: 0.0,
                breacher_pct_cap: 0.0, destroyed: false, expired: false, impacted: false, applied_damage: 0.0,
            });
        }
    });

    let issuer_ship_before = ctx.db.with_snapshot(|s| s.characters.get(&issuer_id).unwrap().active_ship_id);
    step_combat_system(&ctx, sys_id, 1.0).await;
    assert!(sys_handle.get_ball_snapshot(drone_id).await.unwrap().is_none(), "Destroyed drone ball must be removed");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&issuer_id).unwrap().active_ship_id), issuer_ship_before, "Destroying a player's drone must not pod the player");
    let sde_33132_bounty = ctx.sde.types.get(&33132).and_then(|t| t.default_attributes.get(&481).copied()).unwrap();
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, attacker_id)).copied()), Some(sde_33132_bounty), "Missile kill of NPC must queue exact 1x SDE bounty");
    let turret_npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 33132, "CloneSoldier", 500_010, DVec3::new(620.0, 0.0, 0.0)).await;
    let (_, _, turret_npc_ball) = sys_handle.apply_damage_with_attacker(turret_npc_id, DamageVector::new(50_000.0, 50_000.0, 50_000.0, 50_000.0), Some(attacker_id)).await.unwrap().unwrap();
    let _ = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_030_003, 587, 2905, turret_npc_id, &turret_npc_ball, None, sys_id, 50_000.0, ctx.now_unix()).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, attacker_id)).copied()), Some(sde_33132_bounty * 2.0), "Turret kill of NPC must queue exact 1x SDE bounty without doubling");

    // Verify Contract 1 & jettisoned Contract 3 failed, collateral paid to issuer, and no orphaned items remain
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.contracts.get(&contract_id).unwrap().status, "Failed", "Destroyed courier package must fail the contract");
        assert_eq!(snap.contracts.get(&jet_contract_id).unwrap().status, "Failed", "Destroyed jettisoned courier package must fail the contract");
        assert!(!snap.items.contains_key(&jet_wrap_id) && !snap.items.contains_key(&jet_cargo_id), "Destroyed jetcan must remove nested wrap and inner courier cargo without orphans");
        let issuer_after = snap.characters.get(&issuer_id).expect("issuer exists").balance;
        assert_eq!(
            issuer_after,
            issuer_initial_bal.checked_add(IskCents::from_f64_isk(6_150_000.0)).unwrap(),
            "Issuer must receive collateral payout + escrowed reward refund for both destroyed courier packages"
        );
        assert!(!snap.items.contains_key(&wrap_item_id), "Destroyed Plastic Wrap container must be removed");
        let hauler_wreck_id = snap.items.values().find(|i| i.owner_id == hauler_id && i.location_id == sys_id as i64 && i.type_id != 670).map(|i| i.item_id).expect("hauler wreck exists");
        let cargo_valid = match snap.items.get(&wrapped_cargo_id) {
            Some(cargo) => cargo.location_id == hauler_wreck_id && cargo.location_id != start_station_id,
            None => true,
        };
        assert!(cargo_valid, "Wrapped cargo must be either dropped in wreck or destroyed, never at start_station_id");

        // 2. Verify zero-collateral courier deadline expiry unwraps in-place for acceptor without teleporting to start_station_id
        snap.resolve_expired_contract(zero_collat_contract_id, 1_700_200_000)
            .expect("resolve_expired_contract must succeed");
        let c2 = snap.contracts.get(&zero_collat_contract_id).expect("contract 2 exists");
        assert_eq!(c2.status, "Failed");
        let item2 = snap.items.get(&zero_collat_item_id).expect("zero-collat item exists");
        assert_eq!(item2.owner_id, hauler_id, "Unwrapped courier item transfers to acceptor");
        assert_eq!(item2.flag, InventoryFlag::Hangar);
        assert_eq!(
            item2.location_id, 60_008_494,
            "Zero-collateral failed courier item must stay in-place, never teleport to start_station_id"
        );
    });
}

#[tokio::test]
async fn test_c4_npc_target_switching_and_clone_soldier_lowsec_spawns() {
    // parity: feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns, plan:C-4
    eve_testkit::attest(&[
        "feat:npc.npc-target-switching",
        "feat:npc.clone-soldier-spawns",
        "plan:C-4",
    ]);

    let ctx = ServerContext::new();

    // 1. Verify Clone Soldier security band gating (0.1..=0.4 low-sec only; never in high-sec or null-sec)
    for (sec, exp) in [(0.9, None), (0.5, None), (0.4, Some(33132)), (0.3, Some(33142)), (0.2, Some(33163)), (0.1, Some(33175)), (0.0, None), (-0.5, None)] {
        assert_eq!(clone_soldier_type_for_security(sec), exp);
    }

    let lowsec_sys_id = ctx.sde.solar_systems.values().find(|s| (0.10..0.45).contains(&(s.security_status as f64))).map(|s| s.solar_system_id).unwrap_or(30_002_537);
    let expected_faction = regional_pirate_faction_id(&ctx, lowsec_sys_id);
    assert_eq!(regional_pirate_faction_id(&ctx, 30_000_001), 500_019, "Derelik system must resolve to Sansha's Nation via SDE faction_id");
    assert!(clone_soldier_type_for_security_and_faction(&ctx.sde, 0.3, expected_faction).is_some());

    // Drive belt, gate, and Clone Soldier spawning through the real SP-7 production scheduler (`tick_background_cluster`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.npc_spawns.read().unwrap().get(&lowsec_sys_id).is_some_and(|m| !m.entities.is_empty()), "SP-7 tick must populate ctx.npc_spawns belt/gate waves");

    let (cs_ball_id, cs_npc) = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.iter().find(|(_, n)| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier").map(|(&id, n)| (id, n.clone()))
    }).expect("SP-7 tick must spawn a Clone Soldier in low-sec");
    assert_eq!((cs_npc.archetype.as_str(), cs_npc.faction_id), ("CloneSoldier", expected_faction));
    let tag_tid = cs_npc.tag_drop_type_id.expect("Clone Soldier must have a security tag drop");
    assert!(matches!(tag_tid, 33138..=33141) && ctx.sde.types.get(&tag_tid).map(|t| t.group_id) == Some(1206));
    assert!(matches!(ctx.sde.types.get(&cs_npc.type_id).map(|t| t.group_id), Some(551 | 552 | 556 | 566 | 574)));

    let cs_combat = lowsec_handle.get_combat_state(cs_ball_id).await.unwrap().expect("Clone Soldier CombatState present");
    let sde_combat = ctx.build_type_combat_state(cs_npc.type_id);
    assert_eq!((cs_combat.shield_resonance, cs_combat.armor_resonance), (sde_combat.shield_resonance, sde_combat.armor_resonance));

    let killer_id = 90_000_001i64;
    let (cs_wreck_id, bounty, dropped_types) = destroy_specialized_npc_and_queue_bounty(&ctx, lowsec_sys_id, cs_ball_id, killer_id).await;
    assert_eq!(bounty, ctx.sde.types.get(&cs_npc.type_id).and_then(|t| t.default_attributes.get(&481).copied()).unwrap_or(0.0));
    assert!(dropped_types.contains(&tag_tid) && ctx.db.with_snapshot(|snap| snap.items.values().any(|i| i.location_id == cs_wreck_id && i.type_id == tag_tid)));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| !rt.npc_entities.values().any(|n| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier")), "Clone Soldier must not respawn during 1200s cooldown");
    ctx.npc_spawns.write().unwrap().get_mut(&lowsec_sys_id).unwrap().clone_soldier_respawn_timers.insert(lowsec_sys_id, 0);
    with_mut_combat_runtime(&ctx, |rt| { rt.clone_soldier_cooldown_until.insert(lowsec_sys_id, 0); });
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier")), "Clone Soldier must respawn via SP-7 tick after cooldown expires");

    // 2. Test NpcSpawnManager::step_ai (Idle -> Chase -> Orbit/Attack), SP-7 timer scheduling, and target switching
    let sys_id = 30_000_144u32;
    let (ship_a, ship_b, drone_a, cap_c) = (610_040_001i64, 610_040_002i64, 610_040_003i64, 610_040_004i64);
    let (char_a, char_b, char_c) = (90_000_001i64, 90_000_002i64, 90_000_003i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c3) = snap.characters.get(&char_a).cloned() { c3.character_id = char_c; snap.characters.insert(char_c, c3); }
        for &(cid, sid, tid) in &[(char_a, ship_a, 587u32), (char_b, ship_b, 587u32), (char_c, cap_c, 19720u32)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id; ch.station_id = None; ch.active_ship_id = sid; ch.standings.insert(500_010, -5.0);
            }
            snap.items.insert(sid, new_item(sid, tid, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        snap.items.insert(drone_a, new_item(drone_a, 2454, char_a, sys_id as i64, InventoryFlag::DroneBay, 1, true));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(bid, tid) in &[(ship_a, 587u32), (ship_b, 587u32), (drone_a, 2454u32)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(bid, tid, DVec3::new(5_000.0, 0.0, 0.0)), ctx.build_type_combat_state(tid)).await;
    }
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let first_belt_rat = *ctx.npc_spawns.read().unwrap()[&sys_id].entities.keys().next().unwrap();
    let _ = ctx.npc_spawns.write().unwrap().get_mut(&sys_id).unwrap().handle_npc_destroyed(&ctx, sys_id, first_belt_rat, char_a, ctx.now_unix()).unwrap();
    assert!(!ctx.npc_spawns.read().unwrap()[&sys_id].respawn_timers.is_empty());

    let transporter_id = spawn_specialized_npc_entity(&ctx, sys_id, 33163, "CloneSoldier", 500_010, DVec3::new(900_000.0, 0.0, 0.0)).await;
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 33175, "CloneSoldier", 500_010, DVec3::ZERO).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities.get(&transporter_id).unwrap().web_range, 10_000.0, "Transporter webs at 10 km");
        assert_eq!(rt.npc_entities.get(&npc_id).unwrap().scram_range, 24_000.0, "Negotiator scrams at 24 km");
    });

    record_npc_threat_event(&ctx, npc_id, ship_a, 200.0, false, false);
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_a),
        "NPC must initially target Ship A"
    );

    record_npc_threat_event(&ctx, npc_id, ship_b, 300.0, true, false);
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_b),
        "NPC must switch target to higher-threat EWAR+DPS Ship B"
    );

    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cs) = bp.combat_states.get_mut(&ship_b) {
            cs.is_cloaked = true;
        }
    }
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_a),
        "NPC must drop cloaked Ship B (loss of lock) and switch back to Ship A"
    );

    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = bp.balls.get_mut(&ship_a) {
            b.mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000_000.0, 0.0, 0.0), 3.0);
        }
    }
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(drone_a),
        "NPC must drop warping Ship A and switch to remaining hostile player drone on grid"
    );

    // Target destruction & new capital arrival on grid -> switches from destroyed targets to newly arrived cap_c
    for bid in [ship_a, drone_a] { let _ = sys_handle.remove_ball(bid).await; }
    let _ = sys_handle.spawn_ball_with_combat(new_ball(cap_c, 19720, DVec3::new(6_000.0, 0.0, 0.0)), ctx.build_type_combat_state(19720)).await;
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)), Some(cap_c));
}

}

#[allow(unused_imports)]
pub(crate) mod pochven_loot_and_corp_audit {
//! End-to-end integration tests for Pochven loot drop, corporation project contribution
//! methods, corporation member role audit, and corporation structure reinforce default.
//!
//! Parity rows:
//! - `feat:combat.pochven-loot-drop`
//! - `feat:corporation.corp-project-contribution-methods`
//! - `feat:corporation.member-audit`
//! - `feat:corporation.structure-reinforce-default`


use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity, with_combat_runtime,
};
use glam::DVec3;

pub(super) const PILOT_1: i64 = 90_000_001;
pub(super) const PILOT_2: i64 = 90_000_002;

pub(super) async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

pub(super) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-corp-combat-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    if let PyRep::Bytes(b) = field(&rep, "response_proto") {
        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
    }
    panic!("Expected response_proto in SendRequest reply, got {rep:?}");
}

pub(super) fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    let inner = match rep {
        PyRep::Object { args, .. } => args.as_ref(),
        other => other,
    };
    match inner {
        PyRep::KeyVal(map) => {
            if let Some(v) = map.get(key) {
                return v;
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if k == &PyRep::String(key.into()) {
                    return v;
                }
            }
        }
        _ => {}
    }
    panic!("Missing field '{key}' in {rep:?}");
}

pub(super) fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    if let PyRep::Rowset { rows, .. } = rep {
        return rows.clone();
    }
    if let PyRep::Object { args, .. } = rep {
        if let PyRep::Dict(entries) = args.as_ref() {
            for (k, v) in entries {
                if k == &PyRep::String("lines".into()) {
                    if let PyRep::List(rows) = v {
                        return rows
                            .iter()
                            .filter_map(|r| match r {
                                PyRep::List(cols) => Some(cols.clone()),
                                _ => None,
                            })
                            .collect();
                    }
                }
            }
        }
    }
    Vec::new()
}

pub(super) async fn spawn_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
    base_item_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());

        if let Some(base_id) = base_item_id {
            for idx in 0..30i64 {
                let flag = match idx {
                    0 => InventoryFlag::HiSlot0,
                    1 => InventoryFlag::HiSlot1,
                    2 => InventoryFlag::MedSlot0,
                    3 => InventoryFlag::LoSlot0,
                    _ => InventoryFlag::Cargo,
                };
                let type_id = if idx < 4 { 2048 } else { 34 };
                let qty = if idx < 4 { 1 } else { 100 };
                snap.items.insert(
                    base_id + idx,
                    InventoryItemRecord {
                        item_id: base_id + idx,
                        type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: qty,
                        singleton: idx < 4,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(587);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 500.0;
        cs.shield_hp = 500.0;
        cs.max_armor_hp = 400.0;
        cs.armor_hp = 400.0;
        cs.max_structure_hp = 350.0;
        cs.structure_hp = 350.0;
    }
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_feat_combat_pochven_loot_drop_e2e() {
    // parity: feat:combat.pochven-loot-drop
    eve_testkit::attest(&["feat:combat.pochven-loot-drop"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let pochven_sys = 30_000_157u32; // Otela (region 10_000_070 Pochven)
    let jita_sys = 30_000_142u32; // Jita (region 10_000_002 The Forge)

    let attacker_pochven = PILOT_1;
    let victim_pochven = PILOT_2;
    let victim_ship_pochven = 104_000_102i64;

    let attacker_jita = PILOT_1;
    let victim_jita = PILOT_2;
    let victim_ship_jita = 104_000_202i64;

    // Identical item IDs for both ships isolate the 60% Pochven drop threshold vs 50% Jita threshold.
    spawn_ship_in_space(
        &ctx,
        victim_pochven,
        victim_ship_pochven,
        pochven_sys,
        DVec3::new(2_000.0, 0.0, 0.0),
        Some(210_000_000),
    )
    .await;
    let pochven_handle = ctx.systems.get_or_spawn(pochven_sys);
    let lethal = DamageVector {
        em: 50_000.0,
        thermal: 50_000.0,
        kinetic: 50_000.0,
        explosive: 50_000.0,
    };
    let hit_p = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &pochven_handle, pochven_sys, attacker_pochven, victim_ship_pochven, lethal, 1.0, ctx.now_unix(),
    ).await.expect("damage pochven ship").expect("hit pochven");
    assert!(hit_p.1, "Pochven victim ship must be destroyed by lethal combat damage");
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, pochven_sys, attacker_pochven, victim_ship_pochven, 587, DVec3::new(2_000.0, 0.0, 0.0), ctx.now_unix(),
    ).await;

    let (pochven_km_id, pochven_wreck_items) = ctx.db.with_snapshot(|s| {
        let km = s
            .killmails
            .values()
            .find(|k| k.solar_system_id == pochven_sys && k.victim_character_id == victim_pochven)
            .cloned()
            .expect("Pochven killmail must be recorded");
        let wreck = s
            .items
            .values()
            .find(|i| i.location_id == pochven_sys as i64 && i.owner_id == victim_pochven && i.item_id != victim_ship_pochven && s.items.values().any(|c| c.location_id == i.item_id))
            .map(|w| w.item_id)
            .expect("Pochven wreck must be spawned");
        let dropped: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == wreck)
            .map(|i| i.item_id)
            .collect();
        (km.kill_id, dropped)
    });
    let pochven_km_det = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&pochven_km_id).cloned())
        .expect("Pochven detailed killmail");

    spawn_ship_in_space(&ctx, victim_jita, victim_ship_jita, jita_sys, DVec3::new(2_000.0, 0.0, 0.0), Some(210_000_000)).await;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let hit_j = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &jita_handle, jita_sys, attacker_jita, victim_ship_jita, lethal, 1.0, ctx.now_unix(),
    ).await.expect("damage jita ship").expect("hit jita");
    assert!(hit_j.1, "Jita victim ship must be destroyed by lethal combat damage");
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, jita_sys, attacker_jita, victim_ship_jita, 587, DVec3::new(2_000.0, 0.0, 0.0), ctx.now_unix(),
    ).await;

    let (jita_km_id, jita_wreck_items) = ctx.db.with_snapshot(|s| {
        let km = s
            .killmails
            .values()
            .find(|k| k.solar_system_id == jita_sys && k.victim_character_id == victim_jita)
            .cloned()
            .expect("Jita killmail must be recorded");
        let wreck = s
            .items
            .values()
            .find(|i| i.location_id == jita_sys as i64 && i.owner_id == victim_jita && i.item_id != victim_ship_jita && i.item_id != pochven_km_id && s.items.values().any(|c| c.location_id == i.item_id))
            .map(|w| w.item_id)
            .expect("Jita wreck must be spawned");
        let dropped: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == wreck)
            .map(|i| i.item_id)
            .collect();
        (km.kill_id, dropped)
    });
    let jita_km_det = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&jita_km_id).cloned())
        .expect("Jita detailed killmail");

    assert!(
        pochven_wreck_items.len() > jita_wreck_items.len(),
        "Pochven 60% drop ({}) must drop strictly more items than Jita 50% drop ({})",
        pochven_wreck_items.len(),
        jita_wreck_items.len()
    );
    assert_eq!(
        pochven_km_det.dropped_items.len(),
        pochven_wreck_items.len(),
        "Pochven killmail dropped_items must match wreck contents"
    );
    assert_eq!(
        pochven_km_det.dropped_items.len() + pochven_km_det.destroyed_items.len(),
        30,
        "Pochven killmail must account for all 30 fitted + cargo items"
    );
    assert_eq!(
        jita_km_det.dropped_items.len(),
        jita_wreck_items.len(),
        "Jita killmail dropped_items must match wreck contents"
    );
}

#[tokio::test]
async fn test_feat_corporation_corp_project_contribution_methods_e2e() {
    // parity: feat:corporation.corp-project-contribution-methods
    eve_testkit::attest(&["feat:corporation.corp-project-contribution-methods"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_102, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Pochven Logistics Co".into()),
            PyRep::String("PLC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    let jita_sys = 30_000_142u32;
    let jita_station = 60_003_760i64;
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(500_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(500_000_000.0);
        corp.members.insert(PILOT_2);
        let p2 = s.characters.get_mut(&PILOT_2).unwrap();
        p2.corporation_id = corp_id;
        p2.balance = IskCents::from_f64_isk(500_000_000.0);
    });
    sess2.corporation_id = Some(corp_id);

    let (&bp_type_id, bp_def) = ctx
        .sde
        .blueprints
        .iter()
        .find(|(_, b)| {
            b.activities
                .get("manufacturing")
                .is_some_and(|a| !a.products.is_empty() && a.time <= 86_400)
        })
        .expect("SDE manufacturing blueprint");
    let mfg_act = &bp_def.activities["manufacturing"];
    let mfg_prod_type_id = mfg_act.products[0].type_id;

    let pid_npc = create_proj(&ctx, &mut sess1, "Kill Jita Rats", 10, "DestroyNPCShips", jita_sys as i64).await;
    let pid_mine = create_proj(&ctx, &mut sess1, "Mine Veldspar", 100, "MineMaterials", 1230).await;
    let pid_mfg = create_proj(&ctx, &mut sess1, "Build Item", 5, "ManufactureItems", mfg_prod_type_id as i64).await;
    let pid_deliv = create_proj(&ctx, &mut sess1, "Deliver Tritanium", 100, "DeliverItem", 34).await;
    let fw_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id != 10_000_070)
        .expect("lowsec system")
        .solar_system_id;
    let pid_fw = create_proj(&ctx, &mut sess1, "Capture FW", 5, "CaptureFWComplexes", fw_sys as i64).await;
    let pid_dmg = create_proj(&ctx, &mut sess1, "Damage Ships", 10_000, "DamageShips", jita_sys as i64).await;
    let pid_lp = create_proj(&ctx, &mut sess1, "Earn Caldari LP", 5_000, "EarnLoyaltyPoints", 1_000_180).await;

    // Client-reported GoalProgress must be rejected for server-tracked matchers
    let spoof_err = rpc(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GoalProgress",
        &[PyRep::Integer(pid_npc), PyRep::Integer(5)],
    )
    .await
    .expect_err("Client-reported count on server-tracked matcher must fail");
    assert_eq!(spoof_err.code, "ServerTrackedMatcher");

    // 1. Kill NPCs: matching system advances, non-matching system does not
    let npc_type_id = ctx.sde.types.iter().find(|(_, t)| t.category_id == 11 && t.published).map(|(&id, _)| id).unwrap_or(10_000_001);
    let npc_jita = spawn_specialized_npc_entity(&ctx, jita_sys, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, jita_sys, npc_jita, PILOT_2).await;
    let npc_other = spawn_specialized_npc_entity(&ctx, 30_000_144, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, 30_000_144, npc_other, PILOT_2).await;

    // 2. Mine ore: matching ore (1230 Veldspar) advances, Scordite (1228) does not
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 35, jita_sys, 1230);
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 35, jita_sys, 1228);

    // 3. Manufacture items: install & complete manufacturing job from SDE blueprint
    let (bpo_id, ship_p2) = ctx.db.with_mut_snapshot(|s| {
        let ship_id = s.characters.get(&PILOT_2).unwrap().active_ship_id;
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = Some(jita_station);
        ch.solar_system_id = jita_sys;
        s.next_entity_id += 1;
        let bpo = s.next_entity_id;
        s.items.insert(bpo, InventoryItemRecord {
            item_id: bpo, type_id: bp_type_id, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        for mat in &mfg_act.materials {
            s.next_entity_id += 1;
            let mid = s.next_entity_id;
            s.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: mat.type_id, owner_id: PILOT_2, location_id: jita_station,
                flag: InventoryFlag::Hangar, quantity: (mat.quantity as i32) * 10, singleton: false, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        }
        s.next_entity_id += 1;
        let trit_id = s.next_entity_id;
        s.items.insert(trit_id, InventoryItemRecord {
            item_id: trit_id, type_id: 34, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 10_000, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        (bpo, ship_id)
    });
    sess2.station_id = Some(jita_station);
    sess2.solar_system_id = Some(jita_sys);
    let mut job_kv = HashMap::new();
    job_kv.insert("facilityID".to_string(), PyRep::Integer(jita_station));
    job_kv.insert("blueprintItemID".to_string(), PyRep::Integer(bpo_id));
    job_kv.insert("blueprintTypeID".to_string(), PyRep::Integer(bp_type_id as i64));
    job_kv.insert("activityID".to_string(), PyRep::Integer(1));
    job_kv.insert("runs".to_string(), PyRep::Integer(2));
    let job_rep = rpc(
        &ctx,
        &mut sess2,
        "industryManager",
        "InstallJob",
        &[PyRep::KeyVal(job_kv)],
    )
    .await
    .expect("InstallJob");
    let job_id = job_rep.as_i64().unwrap_or_else(|| field(&job_rep, "jobID").as_i64().expect("jobID"));
    ctx.advance_secs(2 * 86_400);
    rpc(&ctx, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
        .await
        .expect("CompleteJob");

    // 4. Deliver items: MachoNet GoalProgress + Quasar freelance DeliverRequest
    rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_deliv), PyRep::Integer(40)])
        .await
        .expect("GoalProgress DeliverItem");

    let mut fl_create = Vec::new();
    encode_string_field(1, "Freelance Tritanium", &mut fl_create);
    encode_string_field(2, "Deliver Tritanium", &mut fl_create);
    encode_varint_field(3, 10, &mut fl_create);
    encode_varint_field(4, 100, &mut fl_create);
    let mut cfg = Vec::new();
    for val_str in ["34", "5"] {
        let mut om = Vec::new();
        encode_string_field(1, val_str, &mut om);
        let mut pm = Vec::new();
        encode_bytes_field(3, &om, &mut pm);
        encode_bytes_field(3, &pm, &mut cfg);
    }
    encode_bytes_field(6, &cfg, &mut fl_create);
    let fl_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.freelance.project.api.CreateRequest", fl_create, b"fl-create-1").await;
    assert_eq!(fl_rsp.status_code, STATUS_OK);
    let fl_uuid = fl_rsp.payload.unwrap().value[6..22].to_vec();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(jita_station);
            c2.solar_system_id = jita_sys;
        }
        s.items.insert(106_500_001, InventoryItemRecord {
            item_id: 106_500_001, type_id: 34, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 20, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &fl_uuid, &mut id_msg);
    let mut fl_commit = Vec::new();
    encode_bytes_field(1, &id_msg, &mut fl_commit);
    let commit_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.freelance.project.api.CommitRequest", fl_commit, b"fl-commit-1").await;
    assert_eq!(commit_rsp.status_code, STATUS_OK);
    let mut fl_deliv = Vec::new();
    encode_bytes_field(1, &id_msg, &mut fl_deliv);
    encode_varint_field(4, 10, &mut fl_deliv);
    let deliv_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", fl_deliv, b"fl-deliv-1").await;
    assert_eq!(deliv_rsp.status_code, STATUS_OK);

    // 5. Capture FW complex + 7. Earn LP
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.war_faction_id = Some(500_001);
        }
        s.system_states.insert(fw_sys, SystemDynamicState {
            solar_system_id: fw_sys, dbs_multiplier: 1.0, ess_main_bank: IskCents(0), ess_reserve_bank: IskCents(0),
            ess_linked_character_id: None, ess_link_started_tick: None, sov_alliance_id: None,
            military_index: 1, industrial_index: 1, strategic_index: 1, fw_contested_pct: 50.0,
            insurgency_corruption_stage: 1, insurgency_suppression_stage: 0, wormhole_remaining_mass_kg: 0.0,
            wormhole_destination_system_id: None, signature_position: None,
            fw_complex_position: Some([1_000.0, 0.0, 0.0]), fw_capture_seconds_elapsed: 595,
            fw_capture_duration_seconds: 600, last_fw_capture_tick: 0, last_sov_claim_tick: 0,
            ess_cycle_started_unix: 0, ess_main_bank_contributors: Default::default(),
        });
    });
    spawn_ship_in_space(&ctx, PILOT_2, ship_p2, fw_sys, DVec3::new(1_000.0, 0.0, 0.0), None).await;
    eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&ctx, fw_sys, Some(10)).await;
    ctx.db.with_mut_snapshot(|s| {
        eve_server::services::lp_store::record_lp_reward_log(s, PILOT_2, 1_000_180, 250, "FW Complex".into(), ctx.now_unix());
    });

    // 6. Damage ships
    let target_ship = 104_000_999i64;
    spawn_ship_in_space(&ctx, PILOT_1, target_ship, jita_sys, DVec3::new(3_000.0, 0.0, 0.0), None).await;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &jita_handle, jita_sys, PILOT_2, target_ship,
        DamageVector { em: 200.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        1.0, ctx.now_unix(),
    ).await.expect("damage ship");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_projects.get(&pid_npc).unwrap().current_quantity, 1, "DestroyNPCShips must advance only for matching system");
        assert_eq!(s.corp_projects.get(&pid_mine).unwrap().current_quantity, 35, "MineMaterials must advance only for matching ore type");
        assert_eq!(s.corp_projects.get(&pid_mfg).unwrap().current_quantity, 2, "ManufactureItems must advance by completed job runs");
        assert_eq!(s.corp_projects.get(&pid_deliv).unwrap().current_quantity, 40, "DeliverItem must advance by delivered inventory");
        assert_eq!(s.corp_projects.get(&pid_fw).unwrap().current_quantity, 1, "CaptureFWComplexes must advance on complex capture");
        assert!(s.corp_projects.get(&pid_dmg).unwrap().current_quantity >= 100, "DamageShips must advance from server-observed ship damage");
        assert_eq!(s.corp_projects.get(&pid_lp).unwrap().current_quantity, 250, "EarnLoyaltyPoints must advance from server-awarded LP");
    });
}

async fn create_proj(
    ctx: &ServerContext,
    sess: &mut SessionState,
    name: &str,
    target: i64,
    matcher: &str,
    filter: i64,
) -> i64 {
    let rep = rpc(
        ctx,
        sess,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(target),
            PyRep::Real(1_000.0),
            PyRep::String(matcher.into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(target),
            PyRep::Integer(7 * 86_400),
            PyRep::Integer(filter),
        ],
    )
    .await
    .expect("CreateGoalSubmit");
    field(&rep, "projectID").as_i64().expect("projectID")
}
}

#[allow(unused_imports)]
pub(crate) mod corp_audit_and_reinforce {
//! Part 2 of end-to-end integration tests for corporation member role audit and
//! corporation structure reinforce default.
//!
//! Parity rows:
//! - `feat:corporation.member-audit`
//! - `feat:corporation.structure-reinforce-default`


use std::collections::HashMap;

use super::pochven_loot_and_corp_audit::{
    field, rowset_rows, rpc, select_pilot, spawn_ship_in_space, PILOT_1, PILOT_2,
};
use crate::common;
use eve_db::corp_roles;
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

#[tokio::test]
async fn test_feat_corporation_member_audit_e2e() {
    // parity: feat:corporation.member-audit
    eve_testkit::attest(&["feat:corporation.member-audit"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_201, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_202, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Audit Test Corp".into()),
            PyRep::String("AUDT".into()),
            PyRep::Real(0.02),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");
    ctx.advance_secs(7 * 86_400 + 10);

    let director_id = 90_000_003i64;
    ctx.db.with_mut_snapshot(|s| {
        let mut dir_char = s.characters.get(&PILOT_2).unwrap().clone();
        dir_char.character_id = director_id;
        dir_char.name = "Director Three".into();
        dir_char.corporation_id = corp_id;
        s.characters.insert(director_id, dir_char);
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.members.insert(director_id);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
    });
    sess2.corporation_id = Some(corp_id);

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(director_id),
            PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
        ],
    )
    .await
    .expect("Grant Director");

    ctx.advance_secs(120);
    let t1 = ctx.now_unix() as i64;
    let mut sess_dir = SessionState::new(64_203);
    sess_dir.user_id = Some(3);
    sess_dir.character_id = Some(director_id);
    sess_dir.corporation_id = Some(corp_id);
    sess_dir.corp_role = corp_roles::ROLE_DIRECTOR;

    let first_roles = (corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER) as i64;
    rpc(
        &ctx,
        &mut sess_dir,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(first_roles),
        ],
    )
    .await
    .expect("Director UpdateMember");

    ctx.advance_secs(300);
    let t2 = ctx.now_unix() as i64;
    let second_roles = (first_roles as u64 | corp_roles::ROLE_PERSONNEL_MANAGER) as i64;
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(second_roles),
        ],
    )
    .await
    .expect("CEO UpdateMember");

    let audit = rpc(&ctx, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("AuditMember");
    let rows = rowset_rows(&audit);
    assert_eq!(rows.len(), 2, "AuditMember must return both role changes for PILOT_2");
    assert_eq!(
        rows[0],
        vec![
            PyRep::Integer(PILOT_2),
            PyRep::Integer(director_id),
            PyRep::Integer(0),
            PyRep::Integer(first_roles),
            PyRep::Integer(t1),
        ]
    );
    assert_eq!(
        rows[1],
        vec![
            PyRep::Integer(PILOT_2),
            PyRep::Integer(PILOT_1),
            PyRep::Integer(first_roles),
            PyRep::Integer(second_roles),
            PyRep::Integer(t2),
        ]
    );

    let mut pp = HashMap::new();
    pp.insert("corporation_id".to_string(), corp_id.to_string());
    let esi_rsp = eve_server::esi::sample_routes::dispatch_operation(
        &ctx,
        ctx.now_unix(),
        "GET",
        "/corporations/{corporation_id}/roles/history",
        &pp,
        &HashMap::new(),
        None,
        director_id as usize,
        &[],
    );
    assert_eq!(esi_rsp.status_code, 200, "ESI roles/history must return 200");
    let arr = esi_rsp.body.expect("json").as_array().cloned().expect("array");
    assert!(arr.len() >= 3, "ESI roles/history must include all corp role changes");
}

#[tokio::test]
async fn test_feat_corporation_structure_reinforce_default_e2e() {
    // parity: feat:corporation.structure-reinforce-default
    eve_testkit::attest(&["feat:corporation.structure-reinforce-default"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_301, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_302, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Citadel Builders".into()),
            PyRep::String("CITB".into()),
            PyRep::Real(0.03),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");
    ctx.advance_secs(7 * 86_400 + 10);

    let perimeter_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id != 10_000_070)
        .expect("lowsec system")
        .solar_system_id;
    let ship1 = ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().members.insert(PILOT_2);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get(&PILOT_1).unwrap().active_ship_id
    });
    sess2.corporation_id = Some(corp_id);
    sess1.station_id = None;
    sess1.solar_system_id = Some(perimeter_sys);

    let err = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect_err("Non-director SetStructureReinforceDefault must fail");
    assert_eq!(err.code, "CorpRoleDenied");

    let set_rep = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(set_rep, PyRep::Integer(19));

    let get_rep = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "GetStructureReinforceDefault",
        &[],
    )
    .await
    .expect("GetStructureReinforceDefault");
    assert_eq!(get_rep, PyRep::Integer(19));

    spawn_ship_in_space(&ctx, PILOT_1, ship1, perimeter_sys, DVec3::new(50_000.0, 0.0, 0.0), None).await;
    ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let astra_id = s.next_entity_id;
        s.items.insert(
            astra_id,
            eve_db::InventoryItemRecord {
                item_id: astra_id,
                type_id: 35_832,
                owner_id: PILOT_1,
                location_id: ship1,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let perimeter_handle = ctx.systems.get_or_spawn(perimeter_sys);

    let anchor_rep = rpc(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(35_832),
            PyRep::List(vec![
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ]),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::String("Corp Astrahus".into()),
        ],
    )
    .await
    .expect("Anchor");
    let structure_id = anchor_rep
        .as_i64()
        .unwrap_or_else(|| field(&anchor_rep, "structureID").as_i64().expect("structureID"));

    let core_type = eve_server::services::station_structure::structure_lifecycle::structure_profile(&ctx, 35_832)
        .and_then(|p| p.core_type_id)
        .unwrap_or(56_201);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            105_000_002,
            eve_db::InventoryItemRecord {
                item_id: 105_000_002,
                type_id: core_type,
                owner_id: PILOT_1,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.advance_secs(eve_server::services::station_structure::structure_lifecycle::ANCHOR_VULNERABLE_SECS + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.advance_secs(86_400 + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.advance_secs(eve_server::services::station_structure::structure_lifecycle::ONLINING_SECS + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

    let (adopted_hour, state) = ctx.db.with_snapshot(|s| {
        let w = s.structure_vulnerability_windows.get(&structure_id).copied().expect("vuln window");
        let st = s.structures.get(&structure_id).expect("structure");
        (w.0, st.state.clone())
    });
    assert_eq!(
        state,
        eve_server::services::station_structure::structure_lifecycle::STATE_SHIELD_VULNERABLE,
        "Newly onlined structure must be ShieldVulnerable"
    );
    assert_eq!(adopted_hour, 19, "Newly onlined corp structure must adopt corp default reinforce hour 19");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.corporation_id = 1_000_009;
        }
    });
    eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx,
        &perimeter_handle,
        perimeter_sys,
        PILOT_2,
        structure_id,
        DamageVector { em: 10_000_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("damage structure");

    let reinforced_until = ctx.db.with_snapshot(|s| {
        s.structures.get(&structure_id).unwrap().reinforced_until_unix
    });
    assert_eq!(
        (reinforced_until / 3_600) % 24,
        19,
        "Structure reinforce timer must exit at adopted corp default hour 19 UTC"
    );
}
}
