use super::*;
use crate::system_actor::{AsteroidState, DEFAULT_DOWNTIME_UTC_SECS_OF_DAY};
use eve_destiny::{Ball, Ballpark, DestinyEvent, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry, SolarSystem, StargateJump, StationInfo};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

fn make_inv_type(type_id: u32, group_id: u32, category_id: u32, radius: f64, mass: f64, volume: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type-{type_id}"),
        mass,
        volume,
        capacity: 0.0,
        portion_size: 1,
        radius,
        base_price: 100.0,
        published: true,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_test_belt_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(6, make_inv_type(6, 6, 2, 50_000.0, 1.0e20, 1.0e12));
    sde.types.insert(15, make_inv_type(15, 9, 3, 1_500.0, 1.0e12, 1.0e9));
    sde.types.insert(16, make_inv_type(16, 10, 2, 12_000.0, 1.0e12, 1.0e9));
    sde.types.insert(1529, make_inv_type(1529, 15, 3, 25_000.0, 1.0e15, 1.0e12));
    sde.types.insert(1230, make_inv_type(1230, 462, 25, 250.0, 1.0e6, 0.1)); // Veldspar
    sde.types.insert(16264, make_inv_type(16264, 465, 25, 450.0, 1.0e6, 1_000.0)); // Blue Ice (group 465)

    let station_info = StationInfo {
        station_id: 60_999_001,
        solar_system_id: 30_009_999,
        constellation_id: 20_009_999,
        region_id: 10_009_999,
        station_type_id: 1529,
        name: "TestSystem IV - Caldari Navy Assembly Plant".into(),
        position: DVec3::new(3.0e9, 1.0e8, -2.0e9),
        owner_corporation_id: 1_000_009,
        owner_faction_id: Some(500_001),
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    };
    sde.stations.insert(60_999_001, station_info.clone());
    sde.solar_systems.insert(
        30_009_999,
        SolarSystem {
            solar_system_id: 30_009_999,
            constellation_id: 20_009_999,
            region_id: 10_009_999,
            name: "TestSystem".into(),
            security_status: 0.75,
            radius: 1.0e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![40_999_001],
            stations: vec![station_info],
            wormhole_class_id: None,
        },
    );
    sde.jumps.insert(
        50_999_001,
        StargateJump {
            stargate_id: 50_999_001,
            from_solar_system_id: 30_009_999,
            to_solar_system_id: 30_000_142,
            destination_stargate_id: 50_000_001,
            position: DVec3::new(-2.0e9, 0.0, 1.0e9),
        },
    );
    Arc::new(sde)
}

#[test]
fn populate_celestials_belts_and_slim_meta_cover_all_entity_categories() {
    let sde = make_test_belt_sde();
    let sde_opt = Some(sde.clone());
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let db_opt = Some(db);
    let mut bp = Ballpark::new(30_009_999);
    let mut asteroids = HashMap::new();
    let mut templates = HashMap::new();

    populate_system_celestials(&mut bp, 30_009_999, &sde);
    assert_eq!(bp.constellation_id, 20_009_999);
    assert!(bp.balls.contains_key(&50_999_001));
    assert!(bp.balls.contains_key(&60_999_001));
    assert_eq!(
        bp.ball_slim_meta.get(&50_999_001).and_then(|m| m.jump_target),
        Some((50_000_001, 30_000_142))
    );

    // Also populate Jita (30_000_142) if SDE has belts to exercise real belt ore spawning
    let spawned = populate_asteroid_belt_entities(&mut bp, 30_000_142, &sde);
    let populated = reconcile_spawned_belt_asteroids(
        &mut bp,
        30_000_142,
        spawned,
        &mut asteroids,
        &mut templates,
        &db_opt,
        86_400,
        DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
    );
    assert_eq!(populated.len(), asteroids.len());

    // Add dynamic balls across ship (6), drone (18), wreck (22), container (23), deployable (49)
    let dynamic_specs = [7001i64, 7002i64, 7003i64, 7004i64, 7005i64];
    for (idx, bid) in dynamic_specs.into_iter().enumerate() {
        let cat = [6u32, 18, 22, 23, 49][idx];
        bp.add_ball(Ball {
            id: bid,
            type_id: 587,
            position: DVec3::new(bid as f64, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        bp.ball_slim_meta.insert(
            bid,
            SlimItemMeta {
                group_id: 25,
                category_id: cat,
                owner_id: 90_000_001,
                char_id: (cat == 6).then_some(90_000_001),
                corp_id: 98_000_001,
                alliance_id: 99_000_001,
                security_status: 2.5,
                ..Default::default()
            },
        );
    }

    // Remove metadata on 7001 then call ensure_default_slim_meta to verify default insertion
    bp.ball_slim_meta.remove(&7001);
    let b7001 = bp.balls[&7001].clone();
    ensure_default_slim_meta(&mut bp, &b7001, &sde_opt);
    assert!(bp.ball_slim_meta.contains_key(&7001));

    let broadcast = build_tick_broadcast(
        30_009_999,
        &mut bp,
        vec![DestinyEvent::RemoveBall { ball_id: 7002 }],
    );
    assert_eq!(broadcast.solar_system_id, 30_009_999);
    assert_eq!(broadcast.events.len(), 6);
}

#[test]
fn mining_waste_depletion_respawn_and_downtime_belt_restore_work_end_to_end() {
    let sde = make_test_belt_sde();
    let sde_opt = Some(sde);
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let db_opt = Some(db.clone());
    let rng = crate::rng::MechanicRng::seeded(7);
    let interval_atomic = AtomicU64::new(86_400);
    let downtime_atomic = AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY);

    let mut bp = Ballpark::new(30_009_999);
    let mut asteroids = HashMap::new();
    let mut templates = HashMap::new();

    // Two regular belt asteroids (belt 40_999_002) and one ice asteroid (group 465, belt 0)
    for (ast_id, belt_id, type_id, units) in [
        (8001i64, 40_999_002i64, 1230u32, 100u64),
        (8002i64, 40_999_002i64, 1230u32, 50u64),
        (8003i64, 0i64, 16264u32, 40u64),
    ] {
        let ast = AsteroidState {
            asteroid_id: ast_id,
            belt_id,
            type_id,
            initial_units: units,
            remaining_units: units,
            unit_volume: 10.0,
            depleted: false,
            respawn_at_tick: None,
        };
        let ball = Ball {
            id: ast_id,
            type_id,
            position: DVec3::new(ast_id as f64 * 100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        };
        bp.add_ball(ball.clone());
        ensure_default_slim_meta(&mut bp, &ball, &sde_opt);
        asteroids.insert(ast_id, ast.clone());
        templates.insert(ast_id, (ast, ball));
    }

    // 1. Mine 8001 with 100% waste probability: 30 mined + 30 wasted = 40 remaining
    let r1 = handle_mine_asteroid(
        30_009_999,
        8001,
        30,
        1.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .expect("first mine succeeds");
    assert_eq!(r1.mined_units, 30);
    assert_eq!(r1.wasted_units, 30);
    assert_eq!(r1.remaining_units, 40);
    assert!(!r1.depleted);

    // 2. Deplete 8001; belt is NOT yet fully depleted because 8002 still has 50 units
    let r2 = handle_mine_asteroid(
        30_009_999,
        8001,
        40,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .expect("deplete 8001 succeeds");
    assert!(r2.depleted);
    assert!(!bp.balls.contains_key(&8001));
    assert!(asteroids[&8001].respawn_at_tick.is_none());

    // 3. Mining an already-depleted asteroid returns Err
    assert!(handle_mine_asteroid(
        30_009_999,
        8001,
        10,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .is_err());

    // 4. Mining nonexistent asteroid returns Err
    assert!(handle_mine_asteroid(
        30_009_999,
        999_999,
        10,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .is_err());

    // 5. Deplete 8002 -> entire belt 40_999_002 is now depleted and schedules respawn_at_tick
    let r3 = handle_mine_asteroid(
        30_009_999,
        8002,
        100,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .expect("deplete 8002 succeeds");
    assert_eq!(r3.mined_units, 50);
    assert!(r3.depleted);
    assert!(asteroids[&8001].respawn_at_tick.is_some());
    assert!(asteroids[&8002].respawn_at_tick.is_some());

    // 6. Partially mine ice block 8003 so downtime restores all 3 (both belt rocks + ice block)
    let _ = handle_mine_asteroid(
        30_009_999,
        8003,
        15,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .expect("partial ice mine succeeds");

    let restored = respawn_depleted_belts_on_downtime_inner(
        &mut bp,
        &mut asteroids,
        &templates,
        &sde_opt,
        &db_opt,
    );
    assert_eq!(restored, 3);
    assert_eq!(asteroids[&8001].remaining_units, 100);
    assert_eq!(asteroids[&8002].remaining_units, 50);
    assert_eq!(asteroids[&8003].remaining_units, 40);

    // 7. Also test time-driven respawn_due_asteroids when entire belt (8001 + 8002) is depleted & due
    for ast_id in [8001i64, 8002i64] {
        asteroids.get_mut(&ast_id).unwrap().remaining_units = 0;
        asteroids.get_mut(&ast_id).unwrap().depleted = true;
        asteroids.get_mut(&ast_id).unwrap().respawn_at_tick = Some(1);
        let _ = bp.remove_ball(ast_id);
    }
    let due_count = respawn_due_asteroids(&mut bp, &mut asteroids, &templates, &sde_opt, &db_opt);
    assert_eq!(due_count.len(), 2);
    assert_eq!(asteroids[&8001].remaining_units, 100);
    assert_eq!(asteroids[&8002].remaining_units, 50);
}

#[test]
fn sync_player_slim_items_updates_corp_alliance_and_security_status() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let char_id = db
        .with_snapshot(|snap| snap.characters.keys().next().copied())
        .expect("demo seed has a character");
    let (corp_id, alliance_id, sec) = db
        .get_character(char_id)
        .map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0), c.security_status))
        .expect("character lookup");

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(Ball {
        id: 10_001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let mut initial_events = BTreeMap::new();
    initial_events.insert(10_001i64, Vec::new());
    bp.observer_events = initial_events;
    bp.ball_slim_meta.insert(
        10_001,
        SlimItemMeta {
            category_id: 6,
            group_id: 25,
            char_id: Some(char_id),
            owner_id: char_id,
            corp_id: corp_id + 999,
            alliance_id,
            security_status: sec + 1.0,
            ..Default::default()
        },
    );

    sync_player_slim_items(&mut bp, &Some(db));
    let updated = bp.ball_slim_meta.get(&10_001).expect("slim meta present");
    assert_eq!(updated.corp_id, corp_id);
    assert!((updated.security_status - sec).abs() < 1e-6);
}

#[test]
fn asteroid_depletion_and_downtime_restore_preserve_lod0_hibernation_and_notify_observers() {
    let sde = make_test_belt_sde();
    let sde_opt = Some(sde.clone());
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let db_opt = Some(db);
    let rng = crate::rng::MechanicRng::seeded(11);
    let interval_atomic = AtomicU64::new(86_400);
    let downtime_atomic = AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY);

    let mut bp = Ballpark::new(30_009_999);
    populate_system_celestials(&mut bp, 30_009_999, &sde);
    assert!(!bp.has_dynamic_balls());

    let mut asteroids = HashMap::new();
    let mut templates = HashMap::new();
    let ast_id = 750_999_001i64;
    let belt_id = 40_999_050i64;
    let ast = AsteroidState {
        asteroid_id: ast_id,
        belt_id,
        type_id: 1230,
        initial_units: 50,
        remaining_units: 50,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    let ast_ball = Ball {
        id: ast_id,
        type_id: 1230,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 1.0e8,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    bp.add_static_ball_with_meta(
        ast_ball.clone(),
        SlimItemMeta {
            group_id: 462,
            category_id: 25,
            owner_id: 1,
            name: Some("Veldspar".into()),
            ..Default::default()
        },
    );
    asteroids.insert(ast_id, ast.clone());
    templates.insert(ast_id, (ast, ast_ball));

    // Populated static asteroid keeps `has_dynamic_balls() == false` so empty systems hibernate at LOD 0.
    assert!(!bp.has_dynamic_balls());
    assert!(bp.is_static_ball(ast_id));

    // Spawn a live miner ship on grid; depleting the static asteroid must emit `RemoveBall` to the miner,
    // and downtime belt restoration must emit `AddBalls2` with `MovementMode::Rigid`.
    let miner_id = 91_001i64;
    bp.add_ball(Ball {
        id: miner_id,
        type_id: 32_880,
        position: DVec3::new(4_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 180.0,
        inertia: 3.0,
        mass: 4_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    assert!(bp.has_dynamic_balls());
    bp.drain_all_observer_events();

    let res = handle_mine_asteroid(
        30_009_999,
        ast_id,
        50,
        0.0,
        true,
        &mut bp,
        &mut asteroids,
        &rng,
        &db_opt,
        &interval_atomic,
        &downtime_atomic,
    )
    .expect("deplete asteroid");
    assert!(res.depleted);
    assert!(!bp.balls.contains_key(&ast_id));
    let deplete_evs = bp.drain_observer_events(miner_id);
    assert!(deplete_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id } if *ball_id == ast_id)));

    let restored = respawn_depleted_belts_on_downtime_inner(
        &mut bp,
        &mut asteroids,
        &templates,
        &sde_opt,
        &db_opt,
    );
    assert_eq!(restored, 1);
    assert_eq!(bp.balls[&ast_id].mode, MovementMode::Rigid);
    assert!(bp.is_static_ball(ast_id));
    let respawn_evs = bp.drain_observer_events(miner_id);
    assert!(respawn_evs.iter().any(|e| {
        matches!(
            e,
            DestinyEvent::AddBalls2 { balls }
                if balls.iter().any(|b| b.id == ast_id && b.mode == MovementMode::Rigid)
        )
    }));

    // Removing the miner returns the system to `has_dynamic_balls() == false` (LOD 0 hibernation).
    bp.remove_ball(miner_id);
    assert!(!bp.has_dynamic_balls());

    // Stationary helper pins wrecks/jetcans/deployables while leaving mobile ships/drones free.
    let wreck_ball = Ball {
        id: 92_001,
        type_id: 26_557,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    assert!(should_pin_stationary_ball(&wreck_ball, None));
    assert!(should_pin_stationary_ball(
        &wreck_ball,
        Some(&SlimItemMeta {
            category_id: 22,
            ..Default::default()
        })
    ));
    let ship_ball = Ball {
        max_velocity: 300.0,
        type_id: 587,
        ..wreck_ball.clone()
    };
    assert!(!should_pin_stationary_ball(&ship_ball, None));
    assert!(!should_pin_stationary_ball(
        &ship_ball,
        Some(&SlimItemMeta {
            category_id: 6,
            ..Default::default()
        })
    ));
}

#[test]
fn stargate_slim_items_emit_faction_ids_in_empire_and_none_in_nullsec() {
    use eve_destiny::wire::slim_item_dict;
    use eve_proto::PyRep;

    let sde = SdeRegistry::with_canonical_seed();

    let extract_owner = |dict: &PyRep, key: &str| -> PyRep {
        let PyRep::Dict(entries) = dict else {
            panic!("expected SlimItem Dict, got {dict:?}");
        };
        entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v.clone())
            .expect("missing slim key")
    };

    // 1. Jita (30000142) stargates carry empire faction IDs (> 1) on originSystemOwnerID & destinationSystemOwnerID
    let mut jita_bp = Ballpark::new(30_000_142);
    populate_system_celestials(&mut jita_bp, 30_000_142, &sde);
    let jita_gates: Vec<_> = jita_bp
        .balls
        .values()
        .filter(|b| jita_bp.ball_group_id(b.id) == Some(10))
        .collect();
    assert!(!jita_gates.is_empty(), "Jita must have stargates");
    for gate in jita_gates {
        let meta = jita_bp.ball_slim_meta(gate.id).expect("gate meta");
        let slim = slim_item_dict(gate, meta);
        let orig = extract_owner(&slim, "originSystemOwnerID");
        let dest = extract_owner(&slim, "destinationSystemOwnerID");
        assert!(
            matches!(orig, PyRep::Integer(id) if id > 500_000),
            "Jita stargate originSystemOwnerID must be real faction (> 500000), got {orig:?}"
        );
        assert!(
            matches!(dest, PyRep::Integer(id) if id > 500_000),
            "Jita->empire stargate destinationSystemOwnerID must be real faction (> 500000), got {dest:?}"
        );
    }

    // 2. Nullsec OEG-K9 (30000439) has no NPC faction -> both keys must be PyRep::None, never Integer(1)
    let mut null_bp = Ballpark::new(30_000_439);
    populate_system_celestials(&mut null_bp, 30_000_439, &sde);
    let null_gates: Vec<_> = null_bp
        .balls
        .values()
        .filter(|b| null_bp.ball_group_id(b.id) == Some(10))
        .collect();
    assert!(!null_gates.is_empty(), "OEG-K9 must have stargates");
    for gate in null_gates {
        let meta = null_bp.ball_slim_meta(gate.id).expect("null gate meta");
        let slim = slim_item_dict(gate, meta);
        assert_eq!(
            extract_owner(&slim, "originSystemOwnerID"),
            PyRep::None,
            "nullsec stargate originSystemOwnerID must be None, never Integer(1)"
        );
        assert_eq!(
            extract_owner(&slim, "destinationSystemOwnerID"),
            PyRep::None,
            "nullsec stargate destinationSystemOwnerID must be None, never Integer(1)"
        );
    }
}

#[tokio::test]
async fn step_manual_and_fast_forward_preserve_pending_set_state_observer_queue() {
    let reg = crate::system_actor::SystemRegistry::new();
    let sys = reg.get_or_spawn(30_000_142);

    let ship_init = 200_092_496i64;
    let ship_ready = 200_092_497i64;
    for (id, x) in [(ship_init, 0.0), (ship_ready, 1_500.0)] {
        sys.spawn_ball(Ball {
            id,
            type_id: 587,
            position: DVec3::new(x, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    }

    // Drain initial spawn events on both ships, then queue SetState on `ship_init`
    // while queuing a normal movement event on `ship_ready`.
    let _ = sys.drain_observer_updates(ship_init).await.unwrap();
    let _ = sys.drain_observer_updates(ship_ready).await.unwrap();
    sys.push_set_state(ship_init).await.unwrap();
    sys.set_movement_mode(
        ship_init,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    )
    .await
    .unwrap();
    sys.set_speed_fraction(ship_init, 1.0).await.unwrap();

    // 1. 1 Hz StepManual must drain `ship_ready`'s peer events into TickBroadcast
    //    WITHOUT stealing `ship_init`'s pending SetState queue (#802).
    let bcast = sys.step_manual(1.0).await.unwrap();
    assert!(
        !bcast.observer_events.contains_key(&ship_init),
        "build_tick_broadcast must not steal observer queue containing pending SetState"
    );
    assert!(
        bcast.observer_events.contains_key(&ship_ready),
        "ready peer without SetState must still be drained on 1 Hz tick"
    );

    // 2. fast_forward_clock must also preserve `ship_init`'s SetState at index 0.
    sys.fast_forward_clock(ship_init, 8).await.unwrap();

    let (_tick, init_evs, _pyrep) = sys.drain_observer_updates(ship_init).await.unwrap();
    assert!(
        matches!(
            init_evs.first(),
            Some(DestinyEvent::SetState {
                observer_ball_id
            }) if *observer_ball_id == ship_init
        ),
        "SetState must remain at index 0 after StepManual + fast_forward_clock, got {init_evs:?}"
    );
}

#[test]
fn build_tick_broadcast_skips_empty_and_set_state_observers_and_drains_ready() {
    let mut bp = Ballpark::new(30_000_142);
    bp.current_tick = 42;
    bp.observer_events.insert(101, Vec::new());
    bp.observer_events.insert(
        102,
        vec![
            DestinyEvent::SetState {
                observer_ball_id: 102,
            },
            DestinyEvent::Stop { ball_id: 102 },
        ],
    );
    bp.observer_events
        .insert(103, vec![DestinyEvent::Stop { ball_id: 103 }]);

    let bcast = build_tick_broadcast(30_000_142, &mut bp, Vec::new());
    assert_eq!(bcast.tick, 42);
    assert!(!bcast.observer_events.contains_key(&101));
    assert!(!bcast.observer_events.contains_key(&102));
    assert_eq!(bp.observer_events.get(&102).map(Vec::len), Some(2));
    assert_eq!(bcast.observer_events.get(&103).map(Vec::len), Some(1));
    assert!(bcast.observer_updates_pyrep.contains_key(&103));
}

#[test]
fn belt_asteroid_base_id_restore_and_reconcile_spawned_belt_asteroids() {
    // 1. `belt_asteroid_base_id` three-branch solar_system_id mapping
    assert_eq!(belt_asteroid_base_id(30_000_144), 750_000_000);
    assert_eq!(belt_asteroid_base_id(30_000_142), 750_000_000 + 142 * 6_000);
    assert_eq!(belt_asteroid_base_id(31_000_005), 750_000_000 + 5_605 * 6_000);

    // 2. `restore_asteroid_from_template` resets remaining_units/depleted/respawn_at_tick and respawns ball
    let mut bp = Ballpark::new(30_000_142);
    let mut asteroids = HashMap::new();
    let orig_ast = AsteroidState {
        asteroid_id: 750_852_001,
        belt_id: 40_009_087,
        type_id: 1230,
        initial_units: 20_000,
        remaining_units: 0,
        unit_volume: 0.1,
        depleted: true,
        respawn_at_tick: Some(500),
    };
    let orig_ball = Ball {
        id: 750_852_001,
        type_id: 1230,
        position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e9,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    restore_asteroid_from_template(
        750_852_001,
        &orig_ast,
        &orig_ball,
        &mut bp,
        &mut asteroids,
        &None,
    );
    let restored = &asteroids[&750_852_001];
    assert_eq!(restored.remaining_units, 20_000);
    assert!(!restored.depleted);
    assert_eq!(restored.respawn_at_tick, None);
    assert!(bp.balls.contains_key(&750_852_001));

    // 3. `reconcile_spawned_belt_asteroids` reconciles DB quantities (depleted <= 0 vs partial > 0 vs newly seeded)
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ast1 = AsteroidState {
        asteroid_id: 750_852_010,
        belt_id: 40_009_087,
        type_id: 1230,
        initial_units: 10_000,
        remaining_units: 10_000,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    let ast2 = AsteroidState {
        asteroid_id: 750_852_011,
        belt_id: 40_009_087,
        type_id: 1230,
        initial_units: 10_000,
        remaining_units: 10_000,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    bp.add_static_ball_with_meta(
        Ball {
            id: 750_852_010,
            type_id: 1230,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        SlimItemMeta::default(),
    );
    bp.add_static_ball_with_meta(
        Ball {
            id: 750_852_011,
            type_id: 1230,
            position: DVec3::new(200.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        SlimItemMeta::default(),
    );
    db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            750_852_010,
            eve_db::AsteroidRecord {
                asteroid_id: 750_852_010,
                solar_system_id: 30_000_142,
                belt_id: 40_009_087,
                type_id: 1230,
                position: (100.0, 0.0, 0.0),
                quantity: 0,
                initial_quantity: 10_000,
                radius: 80.0,
            },
        );
        snap.asteroids.insert(
            750_852_011,
            eve_db::AsteroidRecord {
                asteroid_id: 750_852_011,
                solar_system_id: 30_000_142,
                belt_id: 40_009_087,
                type_id: 1230,
                position: (200.0, 0.0, 0.0),
                quantity: 4_321,
                initial_quantity: 10_000,
                radius: 80.0,
            },
        );
    });
    let mut templates = HashMap::new();
    let out = reconcile_spawned_belt_asteroids(
        &mut bp,
        30_000_142,
        vec![ast1, ast2],
        &mut asteroids,
        &mut templates,
        &Some(db),
        14_400,
        39_600,
    );
    assert_eq!(out.len(), 2);
    assert!(asteroids[&750_852_010].depleted);
    assert!(!bp.balls.contains_key(&750_852_010));
    assert_eq!(asteroids[&750_852_011].remaining_units, 4_321);
    assert!(bp.balls.contains_key(&750_852_011));
}
