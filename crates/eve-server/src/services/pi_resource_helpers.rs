//! Planetary Interaction resource layer evaluation, spherical harmonics, and depletion state.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::list_sde_planet_p0_resources;
use crate::services::industry_pi::pi_helpers::{PiColonyNetworkState, PiPinState};
use eve_db::{OwnerNoteRecord, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, TypeId};
use std::collections::HashMap;
use std::sync::OnceLock;

/// V24 `appConst.planetResourceMaxValue` (`eve/common/lib/appConst.py:1894`).
pub const PLANET_RESOURCE_MAX_VALUE: f64 = 1.21;

/// V24 `appConst.MAX_DISPLAY_QUALITY = planetResourceMaxValue * 255 * 0.5` (`appConst.py:1897`),
/// matching `PlanetaryProductionPlanetContentPiece` and `ResourceController` normalization.
pub const MAX_DISPLAY_QUALITY: f64 = PLANET_RESOURCE_MAX_VALUE * 255.0 * 0.5;

/// V24 `appConst.planetResourceProximityLimits` (`appConst.py:1882-1886`): `(minBand, maxBand)`
/// indexed by `planetResourceProximity{Distant=0, Region=1, Constellation=2, System=3, Planet=4}`.
pub const PLANET_RESOURCE_PROXIMITY_LIMITS: [(usize, usize); 5] =
    [(2, 6), (4, 10), (6, 15), (10, 20), (15, 30)];

/// Exponential recovery half-life (`12h`) for planetary resource depletion once extraction stops.
pub const PI_DEPLETION_RECOVERY_HALF_LIFE_SECS: f64 = 43_200.0;

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
    /// Unix timestamp when resource/local depletion state was last persisted/regenerated.
    #[serde(default)]
    pub last_updated_unix: u64,
    /// Cumulative planetary extraction pressure per resource type (subject to exponential recovery).
    #[serde(default)]
    pub colony_extraction_pressure: HashMap<TypeId, f64>,
    /// Unix timestamp when `colony_extraction_pressure` was last updated or regenerated.
    #[serde(default)]
    pub colony_pressure_last_unix: u64,
    /// GM-injected resource nugget layers boosting local resource density.
    #[serde(default)]
    pub nugget_layers: Vec<PiNuggetLayer>,
}

/// A localized GM-created resource nugget layer on a planet surface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PiNuggetLayer {
    pub resource_type_id: TypeId,
    pub latitude: f64,
    pub longitude: f64,
    pub radius: f64,
    pub boost: f64,
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

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hash_unit(seed: u64) -> f64 {
    ((splitmix64(seed) >> 11) as f64) / ((1_u64 << 53) as f64)
}

/// Static fallback weights matching V24 `res:/staticdata/globaltypes.fsdbinary` when a unit test
/// builds a synthetic [`SdeRegistry`] without binary CFSD files on disk.
fn fallback_globaltypes_resource_weight(restriction_type_id: TypeId, resource_tid: TypeId) -> Option<f64> {
    match restriction_type_id {
        11 => match resource_tid {
            2305 | 2287 => Some(1.0),
            2288 => Some(0.95),
            2268 | 2073 => Some(0.9),
            _ => None,
        },
        12 => match resource_tid {
            2272 => Some(1.0),
            2286 | 2268 => Some(0.95),
            2310 => Some(0.9),
            2073 => Some(0.85),
            _ => None,
        },
        13 => match resource_tid {
            2311 | 2310 => Some(1.0),
            2309 => Some(0.95),
            2268 | 2267 => Some(0.8),
            _ => None,
        },
        2014 => match resource_tid {
            2286 | 2268 => Some(1.0),
            2073 | 2287 => Some(0.95),
            2288 => Some(0.9),
            _ => None,
        },
        2015 => match resource_tid {
            2307 => Some(1.0),
            2308 | 2272 | 2306 => Some(0.95),
            2267 => Some(0.9),
            _ => None,
        },
        2016 => match resource_tid {
            2288 | 2270 | 2073 | 2267 => Some(1.0),
            2268 => Some(0.75),
            _ => None,
        },
        2017 => match resource_tid {
            2309 => Some(1.0),
            2310 => Some(0.95),
            2308 => Some(0.9),
            2268 | 2267 => Some(0.85),
            _ => None,
        },
        2063 => match resource_tid {
            2308 | 2306 => Some(1.0),
            2270 | 2267 => Some(0.95),
            2272 => Some(0.9),
            _ => None,
        },
        _ => None,
    }
}

/// Loads `resourceDistribution` weights per planet type from V24 `res:/staticdata/globaltypes.fsdbinary`.
pub fn load_globaltypes_resource_distributions(
    sde: &SdeRegistry,
) -> Option<&'static HashMap<TypeId, HashMap<TypeId, f64>>> {
    static CACHE: OnceLock<HashMap<TypeId, HashMap<TypeId, f64>>> = OnceLock::new();
    if let Some(cached) = CACHE.get() {
        return Some(cached);
    }
    if sde.resources.is_empty() {
        return None;
    }
    let cfsd = sde.load_cfsd_nested("res:/staticdata/globaltypes.fsdbinary").ok()?;
    let mut parsed: HashMap<TypeId, HashMap<TypeId, f64>> = HashMap::new();
    for (pt_key, entry) in cfsd {
        let Ok(planet_type_id) = pt_key.parse::<TypeId>() else {
            continue;
        };
        let Some(dist) = entry.get("resourceDistribution").and_then(|v| v.as_object()) else {
            continue;
        };
        let mut weights = HashMap::new();
        for (res_key, val) in dist {
            if let (Ok(res_tid), Some(w)) = (res_key.parse::<TypeId>(), val.as_f64()) {
                weights.insert(res_tid, w.clamp(0.0, 1.0));
            }
        }
        if !weights.is_empty() {
            parsed.insert(planet_type_id, weights);
        }
    }
    if parsed.is_empty() {
        return None;
    }
    Some(CACHE.get_or_init(|| parsed))
}

fn resolve_planet_restriction_type_id(sde: &SdeRegistry, planet_id: i64) -> Option<TypeId> {
    let planet_cel = sde
        .celestials_by_system
        .values()
        .flatten()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)?;
    let pt = sde.types.get(&planet_cel.type_id)?;
    Some(pt.variation_parent_type_id.unwrap_or(pt.type_id))
}

/// Resolves the normalized planet-type distribution weight `(0.0..=1.0]` for `(planet_id, resource_tid)`
/// from V24 `globaltypes.fsdbinary`.
pub fn resolve_planet_resource_distribution_weight(
    sde: &SdeRegistry,
    planet_id: i64,
    resource_tid: TypeId,
) -> f64 {
    let Some(restriction_tid) = resolve_planet_restriction_type_id(sde, planet_id) else {
        return 0.78;
    };
    if let Some(map) = load_globaltypes_resource_distributions(sde) {
        if let Some(w) = map.get(&restriction_tid).and_then(|m| m.get(&resource_tid)) {
            return (*w).clamp(0.05, 1.0);
        }
    }
    fallback_globaltypes_resource_weight(restriction_tid, resource_tid).unwrap_or(0.75)
}

pub fn depletion_recovery_factor(last_unix: u64, now_unix: u64) -> f64 {
    if last_unix == 0 || now_unix <= last_unix {
        1.0
    } else {
        let elapsed = (now_unix - last_unix) as f64;
        0.5_f64.powf(elapsed / PI_DEPLETION_RECOVERY_HALF_LIFE_SECS)
    }
}

/// Applies time-based planetary resource replenishment to `state` up to `now_unix`.
pub fn regenerate_planet_depletion(state: &mut PlanetResourceDepletionState, now_unix: u64) {
    if state.last_updated_unix > 0 && now_unix > state.last_updated_unix {
        let factor = depletion_recovery_factor(state.last_updated_unix, now_unix);
        for v in state.resource_depletion.values_mut() {
            *v *= factor;
        }
        state.resource_depletion.retain(|_, v| *v >= 1e-4);
        for spot in &mut state.local_depletion {
            spot.amount_depleted *= factor;
        }
        state.local_depletion.retain(|s| s.amount_depleted >= 1e-4);
        state.gm_depletion_rate *= factor;
        if state.gm_depletion_rate < 1e-4 {
            state.gm_depletion_rate = 0.0;
        }
    }
    if now_unix > 0 {
        state.last_updated_unix = now_unix;
    }
    if state.colony_pressure_last_unix > 0 && now_unix > state.colony_pressure_last_unix {
        let factor = depletion_recovery_factor(state.colony_pressure_last_unix, now_unix);
        for v in state.colony_extraction_pressure.values_mut() {
            *v *= factor;
        }
        state.colony_extraction_pressure.retain(|_, v| *v >= 1.0);
    }
    if now_unix > 0
        && (state.colony_pressure_last_unix > 0 || !state.colony_extraction_pressure.is_empty())
    {
        state.colony_pressure_last_unix = now_unix;
    }
}

fn seed_legacy_colony_pressure(
    snap: &WorldStateSnapshot,
    planet_id: i64,
    now_unix: u64,
    depl_state: &mut PlanetResourceDepletionState,
) {
    if depl_state.colony_pressure_last_unix > 0 || !depl_state.colony_extraction_pressure.is_empty()
    {
        return;
    }
    let mut saw_legacy_colony = false;
    for col in snap.planetary_colonies.values() {
        if col.planet_id != planet_id {
            continue;
        }
        if !col.extracted_commodities.is_empty() {
            saw_legacy_colony = true;
        }
        let factor = depletion_recovery_factor(col.last_extracted_unix, now_unix);
        for (&tid, &qty) in &col.extracted_commodities {
            let eff = (qty as f64) * factor;
            if eff >= 1.0 {
                *depl_state.colony_extraction_pressure.entry(tid).or_insert(0.0) += eff;
            }
        }
    }
    // Mark planets whose legacy colonies already decayed below 1.0 as migrated so later
    // `last_extracted_unix` bumps during colony ticks cannot re-trigger legacy fallback at 1.0x.
    if (!depl_state.colony_extraction_pressure.is_empty() || saw_legacy_colony) && now_unix > 0 {
        depl_state.colony_pressure_last_unix = now_unix;
    }
}

/// Records newly extracted commodity quantities into planetary depletion pressure without mutating pin inventory.
pub fn record_planet_colony_extraction(
    snap: &mut WorldStateSnapshot,
    planet_id: i64,
    extracted_by_type: &HashMap<TypeId, u64>,
    now_unix: u64,
) {
    if extracted_by_type.values().all(|&q| q == 0) {
        return;
    }
    let mut depl_state = load_planet_depletion(snap, planet_id);
    seed_legacy_colony_pressure(snap, planet_id, now_unix, &mut depl_state);
    if depl_state.colony_pressure_last_unix == 0 && now_unix > 0 {
        depl_state.colony_pressure_last_unix = now_unix;
    }
    regenerate_planet_depletion(&mut depl_state, now_unix);
    for (&tid, &qty) in extracted_by_type {
        if qty > 0 {
            *depl_state.colony_extraction_pressure.entry(tid).or_insert(0.0) += qty as f64;
        }
    }
    if now_unix > 0 {
        depl_state.colony_pressure_last_unix = now_unix;
    }
    save_planet_depletion(snap, &depl_state);
}

/// Regenerates persisted planetary depletion when an idle colony ticks with zero new extraction.
pub fn regenerate_idle_planet_depletion(
    snap: &mut WorldStateSnapshot,
    planet_id: i64,
    baseline_unix: u64,
    now_unix: u64,
    always_persist: bool,
) {
    let mut depl_state = load_planet_depletion(snap, planet_id);
    seed_legacy_colony_pressure(snap, planet_id, now_unix, &mut depl_state);
    let has_existing_state = !depl_state.resource_depletion.is_empty()
        || !depl_state.local_depletion.is_empty()
        || depl_state.gm_depletion_rate > 0.0
        || !depl_state.colony_extraction_pressure.is_empty()
        || depl_state.colony_pressure_last_unix > 0
        || depl_state.last_updated_unix > 0;
    if !always_persist && !has_existing_state {
        return;
    }
    if depl_state.last_updated_unix == 0 && baseline_unix > 0 && baseline_unix < now_unix {
        depl_state.last_updated_unix = baseline_unix;
    }
    regenerate_planet_depletion(&mut depl_state, now_unix);
    save_planet_depletion(snap, &depl_state);
}

/// Legacy helper decaying `col.extracted_commodities` over idle `elapsed_secs` (kept for backward compatibility;
/// live pin inventory and planetary extraction depletion pressure are decoupled via `colony_extraction_pressure`).
pub fn decay_colony_extracted_commodities(
    col: &mut eve_db::PlanetaryColonyRecord,
    elapsed_secs: u64,
) {
    if elapsed_secs == 0 || col.extracted_commodities.is_empty() {
        return;
    }
    let factor = 0.5_f64.powf((elapsed_secs as f64) / PI_DEPLETION_RECOVERY_HALF_LIFE_SECS);
    for qty in col.extracted_commodities.values_mut() {
        *qty = ((*qty as f64) * factor).floor() as u64;
    }
    col.extracted_commodities.retain(|_, q| *q > 0);
}

fn global_colony_and_gm_depletion_at(
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    now_unix: u64,
) -> (f64, f64, PlanetResourceDepletionState) {
    let mut depl_state = load_planet_depletion(snap, planet_id);
    regenerate_planet_depletion(&mut depl_state, now_unix);
    let colony_extracted_effective = if depl_state.colony_pressure_last_unix > 0
        || !depl_state.colony_extraction_pressure.is_empty()
    {
        depl_state
            .colony_extraction_pressure
            .get(&resource_tid)
            .copied()
            .unwrap_or(0.0)
    } else {
        let mut total = 0.0_f64;
        for col in snap.planetary_colonies.values() {
            if col.planet_id == planet_id {
                let qty = col
                    .extracted_commodities
                    .get(&resource_tid)
                    .copied()
                    .unwrap_or(0) as f64;
                if qty > 0.0 {
                    total += qty * depletion_recovery_factor(col.last_extracted_unix, now_unix);
                }
            }
        }
        total
    };
    let colony_depletion = (colony_extracted_effective / 100_000.0).min(0.40);
    let gm_depl = depl_state
        .resource_depletion
        .get(&resource_tid)
        .copied()
        .unwrap_or(0.0)
        .max(depl_state.gm_depletion_rate);
    (colony_depletion, gm_depl, depl_state)
}

// DERIVED: `evomu_sde.sqlite` ships base resource distribution weights in `res:/staticdata/globaltypes.fsdbinary`
// rather than per-planet spherical-harmonic band maps (`planet_id` maps via deterministic splitmix64 SH expansion).
fn compute_planet_sh_bands_base(
    sde: &SdeRegistry,
    planet_id: i64,
    resource_tid: TypeId,
    num_bands: usize,
) -> Vec<f64> {
    let bands = num_bands.max(1);
    let total_coeffs = bands * bands;
    let dist_weight = resolve_planet_resource_distribution_weight(sde, planet_id, resource_tid);
    let base_seed = (planet_id as u64)
        .wrapping_mul(0x517C_C1B7_2722_0A95)
        ^ (resource_tid as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let richness_roll = 0.56 + 0.30 * hash_unit(base_seed);
    let base_monopole = (dist_weight * richness_roll).clamp(0.18, 0.88);

    let mut coeffs = vec![0.0_f64; total_coeffs];
    coeffs[0] = base_monopole;
    for l in 1..bands {
        let band_start = l * l;
        let band_len = 2 * l + 1;
        let band_amp = base_monopole * (0.28 / (l as f64).powf(1.35));
        for m_off in 0..band_len {
            let coeff_idx = band_start + m_off;
            let signed = hash_unit(base_seed.wrapping_add((coeff_idx as u64) * 1_047_29)) * 2.0 - 1.0;
            // Ensure low-order harmonics never collapse to exact zero across any planet/resource seed.
            let shaped = if l <= 2 && signed.abs() < 0.22 {
                if signed >= 0.0 { 0.22 } else { -0.22 }
            } else {
                signed
            };
            coeffs[coeff_idx] = shaped * band_amp;
        }
    }
    coeffs
}

/// Computes `num_bands * num_bands` real spherical harmonic coefficients (`f32` order `(l, m)`)
/// from V24 `globaltypes.fsdbinary` resource distribution weights, deterministic planet/resource
/// seeding, live colony extraction + GM depletion, and active nugget layers.
pub fn compute_planet_resource_sh_coefficients(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    num_bands: usize,
) -> Vec<f32> {
    let mut coeffs = compute_planet_sh_bands_base(&ctx.sde, planet_id, resource_tid, num_bands);
    let (colony_depl, gm_depl, depl_state) =
        global_colony_and_gm_depletion_at(snap, planet_id, resource_tid, ctx.now_unix());
    let global_depletion = (colony_depl + gm_depl).min(0.85);
    let scale = 1.0 - global_depletion;
    for c in &mut coeffs {
        *c *= scale;
    }
    coeffs[0] = coeffs[0].max(0.02);

    for layer in &depl_state.nugget_layers {
        if layer.resource_type_id == resource_tid && layer.boost > 0.0 {
            let basis = evaluate_spherical_harmonic_basis(layer.latitude, layer.longitude);
            let w = layer.boost * layer.radius.clamp(0.05, 1.0);
            for (slot, &b_val) in coeffs.iter_mut().zip(basis.iter()) {
                *slot += w * b_val;
            }
        }
    }

    coeffs.into_iter().map(|c| c as f32).collect()
}

/// Evaluates the normalized `[0.02, 0.98]` planetary resource average quality (`l=0` SH monopole)
/// for `(planet_id, resource_tid)`; multiply by [`MAX_DISPLAY_QUALITY`] for V24 `GetPlanetResourceInfo`.
pub fn compute_planet_resource_quality(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
) -> f64 {
    let coeffs = compute_planet_resource_sh_coefficients(ctx, snap, planet_id, resource_tid, 3);
    (coeffs[0] as f64).clamp(0.02, 0.98)
}

/// Computes resource richness (`0.02 .. 1.0`) at surface coordinates `(lat, lon)` on a planet
/// by evaluating the planet's spherical harmonic resource layer and subtracting colony extraction
/// and GM depletion layers.
pub fn compute_planet_resource_density(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    lat: f64,
    lon: f64,
) -> f64 {
    let basis = evaluate_spherical_harmonic_basis(lat, lon);
    let base_coeffs = compute_planet_sh_bands_base(&ctx.sde, planet_id, resource_tid, 3);
    let mut sum = base_coeffs[0] * basis[0];
    for idx in 1..9_usize {
        sum += base_coeffs[idx] * basis[idx] * 0.45;
    }
    let base_richness = sum.clamp(0.15, 0.90);

    let (colony_depletion, gm_depl, depl_state) =
        global_colony_and_gm_depletion_at(snap, planet_id, resource_tid, ctx.now_unix());

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

    let mut nugget_boost = 0.0;
    for layer in &depl_state.nugget_layers {
        if layer.resource_type_id == resource_tid {
            let dist = (lat - layer.latitude).hypot(lon - layer.longitude);
            if dist < layer.radius && layer.radius > 0.0 {
                let falloff = 1.0 - (dist / layer.radius);
                nugget_boost += layer.boost.max(0.0) * falloff;
            }
        }
    }
    nugget_boost = nugget_boost.min(0.80);

    let total_depletion = (colony_depletion + gm_depl + local_depl).min(0.85);
    (base_richness * (1.0 - total_depletion) + nugget_boost).clamp(0.02, 1.0)
}

/// Computes the 9 real spherical harmonic `f32` coefficients (36 bytes little-endian, `numBands = 3`)
/// for `(planet_id, resource_tid)` from live colony extraction, depletion state, and GM nugget layers.
pub fn compute_planet_complete_resource_sh_bytes(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
) -> Vec<u8> {
    let coeffs = compute_planet_resource_sh_coefficients(ctx, snap, planet_id, resource_tid, 3);
    let mut bytes = Vec::with_capacity(coeffs.len() * 4);
    for c in coeffs {
        bytes.extend_from_slice(&c.to_le_bytes());
    }
    bytes
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
        .or_else(|| ctx.sde.location_solar_system(planet_id))
        .or_else(|| snap.planetary_colonies.values().find(|c| c.planet_id == planet_id).map(|c| c.solar_system_id))
        .unwrap_or_else(|| ctx.sde.solar_systems.keys().copied().next().unwrap_or(0));
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

#[cfg(test)]
#[path = "pi_resource_helpers_tests.rs"]
mod pi_resource_helpers_tests;

