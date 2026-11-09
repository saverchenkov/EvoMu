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
