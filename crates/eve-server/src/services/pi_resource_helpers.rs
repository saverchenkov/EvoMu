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
    /// Per-resource Unix timestamps when each `colony_extraction_pressure` entry was last
    /// updated or decayed; legacy persisted records fall back to `colony_pressure_last_unix`.
    #[serde(default)]
    pub colony_pressure_last_unix_by_type: HashMap<TypeId, u64>,
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

const GLOBALTYPES_RES_PATH: &str = "res:/staticdata/globaltypes.fsdbinary";
const DEFAULT_PLANET_RESOURCE_DISTRIBUTION_WEIGHT: f64 = 0.75;

/// Parses V24 `globaltypes.fsdbinary` entries into per-planet-type `resourceDistribution` weight maps.
pub(crate) fn parse_globaltypes_resource_distributions(
    cfsd: &std::collections::BTreeMap<String, serde_json::Value>,
) -> HashMap<TypeId, HashMap<TypeId, f64>> {
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
    parsed
}

/// Looks up and clamps a normalized resource distribution weight from a parsed `globaltypes` map.
pub(crate) fn lookup_planet_resource_distribution_weight(
    distributions: &HashMap<TypeId, HashMap<TypeId, f64>>,
    restriction_type_id: TypeId,
    resource_tid: TypeId,
) -> Option<f64> {
    distributions
        .get(&restriction_type_id)
        .and_then(|m| m.get(&resource_tid))
        .map(|w| (*w).clamp(0.05, 1.0))
}

/// Loads `resourceDistribution` weights per planet type from V24 `res:/staticdata/globaltypes.fsdbinary`.
pub fn load_globaltypes_resource_distributions(
    sde: &SdeRegistry,
) -> Option<&'static HashMap<TypeId, HashMap<TypeId, f64>>> {
    if !sde.resources.contains_key(GLOBALTYPES_RES_PATH) {
        return None;
    }
    static CACHE: OnceLock<HashMap<TypeId, HashMap<TypeId, f64>>> = OnceLock::new();
    if let Some(cached) = CACHE.get() {
        return Some(cached);
    }
    let cfsd = sde.load_cfsd_nested(GLOBALTYPES_RES_PATH).ok()?;
    let parsed = parse_globaltypes_resource_distributions(&cfsd);
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
/// from V24 `globaltypes.fsdbinary`, defaulting to a neutral uniform weight if CFSD data is unavailable.
pub fn resolve_planet_resource_distribution_weight(
    sde: &SdeRegistry,
    planet_id: i64,
    resource_tid: TypeId,
) -> f64 {
    let Some(restriction_tid) = resolve_planet_restriction_type_id(sde, planet_id) else {
        return 0.78;
    };
    load_globaltypes_resource_distributions(sde)
        .and_then(|map| lookup_planet_resource_distribution_weight(map, restriction_tid, resource_tid))
        .unwrap_or(DEFAULT_PLANET_RESOURCE_DISTRIBUTION_WEIGHT)
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
/// Merges active `(start_unix, end_unix)` extractor windows on a planet and returns
/// the number of seconds within `[from_unix, to_unix]` during which no extractor on
/// the planet was active.
pub(crate) fn uncovered_idle_secs(
    windows: &[(u64, u64)],
    from_unix: u64,
    to_unix: u64,
) -> u64 {
    if from_unix == 0 || to_unix <= from_unix {
        return 0;
    }
    let mut clipped: Vec<(u64, u64)> = windows
        .iter()
        .filter_map(|&(s, e)| {
            let cs = s.max(from_unix);
            let ce = e.min(to_unix);
            (ce > cs).then_some((cs, ce))
        })
        .collect();
    clipped.sort_unstable();
    let mut covered = 0_u64;
    let mut cur_s = 0_u64;
    let mut cur_e = 0_u64;
    let mut has_cur = false;
    for (s, e) in clipped {
        if !has_cur {
            cur_s = s;
            cur_e = e;
            has_cur = true;
        } else if s <= cur_e {
            cur_e = cur_e.max(e);
        } else {
            covered = covered.saturating_add(cur_e - cur_s);
            cur_s = s;
            cur_e = e;
        }
    }
    if has_cur {
        covered = covered.saturating_add(cur_e - cur_s);
    }
    (to_unix - from_unix).saturating_sub(covered)
}

/// Applies time-based replenishment strictly to GM/local depletion fields on `last_updated_unix`.
pub(crate) fn regenerate_gm_depletion(state: &mut PlanetResourceDepletionState, now_unix: u64) {
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
        state.last_updated_unix = state.last_updated_unix.max(now_unix);
    }
}

/// Returns the last update/decay timestamp tracked for `tid` in `colony_extraction_pressure`,
/// falling back to `state.colony_pressure_last_unix` for legacy state lacking per-type keys.
pub(crate) fn colony_pressure_last_unix_for_type(
    state: &PlanetResourceDepletionState,
    tid: TypeId,
) -> u64 {
    state
        .colony_pressure_last_unix_by_type
        .get(&tid)
        .copied()
        .unwrap_or(state.colony_pressure_last_unix)
}

/// Decays a single resource's `colony_extraction_pressure` strictly over idle intervals outside
/// `tid`'s own active `windows_by_type`, advancing `colony_pressure_last_unix_by_type[tid]` to `to_unix`.
pub(crate) fn decay_colony_pressure_for_type(
    state: &mut PlanetResourceDepletionState,
    windows_by_type: &HashMap<TypeId, Vec<(u64, u64)>>,
    tid: TypeId,
    to_unix: u64,
) {
    if to_unix == 0 {
        return;
    }
    let last_u = colony_pressure_last_unix_for_type(state, tid);
    if last_u > 0 && to_unix > last_u {
        if let Some(val) = state.colony_extraction_pressure.get_mut(&tid) {
            let windows = windows_by_type
                .get(&tid)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let idle_secs = uncovered_idle_secs(windows, last_u, to_unix);
            if idle_secs > 0 {
                *val *= 0.5_f64.powf((idle_secs as f64) / PI_DEPLETION_RECOVERY_HALF_LIFE_SECS);
            }
            if *val < 1.0 {
                state.colony_extraction_pressure.remove(&tid);
            }
        }
    }
    if state.colony_extraction_pressure.contains_key(&tid) {
        let entry = state
            .colony_pressure_last_unix_by_type
            .entry(tid)
            .or_insert(last_u);
        *entry = (*entry).max(to_unix);
    } else {
        state.colony_pressure_last_unix_by_type.remove(&tid);
    }
}

/// Decays every entry in `colony_extraction_pressure` independently against its own
/// per-resource active windows and advances `colony_pressure_last_unix` / `_by_type` to `now_unix`.
pub(crate) fn decay_colony_extraction_pressure_with_windows(
    state: &mut PlanetResourceDepletionState,
    windows_by_type: &HashMap<TypeId, Vec<(u64, u64)>>,
    now_unix: u64,
) {
    if now_unix == 0 {
        return;
    }
    let tids: Vec<TypeId> = state.colony_extraction_pressure.keys().copied().collect();
    for tid in tids {
        decay_colony_pressure_for_type(state, windows_by_type, tid, now_unix);
    }
    state
        .colony_pressure_last_unix_by_type
        .retain(|tid, _| state.colony_extraction_pressure.contains_key(tid));
    if state.colony_pressure_last_unix > 0 || !state.colony_extraction_pressure.is_empty() {
        state.colony_pressure_last_unix = state.colony_pressure_last_unix.max(now_unix);
    }
}

/// Applies GM depletion replenishment over wall-clock time and colony extraction pressure
/// replenishment strictly over per-resource idle intervals outside `windows_by_type`.
pub(crate) fn regenerate_planet_depletion_with_windows(
    state: &mut PlanetResourceDepletionState,
    windows_by_type: &HashMap<TypeId, Vec<(u64, u64)>>,
    now_unix: u64,
) {
    regenerate_gm_depletion(state, now_unix);
    decay_colony_extraction_pressure_with_windows(state, windows_by_type, now_unix);
}

/// Applies time-based planetary resource replenishment to `state` up to `now_unix`.
pub fn regenerate_planet_depletion(state: &mut PlanetResourceDepletionState, now_unix: u64) {
    let empty_windows = HashMap::new();
    regenerate_planet_depletion_with_windows(state, &empty_windows, now_unix);
}

/// Records newly extracted commodity quantities into unified planetary `colony_extraction_pressure`
/// without decaying mid-cycle across any active extractor window for that resource on `planet_id`.
pub fn record_planet_colony_extraction(
    sde: &SdeRegistry,
    snap: &mut WorldStateSnapshot,
    planet_id: i64,
    extracted_by_type: &HashMap<TypeId, u64>,
    now_unix: u64,
) {
    if extracted_by_type.values().all(|&q| q == 0) {
        return;
    }
    let windows_by_type =
        crate::services::industry_pi::pi_helpers::collect_planet_extractor_windows(
            sde, snap, planet_id, None,
        );
    let mut depl_state = load_planet_depletion(snap, planet_id);
    depl_state.planet_id = planet_id;
    decay_colony_extraction_pressure_with_windows(&mut depl_state, &windows_by_type, now_unix);
    for (&tid, &qty) in extracted_by_type {
        if qty > 0 {
            *depl_state.colony_extraction_pressure.entry(tid).or_insert(0.0) += qty as f64;
            if now_unix > 0 {
                let ts = depl_state
                    .colony_pressure_last_unix_by_type
                    .entry(tid)
                    .or_insert(0);
                *ts = (*ts).max(now_unix);
            }
        }
    }
    if now_unix > 0 {
        depl_state.colony_pressure_last_unix = depl_state.colony_pressure_last_unix.max(now_unix);
    }
    save_planet_depletion(snap, &depl_state);
}

fn colony_and_gm_depletion_from_state(
    depl_state: &PlanetResourceDepletionState,
    resource_tid: TypeId,
) -> (f64, f64) {
    let colony_extracted_effective = depl_state
        .colony_extraction_pressure
        .get(&resource_tid)
        .copied()
        .unwrap_or(0.0);
    let colony_depletion = (colony_extracted_effective / 100_000.0).min(0.40);
    let gm_depl = depl_state
        .resource_depletion
        .get(&resource_tid)
        .copied()
        .unwrap_or(0.0)
        .max(depl_state.gm_depletion_rate);
    (colony_depletion, gm_depl)
}

fn global_colony_and_gm_depletion_at(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    now_unix: u64,
) -> (f64, f64, PlanetResourceDepletionState) {
    let windows = crate::services::industry_pi::pi_helpers::collect_planet_extractor_windows(
        sde, snap, planet_id, None,
    );
    let mut depl_state = load_planet_depletion(snap, planet_id);
    regenerate_planet_depletion_with_windows(&mut depl_state, &windows, now_unix);
    let (colony_depletion, gm_depl) =
        colony_and_gm_depletion_from_state(&depl_state, resource_tid);
    (colony_depletion, gm_depl, depl_state)
}

/// Computes the unified planetary resource yield multiplier `[0.15, 1.0]` directly
/// from an already-regenerated `PlanetResourceDepletionState`.
pub(crate) fn planet_resource_yield_scale_from_state(
    depl_state: &PlanetResourceDepletionState,
    resource_tid: TypeId,
) -> f64 {
    let (colony_depl, gm_depl) = colony_and_gm_depletion_from_state(depl_state, resource_tid);
    let global_depletion = (colony_depl + gm_depl).clamp(0.0, 0.85);
    (1.0 - global_depletion).clamp(0.15, 1.0)
}

/// Computes the unified planetary resource yield multiplier `[0.15, 1.0]` from
/// `colony_extraction_pressure` and GM depletion at `now_unix`, respecting active planet windows.
pub fn planet_resource_yield_scale(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    resource_tid: TypeId,
    now_unix: u64,
) -> f64 {
    let (_, _, depl_state) =
        global_colony_and_gm_depletion_at(sde, snap, planet_id, resource_tid, now_unix);
    planet_resource_yield_scale_from_state(&depl_state, resource_tid)
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
        global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, resource_tid, ctx.now_unix());
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
        global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, resource_tid, ctx.now_unix());

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

