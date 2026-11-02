//! `mining_b33` — Phased Asteroid Field lifecycle & Mobile Phase Anchor integration (`MN-08`).
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

/// Derives Mobile Phase Anchor energy output for an Industrial Command Ship hull from `sde`:
/// - Group `883` (Capital Industrial Ship, e.g. Rorqual `28352`) -> `100`
/// - Group `941` (Industrial Command Ship): Orca (`28606`) -> `50`, Porpoise (`42244`) -> `35`
pub fn ics_energy_for_type(sde: &SdeRegistry, ship_type_id: TypeId) -> u32 {
    let Some(t) = sde.types.get(&ship_type_id) else {
        return 0;
    };
    match t.group_id {
        883 => 100,
        941 if ship_type_id == 28_606 || t.name.contains("Orca") => 50,
        941 => 35,
        _ => 0,
    }
}

/// Returns `true` if `solar_system_id` is low-sec or null-sec K-space (security < 0.45).
pub fn is_low_or_null_kspace(ctx: &ServerContext, solar_system_id: u32) -> bool {
    if (31_000_000..=32_999_999).contains(&solar_system_id) {
        return false;
    }
    ctx.sde
        .solar_systems
        .get(&solar_system_id)
        .is_some_and(|s| s.security_status < 0.45)
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

fn hydrate_phased_fields_if_empty(ctx: &ServerContext) {
    let is_empty = ctx
        .phased_asteroid_fields
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty();
    if !is_empty {
        return;
    }
    let loaded: Option<HashMap<u32, PhasedAsteroidFieldState>> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get(BLOB_KEY_PHASED_FIELDS)
            .and_then(|json| serde_json::from_str(json).ok())
    });
    if let Some(fields) = loaded {
        if !fields.is_empty() {
            let mut map = ctx
                .phased_asteroid_fields
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if map.is_empty() {
                *map = fields;
            }
        }
    }
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
    hydrate_phased_fields_if_empty(ctx);
    let state = PhasedAsteroidFieldState::new(signature_id.to_string(), solar_system_id);
    let snapshot_clone = {
        let mut map = ctx
            .phased_asteroid_fields
            .write()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(solar_system_id, state.clone());
        map.clone()
    };
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        persist_phased_fields(snap, &snapshot_clone);
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
    });
    Some(state)
}

/// Live spawner hook called when a low-sec or null-sec K-space system is seeded.
pub fn maybe_spawn_phased_asteroid_field(
    ctx: &ServerContext,
    solar_system_id: u32,
) -> Option<PhasedAsteroidFieldState> {
    if !is_low_or_null_kspace(ctx, solar_system_id) {
        return None;
    }
    hydrate_phased_fields_if_empty(ctx);
    let already_active = ctx
        .phased_asteroid_fields
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&solar_system_id)
        .is_some_and(|f| !f.collapsed);
    if already_active {
        return None;
    }
    let (sig_seq, ast_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 2;
        (snap.next_entity_id - 1, snap.next_entity_id)
    });
    let angle = f64::from(solar_system_id % 360).to_radians();
    let dist = 220_000.0 + f64::from(solar_system_id % 50) * 1_000.0;
    let pos = (dist * angle.cos(), 15_000.0, dist * angle.sin());
    spawn_phased_asteroid_field(
        ctx,
        solar_system_id,
        &format!("PHS-{sig_seq}"),
        ast_id,
        50_000,
        pos,
    )
}

/// Advances the hyperspace fracture across the Phased Asteroid Field in `solar_system_id`.
/// At `progress >= 0.5`, spawns the system-wide cosmic anomaly beacon at the site's position.
/// At `progress >= 1.0`, collapses the field and depletes remaining phased asteroids.
pub fn advance_phased_asteroid_field(
    ctx: &ServerContext,
    solar_system_id: u32,
    delta_progress: f64,
) -> Option<PhasedAsteroidFieldState> {
    hydrate_phased_fields_if_empty(ctx);
    let (updated, snapshot_clone) = {
        let mut map = ctx
            .phased_asteroid_fields
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let field = map.get_mut(&solar_system_id)?;
        field.advance_fracture(delta_progress);
        (field.clone(), map.clone())
    };
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        persist_phased_fields(snap, &snapshot_clone);
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
                if ast.solar_system_id == solar_system_id
                    && PRISMATICITE_ORE_TYPES.contains(&ast.type_id)
                {
                    ast.quantity = 0;
                }
            }
        }
    });
    Some(updated)
}

/// Connects an Industrial Command Ship (`ship_id`) to the deployed Mobile Phase Anchor in
/// `solar_system_id`, enforcing per-ship ID de-duplication, owner/fleet membership, and range.
/// Returns `Some(true)` once total energy >= 100 and the asteroid is phased in.
pub async fn connect_ics_to_phase_anchor(
    ctx: &ServerContext,
    solar_system_id: u32,
    ship_id: i64,
) -> Option<bool> {
    hydrate_phased_fields_if_empty(ctx);
    let (anchor_id, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let anchor = snap.items.values().find(|i| {
            i.location_id == i64::from(solar_system_id)
                && i.type_id == TYPE_MOBILE_PHASE_ANCHOR
                && i.quantity > 0
        })?;
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
        let same_owner_or_fleet = pilot_cid == anchor.owner_id
            || snap
                .fleets
                .values()
                .any(|fl| fl.members.contains(&anchor.owner_id) && fl.members.contains(&pilot_cid));
        same_owner_or_fleet.then_some((anchor.item_id, ship.type_id))
    })?;

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if let (Ok(Some(anchor_ball)), Ok(Some(ship_ball))) = (
        sys_handle.get_ball_snapshot(anchor_id).await,
        sys_handle.get_ball_snapshot(ship_id).await,
    ) {
        if anchor_ball.1.position.distance(ship_ball.1.position) > ANCHOR_TETHER_RANGE_M {
            return None;
        }
    }

    let energy = ics_energy_for_type(&ctx.sde, ship_type_id);
    if energy == 0 {
        return None;
    }

    let (phased_in, snapshot_clone) = {
        let mut map = ctx
            .phased_asteroid_fields
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let field = map.get_mut(&solar_system_id)?;
        let phased = field.connect_industrial_command_ship(ship_id, energy);
        (phased, map.clone())
    };
    ctx.db.with_mut_snapshot(|snap| {
        persist_phased_fields(snap, &snapshot_clone);
    });
    Some(phased_in)
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

/// Periodic cluster tick for active Phased Asteroid Fields (`MN-08`).
pub async fn tick_phased_asteroid_fields(ctx: &ServerContext, now_unix: u64) {
    hydrate_phased_fields_if_empty(ctx);
    let prev_tick = ctx.db.with_mut_snapshot(|snap| {
        let prev = snap
            .service_state_blobs
            .get("phased_field_last_tick_unix")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(now_unix);
        snap.service_state_blobs
            .insert("phased_field_last_tick_unix".into(), now_unix.to_string());
        prev
    });
    let elapsed_secs = now_unix.saturating_sub(prev_tick);
    if elapsed_secs == 0 {
        return;
    }
    let sys_ids: Vec<u32> = {
        let map = ctx
            .phased_asteroid_fields
            .read()
            .unwrap_or_else(|e| e.into_inner());
        map.keys().copied().collect()
    };
    let delta_progress = (elapsed_secs as f64) / 14_400.0;
    for sys_id in sys_ids {
        let _ = advance_phased_asteroid_field(ctx, sys_id, delta_progress);
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
    if !is_prismaticite_ore(&ctx.sde, ore_type_id) {
        return 1.0;
    }
    hydrate_phased_fields_if_empty(ctx);
    let map = ctx
        .phased_asteroid_fields
        .read()
        .unwrap_or_else(|e| e.into_inner());
    map.get(&solar_system_id)
        .map(|f| f.mining_efficiency())
        .unwrap_or(1.0)
}
