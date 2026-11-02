//! `mining_b33` — Phased Asteroid Field lifecycle & Mobile Phase Anchor integration (`MN-08`).
//!
//! Wires `PhasedAsteroidFieldState` into `ServerContext` cosmic signatures/anomalies,
//! deployable Mobile Phase Anchor (`90037`) ICS energy contribution (Rorqual 100, Orca 50,
//! Porpoise 35), cluster tick fracture progression, and `mining_cycle::resolve_cycle`
//! Prismaticite efficiency (10% breached vs 100% phased-in).

#![forbid(unsafe_code)]

use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_sde::TypeId;

use crate::router::ServerContext;
use crate::services::space_mining_harvest::PhasedAsteroidFieldState;

/// Mobile Phase Anchor deployable item type ID (`group_id = 4913`, `category_id = 22`).
pub const TYPE_MOBILE_PHASE_ANCHOR: TypeId = 90_037;

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

/// Spawns a Phased Asteroid Field in a low-sec or null-sec `solar_system_id` as a probeable
/// cosmic signature (`sig_group: "OreAnomaly"`) with breached Prismaticite (`90041`) at 10%
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
    let state = PhasedAsteroidFieldState::new(signature_id.to_string(), solar_system_id);
    {
        let mut map = ctx.phased_asteroid_fields.write().unwrap_or_else(|e| e.into_inner());
        map.insert(solar_system_id, state.clone());
    }
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.entry(solar_system_id).or_default();
        sigs.retain(|s| s.signature_id != signature_id);
        sigs.push(CosmicSignatureRecord {
            signature_id: signature_id.to_string(),
            solar_system_id,
            sig_group: "OreAnomaly".to_string(),
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
                type_id: 90_041, // Prismaticite
                quantity,
                initial_quantity: quantity,
                position,
                radius: 250.0,
            },
        );
    });
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
    let updated = {
        let mut map = ctx.phased_asteroid_fields.write().unwrap_or_else(|e| e.into_inner());
        let field = map.get_mut(&solar_system_id)?;
        field.advance_fracture(delta_progress);
        field.clone()
    };
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        let site_pos = snap
            .cosmic_signatures
            .get(&solar_system_id)
            .and_then(|sigs| sigs.iter().find(|s| s.signature_id == updated.signature_id))
            .map(|s| s.position)
            .or_else(|| {
                snap.asteroids
                    .values()
                    .find(|a| a.solar_system_id == solar_system_id && a.type_id == 90_041)
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
                    ore_types: vec![90_041],
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
                if ast.solar_system_id == solar_system_id && matches!(ast.type_id, 90_041 | 90_042 | 90_445 | 90_446) {
                    ast.quantity = 0;
                }
            }
        }
    });
    Some(updated)
}

/// Connects an Industrial Command Ship (`ship_type_id`: Rorqual 28352 = 100, Orca 28606 = 50,
/// Porpoise 42244 = 35) to the deployed Mobile Phase Anchor in `solar_system_id`.
/// Returns `Some(true)` once total energy >= 100 and the asteroid is phased in.
pub fn connect_ics_to_phase_anchor(
    ctx: &ServerContext,
    solar_system_id: u32,
    ship_type_id: TypeId,
) -> Option<bool> {
    let has_anchor = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.location_id == i64::from(solar_system_id)
                && i.type_id == TYPE_MOBILE_PHASE_ANCHOR
                && i.quantity > 0
        })
    });
    if !has_anchor {
        return None;
    }
    let mut map = ctx.phased_asteroid_fields.write().unwrap_or_else(|e| e.into_inner());
    let field = map.get_mut(&solar_system_id)?;
    Some(field.connect_industrial_command_ship(ship_type_id))
}

/// Evaluates deployed Mobile Phase Anchor (`90037`) and active Industrial Command Ships in
/// `solar_system_id` when a Mobile Phase Anchor is launched into space.
pub fn on_mobile_phase_anchor_deployed(ctx: &ServerContext, solar_system_id: u32) {
    let ics_types: Vec<TypeId> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
            })
            .filter_map(|c| snap.items.get(&c.active_ship_id).map(|sh| sh.type_id))
            .filter(|&tid| matches!(tid, 28_352 | 28_606 | 42_244))
            .collect()
    });
    for tid in ics_types {
        let _ = connect_ics_to_phase_anchor(ctx, solar_system_id, tid);
    }
}

/// Periodic cluster tick for active Phased Asteroid Fields (`MN-08`).
pub async fn tick_phased_asteroid_fields(ctx: &ServerContext, now_unix: u64) {
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
        let map = ctx.phased_asteroid_fields.read().unwrap_or_else(|e| e.into_inner());
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
    if !matches!(ore_type_id, 90_041 | 90_042 | 90_445 | 90_446) {
        return 1.0;
    }
    let map = ctx.phased_asteroid_fields.read().unwrap_or_else(|e| e.into_inner());
    map.get(&solar_system_id)
        .map(|f| f.mining_efficiency())
        .unwrap_or(1.0)
}
