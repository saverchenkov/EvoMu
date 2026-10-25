//! Planetary Interaction resource layer evaluation, spherical harmonics, and depletion state.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::list_sde_planet_p0_resources;
use crate::services::industry_pi::pi_helpers::{PiColonyNetworkState, PiPinState};
use eve_db::{OwnerNoteRecord, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::TypeId;
use std::collections::HashMap;

/// Persistent planet resource depletion state record stored in `WorldStateSnapshot.owner_notes`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PlanetResourceDepletionState {
    pub planet_id: i64,
    /// Depletion factor (0.0 .. 0.95) per resource type.
    #[serde(default)]
    pub resource_depletion: HashMap<TypeId, f64>,
    /// Localized depletion spots from active or historical extractor heads.
    #[serde(default)]
    pub local_depletion: Vec<PiDepletionSpot>,
    /// Base depletion rate applied during GM depletion simulation.
    #[serde(default)]
    pub gm_depletion_rate: f64,
    /// GM simulated hourly depletion curve.
    #[serde(default)]
    pub gm_depletion_curve: Vec<f64>,
    /// Unix timestamp of last depletion simulation run.
    #[serde(default)]
    pub gm_last_sim_unix: u64,
}

/// A localized surface depletion spot created by an extractor head program.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PiDepletionSpot {
    pub latitude: f64,
    pub longitude: f64,
    pub radius: f64,
    pub resource_type_id: TypeId,
    pub amount_depleted: f64,
}

/// Computes the deterministic note ID used to persist planet depletion state in `snap.owner_notes`.
pub fn pi_depletion_note_id(planet_id: i64) -> i64 {
    -(8_500_000_000_000 + (planet_id.abs() % 1_000_000_000))
}

/// Loads the planet resource depletion state from `snap.owner_notes`, or returns default.
pub fn load_planet_depletion(snap: &WorldStateSnapshot, planet_id: i64) -> PlanetResourceDepletionState {
    let nid = pi_depletion_note_id(planet_id);
    if let Some(rec) = snap.owner_notes.get(&nid) {
        if let Ok(state) = serde_json::from_str::<PlanetResourceDepletionState>(&rec.note) {
            return state;
        }
    }
    PlanetResourceDepletionState {
        planet_id,
        ..Default::default()
    }
}

/// Saves the planet resource depletion state to `snap.owner_notes`.
pub fn save_planet_depletion(
    snap: &mut WorldStateSnapshot,
    state: &PlanetResourceDepletionState,
) {
    let nid = pi_depletion_note_id(state.planet_id);
    if let Ok(js) = serde_json::to_string(state) {
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: state.planet_id,
                label: format!("pi_depletion:{}", state.planet_id),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

/// Evaluates real spherical harmonic basis polynomials up to band l=2 on the unit sphere.
/// Given (latitude, longitude) on the sphere, computes Cartesian coordinates:
/// x = cos(lat) * cos(lon), y = cos(lat) * sin(lon), z = sin(lat).
pub fn evaluate_spherical_harmonic_basis(lat: f64, lon: f64) -> [f64; 9] {
    let clat = lat.cos();
    let x = clat * lon.cos();
    let y = clat * lon.sin();
    let z = lat.sin();

    [
        1.0,                    // l=0, m=0: constant base layer
        z,                      // l=1, m=0
        x,                      // l=1, m=1
        y,                      // l=1, m=-1
        3.0 * z * z - 1.0,      // l=2, m=0
        x * z,                  // l=2, m=1
        y * z,                  // l=2, m=-1
        x * x - y * y,          // l=2, m=2
        2.0 * x * y,            // l=2, m=-2
    ]
}

/// Computes resource richness (0.0 .. 1.0) at surface coordinates (lat, lon) on a planet
/// by evaluating the planet's spherical harmonic resource layer and subtracting colony extraction
/// and GM depletion layers.
pub fn compute_planet_resource_density(
    _ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    lat: f64,
    lon: f64,
) -> f64 {
    let basis = evaluate_spherical_harmonic_basis(lat, lon);
    let mut sum = 0.0;
    for (b, &b_val) in basis.iter().enumerate() {
        let coeff = (((planet_id as u64)
            .wrapping_mul(31)
            .wrapping_add((resource_tid as u64).wrapping_mul(17))
            .wrapping_add(b as u64)
            % 100) as f64)
            / 100.0;
        sum += coeff * b_val;
    }
    let base_richness = (0.50 + 0.08 * sum).clamp(0.15, 0.95);

    // Colony extraction depletion: sum extracted commodities across colonies on this planet
    let mut colony_extracted_qty = 0_u64;
    for col in snap.planetary_colonies.values() {
        if col.planet_id == planet_id {
            colony_extracted_qty += col.extracted_commodities.get(&resource_tid).copied().unwrap_or(0);
        }
    }
    let colony_depletion = ((colony_extracted_qty as f64) / 100_000.0).min(0.40);

    // GM and localized depletion state
    let depl_state = load_planet_depletion(snap, planet_id);
    let gm_depl = depl_state
        .resource_depletion
        .get(&resource_tid)
        .copied()
        .unwrap_or(0.0)
        .max(depl_state.gm_depletion_rate);

    let mut local_depl = 0.0;
    for spot in &depl_state.local_depletion {
        if spot.resource_type_id == resource_tid {
            let dist = (lat - spot.latitude).hypot(lon - spot.longitude);
            if dist < spot.radius && spot.radius > 0.0 {
                let falloff = 1.0 - (dist / spot.radius);
                local_depl += spot.amount_depleted * falloff;
            }
        }
    }
    local_depl = local_depl.min(0.50);

    let total_depletion = (colony_depletion + gm_depl + local_depl).min(0.85);
    (base_richness * (1.0 - total_depletion)).clamp(0.02, 1.0)
}

/// Evaluates the resource distribution report across all P0 resources for a given pin,
/// taking into account the pin's surface coordinates and extractor head positions.
pub fn compute_pin_resource_distribution(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    pin: &PiPinState,
) -> HashMap<TypeId, f64> {
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.planet_ids.contains(&planet_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);
    let p0_resources = list_sde_planet_p0_resources(ctx, sys_id, planet_id);

    let sample_points: Vec<(f64, f64)> = if !pin.extractor_heads.is_empty() {
        pin.extractor_heads.iter().map(|&(_, lat, lon)| (lat, lon)).collect()
    } else {
        vec![(pin.latitude, pin.longitude)]
    };

    let mut report = HashMap::new();
    for &tid in &p0_resources {
        let total_density: f64 = sample_points
            .iter()
            .map(|&(plat, plon)| compute_planet_resource_density(ctx, snap, planet_id, tid, plat, plon))
            .sum();
        let avg_density = total_density / (sample_points.len() as f64);
        report.insert(tid, avg_density);
    }
    report
}

/// Simulates depletion over `duration_hours` using the colony network's extractor pins,
/// generates the depletion curve, and updates the planet's depletion state in `snap`.
pub fn simulate_planet_depletion(
    snap: &mut WorldStateSnapshot,
    planet_id: i64,
    _char_id: i64,
    net: &PiColonyNetworkState,
    duration_hours: i64,
) -> (f64, Vec<f64>) {
    let duration = duration_hours.clamp(1, 168) as usize;
    let mut depl_state = load_planet_depletion(snap, planet_id);

    // Collect all active extractor pins
    let extractor_pins: Vec<&PiPinState> = net
        .pins
        .iter()
        .filter(|p| p.extractor_head_count > 0 || p.type_id == 2848 || p.type_id == 3060)
        .collect();

    let total_heads: u32 = extractor_pins
        .iter()
        .map(|p| p.extractor_head_count.max(1) as u32)
        .sum();
    let base_rate = 0.015 * (total_heads.max(1) as f64);

    let mut curve = Vec::with_capacity(duration);
    for h in 0..duration {
        let yield_factor = (-base_rate * (h as f64) * 0.1).exp();
        curve.push(yield_factor);
    }

    // Update depletion state for harvested resources and local spots
    for pin in &extractor_pins {
        let res_tid = pin.extractor_resource_type_id.unwrap_or(2268);
        let current_depl = depl_state.resource_depletion.entry(res_tid).or_insert(0.0);
        *current_depl = (*current_depl + base_rate * 0.5).min(0.90);

        let head_radius = pin.extractor_head_radius.max(0.02);
        if !pin.extractor_heads.is_empty() {
            for &(_, hlat, hlon) in &pin.extractor_heads {
                depl_state.local_depletion.push(PiDepletionSpot {
                    latitude: hlat,
                    longitude: hlon,
                    radius: head_radius,
                    resource_type_id: res_tid,
                    amount_depleted: 0.05,
                });
            }
        } else {
            depl_state.local_depletion.push(PiDepletionSpot {
                latitude: pin.latitude,
                longitude: pin.longitude,
                radius: head_radius,
                resource_type_id: res_tid,
                amount_depleted: 0.05,
            });
        }
    }

    // Keep local depletion spots bounded
    if depl_state.local_depletion.len() > 64 {
        depl_state.local_depletion.drain(0..(depl_state.local_depletion.len() - 64));
    }

    depl_state.gm_depletion_rate = base_rate;
    depl_state.gm_depletion_curve = curve.clone();
    depl_state.gm_last_sim_unix = 1;
    save_planet_depletion(snap, &depl_state);

    (base_rate, curve)
}

/// Parses (theta, phi) or (lat, lon) coordinates from PyRep arguments.
pub fn parse_theta_phi(theta_rep: Option<&PyRep>, phi_rep: Option<&PyRep>) -> (f64, f64) {
    match (theta_rep, phi_rep) {
        (Some(PyRep::Tuple(t)), _) | (Some(PyRep::List(t)), _) if t.len() >= 2 => {
            let th = t[0].as_f64().unwrap_or(0.0);
            let ph = t[1].as_f64().unwrap_or(0.0);
            (th, ph)
        }
        (Some(th), Some(ph)) => (th.as_f64().unwrap_or(0.0), ph.as_f64().unwrap_or(0.0)),
        (Some(th), None) => (th.as_f64().unwrap_or(0.0), 0.0),
        _ => (0.0, 0.0),
    }
}
