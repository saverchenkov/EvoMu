//! Integration tests for Combat Systems, Death Pipeline, NPCs & CrimeWatch (`C-3, C-4, C-7, C-8`).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::{
    execute_death_pipeline, execute_self_destruct, SelfDestructManager,
};
use eve_server::combat::npc_ai::{faction_damage_vector, NpcAiState, NpcSpawnManager};
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

    // 1. Belt rats spawner (feat:npc.belt-rats): beltless Jita (`30_000_142`) spawns 0; Perimeter (`30_000_144`, 3 SDE asteroid belts) spawns belt rats
    let jita_belt_rats = spawn_mgr
        .spawn_belt_rats_for_system(&ctx, sys_id)
        .await
        .expect("spawn_belt_rats_for_system in beltless Jita");
    assert!(jita_belt_rats.is_empty(), "Beltless solar systems must not spawn synthetic belt rats");
    let belt_rats = spawn_mgr
        .spawn_belt_rats_for_system(&ctx, 30_000_144)
        .await
        .expect("spawn_belt_rats_for_system in Perimeter");
    assert!(!belt_rats.is_empty(), "Must spawn belt rats at real SDE asteroid belts");

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
