//! Integration tests for Phase 5: Combat, Damage & PvE (`C-1, C-2, C-3, C-4, C-7, C-8`).

#![forbid(unsafe_code)]

mod common;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, DestinyEvent, MovementMode};
use eve_server::combat::crimewatch::{AggressionEvaluation, CrimeWatchDirector};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::combat::drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, DroneController,
    SalvageOutcome,
};
use eve_server::combat::npc_ai::{NpcAiState, NpcSpawnManager};
use eve_server::formulas::{
    concord_response_delay_sec, missile_damage_factor, turret_angular_velocity,
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
    // Rifter has 25 Mbit/s bandwidth. 5 light drones (5 Mbit/s each) = 25 Mbit/s (valid).
    // Attempting 6 drones or requesting 30 Mbit/s must fail bandwidth check.
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
        assert_eq!(kr.activation_fee, IskCents::from_isk(1_000_000));
        assert!(!kr.is_active_suspect);

        // Verify criminal security status penalized
        let crim = snap.characters.get(&char_id).unwrap();
        assert!(crim.security_status <= 0.0);
    });
}
