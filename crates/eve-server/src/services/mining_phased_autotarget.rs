//! `mining_phased_autotarget` — Phased Asteroid Field lifecycle & Mobile Phase Anchor integration (`MN-08`).
//!
//! Wires `PhasedAsteroidFieldState` into live system mining-site seeding, `WorldStateSnapshot`
//! persistence (`service_state_blobs["phased_asteroid_fields"]`), deployable Mobile Phase Anchor
//! (`90037`) per-ship ICS energy contribution (SDE group 883 Capital Industrial Ship = 100,
//! group 941 Orca = 50 / Porpoise = 35), cluster tick fracture progression, and
//! `mining_cycle::resolve_cycle` Prismaticite efficiency (10% breached vs 100% phased-in).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, WorldStateSnapshot};
use eve_sde::{SdeRegistry, TypeId};

use crate::router::ServerContext;
use crate::services::space_mining_harvest::PhasedAsteroidFieldState;

/// Mobile Phase Anchor deployable item type ID (`group_id = 4913`, `category_id = 22`).
pub const TYPE_MOBILE_PHASE_ANCHOR: TypeId = 90_037;

/// SDE Prismaticite ore type IDs (Prismaticite, Bright, Gleaming, Shimmering).
pub const PRISMATICITE_ORE_TYPES: [TypeId; 4] = [90_041, 90_042, 90_445, 90_446];

const BLOB_KEY_PHASED_FIELDS: &str = "phased_asteroid_fields";
const ANCHOR_TETHER_RANGE_M: f64 = 50_000.0;

/// Returns `true` if `type_id` is a Prismaticite ore variant in `sde`.
pub fn is_prismaticite_ore(sde: &SdeRegistry, type_id: TypeId) -> bool {
    PRISMATICITE_ORE_TYPES.contains(&type_id)
        || sde
            .types
            .get(&type_id)
            .is_some_and(|t| t.name.contains("Prismaticite"))
}

/// DERIVED: Mobile Phase Anchor energy output per Industrial Command Ship hull (absent from
/// `evomu_sde.sqlite`, derived from EVE Online Mobile Phase Anchor mechanics):
/// - SDE Group `883` (Capital Industrial Ship, e.g. Rorqual `28352`) -> `100`
/// - SDE Group `941` (Industrial Command Ship): Orca (`28606`) -> `50`, Porpoise (`42244`) -> `35`
pub fn ics_energy_for_type(sde: &SdeRegistry, ship_type_id: TypeId) -> u32 {
    let Some(t) = sde.types.get(&ship_type_id) else {
        return 0;
    };
    // DERIVED: 100 / 50 / 35 ICS energy constants keyed by SDE group/type
    match t.group_id {
        883 => 100,
        941 if ship_type_id == 28_606 || t.name.contains("Orca") => 50,
        941 => 35,
        _ => 0,
    }
}

/// Returns `true` if `solar_system_id` is low-sec or null-sec K-space (security < 0.45,
/// excluding wormhole/abyssal space `31_000_000..=32_999_999` and Pochven `region_id == 10_000_070`).
pub fn is_low_or_null_kspace(ctx: &ServerContext, solar_system_id: u32) -> bool {
    if (31_000_000..=32_999_999).contains(&solar_system_id) {
        return false;
    }
    ctx.sde
        .solar_systems
        .get(&solar_system_id)
        .is_some_and(|s| s.security_status < 0.45 && s.region_id != 10_000_070)
}

fn load_phased_fields(snap: &WorldStateSnapshot) -> HashMap<u32, PhasedAsteroidFieldState> {
    snap.service_state_blobs
        .get(BLOB_KEY_PHASED_FIELDS)
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

fn persist_phased_fields(
    snap: &mut WorldStateSnapshot,
    fields: &HashMap<u32, PhasedAsteroidFieldState>,
) {
    if let Ok(json) = serde_json::to_string(fields) {
        snap.service_state_blobs
            .insert(BLOB_KEY_PHASED_FIELDS.to_string(), json);
    }
}

fn sync_ctx_phased_fields(ctx: &ServerContext, fields: HashMap<u32, PhasedAsteroidFieldState>) {
    *ctx.phased_asteroid_fields
        .write()
        .unwrap_or_else(|e| e.into_inner()) = fields;
}

/// Spawns a Phased Asteroid Field in a low-sec or null-sec `solar_system_id` as a probeable
/// cosmic signature (`sig_group: "Ore"`) with breached Prismaticite (`90041`) at 10%
/// efficiency. Returns `None` if `solar_system_id` is high-sec or wormhole/abyssal space.
pub fn spawn_phased_asteroid_field(
    ctx: &ServerContext,
    solar_system_id: u32,
    signature_id: &str,
    asteroid_id: i64,
    quantity: i64,
    position: (f64, f64, f64),
) -> Option<PhasedAsteroidFieldState> {
    if !is_low_or_null_kspace(ctx, solar_system_id) {
        return None;
    }
    let mut state = PhasedAsteroidFieldState::new(signature_id.to_string(), solar_system_id);
    state.asteroid_ids.push(asteroid_id);
    let now = ctx.now_unix_secs();
    let snapshot_clone = ctx.db.with_mut_snapshot(|snap| {
        let mut fields = load_phased_fields(snap);
        if fields.is_empty() {
            snap.service_state_blobs
                .insert("phased_field_last_tick_unix".into(), now.to_string());
        }
        fields.insert(solar_system_id, state.clone());
        persist_phased_fields(snap, &fields);
        let sigs = snap.cosmic_signatures.entry(solar_system_id).or_default();
        sigs.retain(|s| s.signature_id != signature_id);
        sigs.push(CosmicSignatureRecord {
            signature_id: signature_id.to_string(),
            solar_system_id,
            sig_group: "Ore".to_string(),
            type_name: "Phased Asteroid Field".to_string(),
            position: [position.0, position.1, position.2],
            signal_strength: 0.25,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: now,
            lifetime_seconds: 14_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
        snap.asteroids.insert(
            asteroid_id,
            AsteroidRecord {
                asteroid_id,
                solar_system_id,
                belt_id: 0,
                type_id: PRISMATICITE_ORE_TYPES[0],
                quantity,
                initial_quantity: quantity,
                position,
                radius: 250.0,
            },
        );
        fields
    });
    sync_ctx_phased_fields(ctx, snapshot_clone);
    Some(state)
}

/// Live spawner hook called when a low-sec or null-sec K-space system is seeded.
/// Uses bounded probabilistic seeding (20% per unseeded low/null system) and spawns
/// the Ballpark asteroid entity in `SystemActor`.
pub async fn maybe_spawn_phased_asteroid_field(
    ctx: &ServerContext,
    solar_system_id: u32,
) -> Option<PhasedAsteroidFieldState> {
    if !is_low_or_null_kspace(ctx, solar_system_id) {
        return None;
    }
    let roll = ctx
        .mechanic_rng
        .try_next_u64()
        .unwrap_or(u64::from(solar_system_id))
        % 100;
    if roll >= 20 {
        return None;
    }
    let (already_active, sig_seq, ast_id) = ctx.db.with_mut_snapshot(|snap| {
        let fields = load_phased_fields(snap);
        if fields.get(&solar_system_id).is_some_and(|f| !f.collapsed) {
            return (true, 0, 0);
        }
        snap.next_entity_id += 2;
        (false, snap.next_entity_id - 1, snap.next_entity_id)
    });
    if already_active {
        return None;
    }
    let angle = f64::from(solar_system_id % 360).to_radians();
    let dist = 220_000.0 + f64::from(solar_system_id % 50) * 1_000.0;
    let pos = (dist * angle.cos(), 15_000.0, dist * angle.sin());
    let state = spawn_phased_asteroid_field(
        ctx,
        solar_system_id,
        &format!("PHS-{sig_seq}"),
        ast_id,
        50_000,
        pos,
    )?;
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if sys_handle.get_ball_position_sync(ast_id).is_none() {
        let unit_volume = ctx.sde.types.get(&PRISMATICITE_ORE_TYPES[0]).map(|t| t.volume).unwrap_or(16.0);
        let _ = sys_handle
            .spawn_asteroid(
                crate::system_actor::AsteroidState {
                    asteroid_id: ast_id,
                    belt_id: 0,
                    type_id: PRISMATICITE_ORE_TYPES[0],
                    initial_units: 50_000,
                    remaining_units: 50_000,
                    unit_volume,
                    depleted: false,
                    respawn_at_tick: None,
                },
                eve_destiny::Ball {
                    id: ast_id,
                    type_id: PRISMATICITE_ORE_TYPES[0],
                    position: glam::DVec3::new(pos.0, pos.1, pos.2),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1e9,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
            )
            .await;
    }
    Some(state)
}

/// Advances the hyperspace fracture across the Phased Asteroid Field in `solar_system_id`.
/// At `progress >= 0.5`, spawns the system-wide cosmic anomaly beacon at the site's position.
/// At `progress >= 1.0`, collapses the field and depletes remaining phased asteroids.
pub fn advance_phased_asteroid_field(
    ctx: &ServerContext,
    solar_system_id: u32,
    delta_progress: f64,
) -> Option<PhasedAsteroidFieldState> {
    let now = ctx.now_unix_secs();
    let (updated, snapshot_clone) = ctx.db.with_mut_snapshot(|snap| {
        let mut fields = load_phased_fields(snap);
        let Some(field) = fields.get_mut(&solar_system_id) else {
            return (None, fields);
        };
        field.advance_fracture(delta_progress);
        let updated = field.clone();
        persist_phased_fields(snap, &fields);
        let site_pos = snap
            .cosmic_signatures
            .get(&solar_system_id)
            .and_then(|sigs| sigs.iter().find(|s| s.signature_id == updated.signature_id))
            .map(|s| s.position)
            .or_else(|| {
                snap.asteroids
                    .values()
                    .find(|a| {
                        a.solar_system_id == solar_system_id
                            && PRISMATICITE_ORE_TYPES.contains(&a.type_id)
                    })
                    .map(|a| [a.position.0, a.position.1, a.position.2])
            })
            .unwrap_or([0.0, 0.0, 0.0]);
        if updated.beacon_spawned && !updated.collapsed {
            let anoms = snap.cosmic_anomalies.entry(solar_system_id).or_default();
            if !anoms.iter().any(|a| a.anomaly_id == updated.signature_id) {
                anoms.push(CosmicAnomalyRecord {
                    anomaly_id: updated.signature_id.clone(),
                    solar_system_id,
                    name: "Phased Asteroid Field".to_string(),
                    anomaly_type: "OreAnomaly".to_string(),
                    position: site_pos,
                    signal_strength: 1.0,
                    ore_types: vec![PRISMATICITE_ORE_TYPES[0]],
                    remaining_volume: 500_000.0,
                    created_unix: now,
                    lifetime_seconds: 7_200,
                    cleared: false,
                });
            }
        }
        if updated.collapsed {
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                for s in sigs.iter_mut() {
                    if s.signature_id == updated.signature_id {
                        s.collapsed = true;
                    }
                }
            }
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                for a in anoms.iter_mut() {
                    if a.anomaly_id == updated.signature_id {
                        a.cleared = true;
                    }
                }
            }
            for ast in snap.asteroids.values_mut() {
                let belongs = if updated.asteroid_ids.is_empty() {
                    ast.solar_system_id == solar_system_id
                        && ast.belt_id == 0
                        && PRISMATICITE_ORE_TYPES.contains(&ast.type_id)
                } else {
                    updated.asteroid_ids.contains(&ast.asteroid_id)
                };
                if belongs {
                    ast.quantity = 0;
                }
            }
        }
        (Some(updated), fields)
    });
    sync_ctx_phased_fields(ctx, snapshot_clone);
    updated
}

/// Connects an Industrial Command Ship (`ship_id`) to a deployed Mobile Phase Anchor in
/// `solar_system_id`, enforcing per-ship ID de-duplication, owner/fleet membership, and 50 km range.
/// Returns `Some(true)` once total energy >= 100 and the asteroid is phased in.
pub async fn connect_ics_to_phase_anchor(
    ctx: &ServerContext,
    solar_system_id: u32,
    ship_id: i64,
) -> Option<bool> {
    let (anchor_ids, ship_type_id, has_db_active_core) = ctx.db.with_snapshot(|snap| {
        let ship = snap
            .items
            .get(&ship_id)
            .filter(|sh| sh.location_id == i64::from(solar_system_id))?;
        let pilot_cid = snap
            .characters
            .values()
            .find(|c| {
                c.active_ship_id == ship_id
                    && c.solar_system_id == solar_system_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
            })?
            .character_id;
        let matching_anchors: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == i64::from(solar_system_id)
                    && i.type_id == TYPE_MOBILE_PHASE_ANCHOR
                    && i.quantity > 0
                    && (pilot_cid == i.owner_id
                        || snap.fleets.values().any(|fl| {
                            fl.members.contains(&i.owner_id) && fl.members.contains(&pilot_cid)
                        }))
            })
            .map(|i| i.item_id)
            .collect();
        let db_core = snap.items.values().any(|i| {
            i.location_id == ship_id
                && matches!(i.type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
                && snap.module_active_states.contains(&i.item_id)
        });
        (!matching_anchors.is_empty()).then_some((matching_anchors, ship.type_id, db_core))
    })?;

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if !has_db_active_core && !sys_handle.has_active_industrial_core_sync(ship_id) {
        return None;
    }
    let (_, ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.ok()??;
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return None;
    }
    let mut in_range_anchor = false;
    for anchor_id in anchor_ids {
        if let Ok(Some((_, anchor_ball))) = sys_handle.get_ball_snapshot(anchor_id).await {
            if anchor_ball.position.distance(ship_ball.position) <= ANCHOR_TETHER_RANGE_M {
                in_range_anchor = true;
                break;
            }
        }
    }
    if !in_range_anchor {
        return None;
    }

    let energy = ics_energy_for_type(&ctx.sde, ship_type_id);
    if energy == 0 {
        return None;
    }

    let (phased_in, snapshot_clone) = ctx.db.with_mut_snapshot(|snap| {
        let mut fields = load_phased_fields(snap);
        let Some(field) = fields.get_mut(&solar_system_id) else {
            return (None, fields);
        };
        let phased = field.connect_industrial_command_ship(ship_id, energy);
        persist_phased_fields(snap, &fields);
        (Some(phased), fields)
    });
    sync_ctx_phased_fields(ctx, snapshot_clone);
    phased_in
}

/// Evaluates deployed Mobile Phase Anchor (`90037`) and active fleet/owner Industrial Command
/// Ships in `solar_system_id` when a Mobile Phase Anchor is launched into space.
pub async fn on_mobile_phase_anchor_deployed(
    ctx: &ServerContext,
    solar_system_id: u32,
    anchor_item_id: i64,
) {
    let candidate_ship_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let Some(anchor_owner) = snap.items.get(&anchor_item_id).map(|i| i.owner_id) else {
            return Vec::new();
        };
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
                    && (c.character_id == anchor_owner
                        || snap.fleets.values().any(|fl| {
                            fl.members.contains(&anchor_owner)
                                && fl.members.contains(&c.character_id)
                        }))
            })
            .filter_map(|c| {
                let sh = snap.items.get(&c.active_ship_id)?;
                (ics_energy_for_type(&ctx.sde, sh.type_id) > 0).then_some(c.active_ship_id)
            })
            .collect()
    });
    for ship_id in candidate_ship_ids {
        let _ = connect_ics_to_phase_anchor(ctx, solar_system_id, ship_id).await;
    }
}

async fn reconcile_phased_field_connections(ctx: &ServerContext, solar_system_id: u32) {
    let candidates: Vec<(i64, u32, Vec<i64>, bool)> = ctx.db.with_snapshot(|snap| {
        let anchors: Vec<(i64, i64)> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == i64::from(solar_system_id)
                    && i.type_id == TYPE_MOBILE_PHASE_ANCHOR
                    && i.quantity > 0
            })
            .map(|i| (i.item_id, i.owner_id))
            .collect();
        if anchors.is_empty() {
            return Vec::new();
        }
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
            })
            .filter_map(|c| {
                let sh = snap.items.get(&c.active_ship_id)?;
                if sh.location_id != i64::from(solar_system_id) {
                    return None;
                }
                let energy = ics_energy_for_type(&ctx.sde, sh.type_id);
                if energy == 0 {
                    return None;
                }
                let matching_anchors: Vec<i64> = anchors
                    .iter()
                    .filter(|&&(_, anchor_owner)| {
                        c.character_id == anchor_owner
                            || snap.fleets.values().any(|fl| {
                                fl.members.contains(&anchor_owner)
                                    && fl.members.contains(&c.character_id)
                            })
                    })
                    .map(|&(aid, _)| aid)
                    .collect();
                let db_core = snap.items.values().any(|i| {
                    i.location_id == c.active_ship_id
                        && matches!(i.type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
                        && snap.module_active_states.contains(&i.item_id)
                });
                (!matching_anchors.is_empty())
                    .then_some((c.active_ship_id, energy, matching_anchors, db_core))
            })
            .collect()
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut connected_ships: Vec<i64> = Vec::new();
    let mut total_energy: u32 = 0;
    for (ship_id, energy, anchor_ids, has_db_core) in candidates {
        if connected_ships.contains(&ship_id) {
            continue;
        }
        if !has_db_core && !sys_handle.has_active_industrial_core_sync(ship_id) {
            continue;
        }
        let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await else {
            continue;
        };
        if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
            continue;
        }
        let mut tethered = false;
        for anchor_id in anchor_ids {
            if let Ok(Some((_, anchor_ball))) = sys_handle.get_ball_snapshot(anchor_id).await {
                if anchor_ball.position.distance(ship_ball.position) <= ANCHOR_TETHER_RANGE_M {
                    tethered = true;
                    break;
                }
            }
        }
        if tethered {
            connected_ships.push(ship_id);
            total_energy = total_energy.saturating_add(energy);
        }
    }

    let snapshot_clone = ctx.db.with_mut_snapshot(|snap| {
        let mut fields = load_phased_fields(snap);
        if let Some(field) = fields.get_mut(&solar_system_id) {
            field.connected_ship_ids = connected_ships;
            field.mobile_phase_anchor_energy = total_energy;
            field.phased_in = !field.collapsed && total_energy >= 100;
            persist_phased_fields(snap, &fields);
        }
        fields
    });
    sync_ctx_phased_fields(ctx, snapshot_clone);
}

/// Periodic cluster tick for active Phased Asteroid Fields (`MN-08`).
pub async fn tick_phased_asteroid_fields(ctx: &ServerContext, now_unix: u64) {
    let (elapsed_secs, sys_ids) = ctx.db.with_mut_snapshot(|snap| {
        let fields = load_phased_fields(snap);
        let prev = snap
            .service_state_blobs
            .get("phased_field_last_tick_unix")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(now_unix);
        snap.service_state_blobs
            .insert("phased_field_last_tick_unix".into(), now_unix.to_string());
        let ids: Vec<u32> = fields.keys().copied().collect();
        (now_unix.saturating_sub(prev), ids)
    });
    for &sys_id in &sys_ids {
        reconcile_phased_field_connections(ctx, sys_id).await;
    }
    if elapsed_secs == 0 {
        return;
    }
    let delta_progress = (elapsed_secs as f64) / 14_400.0;
    for sys_id in sys_ids {
        if let Some(updated) = advance_phased_asteroid_field(ctx, sys_id, delta_progress) {
            if updated.collapsed {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                for ast_id in updated.asteroid_ids {
                    let _ = sys_handle.remove_ball(ast_id).await;
                }
            }
        }
    }
}

/// Returns the mining efficiency multiplier for `ore_type_id` in `solar_system_id`:
/// 0.10 for breached Prismaticite, 1.0 when phased in by a powered Mobile Phase Anchor,
/// 0.0 when collapsed, or 1.0 when no Phased Asteroid Field is active.
pub fn phased_field_efficiency_for_ore(
    ctx: &ServerContext,
    solar_system_id: u32,
    ore_type_id: TypeId,
) -> f64 {
    phased_field_efficiency_for_asteroid(ctx, solar_system_id, 0, ore_type_id)
}

/// Returns the mining efficiency multiplier for a specific Prismaticite asteroid in `solar_system_id`:
/// 0.10 when phased/breached without anchor power, 1.0 when phased in by powered Industrial Cores,
/// 0.0 when collapsed, or 1.0 when the asteroid does not belong to an active Phased Asteroid Field.
pub fn phased_field_efficiency_for_asteroid(
    ctx: &ServerContext,
    solar_system_id: u32,
    asteroid_id: i64,
    ore_type_id: TypeId,
) -> f64 {
    if !is_prismaticite_ore(&ctx.sde, ore_type_id) {
        return 1.0;
    }
    ctx.db.with_snapshot(|snap| {
        load_phased_fields(snap)
            .get(&solar_system_id)
            .map(|f| {
                if asteroid_id != 0 && !f.asteroid_ids.is_empty() && !f.asteroid_ids.contains(&asteroid_id) {
                    1.0
                } else {
                    f.mining_efficiency()
                }
            })
            .unwrap_or(1.0)
    })
}

/// Enforces SDE `canFitShipGroup*` and `canFitShipType*` command-hull restrictions on Command /
/// Mining Foreman Burst modules (`group_id == 1770`) during activation.
pub fn ensure_command_burst_hull_allowed(
    sde: &SdeRegistry,
    mod_type: &eve_sde::InvType,
    ship_type_id: TypeId,
) -> Result<(), eve_proto::BridgeRpcError> {
    let ship_group_id = sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
    let group_attrs = [
        1298_u16, 1299, 1300, 1301, 1872, 1879, 1880, 1881, 2065, 2396,
        2476, 2477, 2478, 2479, 2480, 2481, 2482, 2483, 2484, 2485,
    ];
    let type_attrs = [
        1302_u16, 1303, 1304, 1305, 1380, 1944, 2103, 2463, 2486, 2487, 2488, 2758, 5948,
    ];
    let allowed_groups: Vec<u32> = group_attrs
        .into_iter()
        .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
        .filter(|&v| v > 0.0)
        .map(|v| v as u32)
        .collect();
    let allowed_types: Vec<TypeId> = type_attrs
        .into_iter()
        .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
        .filter(|&v| v > 0.0)
        .map(|v| v as TypeId)
        .collect();
    if (!allowed_groups.is_empty() || !allowed_types.is_empty())
        && !allowed_groups.contains(&ship_group_id)
        && !allowed_types.contains(&ship_type_id)
    {
        return Err(eve_proto::BridgeRpcError {
            code: "CantFitToShip".into(),
            message: format!(
                "{} cannot be fitted or activated on ship type {ship_type_id} (group {ship_group_id})",
                mod_type.name
            ),
        });
    }
    Ok(())
}

/// Returns `true` if `cycle` represents an offensive/hostile projection rather than friendly
/// remote assistance (remote repairers, remote cap transmitters, remote tracking/sensor links).
pub fn is_offensive_aggressor_cycle(
    c: &crate::system_actor::ActiveModuleCycle,
    sde: Option<&SdeRegistry>,
) -> bool {
    if c.remote_shield_amount > 0.0
        || c.remote_armor_amount > 0.0
        || c.remote_hull_amount > 0.0
        || c.remote_cap_amount > 0.0
    {
        return false;
    }
    if let Some(sde) = sde {
        if sde.types.get(&c.module_type_id).is_some_and(|t| {
            matches!(t.group_id, 41 | 67 | 209 | 290 | 325 | 585 | 1697 | 1698 | 2018)
        }) {
            return false;
        }
        if !c.effect_name.is_empty()
            && sde
                .dogma_effects()
                .values()
                .find(|e| e.name == c.effect_name)
                .is_some_and(|e| e.is_assistance && !e.is_offensive)
        {
            return false;
        }
    }
    true
}

/// Returns `ship_id` plus all active ship IDs belonging to fleetmates of `ship_id`'s pilot.
pub fn fleet_ship_ids_for_ship(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    ship_id: eve_destiny::BallId,
) -> Vec<eve_destiny::BallId> {
    db_opt
        .map(|db| {
            db.with_snapshot(|snap| {
                let owner = snap.items.get(&ship_id).map(|i| i.owner_id).unwrap_or(0);
                let mut ships = vec![ship_id];
                if owner != 0 {
                    for fl in snap.fleets.values() {
                        if fl.members.contains(&owner) {
                            for &mem_cid in &fl.members {
                                if let Some(ch) = snap.characters.get(&mem_cid) {
                                    if ch.active_ship_id > 0 && !ships.contains(&ch.active_ship_id) {
                                        ships.push(ch.active_ship_id);
                                    }
                                }
                            }
                        }
                    }
                }
                ships
            })
        })
        .unwrap_or_else(|| vec![ship_id])
}

/// Returns `true` if an `Entity` (`category_id == 11`) with `group_id` is a hostile NPC rat/drone,
/// excluding CONCORD, Police, Gate/Station Sentry Guns, Customs Officials, static/non-combatant
/// entities, and friendly Faction Warfare militia entities.
pub fn is_hostile_npc_group(group_id: u32, char_faction_id: Option<i64>) -> bool {
    if matches!(
        group_id,
        99 | 180 | 182 | 297 | 298 | 301 | 306 | 319 | 323 | 335 | 446 | 925 | 952 | 1207 | 1872 | 1927 | 1928 | 4073 | 4494 | 4572 | 4576 | 4771
    ) {
        return false;
    }
    let is_amarr_caldari_fw = matches!(group_id, 1167 | 1169 | 1175 | 1176 | 1180 | 1183 | 4431 | 4432 | 5063 | 5064);
    if is_amarr_caldari_fw {
        return matches!(char_faction_id, Some(500_002 | 500_004));
    }
    let is_gallente_minmatar_fw = matches!(
        group_id,
        1166 | 1168 | 1177 | 1178 | 1179 | 1181 | 1182 | 1184 | 1185 | 1186 | 4433 | 4434 | 5065 | 5066
    );
    if is_gallente_minmatar_fw {
        return matches!(char_faction_id, Some(500_001 | 500_003));
    }
    true
}

/// Returns `true` if `ball` is a genuinely hostile/aggressing target for Auto-Targeting System:
/// a hostile NPC entity (`category_id == 11`, excluding CONCORD, Empire sentry/customs guns, and
/// friendly militia entities), or any non-fleet entity/capsuleer with active combat aggression
/// against `char_id`/`ship_id` (active offensive/EWAR cycle, criminal flag, or active limited
/// engagement). Unaggressed outlaws, suspects, passive null-sec neutrals, and friendly remote
/// assistance ships are never auto-locked.
pub async fn is_auto_target_hostile_ball(
    ctx: &ServerContext,
    _sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    _ship_id: i64,
    ball: &eve_destiny::Ball,
    fleet_members: &[i64],
    active_aggressors: &[i64],
) -> bool {
    if active_aggressors.contains(&ball.id) {
        let owner = ctx.db.with_snapshot(|s| s.items.get(&ball.id).map(|i| i.owner_id).unwrap_or(0));
        if owner == 0 || (owner != char_id && !fleet_members.contains(&owner)) {
            return true;
        }
    }
    if let Some(t) = ctx.sde.types.get(&ball.type_id) {
        if t.category_id == 11 {
            let char_faction = ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
            return is_hostile_npc_group(t.group_id, char_faction);
        }
    }
    if !crate::services::tactical_modules::is_player_ship_ball(
        Some(&ctx.sde),
        Some(&ctx.db),
        ball.id,
        ball.type_id,
    ) {
        return false;
    }
    let owner = ctx.db.with_snapshot(|s| s.items.get(&ball.id).map(|i| i.owner_id).unwrap_or(0));
    if owner != 0 && (owner == char_id || fleet_members.contains(&owner)) {
        return false;
    }
    let now_unix = ctx.now_unix();
    let crimewatch_hostile = ctx
        .crimewatch
        .read()
        .ok()
        .is_some_and(|cw| cw.states.get(&owner).is_some_and(|st| st.is_criminal(now_unix)))
        || (owner != 0
            && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.has_limited_engagement_with(char_id, owner, now_unix)
            }));
    crimewatch_hostile
}

/// Evaluates a repeating Auto-Targeting System cycle tick inside `ballpark`, locking only
/// hostile NPCs (`category_id == 11`, excluding CONCORD/sentry/friendly militia) or non-fleet
/// ships with active offensive cycles targeting `ship_id` while obeying normal lock-time rules
/// using target Dogma `signatureRadius` (`CombatState::signature_radius` -> SDE `attr 552` -> `b.radius`).
#[allow(clippy::too_many_arguments)]
pub fn run_auto_targeter_ballpark_tick(
    ballpark: &mut eve_destiny::Ballpark,
    sde: Option<&SdeRegistry>,
    char_faction_id: Option<i64>,
    ship_id: eve_destiny::BallId,
    max_range: f64,
    max_locks: usize,
    scan_resolution: f64,
    aggressors: &[eve_destiny::BallId],
    fleet_ships: &[eve_destiny::BallId],
) {
    let Some(ship_pos) = ballpark.balls.get(&ship_id).map(|b| b.position) else {
        return;
    };
    let existing_locks = ballpark.get_target_locks(ship_id);
    let mut lock_count = existing_locks.len();
    let mut candidates: Vec<(eve_destiny::BallId, f64, f64)> = ballpark
        .balls
        .values()
        .filter_map(|b| {
            let dist = (b.position - ship_pos).length();
            if b.id == ship_id
                || fleet_ships.contains(&b.id)
                || ballpark.is_static_ball(b.id)
                || dist > max_range
                || existing_locks.iter().any(|l| l.target_id == b.id)
            {
                return None;
            }
            let is_hostile = if aggressors.contains(&b.id) {
                true
            } else {
                match ballpark.ball_slim_meta(b.id) {
                    Some(m) if m.category_id == 11 => is_hostile_npc_group(m.group_id, char_faction_id),
                    Some(m) if m.category_id == 6 => false,
                    Some(_) => false,
                    None => sde
                        .and_then(|reg| reg.types.get(&b.type_id))
                        .is_some_and(|t| t.category_id == 11 && is_hostile_npc_group(t.group_id, char_faction_id)),
                }
            };
            if !is_hostile {
                return None;
            }
            let sig = ballpark
                .combat_states
                .get(&b.id)
                .map(|cs| cs.signature_radius)
                .filter(|&s| s > 0.0)
                .or_else(|| {
                    sde.and_then(|reg| reg.types.get(&b.type_id))
                        .and_then(|t| t.default_attributes.get(&552).copied())
                        .filter(|&s| s > 0.0)
                })
                .unwrap_or(b.radius);
            Some((b.id, dist, sig))
        })
        .collect();
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let eff_scan_res = scan_resolution.max(1.0);
    for (cand_id, _dist, sig) in candidates {
        if lock_count >= max_locks {
            break;
        }
        let lock_time_ms = eve_destiny::combat::compute_lock_time_seconds(eff_scan_res, sig) * 1000.0;
        ballpark.add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: cand_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range),
            },
        );
        lock_count += 1;
    }
}

#[cfg(test)]
#[path = "mining_phased_autotarget_tests.rs"]
mod mining_phased_autotarget_tests;

