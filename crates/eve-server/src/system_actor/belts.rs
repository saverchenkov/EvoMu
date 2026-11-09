//! Celestial population, asteroid belt extraction, and slim item synchronization for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    AsteroidState, MiningExtractionResult, TickBroadcast, next_downtime_unix_secs,
};
use eve_destiny::wire::encode_destiny_updates;
use eve_destiny::{
    Ball, BallId, Ballpark, DestinyEvent, MovementMode, SlimItemMeta,
};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Populates a solar system's `Ballpark` with its real SDE stars, planets, moons, asteroid belts,
/// stargates, and stations as static `MovementMode::Rigid` balls (`SP-1`).
pub fn populate_system_celestials(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) {
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        ballpark.constellation_id = sys.constellation_id;
    }
    // 1. Stars (group 6), Planets (group 7), Moons (group 8), Asteroid Belts (group 9), Stargates (group 10)
    for cel in sde.celestials_in_system_slice(solar_system_id) {
        let type_rec = sde.types.get(&cel.type_id);
        let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
        let radius = if cel.radius > 0.0 {
            cel.radius
        } else {
            type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(1_000.0)
        };
        let mass = type_rec
            .map(|t| t.mass)
            .filter(|&m| m > 0.0)
            .unwrap_or(1.0e18);
        let jump_target = if cel.group_id == 10 {
            sde.stargate(cel.celestial_id)
                .map(|j| (j.destination_stargate_id, j.to_solar_system_id))
        } else {
            None
        };
        let (origin_system_owner_id, dest_system_owner_id) = if cel.group_id == 10 {
            (
                sde.solar_system_faction_id(solar_system_id).map(i64::from),
                jump_target
                    .and_then(|(_, dest_sys_id)| sde.solar_system_faction_id(dest_sys_id))
                    .map(i64::from),
            )
        } else {
            (None, None)
        };
        ballpark.add_static_ball_with_meta(
            Ball {
                id: cel.celestial_id,
                type_id: cel.type_id,
                position: cel.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            SlimItemMeta {
                group_id: cel.group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                jump_target,
                name: (!cel.name.is_empty()).then(|| cel.name.clone()),
                origin_system_owner_id,
                dest_system_owner_id,
                ..Default::default()
            },
        );
    }

    // 2. Ensure all stargates in `sde.jumps` originating in `solar_system_id` are present (`group_id = 10`)
    for jump in sde.jumps.values() {
        if jump.from_solar_system_id == solar_system_id
            && !ballpark.balls.contains_key(&jump.stargate_id)
        {
            let (gate_type_id, gate_name) = sde
                .stargate_type_and_name(jump.stargate_id)
                .unwrap_or((16, ""));
            let type_rec = sde
                .types
                .get(&gate_type_id)
                .or_else(|| sde.types.get(&16));
            let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
            let radius = type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0);
            ballpark.add_static_ball_with_meta(
                Ball {
                    id: jump.stargate_id,
                    type_id: gate_type_id,
                    position: jump.position,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e12,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
                SlimItemMeta {
                    group_id: 10,
                    category_id,
                    owner_id: 1,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    jump_target: Some((jump.destination_stargate_id, jump.to_solar_system_id)),
                    name: (!gate_name.is_empty()).then(|| gate_name.to_string()),
                    origin_system_owner_id: sde
                        .solar_system_faction_id(solar_system_id)
                        .map(i64::from),
                    dest_system_owner_id: sde
                        .solar_system_faction_id(jump.to_solar_system_id)
                        .map(i64::from),
                    ..Default::default()
                },
            );
        }
    }

    // 3. Stations (`group_id = 15`) from `sde.solar_systems[solar_system_id].stations`
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        for st in &sys.stations {
            if !ballpark.balls.contains_key(&st.station_id) {
                let Some(type_rec) = sde.types.get(&st.station_type_id) else {
                    continue;
                };
                let group_id = type_rec.group_id;
                let category_id = type_rec.category_id;
                let owner_id = st.owner_corporation_id;
                let radius = type_rec.radius;
                let mass = type_rec.mass;
                ballpark.add_static_ball_with_meta(
                    Ball {
                        id: st.station_id,
                        type_id: st.station_type_id,
                        position: st.position,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass,
                        radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    },
                    SlimItemMeta {
                        group_id,
                        category_id,
                        owner_id,
                        char_id: None,
                        corp_id: owner_id,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false, ..Default::default()
                    },
                );
            }
        }
    }

    crate::services::cluster_macho_misc_services::populate_shipcaster_balls_with_sde(
        solar_system_id,
        ballpark,
        Some(sde),
        None,
    );
}

/// Computes the deterministic base asteroid entity ID for `solar_system_id` (`750_000_000..800_000_000`),
/// allocating a `6,000`-ID block per solar system so even the largest V24.01 belt systems (`5,807` rocks)
/// never collide across solar systems or overlap living-world convoy IDs (`800_100_000+`).
pub fn belt_asteroid_base_id(solar_system_id: SolarSystemId) -> i64 {
    let sys_offset = if solar_system_id == 30_000_144 {
        0
    } else if solar_system_id >= 31_000_000 {
        (5_600 + i64::from(solar_system_id - 31_000_000)) * 6_000
    } else {
        (i64::from(solar_system_id % 10_000_000).max(1)) * 6_000
    };
    750_000_000 + sys_offset
}

/// Populates asteroid belt entities around asteroid belt celestials (`SP-4`, `MN-01`).
/// Uses exact per-belt asteroid counts from `res:/staticdata/asteroidcountbytypeidbeltid.static`
/// (`40,928` belts) when present in `SdeRegistry`, falling back to `SdeRegistry::belt_ores_for_system`
/// for synthetic belt celestials in unit tests.
pub fn populate_asteroid_belt_entities(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) -> Vec<AsteroidState> {
    let mut states = Vec::new();
    let mut ast_id_counter = belt_asteroid_base_id(solar_system_id);

    if sde.solar_systems.get(&solar_system_id).is_none() {
        return states;
    }
    let band_ores = sde.belt_ores_for_system(solar_system_id);

    for cel in sde.celestials_in_system_slice(solar_system_id) {
        if cel.group_id == 9 {
            let belt_pos = cel.position;

            let mut belt_spawn_seq: Vec<&eve_sde::InvType> = Vec::new();
            if let Some(counts_map) = sde.belt_asteroid_counts(cel.celestial_id) {
                let mut typed_counts: Vec<(&eve_sde::InvType, u32)> = counts_map
                    .iter()
                    .filter(|(_, &cnt)| cnt > 0)
                    .filter_map(|(&tid, &cnt)| sde.types.get(&tid).map(|t| (t, cnt)))
                    .collect();
                if !typed_counts.is_empty() {
                    typed_counts.sort_by(|(a, _), (b, _)| {
                        a.volume
                            .partial_cmp(&b.volume)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.type_id.cmp(&b.type_id))
                    });
                    let max_cnt = typed_counts.iter().map(|(_, c)| *c).max().unwrap_or(0);
                    let total_cnt: usize = typed_counts.iter().map(|(_, c)| *c as usize).sum();
                    belt_spawn_seq.reserve(total_cnt);
                    for round in 0..max_cnt {
                        for &(ore_t, cnt) in &typed_counts {
                            if round < cnt {
                                belt_spawn_seq.push(ore_t);
                            }
                        }
                    }
                }
            }
            if belt_spawn_seq.is_empty() {
                belt_spawn_seq.extend(band_ores.iter().copied());
            }

            for (idx, ore_type) in belt_spawn_seq.into_iter().enumerate() {
                let id = ast_id_counter;
                ast_id_counter += 1;

                // DERIVED: V24.01 client static data provides exact asteroid counts per belt and type
                // (`asteroidcountbytypeidbeltid.static`), while per-asteroid ore volume and spatial
                // arc coordinates are server-only; we scale each rock to ~2,000 m³ rounded to SDE
                // `portion_size` and distribute rocks along a semicircular belt arc within 8.5 km.
                let unit_vol = ore_type.volume.max(0.01);
                let batch = ore_type.portion_size.max(1);
                let raw_units = ((2_000.0 / unit_vol).round() as u64).max(batch);
                let units = (raw_units / batch).max(1) * batch;
                let total_m3 = (units as f64) * unit_vol;
                let scaled_radius =
                    (ore_type.radius.max(1.0) * 5.0 * total_m3.cbrt()).clamp(60.0, 2_500.0);

                let offset = if idx == 0 {
                    DVec3::new(200.0, -100.0, 150.0)
                } else {
                    let angle = (idx as f64) * 0.28;
                    let arc_r = 2_000.0 + ((idx % 10) as f64) * 650.0;
                    DVec3::new(
                        arc_r * angle.cos(),
                        (((idx % 5) as f64) - 2.0) * 180.0,
                        arc_r * angle.sin(),
                    )
                };
                let pos = belt_pos + offset;

                let ast_state = AsteroidState {
                    asteroid_id: id,
                    belt_id: cel.celestial_id,
                    type_id: ore_type.type_id,
                    initial_units: units,
                    remaining_units: units,
                    unit_volume: unit_vol,
                    depleted: false,
                    respawn_at_tick: None,
                };

                let ball = Ball {
                    id,
                    type_id: ore_type.type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: (total_m3 * 5.0e7).max(ore_type.mass),
                    radius: scaled_radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                };

                ballpark.add_static_ball_with_meta(
                    ball,
                    SlimItemMeta {
                        group_id: ore_type.group_id,
                        category_id: ore_type.category_id,
                        owner_id: 1,
                        char_id: None,
                        corp_id: -1,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                        name: (!ore_type.name.is_empty()).then(|| ore_type.name.clone()),
                        ..Default::default()
                    },
                );
                states.push(ast_state);
            }
        }
    }
    states
}

/// Returns `true` if `ball` is a stationary space object (wreck, jetcan, loot/secure container,
/// corpse, or anchored deployable/structure) that must omit `BALL_FLAG_IS_FREE` in `AddBalls2` /
/// `SetState` (`DestinyManager.cpp:1780`).
pub fn should_pin_stationary_ball(ball: &Ball, meta: Option<&SlimItemMeta>) -> bool {
    if !matches!(ball.mode, MovementMode::Stop)
        || ball.max_velocity > 0.0
        || ball.velocity.length_squared() > 0.0
    {
        return false;
    }
    if matches!(
        ball.type_id,
        23 | 25 | 3293 | 26_468..=27_030 | 33_474..=33_500
    ) {
        return true;
    }
    meta.is_some_and(|m| matches!(m.category_id, 2 | 3 | 22 | 23 | 40 | 46 | 65))
}

pub(super) fn ensure_default_slim_meta(
    ballpark: &mut Ballpark,
    ball: &Ball,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    if ballpark.ball_slim_meta.contains_key(&ball.id) {
        return;
    }
    let (group_id, category_id, name) = sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&ball.type_id))
        .map(|t| {
            (
                t.group_id,
                t.category_id,
                (!t.name.is_empty()).then(|| t.name.clone()),
            )
        })
        .unwrap_or_else(|| {
            let fallback = eve_destiny::wire::resolve_slim_item_meta(None, ball);
            (fallback.group_id, fallback.category_id, None)
        });
    ballpark.set_ball_slim_meta(
        ball.id,
        SlimItemMeta {
            group_id,
            category_id,
            owner_id: 1,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
            name,
            ..Default::default()
        },
    );
}

pub(super) fn build_tick_broadcast(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    mut events: Vec<DestinyEvent>,
) -> TickBroadcast {
    events.append(&mut ballpark.pending_events);
    let observer_events = ballpark.drain_all_observer_events();
    let stamp = ballpark.current_tick as u32;
    let mut observer_updates_pyrep = BTreeMap::new();
    for (&obs_id, obs_evs) in &observer_events {
        observer_updates_pyrep.insert(
            obs_id,
            encode_destiny_updates(stamp, obs_evs, Some(ballpark)),
        );
    }
    TickBroadcast {
        solar_system_id,
        tick: ballpark.current_tick,
        events,
        observer_events,
        observer_updates_pyrep,
    }
}

pub(super) fn restore_asteroid_from_template(
    id: BallId,
    orig_ast: &AsteroidState,
    orig_ball: &Ball,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    let mut reset_ast = orig_ast.clone();
    reset_ast.remaining_units = reset_ast.initial_units;
    reset_ast.depleted = false;
    reset_ast.respawn_at_tick = None;
    asteroids.insert(id, reset_ast);
    if !ballpark.balls.contains_key(&id) {
        let (group_id, category_id, name) = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&orig_ball.type_id))
            .map(|t| {
                (
                    t.group_id,
                    t.category_id,
                    (!t.name.is_empty()).then(|| t.name.clone()),
                )
            })
            .unwrap_or((25, 25, None));
        let mut restored_ball = orig_ball.clone();
        restored_ball.mode = MovementMode::Rigid;
        ballpark.add_static_ball_with_meta(
            restored_ball.clone(),
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        );
        ballpark.queue_event_for_all_grid_observers(
            Some(id),
            &DestinyEvent::AddBalls2 {
                balls: vec![restored_ball],
            },
        );
    }
}

pub(super) fn reconcile_spawned_belt_asteroids(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    spawned: Vec<AsteroidState>,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &mut HashMap<BallId, (AsteroidState, Ball)>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    belt_interval_secs: u64,
    downtime_utc_secs: u64,
) -> Vec<AsteroidState> {
    let existing_qtys: HashMap<i64, i64> = if let Some(db) = db_opt {
        db.with_snapshot(|snap| {
            spawned
                .iter()
                .filter_map(|ast| {
                    snap.asteroids
                        .get(&ast.asteroid_id)
                        .map(|rec| (ast.asteroid_id, rec.quantity))
                })
                .collect()
        })
    } else {
        HashMap::new()
    };
    let mut to_spawn: Vec<eve_db::AsteroidRecord> = Vec::new();
    let mut populated = Vec::new();
    for mut ast in spawned {
        if let Some(ball) = ballpark.balls.get(&ast.asteroid_id).cloned() {
            asteroid_templates.insert(ast.asteroid_id, (ast.clone(), ball.clone()));
            if db_opt.is_some() {
                if let Some(&existing_qty) = existing_qtys.get(&ast.asteroid_id) {
                    if existing_qty <= 0 {
                        ast.remaining_units = 0;
                        ast.depleted = true;
                        let _ = ballpark.remove_ball(ast.asteroid_id);
                    } else {
                        ast.remaining_units = existing_qty as u64;
                    }
                } else {
                    let init_qty = i64::try_from(ast.initial_units).unwrap_or(i64::MAX);
                    to_spawn.push(eve_db::AsteroidRecord {
                        asteroid_id: ast.asteroid_id,
                        belt_id: ast.belt_id,
                        type_id: ast.type_id,
                        solar_system_id,
                        position: (ball.position.x, ball.position.y, ball.position.z),
                        quantity: i64::try_from(ast.remaining_units).unwrap_or(i64::MAX),
                        initial_quantity: init_qty,
                        radius: ball.radius,
                    });
                }
            }
            asteroids.insert(ast.asteroid_id, ast.clone());
            populated.push(ast);
        }
    }
    if let Some(db) = db_opt {
        if !to_spawn.is_empty() {
            db.with_mut_snapshot(|snap| {
                for record in to_spawn {
                    snap.asteroids.entry(record.asteroid_id).or_insert(record);
                }
            });
        }
    }
    if let Some(db) = db_opt {
        let due_unix = next_downtime_unix_secs(
            db.clock().now_unix_secs(),
            belt_interval_secs,
            downtime_utc_secs,
        );
        let mut belt_ids: Vec<BallId> = asteroid_templates
            .values()
            .filter(|(a, _)| a.belt_id > 0)
            .map(|(a, _)| a.belt_id)
            .collect();
        belt_ids.sort_unstable();
        belt_ids.dedup();
        for belt_id in belt_ids {
            let belt_empty = asteroids
                .values()
                .filter(|a| a.belt_id == belt_id)
                .all(|a| a.remaining_units == 0);
            if belt_empty {
                for a in asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
                    a.respawn_at_tick = Some(due_unix);
                }
                for a in populated.iter_mut().filter(|a| a.belt_id == belt_id) {
                    a.respawn_at_tick = Some(due_unix);
                }
            }
        }
    }
    populated
}

pub(super) fn respawn_due_asteroids(
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &HashMap<BallId, (AsteroidState, Ball)>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) -> Vec<BallId> {
    let Some(db) = db_opt else {
        return Vec::new();
    };
    let now_unix = db.clock().now_unix_secs();
    let mut belt_ids: Vec<BallId> = asteroid_templates
        .values()
        .filter(|(ast, _)| ast.belt_id > 0)
        .map(|(ast, _)| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut respawned = Vec::new();
    for belt_id in belt_ids {
        let belt_empty_and_due = asteroid_templates
            .iter()
            .filter(|(_, (orig_ast, _))| orig_ast.belt_id == belt_id)
            .all(|(&id, _)| {
                asteroids.get(&id).is_some_and(|a| {
                    a.remaining_units == 0
                        && a.respawn_at_tick.is_some_and(|due_unix| now_unix >= due_unix)
                })
            });
        if belt_empty_and_due {
            for (&id, (orig_ast, orig_ball)) in asteroid_templates {
                if orig_ast.belt_id == belt_id {
                    restore_asteroid_from_template(
                        id,
                        orig_ast,
                        orig_ball,
                        ballpark,
                        asteroids,
                        sde_opt,
                    );
                    let init_qty = i64::try_from(orig_ast.initial_units).unwrap_or(i64::MAX);
                    db.with_mut_snapshot(|snap| {
                        snap.asteroids.insert(
                            id,
                            eve_db::AsteroidRecord {
                                asteroid_id: id,
                                belt_id: orig_ast.belt_id,
                                type_id: orig_ast.type_id,
                                solar_system_id: ballpark.solar_system_id,
                                position: (orig_ball.position.x, orig_ball.position.y, orig_ball.position.z),
                                quantity: init_qty,
                                initial_quantity: init_qty,
                                radius: orig_ball.radius,
                            },
                        );
                    });
                    respawned.push(id);
                }
            }
        }
    }
    respawned
}

/// Daily 11:00 UTC downtime maintenance respawn for an active solar system:
/// - Respawns any asteroid belt (`belt_id > 0`) where all asteroids in that belt are depleted (`Asteroids_and_ore.md:69`).
/// - Respawns any depleted ice asteroid (`group_id == 465`) back to `initial_units`.
pub(crate) fn respawn_depleted_belts_on_downtime_inner(
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &HashMap<BallId, (AsteroidState, Ball)>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) -> usize {
    let mut belt_ids: Vec<BallId> = asteroid_templates
        .values()
        .filter(|(ast, _)| ast.belt_id > 0)
        .map(|(ast, _)| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut to_restore: Vec<BallId> = Vec::new();
    for belt_id in belt_ids {
        let belt_fully_depleted = asteroid_templates
            .iter()
            .filter(|(_, (orig_ast, _))| orig_ast.belt_id == belt_id)
            .all(|(&id, _)| {
                asteroids
                    .get(&id)
                    .is_some_and(|a| a.remaining_units == 0 || a.depleted)
            });
        if belt_fully_depleted {
            for (&id, (orig_ast, _)) in asteroid_templates {
                if orig_ast.belt_id == belt_id {
                    to_restore.push(id);
                }
            }
        }
    }

    // Ice belts (`group_id == 465`) respawn depleted ice blocks at downtime
    for (&id, (orig_ast, _)) in asteroid_templates {
        let is_ice = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&orig_ast.type_id))
            .is_some_and(|t| t.group_id == 465);
        if is_ice
            && asteroids
                .get(&id)
                .is_some_and(|a| a.remaining_units < orig_ast.initial_units || a.depleted)
            && !to_restore.contains(&id)
        {
            to_restore.push(id);
        }
    }

    to_restore.sort_unstable();
    to_restore.dedup();
    let count = to_restore.len();
    for id in to_restore {
        if let Some((orig_ast, orig_ball)) = asteroid_templates.get(&id) {
            restore_asteroid_from_template(id, orig_ast, orig_ball, ballpark, asteroids, sde_opt);
            if let Some(db) = db_opt {
                let init_qty = i64::try_from(orig_ast.initial_units).unwrap_or(i64::MAX);
                db.with_mut_snapshot(|snap| {
                    snap.asteroids.insert(
                        id,
                        eve_db::AsteroidRecord {
                            asteroid_id: id,
                            belt_id: orig_ast.belt_id,
                            type_id: orig_ast.type_id,
                            solar_system_id: ballpark.solar_system_id,
                            position: (orig_ball.position.x, orig_ball.position.y, orig_ball.position.z),
                            quantity: init_qty,
                            initial_quantity: init_qty,
                            radius: orig_ball.radius,
                        },
                    );
                });
            }
        }
    }
    count
}

pub(super) fn handle_mine_asteroid(
    solar_system_id: SolarSystemId,
    asteroid_id: BallId,
    requested_units: u64,
    waste_probability: f64,
    persist_db: bool,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    mechanic_rng: &crate::rng::MechanicRng,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    belt_interval_atomic: &std::sync::atomic::AtomicU64,
    downtime_utc_atomic: &std::sync::atomic::AtomicU64,
) -> Result<MiningExtractionResult, String> {
    if let Some(ast) = asteroids.get_mut(&asteroid_id) {
        if ast.remaining_units == 0 {
            Err("Asteroid is already depleted".to_string())
        } else {
            let to_mine = requested_units.min(ast.remaining_units);
            ast.remaining_units -= to_mine;

            let wasted_units = if waste_probability > 0.0 && ast.remaining_units > 0 {
                let rolls = waste_probability >= 1.0 || mechanic_rng.roll(waste_probability);
                if rolls {
                    to_mine.min(ast.remaining_units)
                } else {
                    0
                }
            } else {
                0
            };
            ast.remaining_units -= wasted_units;

            let depleted = ast.remaining_units == 0;
            ast.depleted = depleted;
            let remaining_units = ast.remaining_units;
            let ore_type_id = ast.type_id;
            let belt_id = ast.belt_id;
            if persist_db || depleted {
                if let Some(db) = db_opt {
                    db.with_mut_snapshot(|snap| {
                        if let Some(a) = snap.asteroids.get_mut(&asteroid_id) {
                            a.quantity = i64::try_from(remaining_units).unwrap_or(0);
                        }
                    });
                }
            }
            if depleted {
                let _ = ballpark.remove_ball(asteroid_id);
                let belt_fully_depleted = asteroids
                    .values()
                    .filter(|a| a.belt_id == belt_id)
                    .all(|a| a.remaining_units == 0);
                if belt_fully_depleted {
                    if let Some(db) = db_opt {
                        let interval_secs =
                            belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed);
                        let downtime_secs =
                            downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed);
                        let due_unix = next_downtime_unix_secs(
                            db.clock().now_unix_secs(),
                            interval_secs,
                            downtime_secs,
                        );
                        for belt_ast in asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
                            belt_ast.respawn_at_tick = Some(due_unix);
                        }
                    }
                }
            }

            Ok(MiningExtractionResult {
                mined_units: to_mine,
                wasted_units,
                remaining_units,
                ore_type_id,
                depleted,
            })
        }
    } else {
        Err(format!(
            "Asteroid {asteroid_id} not found in system {solar_system_id}"
        ))
    }
}

// parity: destiny:OnSlimItemChange
pub(super) fn sync_player_slim_items(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    let Some(db) = db_opt else {
        return;
    };
    let updates: Vec<(BallId, SlimItemMeta)> = ballpark
        .ball_slim_meta
        .iter()
        .filter_map(|(&ball_id, meta)| {
            if meta.category_id != 6 {
                return None;
            }
            let char_id = meta.char_id?;
            let ch = db.get_character(char_id).ok()?;
            let new_corp_id = ch.corporation_id;
            let new_alliance_id = ch.alliance_id.unwrap_or(0);
            let new_sec = ch.security_status;
            if meta.corp_id != new_corp_id
                || meta.alliance_id != new_alliance_id
                || (meta.security_status - new_sec).abs() > 1e-9
            {
                let mut updated = meta.clone();
                updated.corp_id = new_corp_id;
                updated.alliance_id = new_alliance_id;
                updated.security_status = new_sec;
                Some((ball_id, updated))
            } else {
                None
            }
        })
        .collect();
    for (ball_id, updated_meta) in updates {
        ballpark.change_slim_item(ball_id, updated_meta);
    }
}

#[cfg(test)]
#[path = "belts_tests.rs"]
mod belts_tests;
