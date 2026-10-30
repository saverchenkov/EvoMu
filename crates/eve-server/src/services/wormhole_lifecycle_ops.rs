//! Wormhole system profiles, static/wandering templates, K162 exit materialization,
//! small-ship wormhole mass/hull qualification, Thera, Standard Shattered, and Drifter systems.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_destiny::{wormhole_item_id_for_signature, AU_IN_METERS};
use eve_db::types::CosmicSignatureRecord;
use eve_destiny::{Ball, MovementMode};
use eve_proto::BridgeRpcError;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::OnceLock;

pub fn wormhole_exit_item_id_for_signature(dest_sys: SolarSystemId, sig_id: &str) -> i64 {
    wormhole_item_id_for_signature(dest_sys, &format!("K162-{sig_id}"))
}

pub fn pick_system_celestial_position(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    seed: u32,
) -> ([f64; 3], Option<i64>) {
    let celestials = ctx.sde.celestials_in_system(sys_id);
    if !celestials.is_empty() {
        let idx = (seed as usize).wrapping_mul(7) % celestials.len();
        let c = &celestials[idx];
        let pos = [
            c.position.x + 15_000.0,
            c.position.y - 10_000.0,
            c.position.z + 5_000.0,
        ];
        return (pos, Some(c.celestial_id));
    }
    let angle = ((seed % 360) as f64).to_radians();
    let r = 8.0 * AU_IN_METERS;
    ([r * angle.cos(), 1.5 * AU_IN_METERS, r * angle.sin()], None)
}

/// Computes `(effective_jump_mass_kg, propulsion_mass_addition_kg)` for `ship_id`,
/// accounting for fitted armor/hull plates (`attr 796`), active AB/MWD (`group 46`, `attr 796`),
/// and active Zero-Point Mass Entangler (`type 49099` / `group 2008`, `attr 1131` = `-80%`).
pub fn compute_effective_ship_jump_mass(
    ctx: &ServerContext,
    ship_id: i64,
    ball_mass: f64,
) -> (f64, f64) {
    let (base_hull_mass, fitted_plate_mass, prop_mass_add, entangler_mult) =
        ctx.db.with_snapshot(|snap| {
            let hull_tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
            let hull_m = ctx
                .sde
                .types
                .get(&hull_tid)
                .map(|t| t.mass)
                .unwrap_or(ball_mass);
            let mut plate_m = 0.0;
            let mut prop_m = 0.0;
            let mut mass_mult = 1.0_f64;
            for item in snap.items.values() {
                if item.location_id != ship_id || item.quantity <= 0 {
                    continue;
                }
                let Some(mt) = ctx.sde.types.get(&item.type_id) else {
                    continue;
                };
                if (mt.group_id == 2008 || item.type_id == 49_099)
                    && snap.module_active_states.contains(&item.item_id)
                {
                    let pct = item
                        .dynamic_attributes
                        .get(&1131)
                        .copied()
                        .or_else(|| mt.default_attributes.get(&1131).copied())
                        .unwrap_or(-80.0);
                    let m = if pct != 0.0 {
                        (1.0 + pct / 100.0).clamp(0.05, 1.0)
                    } else {
                        0.20
                    };
                    mass_mult = mass_mult.min(m);
                }
                let mass_add = item
                    .dynamic_attributes
                    .get(&796)
                    .copied()
                    .or_else(|| mt.default_attributes.get(&796).copied())
                    .unwrap_or(0.0);
                if mass_add <= 0.0 {
                    continue;
                }
                if mt.group_id == 46 {
                    if snap.module_active_states.contains(&item.item_id) {
                        prop_m += mass_add;
                    }
                } else if snap.module_online_states.get(&item.item_id).copied().unwrap_or(true)
                    && matches!(item.flag as u16, 11..=34)
                {
                    plate_m += mass_add;
                }
            }
            (hull_m, plate_m, prop_m, mass_mult)
        });
    if entangler_mult < 1.0 {
        let reduced_hull = (base_hull_mass + fitted_plate_mass) * entangler_mult;
        let effective = if ball_mass > 0.0 && ball_mass < base_hull_mass {
            ball_mass.min(reduced_hull)
        } else {
            reduced_hull
        };
        return (effective, 0.0);
    }
    let base_total = if ball_mass > 0.0 {
        ball_mass.max(base_hull_mass + fitted_plate_mass)
    } else {
        base_hull_mass + fitted_plate_mass
    };
    let effective_mass =
        if prop_mass_add > 0.0 && ball_mass <= base_hull_mass + fitted_plate_mass + 1.0 {
            base_total + prop_mass_add
        } else {
            base_total
        };
    (effective_mass, prop_mass_add)
}

/// Returns `true` if a wormhole or either endpoint is a C13 small-ship shattered wormhole (`<= 5,000,000 kg`).
pub fn is_small_ship_wormhole(
    wormhole_type_id: Option<u32>,
    max_jump_mass_kg: f64,
    origin_class: u8,
    dest_class: u8,
) -> bool {
    origin_class == 13
        || dest_class == 13
        || matches!(wormhole_type_id, Some(34_134..=34_140 | 34_439))
        || (max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 5_000_000.0)
}

/// Validates small-ship wormhole admission (`feat:navigation.small-ship-shattered-wormholes`):
/// admits Frigates, Destroyers, Porpoise (`42244`), and mass-qualified HIC (`group 894`) / Odysseus (`89607` / `group 4902`)
/// whose effective jump mass is `<= 5,000,000 kg`.
pub fn check_small_ship_wormhole_eligibility(
    sde: &eve_sde::SdeRegistry,
    ship_type_id: u32,
    effective_mass_kg: f64,
) -> Result<(), BridgeRpcError> {
    if effective_mass_kg > 5_000_000.0 {
        return Err(BridgeRpcError {
            code: "ShipTooHeavyForWormhole".into(),
            message: format!(
                "Ship mass {:.1} kg exceeds small-ship wormhole limit 5000000.0 kg",
                effective_mass_kg
            ),
        });
    }
    let ship_group_id = sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let is_frigate_or_small = matches!(
        ship_group_id,
        25 | 31 | 237 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527
    );
    let is_destroyer = matches!(ship_group_id, 420 | 541 | 1305 | 1534);
    let is_porpoise = ship_type_id == 42_244 || ship_group_id == 941;
    let is_hic_or_odysseus =
        ship_group_id == 894 || ship_group_id == 4902 || ship_type_id == 89_607;
    if is_frigate_or_small || is_destroyer || is_porpoise || is_hic_or_odysseus {
        Ok(())
    } else {
        Err(BridgeRpcError {
            code: "ShipTooHeavyForWormhole".into(),
            message: format!(
                "Ship type {ship_type_id} (group {ship_group_id}) is not permitted through small-ship wormholes"
            ),
        })
    }
}

static WORMHOLE_PROFILES_CACHE: OnceLock<HashMap<SolarSystemId, (u8, bool, bool, &'static str)>> =
    OnceLock::new();

fn compute_single_system_profile(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
) -> (u8, bool, bool, &'static str) {
    let Some(sys) = sde.solar_systems.get(&sys_id) else {
        return match sys_id {
            31_000_001 => (14, false, true, "None"),
            31_000_002 => (15, false, true, "None"),
            31_000_003 => (16, false, true, "None"),
            31_000_004 => (17, false, true, "None"),
            31_000_005 => (12, true, true, "None"),
            31_000_006 => (18, false, true, "None"),
            31_002_580..=31_002_604 => (13, false, true, "Wolf-Rayet"),
            31_002_505..=31_002_579 => {
                let c = ((sys_id - 31_002_505) % 6) as u8 + 1;
                (c, false, true, "None")
            }
            31_000_007..32_000_000 => {
                let c = ((sys_id - 31_000_001) % 6) as u8 + 1;
                (c, false, false, "None")
            }
            _ => (0, false, false, "None"),
        };
    };
    let sde_class = sys
        .wormhole_class_id
        .or_else(|| {
            sde.constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.wormhole_class_id)
                .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.wormhole_class_id))
        })
        .unwrap_or(0);
    let wh_class = match sde_class {
        c @ (1..=6 | 12..=18 | 25) => c as u8,
        _ => 0,
    };
    if wh_class == 0 {
        return (0, false, false, "None");
    }
    let is_thera = sys_id == 31_000_005 || wh_class == 12;
    let celestials = sde.celestials_in_system(sys_id);
    let has_moons = celestials.iter().any(|c| c.group_id == 8);
    let is_standard_shattered = (1..=6).contains(&wh_class)
        && if !celestials.is_empty() {
            !has_moons
        } else {
            (31_002_505..=31_002_579).contains(&sys_id)
        };
    let is_shattered = is_thera || (13..=18).contains(&wh_class) || is_standard_shattered;
    let env_effect = eve_dogma::resolve_system_wormhole_phenomenon(sde, sys_id, None)
        .map(|(_, _, name, _)| name)
        .unwrap_or("None");
    (wh_class, is_thera, is_shattered, env_effect)
}

fn build_wormhole_profiles_map(
    sde: &eve_sde::SdeRegistry,
) -> HashMap<SolarSystemId, (u8, bool, bool, &'static str)> {
    let mut map = HashMap::new();
    for &sys_id in sde.solar_systems.keys() {
        let prof = compute_single_system_profile(sde, sys_id);
        if prof.0 > 0 {
            map.insert(sys_id, prof);
        }
    }
    map
}

pub fn system_wormhole_profile_sde(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
) -> (u8, bool, bool, &'static str) {
    if let Some(map) = WORMHOLE_PROFILES_CACHE.get() {
        return map.get(&sys_id).copied().unwrap_or((0, false, false, "None"));
    }
    if sde.solar_systems.len() > 100 {
        let map = WORMHOLE_PROFILES_CACHE.get_or_init(|| build_wormhole_profiles_map(sde));
        return map.get(&sys_id).copied().unwrap_or((0, false, false, "None"));
    }
    compute_single_system_profile(sde, sys_id)
}

pub fn system_wormhole_profile(sys_id: SolarSystemId) -> (u8, bool, bool, &'static str) {
    if let Some(map) = WORMHOLE_PROFILES_CACHE.get() {
        return map.get(&sys_id).copied().unwrap_or((0, false, false, "None"));
    }
    let sde = eve_sde::SdeRegistry::with_canonical_seed();
    compute_single_system_profile(&sde, sys_id)
}

pub fn is_drifter_system(sys_id: SolarSystemId) -> bool {
    matches!(
        sys_id,
        31_000_001 | 31_000_002 | 31_000_003 | 31_000_004 | 31_000_006
    ) || (14..=18).contains(&system_wormhole_profile(sys_id).0)
}

pub fn is_drifter_wormhole(sde: &eve_sde::SdeRegistry, sig: &CosmicSignatureRecord) -> bool {
    let origin_c = system_wormhole_profile_sde(sde, sig.solar_system_id).0;
    let dest_c = sig
        .destination_system_id
        .map(|d| system_wormhole_profile_sde(sde, d).0)
        .unwrap_or(0);
    (14..=18).contains(&origin_c)
        || (14..=18).contains(&dest_c)
        || matches!(
            sig.wormhole_type_id,
            Some(34_494 | 35_650..=35_654)
        )
        || sig.type_name.contains("Unidentified Wormhole")
}

pub fn drifter_kspace_wh_type_for_class(drifter_class: u8) -> u32 {
    match drifter_class {
        14 => 35_650, // Wormhole S877 (Sentinel MZ)
        15 => 35_651, // Wormhole B735 (Liberated Barbican)
        16 => 35_652, // Wormhole V928 (Sanctified Vidette)
        17 => 35_653, // Wormhole C414 (Conflux Eyrie)
        18 => 35_654, // Wormhole R259 (Azdaja Redoubt)
        _ => 35_650,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_wh_sig(
    signature_id: String,
    solar_system_id: SolarSystemId,
    type_name: String,
    pos_au: [f64; 3],
    wormhole_type_id: u32,
    destination_system_id: SolarSystemId,
    created_unix: u64,
    lifetime_seconds: u64,
    max_mass_kg: f64,
    max_jump_mass_kg: f64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: "Wormhole".into(),
        type_name,
        position: [
            pos_au[0] * AU_IN_METERS,
            pos_au[1] * AU_IN_METERS,
            pos_au[2] * AU_IN_METERS,
        ],
        signal_strength: 0.0,
        wormhole_type_id: Some(wormhole_type_id),
        destination_system_id: Some(destination_system_id),
        created_unix,
        lifetime_seconds,
        max_mass_kg,
        remaining_mass_kg: max_mass_kg,
        max_jump_mass_kg,
        collapsed: false,
    }
}

pub(crate) fn make_site_sig(
    signature_id: String,
    solar_system_id: SolarSystemId,
    sig_group: &str,
    type_name: &str,
    pos_au: [f64; 3],
    created_unix: u64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: sig_group.into(),
        type_name: type_name.into(),
        position: [
            pos_au[0] * AU_IN_METERS,
            pos_au[1] * AU_IN_METERS,
            pos_au[2] * AU_IN_METERS,
        ],
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    }
}

/// Selects a real SDE wormhole type from `sde.wormhole_types` matching `(origin_class, target_class)`,
/// excluding `K162` (`30831`/`26272`) and QA types (`32894`/`32895`), indexed deterministically by `seed`.
pub fn lookup_wormhole_type_for_origin_and_target(
    sde: &eve_sde::SdeRegistry,
    origin_class: u8,
    target_class: i32,
    seed: u32,
) -> u32 {
    let mut cands: Vec<u32> = sde
        .wormhole_types
        .values()
        .filter(|w| {
            if matches!(w.type_id, 30_831 | 26_272 | 32_894 | 32_895) || w.name.contains("QA ") {
                return false;
            }
            if w.target_system_class != target_class {
                return false;
            }
            let tid = w.type_id;
            match origin_class {
                13 => w.max_jump_mass > 0.0 && w.max_jump_mass <= 5_000_000.0,
                1 => (30_660..=30_668).contains(&tid),
                2 => (30_671..=30_679).contains(&tid),
                3 => (30_680..=30_685).contains(&tid) || matches!(tid, 30_695 | 30_687 | 30_688),
                4 => (30_689..=30_694).contains(&tid) || matches!(tid, 30_686 | 30_696 | 30_697),
                5 => (30_698..=30_706).contains(&tid),
                6 => (30_707..=30_715).contains(&tid),
                _ => (30_579..=30_659).contains(&tid) && w.max_jump_mass > 5_000_000.0,
            }
        })
        .map(|w| w.type_id)
        .collect();
    if cands.is_empty() {
        cands = sde
            .wormhole_types
            .values()
            .filter(|w| {
                !matches!(w.type_id, 30_831 | 26_272 | 32_894 | 32_895)
                    && !w.name.contains("QA ")
                    && w.target_system_class == target_class
                    && (origin_class == 13 || w.max_jump_mass > 5_000_000.0)
            })
            .map(|w| w.type_id)
            .collect();
    }
    cands.sort_unstable();
    if cands.is_empty() {
        30_579
    } else {
        cands[(seed as usize) % cands.len()]
    }
}

pub fn lookup_wormhole_type_for_target_class(sde: &eve_sde::SdeRegistry, target_class: i32) -> u32 {
    lookup_wormhole_type_for_origin_and_target(sde, 0, target_class, target_class as u32)
}

pub fn small_ship_wormhole_type_for_target_class(target_class: i32) -> u32 {
    match target_class {
        1 => 34_134, // Wormhole E004
        2 => 34_135, // Wormhole L005
        3 => 34_136, // Wormhole Z006
        4 => 34_137, // Wormhole M001
        5 => 34_138, // Wormhole C008
        6 => 34_139, // Wormhole G008
        9 => 34_140, // Wormhole Q003
        _ => 34_439, // Wormhole A009
    }
}

pub fn pick_destination_system_for_target_class_excluding(
    sde: &eve_sde::SdeRegistry,
    source_sys_id: SolarSystemId,
    target_class: i32,
    seq: u32,
    exclude_sys_id: Option<SolarSystemId>,
) -> SolarSystemId {
    let matches_class = |s: &eve_sde::SolarSystem| -> bool {
        s.solar_system_id != source_sys_id
            && match target_class {
                7 => {
                    s.solar_system_id < 31_000_000
                        && s.solar_system_id != 30_100_000
                        && s.region_id != 10_000_070
                        && s.security_status >= 0.45
                }
                8 => {
                    s.solar_system_id < 31_000_000
                        && s.solar_system_id != 30_100_000
                        && s.region_id != 10_000_070
                        && s.security_status > 0.0
                        && s.security_status < 0.45
                }
                9 => {
                    s.solar_system_id < 31_000_000
                        && s.solar_system_id != 30_100_000
                        && !matches!(
                            s.region_id,
                            10_000_070 | 10_000_004 | 10_000_017 | 10_000_019 | 10_001_000
                        )
                        && s.security_status <= 0.0
                }
                12 => s.solar_system_id == 31_000_005 || s.wormhole_class_id == Some(12),
                25 => s.region_id == 10_000_070,
                c => s.solar_system_id >= 31_000_000 && s.wormhole_class_id == Some(c),
            }
    };
    let mut candidates: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| matches_class(s) && Some(s.solar_system_id) != exclude_sys_id)
        .map(|s| s.solar_system_id)
        .collect();
    if candidates.is_empty() {
        candidates = sde
            .solar_systems
            .values()
            .filter(|s| matches_class(s))
            .map(|s| s.solar_system_id)
            .collect();
    }
    candidates.sort_unstable();
    if candidates.is_empty() {
        30_000_142
    } else {
        let mix = (source_sys_id as usize)
            .wrapping_mul(2_654_435_761)
            .wrapping_add((seq as usize).wrapping_mul(40_503));
        candidates[mix % candidates.len()]
    }
}

pub fn pick_destination_system_for_target_class(
    sde: &eve_sde::SdeRegistry,
    source_sys_id: SolarSystemId,
    target_class: i32,
    seq: u32,
) -> SolarSystemId {
    pick_destination_system_for_target_class_excluding(sde, source_sys_id, target_class, seq, None)
}

fn build_wh_from_type(
    sde: &eve_sde::SdeRegistry,
    sig_id: String,
    src_sys: SolarSystemId,
    pos_au: [f64; 3],
    wh_type_id: u32,
    dest_sys: SolarSystemId,
    now_unix: u64,
    fallback_name: &str,
    fallback_life_s: u64,
    fallback_mass: f64,
    fallback_jump: f64,
) -> CosmicSignatureRecord {
    let wh_info = sde.wormhole_types.get(&wh_type_id);
    make_wh_sig(
        sig_id,
        src_sys,
        wh_info
            .map(|w| w.name.clone())
            .unwrap_or_else(|| fallback_name.into()),
        pos_au,
        wh_type_id,
        dest_sys,
        now_unix,
        wh_info
            .map(|w| (w.max_stable_time_min * 60.0) as u64)
            .filter(|&v| v > 0)
            .unwrap_or(fallback_life_s),
        wh_info
            .map(|w| w.max_stable_mass)
            .filter(|&v| v > 0.0)
            .unwrap_or(fallback_mass),
        wh_info
            .map(|w| w.max_jump_mass)
            .filter(|&v| v > 0.0)
            .unwrap_or(fallback_jump),
    )
}

fn system_spawn_hash(sys_id: SolarSystemId, salt: u32) -> u32 {
    sys_id
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(salt)
        .rotate_left(13)
        .wrapping_mul(0x85EB_CA6B)
}

fn target_class_label(target_cls: i32) -> String {
    match target_cls {
        7 => "HighSec-Static".into(),
        8 => "LowSec-Static".into(),
        9 => "NullSec-Static".into(),
        c => format!("C{c}-Static"),
    }
}

fn shattered_kspace_static_type(wh_class: u8) -> (u32, i32) {
    match wh_class {
        1 => (30_666, 7), // N110 -> High-Sec
        2 => (30_678, 8), // A239 -> Low-Sec
        3 => (30_688, 9), // K346 -> Null-Sec
        4 => (30_695, 7), // D845 (C3-type K-space static) -> High-Sec (375M jump mass)
        5 => (30_706, 9), // Z142 -> Null-Sec
        _ => (30_715, 9), // C248 -> Null-Sec
    }
}

// DERIVED: Per-constellation static wormhole slot assignments; client FSD SDE (`evomu_sde.sqlite`)
// provides the 132 wormhole types and system `wormhole_class_id`, not per-system static tables.
pub fn wspace_static_specs_for_system(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
) -> Vec<(&'static str, String, u32, i32)> {
    let (wh_class, _, is_shattered, _) = system_wormhole_profile_sde(sde, sys_id);
    let const_seed = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id as usize)
        .unwrap_or(sys_id as usize);
    let mut specs: Vec<(&'static str, String, u32, i32)> = Vec::new();
    match wh_class {
        1 | 3 => {
            let k_cls = [7, 8, 9][const_seed % 3];
            let tid = lookup_wormhole_type_for_origin_and_target(sde, wh_class, k_cls, sys_id);
            specs.push(("WST", target_class_label(k_cls), tid, k_cls));
        }
        2 => {
            // Dual statics in C2: 1 W-space static + 1 K-space static
            let (k_cls, w_cls) =
                [(7, 1), (7, 3), (7, 4), (8, 2), (9, 5), (9, 6)][const_seed % 6];
            let w_tid = lookup_wormhole_type_for_origin_and_target(sde, 2, w_cls, sys_id);
            let k_tid = lookup_wormhole_type_for_origin_and_target(sde, 2, k_cls, sys_id + 1);
            specs.push(("WST", target_class_label(w_cls), w_tid, w_cls));
            specs.push(("WST2", target_class_label(k_cls), k_tid, k_cls));
        }
        4 => {
            // Dual W-space statics of distinct classes in C4
            let (c_a, c_b) = [
                (1, 2), (1, 3), (1, 4), (2, 3), (2, 4), (2, 5),
                (3, 4), (3, 5), (3, 6), (4, 5), (4, 6), (5, 6),
            ][const_seed % 12];
            let tid_a = lookup_wormhole_type_for_origin_and_target(sde, 4, c_a, sys_id);
            let tid_b = lookup_wormhole_type_for_origin_and_target(sde, 4, c_b, sys_id + 1);
            specs.push(("WST", target_class_label(c_a), tid_a, c_a));
            specs.push(("WST2", target_class_label(c_b), tid_b, c_b));
        }
        5 | 6 => {
            let w_cls = [1, 2, 3, 4, 5, 6][const_seed % 6];
            let tid = lookup_wormhole_type_for_origin_and_target(sde, wh_class, w_cls, sys_id);
            specs.push(("WST", target_class_label(w_cls), tid, w_cls));
        }
        _ => {}
    }
    if is_shattered && (1..=6).contains(&wh_class) {
        let (k_tid, k_cls) = shattered_kspace_static_type(wh_class);
        specs.push(("WSK", "KSpace-Shattered-Static".into(), k_tid, k_cls));
    }
    specs
}

/// Returns the static wormhole descriptors `(name, code, target_class)` for `wormholeMgr.GetWormholePolarization`.
pub fn statics_for_system_sde(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
) -> Vec<(String, String, i64)> {
    let (wh_class, is_thera, _, _) = system_wormhole_profile_sde(sde, sys_id);
    if is_thera {
        return vec![
            ("HighSec".into(), "B449".into(), 7),
            ("LowSec".into(), "N944".into(), 8),
            ("NullSec".into(), "S199".into(), 9),
            ("W-Space".into(), "F135".into(), 12),
        ];
    }
    if (14..=18).contains(&wh_class) {
        return vec![
            ("C1-Static".into(), "Z647".into(), 1),
            ("C2-Static".into(), "D382".into(), 2),
            ("C3-Static".into(), "O477".into(), 3),
            ("C4-Static".into(), "Y683".into(), 4),
            ("C5-Static".into(), "N062".into(), 5),
            ("C6-Static".into(), "R474".into(), 6),
        ];
    }
    if wh_class == 13 {
        let t_cls = ((sys_id % 3) + 1) as i32;
        let tid = small_ship_wormhole_type_for_target_class(t_cls);
        let code = sde
            .wormhole_types
            .get(&tid)
            .and_then(|w| w.name.split_whitespace().last())
            .unwrap_or("E004")
            .to_string();
        return vec![("C1-3-Small-Static".into(), code, t_cls as i64)];
    }
    if (1..=6).contains(&wh_class) {
        return wspace_static_specs_for_system(sde, sys_id)
            .into_iter()
            .map(|(_, label, tid, t_cls)| {
                let code = sde
                    .wormhole_types
                    .get(&tid)
                    .and_then(|w| w.name.split_whitespace().last())
                    .unwrap_or("H296")
                    .to_string();
                (label, code, t_cls as i64)
            })
            .collect();
    }
    vec![("Wandering".into(), "Unknown".into(), 0)]
}

// DERIVED: Per-system static wormhole tables are not in the client FSD SDE (`evomu_sde.sqlite`);
// only the 132 wormhole types (`sde_wormhole_types`) and system `wormhole_class_id` are present.
pub(crate) fn static_wormhole_template(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
    seq: u32,
) -> Vec<CosmicSignatureRecord> {
    if ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return crate::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(
            &ctx.sde, sys_id, seq, now_unix,
        );
    }
    if sys_id == 31_000_005 {
        let hs_tid = lookup_wormhole_type_for_origin_and_target(&ctx.sde, 12, 7, sys_id + seq);
        let ls_tid = lookup_wormhole_type_for_origin_and_target(&ctx.sde, 12, 8, sys_id + seq);
        let ns_tid = lookup_wormhole_type_for_origin_and_target(&ctx.sde, 12, 9, sys_id + seq);
        let ws_tid = 34_372; // Wormhole F135 -> W-space
        let mut out = vec![
            build_wh_from_type(
                &ctx.sde,
                format!("THS-{}", 100 + ((sys_id + seq * 17) % 900)),
                sys_id,
                [10.0, 2.0, -3.0],
                hs_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 7, seq),
                now_unix,
                "Wormhole B449",
                57_600,
                2_000_000_000.0,
                375_000_000.0,
            ),
            build_wh_from_type(
                &ctx.sde,
                format!("TLS-{}", 200 + ((sys_id + seq * 19) % 700)),
                sys_id,
                [-9.0, 4.0, 6.0],
                ls_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 8, seq),
                now_unix,
                "Wormhole N944",
                86_400,
                3_000_000_000.0,
                375_000_000.0,
            ),
            build_wh_from_type(
                &ctx.sde,
                format!("TNS-{}", 300 + ((sys_id + seq * 23) % 600)),
                sys_id,
                [6.0, -8.0, 5.0],
                ns_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 9, seq),
                now_unix,
                "Wormhole S199",
                57_600,
                3_000_000_000.0,
                375_000_000.0,
            ),
            build_wh_from_type(
                &ctx.sde,
                format!("TWS-{}", 400 + ((sys_id + seq * 29) % 500)),
                sys_id,
                [-5.0, -6.0, -7.0],
                ws_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 3, seq + 2),
                now_unix,
                "Wormhole F135",
                57_600,
                750_000_000.0,
                375_000_000.0,
            ),
        ];
        for w in &mut out {
            w.max_jump_mass_kg = w.max_jump_mass_kg.min(375_000_000.0);
        }
        return out;
    }

    let (wh_class, _, _, _) = system_wormhole_profile_sde(&ctx.sde, sys_id);

    // Drifter wormhole systems (`31000001..=31000004`, `31000006`):
    // 6 statics to C1-C6 (`Z647`, `D382`, `O477`, `Y683`, `N062`, `R474`) + 56 K-space connections (`50-70`).
    if (14..=18).contains(&wh_class) {
        let mut out = Vec::new();
        let drifter_statics: [(u32, i32, &str, [f64; 3]); 6] = [
            (30_671, 1, "Wormhole Z647", [12.0, 1.0, -2.0]),
            (30_672, 2, "Wormhole D382", [-11.0, 2.0, 3.0]),
            (30_673, 3, "Wormhole O477", [9.0, -3.0, 4.0]),
            (30_674, 4, "Wormhole Y683", [-8.0, -4.0, -5.0]),
            (30_675, 5, "Wormhole N062", [7.0, 5.0, 6.0]),
            (30_676, 6, "Wormhole R474", [-6.0, 6.0, -7.0]),
        ];
        for (idx, (tid, target_cls, fname, pos_au)) in drifter_statics.iter().enumerate() {
            let dest = pick_destination_system_for_target_class(
                &ctx.sde,
                sys_id,
                *target_cls,
                seq + idx as u32,
            );
            let mut sig = build_wh_from_type(
                &ctx.sde,
                format!("WST{}-{}", idx + 1, 100 + ((sys_id + seq * 17 + idx as u32) % 900)),
                sys_id,
                *pos_au,
                *tid,
                dest,
                now_unix,
                fname,
                57_600,
                2_000_000_000.0,
                375_000_000.0,
            );
            if seq > 0 {
                let (cel_pos, _) = pick_system_celestial_position(ctx, sys_id, seq + idx as u32);
                sig.position = cel_pos;
            }
            out.push(sig);
        }
        if seq == 0 {
            let kspace_tid = drifter_kspace_wh_type_for_class(wh_class);
            for k_idx in 0..56_u32 {
                let k_cls = match k_idx % 3 {
                    0 => 7,
                    1 => 8,
                    _ => 9,
                };
                let k_dest = pick_destination_system_for_target_class(
                    &ctx.sde,
                    sys_id,
                    k_cls,
                    k_idx.wrapping_mul(13).wrapping_add(sys_id % 97),
                );
                let angle = ((k_idx as f64) * 6.4).to_radians();
                let pos_au = [14.0 * angle.cos(), ((k_idx % 7) as f64) - 3.0, 14.0 * angle.sin()];
                let mut k_sig = build_wh_from_type(
                    &ctx.sde,
                    format!("DRK-{:03}", 100 + k_idx),
                    sys_id,
                    pos_au,
                    kspace_tid,
                    k_dest,
                    now_unix,
                    "Unidentified Wormhole",
                    57_600,
                    750_000_000.0,
                    375_000_000.0,
                );
                k_sig.type_name = "Unidentified Wormhole".into();
                out.push(k_sig);
            }
        }
        return out;
    }

    // Small-ship shattered wormhole systems (`J000xxx`, `wh_class == 13`):
    // Only connect through small-ship wormholes (`<= 5,000,000 kg`, `E004`, `L005`, `Z006`, `A009`).
    if wh_class == 13 {
        let mut out = Vec::new();
        let target_cls = ((sys_id % 3) + 1) as i32;
        let static_tid = small_ship_wormhole_type_for_target_class(target_cls);
        let dest_sys = pick_destination_system_for_target_class(&ctx.sde, sys_id, target_cls, seq);
        if seq == 0 && (system_spawn_hash(sys_id, 0x0D) % 100) < 40 {
            let wnd_cls = (((sys_id + 1) % 3) + 1) as i32;
            let wnd_tid = small_ship_wormhole_type_for_target_class(wnd_cls);
            let wnd_dest =
                pick_destination_system_for_target_class(&ctx.sde, sys_id, wnd_cls, seq + 1);
            let mut wnd = build_wh_from_type(
                &ctx.sde,
                format!("WND-{}", 200 + ((sys_id + 13) % 700)),
                sys_id,
                [-8.0, 5.0, 3.0],
                wnd_tid,
                wnd_dest,
                now_unix,
                "Wormhole L005",
                16_200,
                3_000_000_000.0,
                5_000_000.0,
            );
            wnd.max_jump_mass_kg = 5_000_000.0;
            out.push(wnd);
        }
        let mut wst = build_wh_from_type(
            &ctx.sde,
            format!("WST-{}", 100 + ((sys_id + seq * 17) % 900)),
            sys_id,
            [12.0, 2.5, -4.0],
            static_tid,
            dest_sys,
            now_unix,
            "Wormhole E004",
            16_200,
            3_000_000_000.0,
            5_000_000.0,
        );
        wst.max_jump_mass_kg = 5_000_000.0;
        if seq > 0 {
            let (cel_pos, _) = pick_system_celestial_position(ctx, sys_id, seq);
            wst.position = cel_pos;
        }
        out.push(wst);
        return out;
    }

    if (31_000_000..=31_002_605).contains(&sys_id) {
        // DERIVED: per-constellation statics (`WST`, `WST2`, `WSK`) and probabilistic wandering connections
        let mut out = Vec::new();
        let slot_positions: [[f64; 3]; 3] = [
            [12.0, 2.5, -4.0],
            [-10.5, 4.0, -7.5],
            [-12.0, -3.5, 8.0],
        ];
        for (slot_idx, (prefix, _, wh_tid, target_cls)) in
            wspace_static_specs_for_system(&ctx.sde, sys_id)
                .into_iter()
                .enumerate()
        {
            let dest_sys = pick_destination_system_for_target_class(
                &ctx.sde,
                sys_id,
                target_cls,
                seq.wrapping_add(slot_idx as u32),
            );
            let mut wst = build_wh_from_type(
                &ctx.sde,
                format!(
                    "{prefix}-{}",
                    100 + ((sys_id + seq * 17 + (slot_idx as u32) * 31) % 900)
                ),
                sys_id,
                slot_positions[slot_idx.min(2)],
                wh_tid,
                dest_sys,
                now_unix,
                "Unstable Wormhole",
                86_400,
                2_000_000_000.0,
                375_000_000.0,
            );
            if seq > 0 {
                let (cel_pos, _) =
                    pick_system_celestial_position(ctx, sys_id, seq + slot_idx as u32);
                wst.position = cel_pos;
            }
            out.push(wst);
        }
        if seq == 0 {
            if (system_spawn_hash(sys_id, 0x11) % 100) < 35 {
                let wnd_cls = [1, 2, 3, 4, 5, 6, 7, 8, 9][((sys_id as usize) + 5) % 9];
                let wnd_tid = lookup_wormhole_type_for_origin_and_target(
                    &ctx.sde,
                    wh_class,
                    wnd_cls,
                    sys_id.wrapping_add(3),
                );
                let wnd_dest =
                    pick_destination_system_for_target_class(&ctx.sde, sys_id, wnd_cls, 3);
                out.push(build_wh_from_type(
                    &ctx.sde,
                    format!("WND-{}", 200 + ((sys_id + 13) % 700)),
                    sys_id,
                    [-8.0, 5.0, 3.0],
                    wnd_tid,
                    wnd_dest,
                    now_unix,
                    "Wandering Wormhole",
                    57_600,
                    2_000_000_000.0,
                    375_000_000.0,
                ));
            }
            if matches!(wh_class, 2..=4) && (system_spawn_hash(sys_id, 0x22) % 100) < 12 {
                let poch_dest =
                    pick_destination_system_for_target_class(&ctx.sde, sys_id, 25, seq);
                out.push(build_wh_from_type(
                    &ctx.sde,
                    format!("WPC-{}", 600 + ((sys_id + 31) % 300)),
                    sys_id,
                    [6.5, -7.0, 9.0],
                    56_541,
                    poch_dest,
                    now_unix,
                    "Wormhole R081",
                    43_200,
                    1_000_000_000.0,
                    375_000_000.0,
                ));
            }
            if (system_spawn_hash(sys_id, 0x33) % 100) < 8 {
                out.push(build_wh_from_type(
                    &ctx.sde,
                    format!("WTH-{}", 700 + ((sys_id + 47) % 290)),
                    sys_id,
                    [-4.5, 8.0, -6.5],
                    34_372,
                    31_000_005,
                    now_unix,
                    "Wormhole F135",
                    57_600,
                    750_000_000.0,
                    375_000_000.0,
                ));
            }
        }
        out
    } else {
        // K-space wormholes: probabilistic wandering, rare Thera K162, and proximity-only Pochven C729 (`<= 5.0 LY`)
        let mut out = Vec::new();
        if seq > 0 || (system_spawn_hash(sys_id, 0x44) % 100) < 8 {
            let sig_prefix = if seq == 0 { "JMM" } else { "STA" };
            let mut wh = make_wh_sig(
                format!("{sig_prefix}-{}", 100 + ((sys_id + seq * 37) % 900)),
                sys_id,
                "Unstable Wormhole (K162)".into(),
                [12.0, 2.5, -4.0],
                26_272,
                31_000_005,
                now_unix,
                86_400,
                2_000_000_000.0,
                375_000_000.0,
            );
            if seq > 0 {
                let (cel_pos, _) = pick_system_celestial_position(ctx, sys_id, seq);
                wh.position = cel_pos;
            }
            out.push(wh);
        }
        if seq == 0 {
            if let Some(poch_dest) =
                crate::services::space_destiny::space_pochven_wormholes::nearest_pochven_system_within_ly(
                    &ctx.sde, sys_id, 5.0, 0,
                )
            {
                if (system_spawn_hash(sys_id, 0x55) % 100) < 35 {
                    out.push(build_wh_from_type(
                        &ctx.sde,
                        format!("PKW-{}", 500 + ((sys_id + 41) % 400)),
                        sys_id,
                        [14.0, -2.0, 6.0],
                        56_026,
                        poch_dest,
                        now_unix,
                        "Wormhole C729",
                        43_200,
                        1_000_000_000.0,
                        410_000_000.0,
                    ));
                }
            }
            if (system_spawn_hash(sys_id, 0x66) % 100) < 25 {
                let wnd_cls = [1, 2, 3, 4, 5, 6][(sys_id as usize) % 6];
                let wnd_tid =
                    lookup_wormhole_type_for_origin_and_target(&ctx.sde, 0, wnd_cls, sys_id);
                let wnd_dest =
                    pick_destination_system_for_target_class(&ctx.sde, sys_id, wnd_cls, 1);
                out.push(build_wh_from_type(
                    &ctx.sde,
                    format!("WND-{}", 300 + ((sys_id + 19) % 600)),
                    sys_id,
                    [-9.5, 4.5, -2.0],
                    wnd_tid,
                    wnd_dest,
                    now_unix,
                    "Wandering Wormhole",
                    57_600,
                    2_000_000_000.0,
                    375_000_000.0,
                ));
            }
        }
        out
    }
}

pub fn build_static_replacement_wormhole(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    collapsed_wh: &CosmicSignatureRecord,
    now_unix: u64,
    seq: u32,
) -> Option<CosmicSignatureRecord> {
    let prefix = collapsed_wh
        .signature_id
        .split('-')
        .next()
        .unwrap_or("WST");
    if matches!(prefix, "WND" | "K162" | "WTH" | "WPC" | "PKW") {
        return None;
    }
    let mut eff_seq = seq.max(1);
    let find_matching = |s_seq: u32| -> Option<CosmicSignatureRecord> {
        let tpl = static_wormhole_template(ctx, sys_id, now_unix, s_seq);
        tpl.iter()
            .find(|s| s.signature_id.split('-').next() == Some(prefix))
            .cloned()
            .or_else(|| tpl.into_iter().next())
    };
    let mut rep = find_matching(eff_seq)?;
    if rep.signature_id == collapsed_wh.signature_id {
        eff_seq += 1;
        rep = find_matching(eff_seq)?;
    }
    let (cel_pos, _) = pick_system_celestial_position(ctx, sys_id, eff_seq);
    rep.position = cel_pos;
    if let Some(wtid) = collapsed_wh.wormhole_type_id {
        rep.wormhole_type_id = Some(wtid);
        if let Some(wh_info) = ctx.sde.wormhole_types.get(&wtid) {
            rep.type_name = wh_info.name.clone();
            rep.lifetime_seconds = (wh_info.max_stable_time_min * 60.0) as u64;
            rep.max_mass_kg = wh_info.max_stable_mass;
            rep.remaining_mass_kg = wh_info.max_stable_mass;
            if wh_info.max_jump_mass > 0.0 {
                rep.max_jump_mass_kg = wh_info.max_jump_mass;
            }
        } else {
            rep.type_name = collapsed_wh.type_name.clone();
        }
    }
    // Roll a new destination solar system of the static's target class (`!= collapsed_wh.destination_system_id`).
    let prev_dest = collapsed_wh.destination_system_id;
    let is_pochven = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    if is_pochven && prefix == "PCH" {
        let mut cands: Vec<SolarSystemId> =
            crate::services::space_destiny::space_pochven_wormholes::pochven_kspace_neighbors_within_ly(
                &ctx.sde, sys_id, 5.0,
            )
            .into_iter()
            .filter(|&d| Some(d) != prev_dest)
            .collect();
        cands.sort_unstable();
        if !cands.is_empty() {
            rep.destination_system_id = Some(cands[(eff_seq as usize) % cands.len()]);
        }
    } else {
        let target_cls_opt = rep
            .wormhole_type_id
            .and_then(|wtid| ctx.sde.wormhole_types.get(&wtid))
            .map(|w| w.target_system_class)
            .or_else(|| {
                prev_dest
                    .and_then(|d| ctx.sde.solar_systems.get(&d))
                    .map(|s| {
                        if let Some(wc) = s.wormhole_class_id {
                            wc
                        } else if s.security_status >= 0.45 {
                            7
                        } else if s.security_status > 0.0 {
                            8
                        } else {
                            9
                        }
                    })
            });
        if let Some(t_cls) = target_cls_opt {
            if t_cls != 12 {
                rep.destination_system_id = Some(pick_destination_system_for_target_class_excluding(
                    &ctx.sde, sys_id, t_cls, eff_seq, prev_dest,
                ));
            }
        }
    }
    Some(rep)
}

/// Remaining lifetime threshold (`< 15h` = `54,000 s`) below which an unmaterialized destination
/// `K162` exit signature & wormhole ball automatically materialize during periodic sweeps.
pub const K162_MATERIALIZE_REMAINING_SECS: u64 = 15 * 3_600;

/// Returns `true` if `sig` is an uncollapsed non-`K162` wormhole whose remaining lifetime has
/// dropped below `15h` (`54,000 s`) prior to full expiration.
pub fn should_materialize_k162_by_lifetime(sig: &CosmicSignatureRecord, now_unix: u64) -> bool {
    sig.sig_group == "Wormhole"
        && !sig.collapsed
        && !sig.signature_id.starts_with("K162-")
        && sig.lifetime_seconds > 0
        && now_unix < sig.created_unix.saturating_add(sig.lifetime_seconds)
        && sig.lifetime_seconds.saturating_sub(now_unix.saturating_sub(sig.created_unix))
            < K162_MATERIALIZE_REMAINING_SECS
}

/// Returns `true` if the destination K162 signature (`K162-<orig_sig_id>`) has materialized in `dest_sys`.
pub fn is_k162_materialized_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    dest_sys: SolarSystemId,
    orig_sig_id: &str,
) -> bool {
    let k162_id = format!("K162-{orig_sig_id}");
    snap.cosmic_signatures
        .get(&dest_sys)
        .is_some_and(|list| list.iter().any(|s| s.signature_id == k162_id && !s.collapsed))
}

/// Materializes `K162-<orig_sig_id>` in `dest_sys` when a ship first jumps through `parent_sig` or
/// when remaining lifetime drops below `15h` (`54,000 s`) (`feat:navigation.wormhole-k162-exit-materialization`).
/// Returns `(exit_ball_id, sig_record, ball_pos)` if newly materialized.
pub fn materialize_k162_exit_in_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    parent_sig: &CosmicSignatureRecord,
    dest_sys: SolarSystemId,
) -> Option<(i64, CosmicSignatureRecord, DVec3)> {
    let k162_sig_id = format!("K162-{}", parent_sig.signature_id);
    let d_list = snap.cosmic_signatures.entry(dest_sys).or_default();
    if d_list.iter().any(|s| s.signature_id == k162_sig_id) {
        return None;
    }
    let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &parent_sig.signature_id);
    let (exit_pos, _) =
        pick_system_celestial_position(ctx, dest_sys, (exit_id as u32).wrapping_add(1));
    let rec = CosmicSignatureRecord {
        signature_id: k162_sig_id,
        solar_system_id: dest_sys,
        sig_group: "Wormhole".into(),
        type_name: "Unstable Wormhole (K162)".into(),
        position: exit_pos,
        signal_strength: 0.0,
        wormhole_type_id: Some(26_272),
        destination_system_id: Some(parent_sig.solar_system_id),
        created_unix: parent_sig.created_unix,
        lifetime_seconds: parent_sig.lifetime_seconds,
        max_mass_kg: parent_sig.max_mass_kg,
        remaining_mass_kg: parent_sig.remaining_mass_kg,
        max_jump_mass_kg: parent_sig.max_jump_mass_kg,
        collapsed: parent_sig.collapsed,
    };
    d_list.push(rec.clone());
    Some((exit_id, rec, DVec3::from_array(exit_pos)))
}

/// Synchronizes `K162-<orig_sig_id>` in `dest_sys` whenever the parent wormhole changes mass or collapses.
pub fn sync_k162_mirror_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    parent_sig: &CosmicSignatureRecord,
) {
    let Some(dest_sys) = parent_sig.destination_system_id else {
        return;
    };
    let k162_sig_id = format!("K162-{}", parent_sig.signature_id);
    if let Some(d_list) = snap.cosmic_signatures.get_mut(&dest_sys) {
        if let Some(k) = d_list.iter_mut().find(|s| s.signature_id == k162_sig_id) {
            k.remaining_mass_kg = parent_sig.remaining_mass_kg;
            k.collapsed = parent_sig.collapsed;
        }
    }
}

/// Seeds K-space side `"Unidentified Wormhole"` beacons (`signal_strength = 1.0`, no scanning required)
/// for Drifter system K-space connections (`feat:navigation.drifter-wormhole-systems`).
pub fn seed_drifter_kspace_beacons_in_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    drifter_sys_id: SolarSystemId,
    drifter_sigs: &[CosmicSignatureRecord],
) {
    if !is_drifter_system(drifter_sys_id) {
        return;
    }
    for sig in drifter_sigs {
        if !sig.signature_id.starts_with("DRK-") {
            continue;
        }
        let Some(k_sys) = sig.destination_system_id else {
            continue;
        };
        if system_wormhole_profile_sde(&ctx.sde, k_sys).0 != 0 {
            continue;
        }
        let k162_sig_id = format!("K162-{}", sig.signature_id);
        let k_list = snap.cosmic_signatures.entry(k_sys).or_default();
        if k_list.iter().any(|s| s.signature_id == k162_sig_id) {
            continue;
        }
        let exit_id = wormhole_exit_item_id_for_signature(k_sys, &sig.signature_id);
        let (exit_pos, _) =
            pick_system_celestial_position(ctx, k_sys, (exit_id as u32).wrapping_add(1));
        k_list.push(CosmicSignatureRecord {
            signature_id: k162_sig_id,
            solar_system_id: k_sys,
            sig_group: "Wormhole".into(),
            type_name: "Unidentified Wormhole".into(),
            position: exit_pos,
            signal_strength: 1.0,
            wormhole_type_id: sig.wormhole_type_id.or(Some(34_494)),
            destination_system_id: Some(drifter_sys_id),
            created_unix: sig.created_unix,
            lifetime_seconds: sig.lifetime_seconds,
            max_mass_kg: sig.max_mass_kg,
            remaining_mass_kg: sig.remaining_mass_kg,
            max_jump_mass_kg: sig.max_jump_mass_kg,
            collapsed: false,
        });
    }
}

/// Ensures Drifter Battleship (`34495` / `34561`) guards are present on the K-space side
/// of any Drifter wormhole beacon (`feat:navigation.drifter-wormhole-systems`).
pub async fn ensure_drifter_kspace_guards(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    sig: &CosmicSignatureRecord,
    wh_ball_id: i64,
    wh_pos: DVec3,
) -> Vec<i64> {
    if system_wormhole_profile_sde(&ctx.sde, sys_id).0 != 0 || !is_drifter_wormhole(&ctx.sde, sig) {
        return Vec::new();
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut guard_ids = Vec::with_capacity(2);
    for (idx, npc_tid) in [34_495_u32, 34_561_u32].into_iter().enumerate() {
        let gid = wh_ball_id.saturating_add(10 + idx as i64);
        guard_ids.push(gid);
        if sys_handle.get_ball_snapshot(gid).await.ok().flatten().is_some() {
            continue;
        }
        let g_pos = wh_pos + DVec3::new(3_000.0 + (idx as f64) * 2_000.0, 0.0, 0.0);
        let sde_type = ctx.sde.types.get(&npc_tid);
        let attr = |aid: u16| sde_type.and_then(|t| t.default_attributes.get(&aid).copied());
        let rof_s = (attr(51).unwrap_or(5_000.0) / 1_000.0).max(1.0);
        let dmg_m = attr(64).unwrap_or(16.0) / rof_s;
        let _ = sys_handle.spawn_ball_with_combat(Ball {
            id: gid, type_id: npc_tid, position: g_pos, velocity: DVec3::ZERO,
            max_velocity: attr(37).unwrap_or(3_500.0), inertia: 1.0,
            mass: sde_type.map(|t| t.mass).unwrap_or(10_900_000.0),
            radius: sde_type.map(|t| t.radius).unwrap_or(350.0),
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }, ctx.build_type_combat_state(npc_tid)).await;
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(gid, crate::services::combat_crimewatch::ExtendedNpcEntity {
                ball_id: gid, type_id: npc_tid, faction_id: 500_024, archetype: "Drifter".into(),
                solar_system_id: sys_id, spawn_group_id: 888_014, target_id: None,
                aggression_threat: HashMap::new(), threat_table: HashMap::new(), bounty_isk: 0.0,
                optimal_range: attr(54).unwrap_or(150_000.0), orbit_range: 15_000.0,
                damage_vector: eve_destiny::DamageVector::new(
                    attr(114).unwrap_or(80.0) * dmg_m, attr(118).unwrap_or(80.0) * dmg_m,
                    attr(117).unwrap_or(80.0) * dmg_m, attr(116).unwrap_or(80.0) * dmg_m,
                ),
                entropic_cycles: 0, drifter_overshield_hp: attr(2009).unwrap_or(5_000.0),
                drifter_doomsday_fired: false,
                web_speed_factor: attr(20).filter(|&v| v < 0.0).unwrap_or(-50.0),
                web_range: attr(514).unwrap_or(150_000.0), scram_strength: attr(105).unwrap_or(5.0),
                scram_range: attr(103).unwrap_or(150_000.0), neut_amount: attr(97).unwrap_or(60.0),
                ecm_jammed: false, remote_shield_rep: 0.0, remote_armor_rep: 0.0, remote_rep_range: 0.0,
                tag_drop_type_id: None, spawn_position: g_pos, sleeper_wh_class: 0, custom_cargo_loot: None,
            });
        });
    }
    guard_ids
}
