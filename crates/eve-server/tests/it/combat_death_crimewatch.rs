//! Integration tests for Combat Systems, Death Pipeline, NPCs & CrimeWatch (`C-3, C-4, C-7, C-8`).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::death_pipeline::{
    execute_death_pipeline, execute_self_destruct, SelfDestructManager,
};
use eve_server::combat::drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, DroneController,
    SalvageOutcome,
};
use eve_server::combat::npc_ai::{
    compute_sec_status_gain, faction_damage_vector, NpcAiState, NpcSpawnManager,
};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_c3_death_pipeline_ship_and_pod_destruction_medical_respawn_and_implants_lost() {
    // parity: feat:combat.ship-destruction
    // parity: feat:combat.pod-destruction
    // parity: feat:attributes.implants-pod-death
    // parity: feat:clones.medical-clone-respawn
    eve_testkit::attest(&[
        "feat:combat.ship-destruction",
        "feat:combat.pod-destruction",
        "feat:attributes.implants-pod-death",
        "feat:clones.medical-clone-respawn",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let victim_ship_id = 100_000_001;
    let attacker_char_id = 90_000_002;
    let sys_id = 30_000_142; // Jita
    let home_station_id = 60_003_760; // Jita 4-4
    let death_pos = DVec3::new(15_000.0, 200.0, -5_000.0);

    // 1. Seed character with active ship, balance, and active clone implants (Snake Alpha 9899)
    let initial_balance = IskCents::from_isk(10_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = initial_balance;
            ch.active_ship_id = victim_ship_id;
            ch.home_station_id = home_station_id;
        }

        // Insure ship
        if let Some(ship) = snap.items.get_mut(&victim_ship_id) {
            ship.insured_payout_isk = 250_000.0;
        }

        // Plug in clone implants
        snap.active_implants.insert(char_id, vec![9899, 9941]);

        // Place cargo inside ship
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
                    quantity: 500,
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

    // 2. Physical space ship destruction at 0 structure HP (feat:combat.ship-destruction)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = Ball {
        id: victim_ship_id,
        type_id: 587, // Rifter
        position: death_pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(587))
        .await;

    // Apply lethal damage directly in Destiny Ballpark
    let (applied_dmg, destroyed, _) = sys_handle
        .apply_damage(
            victim_ship_id,
            DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
        )
        .await
        .expect("apply_damage")
        .expect("damage result");
    assert!(destroyed, "Ship must be destroyed at 0 structure HP");
    assert!(applied_dmg > 0.0);

    let initial_ship_items_count = ctx
        .db
        .with_snapshot(|snap| snap.items.values().filter(|it| it.location_id == victim_ship_id).count());

    // Execute the death pipeline for the destroyed ship
    let ship_death = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        victim_ship_id,
        attacker_char_id,
        death_pos,
        12345,
    )
    .await
    .expect("execute_death_pipeline");

    // Ship exploded, wreck spawned, cargo dropped/destroyed by 50% Loot Fairy
    assert!(ship_death.wreck_item_id > 0);
    assert!(!ship_death.loot_dropped_item_ids.is_empty());
    assert!(!ship_death.loot_destroyed_item_ids.is_empty());
    assert_eq!(
        ship_death.loot_dropped_item_ids.len() + ship_death.loot_destroyed_item_ids.len(),
        initial_ship_items_count
    );

    // Insurance payout credited
    assert_eq!(ship_death.insurance_payout_isk, 250_000.0);
    let final_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert_eq!(
        final_bal,
        initial_balance
            .checked_add(IskCents::from_isk(250_000))
            .unwrap()
    );

    // Pod ejected into space ballpark
    assert!(ship_death.ejected_capsule_id.is_some());
    let ejected_pod_id = ship_death.ejected_capsule_id.unwrap();
    let pod_ball = sys_handle
        .get_ball_snapshot(ejected_pod_id)
        .await
        .unwrap()
        .expect("ejected capsule ball in space");
    assert_eq!(pod_ball.1.type_id, 670); // Capsule
    assert_eq!(pod_ball.1.position, death_pos);

    // Character active ship is now the ejected capsule
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.active_ship_id, ejected_pod_id);
        // CRITICAL: Implants are STILL intact while inside the capsule!
        let implants = snap.active_implants.get(&char_id);
        assert_eq!(implants, Some(&vec![9899, 9941]));
    });

    // 3. Pod destruction (feat:combat.pod-destruction, feat:attributes.implants-pod-death, feat:clones.medical-clone-respawn)
    let pod_death = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ejected_pod_id,
        attacker_char_id,
        death_pos,
        999111,
    )
    .await
    .expect("execute_death_pipeline for pod");

    // Pod does NOT eject another capsule
    assert!(pod_death.ejected_capsule_id.is_none());
    assert_eq!(pod_death.insurance_payout_isk, 0.0);

    // Pod ball removed from space ballpark
    assert!(sys_handle
        .get_ball_snapshot(ejected_pod_id)
        .await
        .unwrap()
        .is_none());

    // Pilot woke in their medical clone at home station with a new capsule (feat:clones.medical-clone-respawn)
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.station_id, Some(home_station_id));
        assert!(ch.active_ship_id > 0);
        assert_ne!(ch.active_ship_id, ejected_pod_id);

        let new_ship = snap.items.get(&ch.active_ship_id).expect("new capsule item");
        assert_eq!(new_ship.type_id, 670);
        assert_eq!(new_ship.location_id, home_station_id);

        // Pod death destroys active clone implants! (feat:attributes.implants-pod-death)
        let active_implants = snap.active_implants.get(&char_id);
        assert!(
            active_implants.is_none() || active_implants.unwrap().is_empty(),
            "All clone implants must be lost on pod destruction"
        );
    });
}

#[tokio::test]
async fn test_c3_ship_and_pod_self_destruct() {
    // parity: feat:ships.self-destruct, rpc:charUnboundMgr.SelectCharacterID, rpc:ship.AbortSelfDestruct
    eve_testkit::attest(&["feat:ships.self-destruct"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let now = 1_700_000_000;
    let pos = DVec3::new(20_000.0, 0.0, 0.0);

    // Spawn ship in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(587))
        .await;

    // 1. Self-destruct manager: initiate 2-minute (120s) countdown
    let mut sd_mgr = SelfDestructManager::new();
    let det_time = sd_mgr.start_self_destruct(ship_id, char_id, sys_id, now);
    assert_eq!(det_time, now + 120);
    assert!(sd_mgr.is_self_destructing(ship_id));
    assert_eq!(sd_mgr.remaining_seconds(ship_id, now), Some(120));
    assert_eq!(sd_mgr.remaining_seconds(ship_id, now + 30), Some(90));

    // Abort self-destruct cancels sequence
    let aborted = sd_mgr.abort_self_destruct(ship_id);
    assert!(aborted);
    assert!(!sd_mgr.is_self_destructing(ship_id));
    assert_eq!(sd_mgr.remaining_seconds(ship_id, now), None);

    // 2. Restart and complete 120s self-destruct sequence -> produces killmail
    sd_mgr.start_self_destruct(ship_id, char_id, sys_id, now);
    // Before 120s expires: no detonation
    let det_before = sd_mgr.step_timers(&ctx, now + 119, 123).await;
    assert!(det_before.is_empty());

    // At 120s: ship self-destructs and explodes
    let det_results = sd_mgr.step_timers(&ctx, now + 120, 123).await;
    assert_eq!(det_results.len(), 1);
    let (destroyed_ship_id, death_res) = &det_results[0];
    assert_eq!(*destroyed_ship_id, ship_id);

    let death = death_res.as_ref().expect("death pipeline result");
    assert!(death.wreck_item_id > 0);
    assert!(death.killmail_id > 0);
    assert!(death.ejected_capsule_id.is_some());

    // Verify Killmail attributes attacker as self (character self-destructed)
    ctx.db.with_snapshot(|snap| {
        let km = snap.killmails.get(&death.killmail_id).unwrap();
        assert_eq!(km.victim_character_id, char_id);
        assert_eq!(km.attacker_character_id, char_id);
        assert_eq!(km.ship_type_id, 587);
    });

    // Test direct self-destruct on pod (feat:ships.self-destruct)
    if let Some(cap_id) = death.ejected_capsule_id {
        let pod_sd = execute_self_destruct(&ctx, sys_id, char_id, cap_id, pos, 456)
            .await
            .expect("pod self-destruct");
        assert!(pod_sd.ejected_capsule_id.is_none());
        assert!(pod_sd.killmail_id > 0);
    }

    // 3. Test RPC dispatch: ship.SelfDestruct and ship.AbortSelfDestruct
    let mut sess = SessionState::new(9901);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let abort_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "AbortSelfDestruct", &[], &kw, None)
        .await;
    assert!(abort_res.is_ok());
}

#[tokio::test]
async fn test_c4_npc_spawn_groups_aggro_targeting_damage_types_loot_and_bounties() {
    // parity: feat:npc.belt-rats
    // parity: feat:npc.npc-ai-targeting
    // parity: feat:npc.npc-damage-types
    // parity: feat:npc.npc-loot
    // parity: feat:combat.bounty-payout-npc
    eve_testkit::attest(&[
        "feat:npc.belt-rats",
        "feat:npc.npc-ai-targeting",
        "feat:npc.npc-damage-types",
        "feat:npc.npc-loot",
        "feat:combat.bounty-payout-npc",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142; // Jita
    let killer_char_id = 90_000_001;
    let spawn_pos = DVec3::new(80_000.0, 0.0, 40_000.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let mut spawn_mgr = NpcSpawnManager::new();

    // 1. Belt rats spawner (feat:npc.belt-rats)
    let belt_rats = spawn_mgr
        .spawn_belt_rats_for_system(&ctx, sys_id)
        .await
        .expect("spawn_belt_rats_for_system");
    assert!(!belt_rats.is_empty(), "Must spawn belt rats at asteroid belts");

    // 2. Spawn group and Faction damage types (feat:npc.npc-damage-types)
    let spawned = spawn_mgr
        .spawn_group(&ctx, sys_id, 1, spawn_pos)
        .await
        .expect("spawn_group");
    let npc_ball_id = spawned[0];
    let entity = &spawn_mgr.entities[&npc_ball_id];

    // Verify faction damage profiles
    let guristas_dmg = faction_damage_vector(500010, 100.0);
    assert_eq!(guristas_dmg.kinetic, 70.0);
    assert_eq!(guristas_dmg.thermal, 30.0);
    assert_eq!(guristas_dmg.em, 0.0);

    let blood_dmg = faction_damage_vector(500012, 100.0);
    assert_eq!(blood_dmg.em, 60.0);
    assert_eq!(blood_dmg.thermal, 40.0);

    let serpentis_dmg = faction_damage_vector(500020, 100.0);
    assert_eq!(serpentis_dmg.thermal, 60.0);
    assert_eq!(serpentis_dmg.kinetic, 40.0);

    let angel_dmg = faction_damage_vector(500011, 100.0);
    assert_eq!(angel_dmg.explosive, 60.0);
    assert_eq!(angel_dmg.kinetic, 30.0);

    // 3. NPC AI targeting and engagement (feat:npc.npc-ai-targeting)
    assert_eq!(entity.state, NpcAiState::Idle);

    // Spawn player ship within 50km aggro range
    let player_ship_id = 800_001;
    let player_pos = spawn_pos + DVec3::new(25_000.0, 0.0, 0.0);
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

    // Step AI: transitions to Chase
    spawn_mgr.step_ai(&ctx, sys_id, 1, 1.0).await;
    assert_eq!(
        spawn_mgr.entities[&npc_ball_id].state,
        NpcAiState::Chase,
        "Hostile within 50km triggers Chase state"
    );

    // Move player into orbit/attack distance (4 km)
    sys_handle
        .set_ball_position(player_ship_id, spawn_pos + DVec3::new(4_000.0, 0.0, 0.0))
        .await;

    // Step AI: transitions to Orbit / Attack and applies cycle damage
    spawn_mgr.step_ai(&ctx, sys_id, 3, 1.0).await;
    assert!(
        matches!(
            spawn_mgr.entities[&npc_ball_id].state,
            NpcAiState::Orbit | NpcAiState::Attack
        ),
        "Close range transitions to Orbit/Attack"
    );

    // 4. Destruction: NPC loot table drops and bounty payout (feat:npc.npc-loot, feat:combat.bounty-payout-npc)
    let initial_killer_balance = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_char_id].balance);

    let bounty = spawn_mgr
        .handle_npc_destroyed(&ctx, sys_id, npc_ball_id, killer_char_id, 1_700_000_100)
        .expect("handle_npc_destroyed");
    assert!(bounty > 0.0);

    // Bounty recorded to wallet journal
    let final_killer_balance = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_char_id].balance);
    assert_eq!(
        final_killer_balance,
        initial_killer_balance
            .checked_add(IskCents::from_f64_isk(bounty))
            .unwrap()
    );

    // Verify NPC wreck contains loot table items (feat:npc.npc-loot)
    ctx.db.with_snapshot(|snap| {
        let wreck = snap
            .items
            .values()
            .find(|i| i.location_id == sys_id as i64 && i.type_id == 26557)
            .expect("npc wreck in space");

        let loot_items: Vec<&InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck.item_id)
            .collect();
        assert!(!loot_items.is_empty(), "NPC wreck must contain loot drops");

        // Verify metal scraps or ammunition are present
        let has_scraps = loot_items.iter().any(|i| i.type_id == 15331);
        assert!(has_scraps, "NPC wreck must drop metal scraps");
    });
}

#[tokio::test]
async fn test_c7_salvager_tractor_beam_and_drones() {
    // parity: feat:modules.salvager
    // parity: feat:modules.tractor-beam
    // parity: feat:drones.launch-drones
    // parity: feat:drones.recall-drones
    // parity: feat:drones.return-orbit
    eve_testkit::attest(&[
        "feat:modules.salvager",
        "feat:modules.tractor-beam",
        "feat:drones.launch-drones",
        "feat:drones.recall-drones",
        "feat:drones.return-orbit",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Tristan (type 593): 25 Mbit/s bandwidth
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.type_id = 593;
        }
    });

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

    // 1. Salvager module (feat:modules.salvager)
    let wreck_id = 660_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
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

    let wreck_pos = DVec3::new(3_000.0, 0.0, 0.0); // 3 km away (within 5 km range)
    let wreck_ball = Ball {
        id: wreck_id,
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

    // Successful salvage extracts materials and removes empty wreck
    let salvage_res = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_id, 0.30, 0.10)
        .await
        .expect("attempt_salvage_wreck");
    match salvage_res {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert!(!yielded_salvage_type_ids.is_empty());
            assert!(!yielded_quantities.is_empty());
        }
        SalvageOutcome::Failed { .. } => panic!("Expected salvage success on roll 0.10"),
    }
    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_none());

    // 2. Tractor beam mechanics (feat:modules.tractor-beam)
    let far_wreck = DVec3::new(18_000.0, 0.0, 0.0);
    let (pulled_pos, arrived_1) = step_tractor_beam(far_wreck, ship_pos, 500.0, 10.0);
    assert!(!arrived_1);
    assert!((pulled_pos.length() - 13_000.0).abs() < 1.0);

    let (final_pos, arrived_2) = step_tractor_beam(pulled_pos, ship_pos, 500.0, 30.0);
    assert!(arrived_2);
    assert!(final_pos.length() <= 1_201.0);

    // 3. Drones: launch, return-orbit, recall (feat:drones.launch-drones, feat:drones.return-orbit, feat:drones.recall-drones)
    let d1 = 810_001;
    let d2 = 810_002;
    ctx.db.with_mut_snapshot(|snap| {
        for &did in &[d1, d2] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454, // Hobgoblin I (5 Mbit/s)
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

    // Validate drone bandwidth (feat:drones.launch-drones)
    let bw_check = validate_drone_bandwidth(&ctx, ship_id, &[2454, 2454], &[]);
    assert!(bw_check.is_ok());

    let mut drone_ctrl = DroneController::new();

    // Launch drones (feat:drones.launch-drones)
    let launched = drone_ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[d1, d2])
        .await
        .expect("launch_drones");
    assert_eq!(launched.len(), 2);
    assert_eq!(drone_ctrl.active_drones.len(), 2);

    // Return and orbit ship (feat:drones.return-orbit)
    let orbit_count = drone_ctrl
        .return_and_orbit(&ctx, sys_id, ship_id)
        .await
        .expect("return_and_orbit");
    assert_eq!(orbit_count, 2);

    for drone in drone_ctrl.active_drones.values() {
        assert!(drone.target_id.is_none());
        let b = sys_handle
            .get_ball_snapshot(drone.ball_id)
            .await
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(
            b.mode,
            MovementMode::Orbit {
                target_id: ship_id,
                range: 1_000.0
            }
        );
    }

    // Recall drones to DroneBay (feat:drones.recall-drones)
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
async fn test_c8_crimewatch_concord_timers_and_security_status() {
    // parity: feat:dogma.weapon-timer
    // parity: feat:combat.aggression-tracking
    // parity: feat:crimewatch.security-status
    // parity: feat:crimewatch.sec-status-loss
    // parity: feat:crimewatch.sec-status-gain, rpc:charUnboundMgr.SelectCharacterID, rpc:crimewatch.GetMySecurityStatus
    eve_testkit::attest(&[
        "feat:dogma.weapon-timer",
        "feat:combat.aggression-tracking",
        "feat:crimewatch.security-status",
        "feat:crimewatch.sec-status-loss",
        "feat:crimewatch.sec-status-gain",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142; // Jita 0.95
    let now = 1_700_000_000;

    let mut cw_director = CrimeWatchDirector::new();

    // 1. Weapons timer (feat:dogma.weapon-timer)
    cw_director.trigger_weapon_timer(char_id, now);
    let state = cw_director.get_or_create(char_id);
    assert!(state.is_weapon_timer_active(now + 30));
    assert!(state.is_weapon_timer_active(now + 59));
    assert!(!state.is_weapon_timer_active(now + 61));

    // 2. Aggression tracking (feat:combat.aggression-tracking)
    let aggro = cw_director.aggression_mgr.record_aggression(
        char_id,
        victim_char_id,
        sys_id,
        false,
        false,
        now,
    );
    assert_eq!(aggro.aggressor_char_id, char_id);
    assert_eq!(aggro.victim_char_id, victim_char_id);
    assert!(cw_director
        .aggression_mgr
        .is_aggressor_against(char_id, victim_char_id, now + 100));

    let aggressors = cw_director
        .aggression_mgr
        .list_active_aggressors_for_victim(victim_char_id, now + 100);
    assert!(aggressors.contains(&char_id));

    // 3. Security status loss and gain formulas (feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-gain)
    let loss_ship = compute_sec_status_loss(0.5, false, 0.95);
    assert!(loss_ship >= 0.5, "Ship unprovoked attack penalty >= 0.5");

    let loss_pod = compute_sec_status_loss(0.5, true, 0.95);
    assert!(loss_pod >= 2.0, "Pod kill penalty >= 2.0");

    let gain = compute_sec_status_gain(0.0, 500_000.0);
    assert!(gain > 0.0, "Ratting bounty must increase security status");
    assert!(gain <= 0.5);

    // 4. Character Security Status persistence & RPC (feat:crimewatch.security-status)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.security_status = 2.4;
        }
    });

    let mut sess = SessionState::new(9902);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let sec_rep = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "GetMySecurityStatus", &[], &kw, None)
        .await
        .expect("GetMySecurityStatus");
    match sec_rep {
        PyRep::Real(v) => assert!((v - 2.4).abs() < 1e-6),
        other => panic!("Expected Real, got {other:?}"),
    }

    // 5. High-sec aggression -> Criminal flag + CONCORD response
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
    assert!(cw_director.get_or_create(char_id).is_criminal(now + 100));

    // Spawn criminal ship in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let crim_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(30_000.0, 0.0, 0.0),
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
    let reports = cw_director.step_concord(&ctx, now + 6, 7777).await;
    assert_eq!(reports.len(), 1);

    // CONCORD destroyed the criminal ship
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
    assert!(reports[0].death_result.is_ok());

    // Criminal security status penalized
    let crim_sec = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(crim_sec < 2.4, "Criminal security status must be reduced");
}
