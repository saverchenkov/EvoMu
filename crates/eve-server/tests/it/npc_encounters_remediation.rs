//! Integration Tests for Remediated NPC & Crimewatch Encounter Mechanics.
//!
//! Covers:
//! - `feat:npc.navy-npc`: Faction police response to outlaw pilots in high-sec, pod safety, and docking refusal.
//! - `feat:npc.npc-standings-aggression`: Standing-based aggression for Empire Navy vs Pirate Belt Rats.
//! - `feat:npc.npc-hauler`: Multi-system living-world convoy stargate route traversal and authentic cargo drops.
//! - `feat:npc.npc-spawn-encounter`: Real damage destruction hook triggering authentic multi-wave progression to site completion.

use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_server::living_world::{ConvoyStepStatus, LivingWorldConvoy, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    check_and_spawn_faction_police, create_custom_encounter_site,
    destroy_specialized_npc_and_queue_bounty, is_outlaw_ship_docking_refused,
    spawn_specialized_npc_entity, step_extended_npc_ai_sync, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

const SYS_JITA: u32 = 30_000_142; // Highsec (0.95)
const SYS_PERIMETER: u32 = 30_000_144; // Highsec (0.90)
const SYS_URLEN: u32 = 30_000_143; // Highsec (0.85)

async fn register_test_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    sec_status: f64,
    standings: HashMap<i64, f64>,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("Pilot_{char_id}"),
                corporation_id: 1_000_002,
                alliance_id: None,
                war_faction_id: None,
                fleet_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings,
                security_status: sec_status,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: eve_db::NeuralAttributes::default(),
                skill_points: 5_000_000.0,
                activity_status: None,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                version: 1,
                system_epoch: 1,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
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
    });

    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = handle
        .spawn_ball_with_combat(ball, ctx.build_type_combat_state(ship_type_id))
        .await;

    let mut sess = SessionState::new(char_id as u64);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);
    sess
}

#[tokio::test]
async fn test_navy_npc_and_faction_police_response() {
    // parity: feat:npc.navy-npc
    eve_testkit::attest(&["feat:npc.navy-npc"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // 1. Outlaw pilot in high-sec piloting a cruiser
    let outlaw_char_id = 910_001i64;
    let outlaw_ship_id = 810_001i64;
    let _sess_outlaw = register_test_pilot(
        &ctx,
        outlaw_char_id,
        outlaw_ship_id,
        624, // Moa cruiser
        SYS_JITA,
        -6.5, // Well below -5.0 outlaw threshold
        HashMap::new(),
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    // Faction police check spawns police ships on grid targeting outlaw
    let police_spawned = check_and_spawn_faction_police(&ctx, SYS_JITA).await;
    assert!(
        !police_spawned.is_empty(),
        "Faction Police must spawn when outlaw ship enters highsec"
    );
    let police_bid = police_spawned[0];

    // Verify police NPC properties
    with_combat_runtime(&ctx, |rt| {
        let police = rt.npc_entities.get(&police_bid).expect("Police entity");
        assert_eq!(police.archetype, "FactionPolice");
        assert_eq!(police.target_id, Some(outlaw_ship_id));
        assert!(police.aggression_threat.contains_key(&outlaw_ship_id));
        assert_eq!(police.scram_strength, 2.0);
    });

    // Verify docking is refused to outlaw ship
    let refused = is_outlaw_ship_docking_refused(&ctx, SYS_JITA, outlaw_char_id, outlaw_ship_id);
    assert!(
        refused,
        "Docking must be refused to an outlaw ship (sec <= -5.0) in highsec"
    );

    // 2. Outlaw pilot in a capsule (pod)
    let pod_char_id = 910_002i64;
    let pod_ship_id = 810_002i64;
    let _sess_pod = register_test_pilot(
        &ctx,
        pod_char_id,
        pod_ship_id,
        670, // Capsule
        SYS_JITA,
        -6.5,
        HashMap::new(),
        DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;

    // Verify capsules are NOT engaged by faction police and are allowed to dock
    let pod_refused = is_outlaw_ship_docking_refused(&ctx, SYS_JITA, pod_char_id, pod_ship_id);
    assert!(
        !pod_refused,
        "Outlaw capsules (pods) must NEVER be refused docking"
    );

    // 3. Lawful pilot (sec status > -5.0)
    let lawful_char_id = 910_003i64;
    let lawful_ship_id = 810_003i64;
    let _sess_lawful = register_test_pilot(
        &ctx,
        lawful_char_id,
        lawful_ship_id,
        624,
        SYS_JITA,
        1.5,
        HashMap::new(),
        DVec3::new(30_000.0, 0.0, 0.0),
    )
    .await;

    let lawful_refused = is_outlaw_ship_docking_refused(&ctx, SYS_JITA, lawful_char_id, lawful_ship_id);
    assert!(!lawful_refused, "Lawful pilots must not be refused docking");
}

#[tokio::test]
async fn test_npc_standings_aggression_rules() {
    // parity: feat:npc.npc-standings-aggression
    eve_testkit::attest(&["feat:npc.npc-standings-aggression"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // Pilot 1: Neutral standing (0.0) to Caldari Navy (500001)
    let neutral_char_id = 920_001i64;
    let neutral_ship_id = 820_001i64;
    let mut neutral_standings = HashMap::new();
    neutral_standings.insert(500_001i64, 0.0);
    let _sess_neutral = register_test_pilot(
        &ctx,
        neutral_char_id,
        neutral_ship_id,
        624,
        SYS_JITA,
        0.0,
        neutral_standings,
        DVec3::new(5_000.0, 0.0, 0.0),
    )
    .await;

    // Pilot 2: Hostile standing (-6.0) to Caldari Navy (500001)
    let hostile_char_id = 920_002i64;
    let hostile_ship_id = 820_002i64;
    let mut hostile_standings = HashMap::new();
    hostile_standings.insert(500_001i64, -6.0);
    let _sess_hostile = register_test_pilot(
        &ctx,
        hostile_char_id,
        hostile_ship_id,
        624,
        SYS_JITA,
        0.0,
        hostile_standings,
        DVec3::new(6_000.0, 0.0, 0.0),
    )
    .await;

    // Pilot 3: Friendly standing (+8.0) to Guristas Pirates (500010)
    let pirate_friend_char_id = 920_003i64;
    let pirate_friend_ship_id = 820_003i64;
    let mut pirate_standings = HashMap::new();
    pirate_standings.insert(500_010i64, 8.0);
    let _sess_friend = register_test_pilot(
        &ctx,
        pirate_friend_char_id,
        pirate_friend_ship_id,
        624,
        SYS_JITA,
        0.0,
        pirate_standings,
        DVec3::new(7_000.0, 0.0, 0.0),
    )
    .await;

    // Spawn Empire Navy NPC ship on grid
    let navy_ball_id = spawn_specialized_npc_entity(
        &ctx,
        SYS_JITA,
        10046, // Navy ship
        "Navy",
        500_001,
        DVec3::new(5_500.0, 0.0, 0.0),
    )
    .await;

    // Spawn Pirate Belt Rat NPC ship on grid
    let rat_ball_id = spawn_specialized_npc_entity(
        &ctx,
        SYS_JITA,
        16997, // Guristas Pithi Invader
        "BeltRat",
        500_010,
        DVec3::new(7_500.0, 0.0, 0.0),
    )
    .await;

    // Run extended NPC AI sync step
    step_extended_npc_ai_sync(&ctx, SYS_JITA);

    // Verify standing aggression outcomes
    with_combat_runtime(&ctx, |rt| {
        // Empire Navy rules:
        // Must NOT aggress neutral 0.0 standing pilot
        let navy = rt.npc_entities.get(&navy_ball_id).expect("Navy entity");
        assert!(
            !navy.aggression_threat.contains_key(&neutral_ship_id),
            "Empire Navy must NOT aggress pilot with 0.0 standing"
        );
        // MUST aggress <= -5.0 standing pilot
        assert!(
            navy.aggression_threat.contains_key(&hostile_ship_id),
            "Empire Navy MUST aggress pilot with <= -5.0 standing to empire faction"
        );

        // Pirate Belt Rat rules:
        // Belt rats aggress all players on grid regardless of +8.0 positive standing
        let rat = rt.npc_entities.get(&rat_ball_id).expect("Belt rat entity");
        assert!(
            rat.aggression_threat.contains_key(&pirate_friend_ship_id),
            "Pirate belt rats MUST aggress pilot despite +8.0 positive standing to pirate faction"
        );
    });
}

#[tokio::test]
async fn test_hauler_convoy_multi_system_movement_and_cargo_drop() {
    // parity: feat:npc.npc-hauler
    eve_testkit::attest(&["feat:npc.npc-hauler"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let convoy_id = 850_001i64;
    let hauler_type_id = 587u32; // Tayra / Industrial hauler
    let cargo_ore_type_id = 1230u32; // Veldspar
    let cargo_qty = 5_000i32;

    // Register a living-world hauler convoy with a multi-system route: Jita -> Perimeter -> Urlen
    let route = vec![SYS_JITA, SYS_PERIMETER, SYS_URLEN];
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: hauler_type_id,
        origin_system_id: SYS_JITA,
        current_system_id: SYS_JITA,
        destination_system_id: SYS_URLEN,
        destination_station_id: 60_003_766,
        cargo_type_id: cargo_ore_type_id,
        cargo_quantity: cargo_qty,
        lod_level: 2, // Spawned in space
        destroyed: false,
    });
    ctx.living_world.set_convoy_route(convoy_id, route.clone());

    // Materialize in Jita space
    let _ = ctx.living_world.materialize_system_lod2(SYS_JITA, &ctx.systems, &ctx.sde).await;

    // Step 1: Jumps stargate from Jita into Perimeter
    let step1 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step 1");
    assert_eq!(
        step1,
        ConvoyStepStatus::JumpedGate {
            from_system: SYS_JITA,
            to_system: SYS_PERIMETER,
        }
    );
    assert_eq!(
        ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
        SYS_PERIMETER
    );

    // Step 2: Jumps stargate from Perimeter into Urlen
    let step2 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step 2");
    assert_eq!(
        step2,
        ConvoyStepStatus::JumpedGate {
            from_system: SYS_PERIMETER,
            to_system: SYS_URLEN,
        }
    );
    assert_eq!(
        ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
        SYS_URLEN
    );

    // Step 3: Arrives at destination system, docks and despawns at destination station
    let step3 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step 3");
    assert_eq!(
        step3,
        ConvoyStepStatus::DockedAtDestination {
            station_id: 60_003_766,
            system_id: SYS_URLEN,
        }
    );
    assert!(ctx.living_world.is_convoy_docked(convoy_id));

    // 4. Test authentic cargo loot drop upon hauler destruction
    let hauler2_id = 850_002i64;
    let custom_cargo_tid = 1230u32; // Veldspar
    let custom_cargo_amount = 7_500i32;

    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id: hauler2_id,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: hauler_type_id,
        origin_system_id: SYS_JITA,
        current_system_id: SYS_JITA,
        destination_system_id: SYS_JITA,
        destination_station_id: 60_003_760,
        cargo_type_id: custom_cargo_tid,
        cargo_quantity: custom_cargo_amount,
        lod_level: 2,
        destroyed: false,
    });
    let _ = ctx.living_world.materialize_system_lod2(SYS_JITA, &ctx.systems, &ctx.sde).await;

    let killer_char_id = 930_001i64;
    let (wreck_id, _bounty, dropped_types) = destroy_specialized_npc_and_queue_bounty(
        &ctx,
        SYS_JITA,
        hauler2_id,
        killer_char_id,
    )
    .await;

    assert!(
        dropped_types.contains(&custom_cargo_tid),
        "Dropped loot must contain the hauler's defined cargo type ID"
    );

    // Verify loot item inside wreck cargo
    ctx.db.with_snapshot(|snap| {
        let cargo_item = snap
            .items
            .values()
            .find(|i| i.location_id == wreck_id && i.flag == InventoryFlag::Cargo && i.type_id == custom_cargo_tid)
            .expect("Wreck must contain dropped convoy cargo");
        assert_eq!(
            cargo_item.quantity,
            custom_cargo_amount,
            "Wreck cargo quantity must match hauler's defined quantity, not hardcoded defaults"
        );
    });

    // Convoy marked destroyed
    assert!(ctx.living_world.get_convoy(hauler2_id).unwrap().destroyed);
}

#[tokio::test]
async fn test_encounter_site_destruction_trigger_and_wave_progression() {
    // parity: feat:npc.npc-spawn-encounter
    eve_testkit::attest(&["feat:npc.npc-spawn-encounter"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let encounter_id = 77_001u32;
    let origin_pos = DVec3::new(100_000.0, 0.0, 0.0);

    // Wave 1: [3751, 3752], Wave 2: [3753, 3754]
    let wave_defs = vec![vec![3751, 3752], vec![3753, 3754]];
    let wave1_balls = create_custom_encounter_site(
        &ctx,
        encounter_id,
        SYS_JITA,
        wave_defs,
        origin_pos,
    )
    .await;

    assert_eq!(wave1_balls.len(), 2);
    let trigger_ball_1 = wave1_balls[0];

    with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
        assert_eq!(enc.current_wave, 1);
        assert_eq!(enc.trigger_ball_id, trigger_ball_1);
        assert!(!enc.completed);
    });

    // Deal lethal weapon damage in space to the designated trigger NPC
    let handle = ctx.systems.get_or_spawn(SYS_JITA);
    let lethal_damage = DamageVector {
        em: 50_000.0,
        thermal: 50_000.0,
        kinetic: 50_000.0,
        explosive: 50_000.0,
    };
    let dmg_res = handle.apply_damage(trigger_ball_1, lethal_damage).await;
    assert!(dmg_res.is_ok(), "apply_damage must succeed");

    // Hull reduced to 0; handle_npc_destroyed_in_space runs via npc_death_hook
    // Verify Wave 2 spawned with authentic types [3753, 3754]
    let (wave2_balls, trigger_ball_2) = with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
        assert_eq!(enc.current_wave, 2, "Wave 2 must be triggered upon trigger NPC destruction");
        assert!(!enc.completed);
        (enc.active_wave_ball_ids.clone(), enc.trigger_ball_id)
    });

    assert!(!wave2_balls.is_empty(), "Wave 2 NPCs must be spawned in space");
    assert_ne!(trigger_ball_2, trigger_ball_1);

    // Verify wave 2 types in npc_entities
    let wave2_new_balls: Vec<i64> = wave2_balls
        .iter()
        .filter(|&&b| !wave1_balls.contains(&b))
        .copied()
        .collect();
    assert_eq!(wave2_new_balls.len(), 2, "Wave 2 must spawn 2 new NPC balls");

    with_combat_runtime(&ctx, |rt| {
        for &bid in &wave2_new_balls {
            let entity = rt.npc_entities.get(&bid).expect("Wave 2 entity");
            assert!(
                entity.type_id == 3753 || entity.type_id == 3754,
                "Wave 2 NPC must have authentic types from wave definition"
            );
        }
    });

    // Eliminate all active NPCs with lethal damage (wave 1 remainder and wave 2)
    for &bid in &wave2_balls {
        let _ = handle.apply_damage(bid, lethal_damage).await;
    }

    // Site completed!
    with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
        assert!(enc.completed, "Site must be marked completed when final wave is eliminated");
    });
}
