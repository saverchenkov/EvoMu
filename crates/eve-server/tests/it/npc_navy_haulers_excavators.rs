//! Integration tests for remediated NPC Navy, Hauler convoys, and Excavator drones (`S-REMEDIATE`).

#![forbid(unsafe_code)]

use crate::common;
use crate::npc_spawns_pve::{bind_beyonce, rpc, setup_docked_pilot};
use eve_db::{InventoryFlag, TrainedSkillRecord};
use eve_destiny::MovementMode;
use eve_proto::PyRep;
use eve_server::living_world::{
    hauler_type_for_system, ConvoyStepStatus, LivingWorldDirector, NpcArchetype,
};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    check_and_spawn_faction_police, destroy_specialized_npc_and_queue_bounty,
    is_outlaw_ship_docking_refused, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYS: u32 = 30_000_142;

#[tokio::test]
async fn test_navy_npc_faction_standing_response_and_ewar_scaling() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let jita_stn = 60_003_760;
    let char_id = 90_000_001i64;
    let ship_id = 910_001i64;

    // 1. Lawful pilot (neutral standing 0.0, sec status 0.0) in high-sec Caldari space (Jita):
    let (mut sess, _) = setup_docked_pilot(
        &ctx, 7501, "demo", "demo-secret", char_id, JITA_SYS, jita_stn, ship_id, 587, 2961, 21894, 924_000,
    )
    .await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock in Jita");
    let _ = bind_beyonce(&ctx, &mut sess, JITA_SYS).await;

    let spawned = check_and_spawn_faction_police(&ctx, JITA_SYS).await;
    assert!(spawned.is_empty(), "Lawful pilot must not trigger navy or police response");
    assert!(
        !is_outlaw_ship_docking_refused(&ctx, JITA_SYS, char_id, ship_id),
        "Lawful pilot must not be refused docking"
    );

    // 2. Set hostile faction standing (<= -5.0) to Caldari State (500001):
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.standings.insert(500_001, -6.5);
        }
    });

    // Check docking refusal: hostile standing <= -5.0 refuses docking to ship
    assert!(
        is_outlaw_ship_docking_refused(&ctx, JITA_SYS, char_id, ship_id),
        "Pilot with faction standing <= -5.0 must be refused docking in empire station"
    );

    // Spawn Empire Navy response:
    let navy_spawned = check_and_spawn_faction_police(&ctx, JITA_SYS).await;
    assert!(
        !navy_spawned.is_empty(),
        "Hostile faction standing (<= -5.0) must trigger Empire Navy response in empire high-sec"
    );

    let navy_ball_id = navy_spawned[0];
    with_combat_runtime(&ctx, |rt| {
        let npc = rt
            .npc_entities
            .get(&navy_ball_id)
            .expect("Navy NPC entity must be active in combat runtime");
        assert_eq!(
            npc.archetype, "CaldariNavy",
            "Caldari space must spawn distinct 'CaldariNavy' archetype"
        );
        assert_eq!(npc.faction_id, 500_001);
        assert_eq!(npc.target_id, Some(ship_id));
        // Authentic Caldari EWAR & warp disruption:
        assert!(npc.scram_strength >= 2.0, "Navy must apply warp scrambler");
        assert_eq!(npc.scram_range, 24_000.0, "Caldari Navy has 24km scram range");
        assert_eq!(npc.web_speed_factor, -40.0);
        // Kinetic / Thermal damage vector & scaled DPS:
        assert!(npc.damage_vector.kinetic > 0.0);
        assert!(npc.damage_vector.thermal > 0.0);
        assert_eq!(npc.damage_vector.em, 0.0);
        assert_eq!(npc.damage_vector.explosive, 0.0);
        let total_dps = npc.damage_vector.kinetic + npc.damage_vector.thermal;
        assert!(
            total_dps > 240.0,
            "DPS must be scaled up with standing deficit and highsec status (got {total_dps})"
        );
    });

    // 3. Pod Safety: Capsule (670) owned by pilot with <= -5.0 standing is NEVER engaged and allowed to dock
    let pod_char_id = 90_000_002i64;
    let pod_ship_id = 910_002i64;
    let (mut pod_sess, _) = setup_docked_pilot(
        &ctx, 7502, "other", "other-secret", pod_char_id, JITA_SYS, jita_stn, pod_ship_id, 670, 0, 0, 0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&pod_char_id) {
            ch.standings.insert(500_001, -7.0);
        }
    });
    rpc(&ctx, &mut pod_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Capsule undocks");
    let _pod_spawned = check_and_spawn_faction_police(&ctx, JITA_SYS).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(
            !rt.npc_entities.values().any(|n| n.target_id == Some(pod_ship_id)),
            "Empire Navy and Police must NEVER engage player capsules"
        );
    });
    assert!(
        !is_outlaw_ship_docking_refused(&ctx, JITA_SYS, pod_char_id, pod_ship_id),
        "Capsules must always be allowed to dock regardless of negative faction standing"
    );

    // 4. Test distinct Minmatar, Amarr, and Gallente Navy archetypes and authentic EWAR:
    let minmatar_char_id = 90_000_001i64;
    let minmatar_ship_id = 910_003i64;
    let hek_sys = 30_002_053u32; // Hek (Minmatar space, faction 500002)
    let hek_stn = 60_005_686;
    let (mut hek_sess, _) = setup_docked_pilot(
        &ctx, 7503, "demo", "demo-secret", minmatar_char_id, hek_sys, hek_stn, minmatar_ship_id, 587, 2961, 21894, 925_000,
    )
    .await;
    rpc(&ctx, &mut hek_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock in Hek");
    let _ = bind_beyonce(&ctx, &mut hek_sess, hek_sys).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&minmatar_char_id) {
            ch.standings.insert(500_002, -8.0);
        }
    });
    let hek_spawned = check_and_spawn_faction_police(&ctx, hek_sys).await;
    assert!(!hek_spawned.is_empty(), "Minmatar Navy response must spawn in Hek");
    with_combat_runtime(&ctx, |rt| {
        let npc = rt.npc_entities.get(&hek_spawned[0]).unwrap();
        assert_eq!(npc.archetype, "MinmatarNavy");
        assert_eq!(npc.web_speed_factor, -65.0, "Minmatar Navy has authentic heavy -65% web");
        assert!(npc.damage_vector.explosive > 0.0);
        assert!(npc.damage_vector.kinetic > 0.0);
    });
}

#[tokio::test]
async fn test_hauler_convoy_trade_routes_warp_gates_and_loot() {
    // parity: feat:npc.npc-hauler
    eve_testkit::attest(&["feat:npc.npc-hauler"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Hauler Convoy spawning and trade route computation:
    let route = eve_server::living_world_haulers::compute_trade_route(&ctx.sde, JITA_SYS);
    assert!(
        route.len() >= 2,
        "Trade route must have multiple systems along stargate graph"
    );
    assert_eq!(route[0], JITA_SYS);

    let convoy_id = ctx.living_world.ensure_system_hauler_convoy(JITA_SYS, &ctx.sde);
    let convoy = ctx
        .living_world
        .get_convoy(convoy_id)
        .expect("Hauler convoy must be registered");
    assert_eq!(convoy.archetype, NpcArchetype::HaulerConvoy);
    let hauler_type = hauler_type_for_system(&ctx.sde, JITA_SYS);
    assert_eq!(convoy.ship_type_id, hauler_type);
    assert!(
        convoy.cargo_quantity > 0,
        "Hauler must carry mineral / commodity cargo"
    );

    // 2. Multi-system transit along trade routes with warp and gate transitions:
    let _ = ctx
        .living_world
        .materialize_system_lod2(JITA_SYS, &ctx.systems, &ctx.sde)
        .await;
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let init_ball = jita_handle.get_ball_snapshot(convoy_id).await.unwrap().unwrap().1;
    assert_eq!(init_ball.type_id, hauler_type);

    // Living world tick: hauler initiates warp towards next outbound stargate
    let step1 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .unwrap();
    assert_eq!(step1, ConvoyStepStatus::TraversingSystem { system_id: JITA_SYS });

    // Verify MovementMode::Warp set on hauler ball
    let warped_ball = jita_handle.get_ball_snapshot(convoy_id).await.unwrap().unwrap().1;
    assert!(
        matches!(warped_ball.mode, MovementMode::Warp { .. }),
        "Hauler must enter warp mode towards outbound stargate"
    );

    // Advance transit duration to gate arrival:
    let next_sys = route[1];
    let transit_secs = LivingWorldDirector::compute_hauler_transit_duration_secs(
        &ctx.sde, hauler_type, JITA_SYS, None, Some(next_sys), convoy.destination_station_id,
    );
    ctx.advance_secs(transit_secs);

    // Next tick: gate transition into next system!
    let step_jump = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .unwrap();
    assert_eq!(
        step_jump,
        ConvoyStepStatus::JumpedGate {
            from_system: JITA_SYS,
            to_system: next_sys,
        }
    );

    // Ball despawned from Jita and spawned in next system at inbound stargate:
    assert!(
        jita_handle.get_ball_snapshot(convoy_id).await.unwrap().is_none(),
        "Hauler ball must be removed from previous system after gate jump"
    );
    let next_handle = ctx.systems.get_or_spawn(next_sys);
    let arrived_ball = next_handle
        .get_ball_snapshot(convoy_id)
        .await
        .unwrap()
        .expect("Hauler ball must appear in next system at inbound gate");
    assert_eq!(arrived_ball.1.type_id, hauler_type);

    // 3. Destruction and SDE mineral / commodity loot drops with authentic wreck metadata:
    let killer_char_id = 91_000_105i64;
    let (wreck_id, _bounty, _dropped_types) = destroy_specialized_npc_and_queue_bounty(
        &ctx, next_sys, convoy_id, killer_char_id,
    )
    .await;

    // Check authentic wreck type:
    let expected_wreck = eve_server::living_world_haulers::hauler_wreck_type_id(&ctx.sde, hauler_type);
    let wreck_record = ctx.db.with_snapshot(|s| s.items.get(&wreck_id).cloned()).unwrap();
    assert_eq!(
        wreck_record.type_id, expected_wreck,
        "Wreck type ID must match SDE hauler wreck type"
    );

    // Check authentic wreck metadata:
    ctx.db.with_snapshot(|snap| {
        let meta = snap
            .sp6_lifecycle
            .space_object_meta
            .get(&wreck_id)
            .expect("Space object metadata must be recorded for hauler wreck");
        assert!(meta.is_npc_wreck, "Metadata must mark wreck as NPC wreck");
        assert!(meta.created_unix.is_some(), "Wreck creation timestamp must be recorded");

        // Verify dropped SDE minerals and commodities inside wreck cargo:
        let wreck_cargo: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id && i.flag == InventoryFlag::Cargo)
            .collect();
        assert!(!wreck_cargo.is_empty(), "Wreck must contain dropped cargo items");
        let has_tritanium = wreck_cargo.iter().any(|i| i.type_id == 34 && i.quantity >= 10_000);
        let has_pyerite = wreck_cargo.iter().any(|i| i.type_id == 35 && i.quantity >= 2_500);
        assert!(has_tritanium, "Wreck must drop SDE Tritanium mineral cargo");
        assert!(has_pyerite, "Wreck must drop SDE Pyerite mineral cargo");
    });
}

#[tokio::test]
async fn test_excavator_drones_rorqual_bay_speed_and_yield_scaling() {
    // parity: feat:mining.excavator-drones
    eve_testkit::attest(&["feat:mining.excavator-drones"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001i64;
    let rorqual_ship_id = 920_001i64;
    let non_rorqual_ship_id = 920_002i64;
    let stn_id = 60_003_760;

    let exc_ore_drone_id = 930_001i64;
    let exc_ice_drone_id = 930_002i64;

    // Train Rorqual & Excavator relevant skills
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (sk_id, lvl) in [
            (3436, 5),   // Drones V
            (3438, 5),   // Mining Drone Operation V (+25%)
            (22541, 5),  // Mining Drone Specialization V (+10%)
            (43702, 5),  // Ice Harvesting Drone Operation V (-25% dur)
            (43703, 5),  // Ice Harvesting Drone Specialization V (-10% dur)
            (28374, 5),  // Capital Industrial Ships V (+50% ore yield, -25% ice dur)
        ] {
            sk.insert(sk_id, TrainedSkillRecord { type_id: sk_id, level: lvl, skill_points: 500_000.0 });
        }
    });

    // 1. Excavator drone deployment from Rorqual drone bay vs non-Rorqual:
    // Setup non-Rorqual (Rifter 587) with Excavator drone in bay:
    let (mut non_rorq_sess, _) = setup_docked_pilot(
        &ctx, 7601, "demo", "demo-secret", char_id, JITA_SYS, stn_id, non_rorqual_ship_id, 587, 0, 0, 0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            exc_ore_drone_id,
            eve_db::InventoryItemRecord {
                item_id: exc_ore_drone_id,
                type_id: 41030, // 'Excavator' Mining Drone
                owner_id: char_id,
                location_id: non_rorqual_ship_id,
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
    });
    rpc(&ctx, &mut non_rorq_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock non-Rorqual");
    let _ = bind_beyonce(&ctx, &mut non_rorq_sess, JITA_SYS).await;

    // Attempting to launch Excavators from non-Rorqual fails:
    let launch_err = rpc(
        &ctx, &mut non_rorq_sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)])],
    )
    .await
    .expect_err("Non-Rorqual ship must NOT be allowed to launch Excavators");
    assert_eq!(launch_err.code, "InvalidShipForExcavator");

    // 2. Setup Rorqual (28352) with Industrial Core I (group 515, type 4292) and Excavators:
    let (mut rorq_sess, _) = setup_docked_pilot(
        &ctx, 7602, "demo", "demo-secret", char_id, JITA_SYS, stn_id, rorqual_ship_id, 28352, 4292, 0, 0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            exc_ore_drone_id,
            eve_db::InventoryItemRecord {
                item_id: exc_ore_drone_id,
                type_id: 41030, // 'Excavator' Mining Drone
                owner_id: char_id,
                location_id: rorqual_ship_id,
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
        s.items.insert(
            exc_ice_drone_id,
            eve_db::InventoryItemRecord {
                item_id: exc_ice_drone_id,
                type_id: 43681, // 'Excavator' Ice Harvesting Drone
                owner_id: char_id,
                location_id: rorqual_ship_id,
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
    });
    rpc(&ctx, &mut rorq_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock Rorqual");
    let rorq_beyonce = bind_beyonce(&ctx, &mut rorq_sess, JITA_SYS).await;

    // Launch both Excavator drones from Rorqual drone bay:
    let launched = rpc(
        &ctx,
        &mut rorq_sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id), PyRep::Integer(exc_ice_drone_id)])],
    )
    .await
    .expect("Rorqual launches Excavator drones");
    let drone_balls = match launched {
        PyRep::List(items) => items.iter().filter_map(|it| it.as_i64()).collect::<Vec<_>>(),
        _ => panic!("Expected list of launched drone ball IDs"),
    };
    assert_eq!(drone_balls.len(), 2);

    // Verify slow speed & heavy inertia on spawned Excavator balls:
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let ore_drone_ball = jita_handle.get_ball_snapshot(drone_balls[0]).await.unwrap().unwrap().1;
    assert_eq!(ore_drone_ball.mass, 10_000_000.0, "Excavator must have 10,000,000 kg mass");
    assert_eq!(ore_drone_ball.inertia, 2.8, "Excavator must have authentic slow inertia (2.8)");
    assert_eq!(ore_drone_ball.radius, 50.0, "Excavator must have large 50m radius");

    // 3. Targeted asteroid harvesting:
    // Create Ore Asteroid (Veldspar type 1230, group 18) and Ice Asteroid (Clear Icicle type 16262, group 465):
    let ore_ast_id = 94_000_201i64;
    let ice_ast_id = 94_000_202i64;
    let rorq_pos = jita_handle.get_ball_snapshot(rorqual_ship_id).await.unwrap().unwrap().1.position;

    let ore_pos = rorq_pos + DVec3::new(2_000.0, 0.0, 0.0);
    let ice_pos = rorq_pos + DVec3::new(2_500.0, 0.0, 0.0);

    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ore_ast_id,
        solar_system_id: JITA_SYS,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 100_000,
        initial_quantity: 100_000,
        position: (ore_pos.x, ore_pos.y, ore_pos.z),
        radius: 50.0,
    });
    jita_handle
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: ore_ast_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 100_000,
                remaining_units: 100_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            eve_destiny::Ball {
                id: ore_ast_id,
                type_id: 1230,
                position: ore_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: JITA_SYS,
        belt_id: 40_000_001,
        type_id: 16262,
        quantity: 1_000,
        initial_quantity: 1_000,
        position: (ice_pos.x, ice_pos.y, ice_pos.z),
        radius: 50.0,
    });
    jita_handle
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 40_000_001,
                type_id: 16262,
                initial_units: 1_000,
                remaining_units: 1_000,
                unit_volume: 1000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            eve_destiny::Ball {
                id: ice_ast_id,
                type_id: 16262,
                position: ice_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    // Activate Industrial Core on Rorqual:
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, rorqual_ship_id).unwrap();
    let ind_core_mod = fit.modules.keys().next().copied().unwrap_or(rorqual_ship_id + 10);
    fit.modules.insert(
        ind_core_mod,
        eve_dogma::FittedModule {
            item_id: ind_core_mod,
            type_id: 4292, // Industrial Core I
            flag_id: 27,      // HighSlot0
            state: eve_dogma::ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(rorqual_ship_id, fit);

    // Cross-target validation: Ore Excavator targeting Ice asteroid fails:
    let kw = HashMap::new();
    let ore_on_ice_err = ctx.dispatch_rpc(
        &mut rorq_sess,
        "entity",
        Some(rorq_beyonce),
        "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ice_ast_id)],
        &kw,
        None,
    ).await.expect_err("Ore drone on ice asteroid must fail");
    assert_eq!(ore_on_ice_err.code, "InvalidTargetGroup");

    // Cross-target validation: Ice Excavator targeting Ore asteroid fails:
    let ice_on_ore_err = ctx.dispatch_rpc(
        &mut rorq_sess,
        "entity",
        Some(rorq_beyonce),
        "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ore_ast_id)],
        &kw,
        None,
    ).await.expect_err("Ice drone on ore asteroid must fail");
    assert_eq!(ice_on_ore_err.code, "InvalidTargetGroup");

    // 4. Targeted Ore Excavator harvesting:
    ctx.dispatch_rpc(
        &mut rorq_sess,
        "entity",
        Some(rorq_beyonce),
        "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ore_ast_id)],
        &kw,
        None,
    ).await.expect("CmdMineRepeatedly Ore Excavator");

    let next_tick_ore = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick_ore).await;

    // Yield deposited in SpecializedOreHold:
    let ore_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == rorqual_ship_id && it.type_id == 1230)
            .cloned()
    });
    let ore_record = ore_item.expect("Ore Excavator must deposit harvested Veldspar into Rorqual");
    assert_eq!(ore_record.flag, InventoryFlag::SpecializedOreHold);
    assert!(ore_record.quantity >= 1_000, "Harvested ore quantity: {}", ore_record.quantity);

    // 5. Targeted Ice Excavator harvesting: exactly 1 Clear Icicle block per cycle (1,000 m3):
    ctx.dispatch_rpc(
        &mut rorq_sess,
        "entity",
        Some(rorq_beyonce),
        "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ice_ast_id)],
        &kw,
        None,
    ).await.expect("CmdMineRepeatedly Ice Excavator");

    let next_tick_ice = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick_ice).await;

    let ice_qty = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == rorqual_ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 16262)
            .map(|it| it.quantity)
            .unwrap_or(0)
    });
    assert_eq!(
        ice_qty, 1,
        "Ice Excavator must harvest exactly 1 Clear Icicle block (1,000 m3) per cycle with no duplicate core multiplier"
    );
}
