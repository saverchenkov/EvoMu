//! Wormhole profiles, cosmic signatures/anomalies, cosmic wormhole jumps, and `scanMgr` probe scanning re-exports.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_destiny::{
    record_session_change_timer, wormhole_item_id_for_signature, AU_IN_METERS,
};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;

#[path = "space_probe_scanning.rs"]
pub mod space_probe_scanning;
pub use space_probe_scanning::*;

const BEYONCE_SVC: &str = "beyonce";

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

pub fn compute_effective_ship_jump_mass(
    ctx: &ServerContext,
    ship_id: i64,
    ball_mass: f64,
) -> (f64, f64) {
    let (base_hull_mass, fitted_plate_mass, prop_mass_add) = ctx.db.with_snapshot(|snap| {
        let hull_tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        let hull_m = ctx
            .sde
            .types
            .get(&hull_tid)
            .map(|t| t.mass)
            .unwrap_or(ball_mass);
        let mut plate_m = 0.0;
        let mut prop_m = 0.0;
        for item in snap.items.values() {
            if item.location_id != ship_id || item.quantity <= 0 {
                continue;
            }
            let Some(mt) = ctx.sde.types.get(&item.type_id) else {
                continue;
            };
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
        (hull_m, plate_m, prop_m)
    });
    let base_total = if ball_mass > 0.0 {
        ball_mass.max(base_hull_mass + fitted_plate_mass)
    } else {
        base_hull_mass + fitted_plate_mass
    };
    let effective_mass = if prop_mass_add > 0.0 && ball_mass <= base_hull_mass + fitted_plate_mass + 1.0 {
        base_total + prop_mass_add
    } else {
        base_total
    };
    (effective_mass, prop_mass_add)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WormholeBuckets {
    pub destination_class: &'static str,
    pub max_ship_size: &'static str,
    pub lifetime_bucket: &'static str,
    pub mass_stability_bucket: &'static str,
    pub lifetime_description: &'static str,
    pub mass_stability_description: &'static str,
}

// parity: feat:wormholes.mass-reduction-and-destabilization
pub fn derive_wormhole_buckets(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    derive_wormhole_buckets_raw(
        wh.destination_system_id.unwrap_or(0),
        wh.max_jump_mass_kg,
        wh.remaining_mass_kg,
        wh.max_mass_kg,
        wh.created_unix,
        wh.lifetime_seconds,
        wh.collapsed,
        now_unix,
        sde,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn derive_wormhole_buckets_raw(
    dest_sys: u32,
    max_jump_mass_kg: f64,
    remaining_mass_kg: f64,
    max_mass_kg: f64,
    created_unix: u64,
    lifetime_seconds: u64,
    collapsed: bool,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    let wh_class_id = sde.solar_systems.get(&dest_sys).and_then(|s| {
        s.wormhole_class_id.or_else(|| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.wormhole_class_id)
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.wormhole_class_id))
        })
    });

    // DERIVED: Destination classification resolved from SDE solar system, constellation, or region wormholeClassID
    let dest_class = match wh_class_id {
        Some(12) => "Thera",
        Some(14..=18) => "Drifter",
        Some(25) => "Triglavian",
        Some(1..=3 | 13) => "C1-3",
        Some(4..=5) => "C4-5",
        Some(6) => "C6",
        Some(7) => "High-Sec",
        Some(8) => "Low-Sec",
        Some(9..=11) => "Null-Sec",
        _ => {
            let sec = sde
                .solar_systems
                .get(&dest_sys)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            if sec >= 0.45 {
                "High-Sec"
            } else if sec > 0.0 {
                "Low-Sec"
            } else {
                "Null-Sec"
            }
        }
    };

    // DERIVED: Ship size category bucket thresholds derived from mass
    let max_ship_size = if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 20_000_000.0 {
        "Small"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 62_000_000.0 {
        "Medium"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 375_000_000.0 {
        "Large"
    } else if max_jump_mass_kg > 375_000_000.0 {
        "Extra Large"
    } else {
        "Large"
    };

    // DERIVED: Lifetime stage bucket intervals derived from elapsed time
    let elapsed = now_unix.saturating_sub(created_unix);
    let remaining_seconds = lifetime_seconds.saturating_sub(elapsed);
    let lifetime_bucket = if collapsed
        || lifetime_seconds == 0
        || elapsed >= lifetime_seconds
        || remaining_seconds == 0
    {
        "expired"
    } else if remaining_seconds < 3_600 {
        "<1h"
    } else if remaining_seconds < 4 * 3_600 {
        "<4h"
    } else {
        "<1 day"
    };

    // Show-info lifetime strings for the 3 remaining-lifetime bands (>24h, 4h-24h, <4h).
    let lifetime_description = if remaining_seconds > 24 * 3_600 && !collapsed {
        "This wormhole is not yet beginning to decay"
    } else if remaining_seconds >= 4 * 3_600 && !collapsed {
        "This wormhole is beginning to decay, and probably won't last another day"
    } else {
        "This wormhole is reaching the end of its natural lifetime"
    };

    // DERIVED: Mass stability bucket threshold derived from mass ratio
    let mass_ratio = if max_mass_kg > 0.0 {
        remaining_mass_kg / max_mass_kg
    } else {
        0.0
    };
    let (mass_stability_bucket, mass_stability_description) =
        if remaining_mass_kg <= 0.0 || mass_ratio < 0.10 {
            (
                "<10%",
                "This wormhole has had its stability critically disrupted",
            )
        } else if mass_ratio < 0.50 {
            (
                "<50%",
                "This wormhole has had its stability reduced by ships passing through it",
            )
        } else {
            (
                ">50%",
                "This wormhole has not yet had its stability significantly disrupted",
            )
        };

    WormholeBuckets {
        destination_class: dest_class,
        max_ship_size,
        lifetime_bucket,
        mass_stability_bucket,
        lifetime_description,
        mass_stability_description,
    }
}

static WORMHOLE_PROFILES_CACHE: std::sync::OnceLock<
    std::collections::HashMap<SolarSystemId, (u8, bool, bool, &'static str)>,
> = std::sync::OnceLock::new();

fn build_wormhole_profiles_map(
    sde: &eve_sde::SdeRegistry,
) -> std::collections::HashMap<SolarSystemId, (u8, bool, bool, &'static str)> {
    let mut map = std::collections::HashMap::new();
    for (&sys_id, sys) in &sde.solar_systems {
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
            c @ (1..=6 | 12..=18) => c as u8,
            _ => 0,
        };
        if wh_class == 0 {
            continue;
        }
        let is_thera = sys_id == 31_000_005 || wh_class == 12;
        let is_shattered =
            is_thera || (13..=18).contains(&wh_class) || sys.name.starts_with("J0");
        let env_effect = eve_dogma::resolve_system_wormhole_phenomenon(sde, sys_id, None)
            .map(|(_, _, name, _)| name)
            .unwrap_or("None");
        map.insert(sys_id, (wh_class, is_thera, is_shattered, env_effect));
    }
    map
}

pub fn system_wormhole_profile_sde(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
) -> (u8, bool, bool, &'static str) {
    let map = WORMHOLE_PROFILES_CACHE.get_or_init(|| build_wormhole_profiles_map(sde));
    map.get(&sys_id).copied().unwrap_or((0, false, false, "None"))
}

pub fn system_wormhole_profile(sys_id: SolarSystemId) -> (u8, bool, bool, &'static str) {
    let map = WORMHOLE_PROFILES_CACHE.get_or_init(|| {
        let sde = eve_sde::SdeRegistry::with_canonical_seed();
        build_wormhole_profiles_map(&sde)
    });
    map.get(&sys_id).copied().unwrap_or((0, false, false, "None"))
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

fn make_site_sig(
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

fn lookup_wormhole_type_for_target_class(sde: &eve_sde::SdeRegistry, target_class: i32) -> u32 {
    sde.wormhole_types
        .values()
        .filter(|w| w.target_system_class == target_class)
        .min_by_key(|w| w.type_id)
        .map(|w| w.type_id)
        .unwrap_or(30579)
}

fn pick_destination_system_for_target_class(
    sde: &eve_sde::SdeRegistry,
    source_sys_id: SolarSystemId,
    target_class: i32,
    seq: u32,
) -> SolarSystemId {
    let mut candidates: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| {
            s.solar_system_id != source_sys_id
                && match target_class {
                    7 => s.wormhole_class_id.is_none() && s.security_status >= 0.45,
                    8 => s.wormhole_class_id.is_none() && s.security_status > 0.0 && s.security_status < 0.45,
                    9 => s.wormhole_class_id.is_none() && s.security_status <= 0.0 && s.solar_system_id < 31_000_000,
                    c => s.wormhole_class_id == Some(c),
                }
        })
        .map(|s| s.solar_system_id)
        .collect();
    candidates.sort_unstable();
    if candidates.is_empty() {
        30_000_142
    } else {
        let idx = ((source_sys_id as usize).wrapping_add(seq as usize)) % candidates.len();
        candidates[idx]
    }
}

pub use super::space_pochven_wormholes::{is_pochven_wormhole, pochven_wormhole_ball_position};

// DERIVED: Per-system static wormhole tables are not in the client FSD SDE (`evomu_sde.sqlite`);
// only the 132 wormhole types (`sde_wormhole_types`) and system `wormhole_class_id` are present.
pub(crate) fn static_wormhole_template(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
    seq: u32,
) -> Vec<CosmicSignatureRecord> {
    if ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return super::space_pochven_wormholes::pochven_static_wormholes(sys_id, seq, now_unix);
    }
    if sys_id == 31_000_005 {
        let hs_tid = lookup_wormhole_type_for_target_class(&ctx.sde, 7);
        let ls_tid = lookup_wormhole_type_for_target_class(&ctx.sde, 8);
        let ns_tid = lookup_wormhole_type_for_target_class(&ctx.sde, 9);
        let hs_wh = ctx.sde.wormhole_types.get(&hs_tid);
        let ls_wh = ctx.sde.wormhole_types.get(&ls_tid);
        let ns_wh = ctx.sde.wormhole_types.get(&ns_tid);
        vec![
            make_wh_sig(
                format!("THS-{}", 100 + ((sys_id + seq) % 900)),
                sys_id,
                hs_wh
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wormhole B449".into()),
                [10.0, 2.0, -3.0],
                hs_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 7, seq),
                now_unix,
                hs_wh
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(57_600),
                hs_wh.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0),
                375_000_000.0,
            ),
            make_wh_sig(
                format!("TLS-{}", 200 + ((sys_id + seq) % 700)),
                sys_id,
                ls_wh.map(|w| w.name.clone()).unwrap_or_else(|| "Wormhole N944".into()),
                [-9.0, 4.0, 6.0],
                ls_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 8, seq),
                now_unix,
                ls_wh.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(86_400),
                ls_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                375_000_000.0,
            ),
            make_wh_sig(
                format!("TNS-{}", 300 + ((sys_id + seq) % 600)),
                sys_id,
                ns_wh.map(|w| w.name.clone()).unwrap_or_else(|| "Wormhole S199".into()),
                [6.0, -8.0, 5.0],
                ns_tid,
                pick_destination_system_for_target_class(&ctx.sde, sys_id, 9, seq),
                now_unix,
                ns_wh.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(57_600),
                ns_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                375_000_000.0,
            ),
        ]
    } else if (31_000_000..=31_002_605).contains(&sys_id) {
        // DERIVED: static (`WST-...`) and wandering (`WND-...`) wormhole connection templates per W-space class
        let (wh_class, _, is_shattered, _) = system_wormhole_profile_sde(&ctx.sde, sys_id);
        let (static_target_class, wnd_target_class) = match wh_class {
            1 => (1, 3),
            2 => (2, 1),
            3 => (3, 4),
            4 => (4, 5),
            5 => (5, 6),
            6 => (6, 5),
            13 => (3, 2),
            c @ 14..=18 => (c as i32, 9),
            _ => (1, 3),
        };
        let wh_type_id = lookup_wormhole_type_for_target_class(&ctx.sde, static_target_class);
        let wnd_type_id = lookup_wormhole_type_for_target_class(&ctx.sde, wnd_target_class);
        let dest_sys = pick_destination_system_for_target_class(&ctx.sde, sys_id, static_target_class, seq);
        let wnd_dest_sys = pick_destination_system_for_target_class(&ctx.sde, sys_id, wnd_target_class, seq + 1);
        let wh_info = ctx.sde.wormhole_types.get(&wh_type_id);
        let max_mass = wh_info.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0);
        let max_jump = if is_shattered {
            62_000_000.0
        } else {
            wh_info.map(|w| w.max_jump_mass).unwrap_or(375_000_000.0)
        };
        let life_s = wh_info
            .map(|w| (w.max_stable_time_min * 60.0) as u64)
            .unwrap_or(86_400);
        let mut out = Vec::new();
        if seq == 0 {
            let wnd_info = ctx.sde.wormhole_types.get(&wnd_type_id);
            out.push(make_wh_sig(
                format!("WND-{}", 200 + ((sys_id + 13) % 700)),
                sys_id,
                wnd_info
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wandering Wormhole".into()),
                [-8.0, 5.0, 3.0],
                wnd_type_id,
                wnd_dest_sys,
                now_unix,
                wnd_info
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(57_600),
                wnd_info.map(|w| w.max_stable_mass).unwrap_or(1_000_000_000.0),
                max_jump,
            ));
        }
        let mut wst = make_wh_sig(
            format!("WST-{}", 100 + ((sys_id + seq * 17) % 900)),
            sys_id,
            wh_info
                .map(|w| w.name.clone())
                .unwrap_or_else(|| "Unstable Wormhole".into()),
            [12.0, 2.5, -4.0],
            wh_type_id,
            dest_sys,
            now_unix,
            life_s,
            max_mass,
            max_jump,
        );
        if seq > 0 {
            let (cel_pos, _) = pick_system_celestial_position(ctx, sys_id, seq);
            wst.position = cel_pos;
        }
        out.push(wst);
        out
    } else {
        let sig_prefix = if seq == 0 { "JMM" } else { "STA" };
        let mut wh = make_wh_sig(
            format!("{sig_prefix}-{}", 100 + ((sys_id + seq * 37) % 900)),
            sys_id,
            "Unstable Wormhole (K162)".into(),
            [12.0, 2.5, -4.0],
            26272,
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
        vec![wh]
    }
}

pub fn build_static_replacement_wormhole(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    collapsed_wh: &CosmicSignatureRecord,
    now_unix: u64,
    seq: u32,
) -> Option<CosmicSignatureRecord> {
    if collapsed_wh.signature_id.starts_with("WND-") {
        return None;
    }
    let mut eff_seq = seq.max(1);
    let mut rep = static_wormhole_template(ctx, sys_id, now_unix, eff_seq).into_iter().next()?;
    if rep.signature_id == collapsed_wh.signature_id {
        eff_seq += 1;
        rep = static_wormhole_template(ctx, sys_id, now_unix, eff_seq).into_iter().next()?;
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
    if let Some(dest) = collapsed_wh.destination_system_id {
        rep.destination_system_id = Some(dest);
    }
    Some(rep)
}

pub async fn ensure_system_signatures_and_anomalies(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    let now_unix = ctx.now_unix();
    let mut expired_balls_to_despawn: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut expired_sig_ids: Vec<String> = Vec::new();
    let mut added_sigs: Vec<CosmicSignatureRecord> = Vec::new();
    let mut expired_anom_ids: Vec<String> = Vec::new();
    let (sigs, anomalies) = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            let list = snap.cosmic_signatures.entry(sys_id).or_insert_with(|| {
                let mut v = static_wormhole_template(ctx, sys_id, now_unix, 0);
                v.extend([
                    make_site_sig(
                        format!("REL-{}", 200 + (sys_id % 700)),
                        sys_id,
                        "Relic Site",
                        "Ruined Sansha Relic Site",
                        [-8.0, 1.0, 15.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("DAT-{}", 300 + (sys_id % 600)),
                        sys_id,
                        "Data Site",
                        "Local Information Repository",
                        [5.0, -11.0, 3.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("CBT-{}", 400 + (sys_id % 500)),
                        sys_id,
                        "Combat Site",
                        "Pirate Fleet Base",
                        [-14.0, 0.0, -8.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("GAS-{}", 500 + (sys_id % 400)),
                        sys_id,
                        "Gas Site",
                        "Amber Cytoserocin Nebula",
                        [9.0, -4.0, 7.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("ORE-{}", 600 + (sys_id % 300)),
                        sys_id,
                        "Ore Site",
                        "Rare Ore Site (Jaspet & Hemorphite)",
                        [-11.0, 6.0, -13.0],
                        now_unix,
                    ),
                ]);
                v
            });

            let mut spawned_replacements = Vec::new();
            let base_len = list.len() as u32;
            for (idx, sig) in list.iter_mut().enumerate() {
                if !sig.collapsed
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                {
                    sig.collapsed = true;
                    expired_sig_ids.push(sig.signature_id.clone());
                    if sig.sig_group == "Wormhole" {
                        let ent_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                        expired_balls_to_despawn.push((sys_id, ent_id));
                        if let Some(dest_sys) = sig.destination_system_id {
                            let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
                            expired_balls_to_despawn.push((dest_sys, exit_id));
                        }
                        if let Some(rep) = build_static_replacement_wormhole(
                            ctx,
                            sys_id,
                            sig,
                            now_unix,
                            base_len + (idx as u32) + 1,
                        ) {
                            spawned_replacements.push(rep);
                        }
                    }
                }
            }
            added_sigs = spawned_replacements.clone();
            list.extend(spawned_replacements);

            let sigs_clone = list.clone();
            let anom_list = snap
                .cosmic_anomalies
                .entry(sys_id)
                .or_insert_with(|| {
                    vec![CosmicAnomalyRecord {
                        anomaly_id: format!("ANO-{}", 100 + (sys_id % 900)),
                        solar_system_id: sys_id,
                        name: "Dense Veldspar Deposit".into(),
                        anomaly_type: "Ore".into(),
                        position: [
                            4.0 * AU_IN_METERS,
                            3.0 * AU_IN_METERS,
                            -2.0 * AU_IN_METERS,
                        ],
                        signal_strength: 1.0,
                        ore_types: vec![1230, 1228],
                        remaining_volume: 50_000.0,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        cleared: false,
                    }]
                });
            for anom in anom_list.iter_mut() {
                if !anom.cleared
                    && anom.lifetime_seconds > 0
                    && now_unix >= anom.created_unix.saturating_add(anom.lifetime_seconds)
                {
                    anom.cleared = true;
                    expired_anom_ids.push(anom.anomaly_id.clone());
                }
            }
            let anoms = anom_list.clone();
            Ok::<_, std::convert::Infallible>((sigs_clone, anoms))
        })
        .unwrap();

    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
        ctx,
        sys_id,
        &added_sigs,
        &expired_sig_ids,
    );
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
        ctx,
        sys_id,
        &[],
        &expired_anom_ids,
    );

    for (b_sys, b_id) in expired_balls_to_despawn {
        let h = ctx.systems.get_or_spawn(b_sys);
        let _ = h.remove_ball(b_id).await;
    }

    let is_pochven_sys = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sig in &sigs {
        if sig.sig_group == "Wormhole" && !sig.collapsed {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
            let ball_pos = pochven_wormhole_ball_position(&ctx.sde, sig);
            if sys_handle
                .get_ball_snapshot(wh_item_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: wh_item_id,
                        type_id: sig.wormhole_type_id.unwrap_or(26272),
                        position: ball_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: sig.remaining_mass_kg.max(1.0),
                        radius: 1500.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    })
                    .await;
            }
            // Pochven-origin wormholes only open their K162 exit when scanned from the Pochven side (`feat:navigation.pochven-wormholes`).
            let delay_k162_until_scanned = is_pochven_sys
                && is_pochven_wormhole(&ctx.sde, sig)
                && sig.signal_strength < 1.0;
            if let Some(dest_sys) = sig.destination_system_id.filter(|_| !delay_k162_until_scanned) {
                let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
                let dest_handle = ctx.systems.get_or_spawn(dest_sys);
                if dest_handle
                    .get_ball_snapshot(exit_id)
                    .await
                    .ok()
                    .flatten()
                    .is_none()
                {
                    let (exit_pos, _) = pick_system_celestial_position(ctx, dest_sys, (wh_item_id as u32).wrapping_add(1));
                    let _ = dest_handle
                        .spawn_ball(Ball {
                            id: exit_id,
                            type_id: 26272, // Wormhole K162 exit
                            position: DVec3::from_array(exit_pos),
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: sig.remaining_mass_kg.max(1.0),
                            radius: 1500.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Rigid,
                        })
                        .await;
                }
            }
        }
    }

    (sigs, anomalies)
}

/// Background cluster wormhole expiry sweep:
/// despawns both entrance and exit balls of expired wormholes and spawns static replacement wormholes.
pub async fn expire_and_respawn_wormholes_in_cluster(ctx: &ServerContext, now_unix: u64) {
    let mut expired_balls_to_despawn: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut new_wormholes_to_spawn: Vec<(SolarSystemId, CosmicSignatureRecord)> = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        for (&sys_id, list) in snap.cosmic_signatures.iter_mut() {
            let mut spawned_replacements = Vec::new();
            let base_len = list.len() as u32;
            for (idx, sig) in list.iter_mut().enumerate() {
                if sig.sig_group == "Wormhole"
                    && !sig.collapsed
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                {
                    sig.collapsed = true;
                    let ent_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                    expired_balls_to_despawn.push((sys_id, ent_id));
                    if let Some(dest_sys) = sig.destination_system_id {
                        let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
                        expired_balls_to_despawn.push((dest_sys, exit_id));
                    }
                    if let Some(rep) = build_static_replacement_wormhole(
                        ctx,
                        sys_id,
                        sig,
                        now_unix,
                        base_len + (idx as u32) + 1,
                    ) {
                        new_wormholes_to_spawn.push((sys_id, rep.clone()));
                        spawned_replacements.push(rep);
                    }
                }
            }
            list.extend(spawned_replacements);
        }
    });
    for (b_sys, b_id) in expired_balls_to_despawn {
        let h = ctx.systems.get_or_spawn(b_sys);
        let _ = h.remove_ball(b_id).await;
    }
    for (sys_id, rep) in new_wormholes_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(sys_id, &rep.signature_id);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: new_wh_id,
                type_id: rep.wormhole_type_id.unwrap_or(26272),
                position: DVec3::from_array(rep.position),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: rep.remaining_mass_kg.max(1.0),
                radius: 1500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
        if let Some(d_sys) = rep.destination_system_id {
            let new_exit_id = wormhole_exit_item_id_for_signature(d_sys, &rep.signature_id);
            let (exit_pos, _) = pick_system_celestial_position(ctx, d_sys, (new_wh_id as u32).wrapping_add(1));
            let dh = ctx.systems.get_or_spawn(d_sys);
            let _ = dh
                .spawn_ball(Ball {
                    id: new_exit_id,
                    type_id: 26272,
                    position: DVec3::from_array(exit_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: rep.remaining_mass_kg.max(1.0),
                    radius: 1500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
        }
    }
}

pub fn compute_wormhole_stages(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
) -> (&'static str, f64, &'static str, i64, bool) {
    let elapsed = now_unix.saturating_sub(wh.created_unix);
    let lifetime_pct = if wh.lifetime_seconds == 0 || elapsed >= wh.lifetime_seconds {
        0.0
    } else {
        (wh.lifetime_seconds - elapsed) as f64 / wh.lifetime_seconds as f64
    };
    let (lifetime_stage, wormhole_age) = if lifetime_pct <= 0.0 {
        ("Collapsed_Expired", 3)
    } else if lifetime_pct < 0.25 {
        ("ReachingEndOfLifetime", 2)
    } else {
        ("Healthy", 1)
    };

    let mass_pct = if wh.max_mass_kg > 0.0 {
        wh.remaining_mass_kg / wh.max_mass_kg
    } else {
        0.0
    };
    let (mass_stage, wormhole_size) = if wh.remaining_mass_kg <= 0.0 {
        ("Collapsed_Exhausted", 0.0)
    } else if mass_pct < 0.10 {
        ("Critical", 0.1)
    } else if mass_pct < 0.50 {
        ("Destabilized", 0.5)
    } else {
        ("Stable", 1.0)
    };
    let is_collapsed = wh.collapsed || lifetime_pct <= 0.0 || wh.remaining_mass_kg <= 0.0;
    (
        mass_stage,
        wormhole_size,
        lifetime_stage,
        wormhole_age,
        is_collapsed,
    )
}

// parity: feat:wormholes.mass-reduction-and-destabilization
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_cosmic_wormhole_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    old_sys: SolarSystemId,
    ship_id: i64,
    current_ball: Ball,
    wh_arg_i64: Option<i64>,
    wh_arg_str: Option<&str>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let now_unix = ctx.now_unix();
    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None)
    {
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot jump wormhole while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        }));
    }

    let has_explicit_expired = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        }) || snap.cosmic_signatures.iter().any(|(_, sigs)| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && s.destination_system_id == Some(old_sys)
                    && wh_arg_i64
                        == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
            })
        })
    });
    if !has_explicit_expired {
        let _ = ensure_system_signatures_and_anomalies(ctx, old_sys).await;
    }
    let has_matching_sig = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        }) || snap.cosmic_signatures.iter().any(|(_, sigs)| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && s.destination_system_id == Some(old_sys)
                    && wh_arg_i64
                        == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
            })
        })
    });
    if !has_matching_sig && wh_arg_str.is_none() {
        return None;
    }

    let (ship_mass, prop_mass_add) = compute_effective_ship_jump_mass(ctx, ship_id, current_ball.mass);
    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let exit_ball_pos_opt = if let Some(arg_id) = wh_arg_i64 {
        old_handle
            .get_ball_snapshot(arg_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
    } else {
        None
    };

    let mut collapsed_balls_to_remove: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut replacement_to_spawn: Option<CosmicSignatureRecord> = None;
    let mut collapsed_sig_delta: Option<(SolarSystemId, String)> = None;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let mut reverse_parent_sys_opt: Option<SolarSystemId> = None;
        let has_local = snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        });
        if !has_local {
            for (&psys, sigs) in &snap.cosmic_signatures {
                if sigs.iter().any(|s| {
                    s.sig_group == "Wormhole"
                        && s.destination_system_id == Some(old_sys)
                        && wh_arg_i64
                            == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
                }) {
                    reverse_parent_sys_opt = Some(psys);
                    break;
                }
            }
        }
        let sig_host_sys = reverse_parent_sys_opt.unwrap_or(old_sys);
        let is_reverse_exit_jump = reverse_parent_sys_opt.is_some();
        let Some(sigs) = snap.cosmic_signatures.get_mut(&sig_host_sys) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: "No signatures in system".into(),
            });
        };
        let next_seq = sigs.len() as u32 + 1;
        let Some(wh_idx) = sigs.iter().position(|s| {
            s.sig_group == "Wormhole"
                && if is_reverse_exit_jump {
                    s.destination_system_id == Some(old_sys)
                        && wh_arg_i64
                            == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
                } else {
                    wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id))
                }
        }) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: format!("Wormhole not found in system {old_sys}"),
            });
        };

        let sig_id_str = sigs[wh_idx].signature_id.clone();
        let (wh_id, dest_sys, exit_wh_id) = if is_reverse_exit_jump {
            let local_exit_id = wormhole_exit_item_id_for_signature(old_sys, &sig_id_str);
            let parent_entrance_id = wormhole_item_id_for_signature(sig_host_sys, &sig_id_str);
            (local_exit_id, sig_host_sys, parent_entrance_id)
        } else {
            let entrance_id = wormhole_item_id_for_signature(old_sys, &sig_id_str);
            let d_sys = sigs[wh_idx].destination_system_id.unwrap_or(31_000_005);
            let exit_id = wormhole_exit_item_id_for_signature(d_sys, &sig_id_str);
            (entrance_id, d_sys, exit_id)
        };

        let pol_expiry = snap.wh_polarizations.get(&(char_id, wh_id)).copied().unwrap_or(0);
        if pol_expiry > now_unix {
            return Err(BridgeRpcError {
                code: "WormholePolarized".into(),
                message: format!(
                    "Character is polarized for wormhole {} for another {} seconds",
                    wh_id,
                    pol_expiry.saturating_sub(now_unix)
                ),
            });
        }

        let wh_pos = if is_reverse_exit_jump {
            exit_ball_pos_opt.unwrap_or_else(|| pochven_wormhole_ball_position(&ctx.sde, &sigs[wh_idx]))
        } else {
            pochven_wormhole_ball_position(&ctx.sde, &sigs[wh_idx])
        };
        let dist = current_ball.position.distance(wh_pos);
        if dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "OutsideWormholeJumpRange".into(),
                message: format!(
                    "Ship is {:.1} m from wormhole (must be <= 2500 m)",
                    dist
                ),
            });
        }

        let mut spawn_replacement = |sigs_vec: &mut Vec<CosmicSignatureRecord>, idx: usize| {
            if let Some(rep) =
                build_static_replacement_wormhole(ctx, sig_host_sys, &sigs_vec[idx], now_unix, next_seq)
            {
                replacement_to_spawn = Some(rep.clone());
                sigs_vec.push(rep);
            }
        };

        let elapsed = now_unix.saturating_sub(sigs[wh_idx].created_unix);
        if elapsed >= sigs[wh_idx].lifetime_seconds || sigs[wh_idx].collapsed {
            let was_already_collapsed = sigs[wh_idx].collapsed;
            sigs[wh_idx].collapsed = true;
            collapsed_balls_to_remove.push((old_sys, wh_id));
            collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
            if !was_already_collapsed {
                collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
                spawn_replacement(sigs, wh_idx);
            }
            let code = if elapsed >= sigs[wh_idx].lifetime_seconds {
                "WormholeCollapsed"
            } else {
                "WormholeMassCollapsed"
            };
            return Err(BridgeRpcError {
                code: code.into(),
                message: "Wormhole has collapsed".into(),
            });
        }

        let (dest_class, dest_is_thera, dest_is_shattered, _) = system_wormhole_profile(dest_sys);
        let max_jump = sigs[wh_idx].max_jump_mass_kg;
        if (max_jump > 0.0 && ship_mass > max_jump)
            || ((dest_is_thera || dest_is_shattered || (1..=4).contains(&dest_class))
                && ship_mass > 375_000_000.0)
        {
            return Err(BridgeRpcError {
                code: "ShipTooHeavyForWormhole".into(),
                message: format!(
                    "Ship mass {:.1} kg exceeds wormhole jump limit {:.1} kg",
                    ship_mass, max_jump
                ),
            });
        }

        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
            collapsed_balls_to_remove.push((old_sys, wh_id));
            collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
            spawn_replacement(sigs, wh_idx);
            return Err(BridgeRpcError {
                code: "WormholeMassCollapsed".into(),
                message: "Wormhole lacks remaining mass capacity and has collapsed".into(),
            });
        }

        sigs[wh_idx].remaining_mass_kg -= ship_mass;
        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
            collapsed_balls_to_remove.push((old_sys, wh_id));
            collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
            spawn_replacement(sigs, wh_idx);
        }

        let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
            compute_wormhole_stages(&sigs[wh_idx], now_unix);
        let buckets = derive_wormhole_buckets(&sigs[wh_idx], now_unix, &ctx.sde);
        let rem_mass = sigs[wh_idx].remaining_mass_kg;

        snap.wh_polarizations.insert((char_id, dest_sys as i64), now_unix + 300);
        snap.wh_polarizations.insert((char_id, wh_id), now_unix + 300);

        Ok((dest_sys, wh_id, exit_wh_id, sig_id_str, rem_mass, max_jump, mass_stage, wh_size, life_stage, wh_age, is_collapsed, buckets))
    });

    for (b_sys, rem_id) in &collapsed_balls_to_remove {
        let h = ctx.systems.get_or_spawn(*b_sys);
        let _ = h.remove_ball(*rem_id).await;
    }
    if let Some((host_sys, collapsed_id)) = collapsed_sig_delta {
        let added_vec: Vec<CosmicSignatureRecord> = replacement_to_spawn.iter().cloned().collect();
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            host_sys,
            &added_vec,
            &[collapsed_id],
        );
    }
    let mut rep_sig_id: Option<String> = None;
    let mut rep_wh_id: Option<i64> = None;
    let mut rep_wh_type_id: Option<u32> = None;
    if let Some(rep) = replacement_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(old_sys, &rep.signature_id);
        rep_sig_id = Some(rep.signature_id.clone());
        rep_wh_id = Some(new_wh_id);
        rep_wh_type_id = rep.wormhole_type_id;
        let _ = old_handle.spawn_ball(Ball {
            id: new_wh_id, type_id: rep.wormhole_type_id.unwrap_or(26272),
            position: DVec3::from_array(rep.position), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: rep.remaining_mass_kg.max(1.0),
            radius: 1500.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
        }).await;
        if let Some(d_sys) = rep.destination_system_id {
            let new_exit_id = wormhole_exit_item_id_for_signature(d_sys, &rep.signature_id);
            let (exit_pos, _) = pick_system_celestial_position(ctx, d_sys, (new_wh_id as u32).wrapping_add(1));
            let dh = ctx.systems.get_or_spawn(d_sys);
            let _ = dh.spawn_ball(Ball {
                id: new_exit_id, type_id: 26272, position: DVec3::from_array(exit_pos),
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0,
                mass: rep.remaining_mass_kg.max(1.0), radius: 1500.0,
                speed_fraction: 0.0, mode: MovementMode::Rigid,
            }).await;
        }
    }

    match res {
        Ok((dest_sys_id, wh_item_id, exit_wh_id, sig_id_str, remaining_mass, max_jump_mass, mass_stage, wh_size, life_stage, wh_age, is_collapsed, buckets)) => {
            let prev_cs = old_handle.get_combat_state(ship_id).await.ok().flatten();
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
            crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
            let _ = old_handle.remove_ball(ship_id).await;
            let updated_ch = match ctx.db.set_character_solar_system(char_id, dest_sys_id, &ctx.sde) {
                Ok(c) => c,
                Err(e) => return Some(Err(BridgeRpcError { code: "SystemTransitionFailed".into(), message: e.to_string() })),
            };
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let _ = ensure_system_signatures_and_anomalies(ctx, dest_sys_id).await;
            let (dest_wh_class, dest_is_thera, dest_is_shattered, env_effect) = system_wormhole_profile(dest_sys_id);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = current_ball;
            new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;

            let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            combat_state.is_cloaked = true;
            combat_state.gate_cloak_remaining_ticks = 60;
            combat_state.undock_invulnerable_remaining_ticks = 60;
            combat_state.sync_invuln_with_undock_timer = true;
            match env_effect {
                "Pulsar" => {
                    combat_state.max_shield_hp *= 1.30;
                    combat_state.shield_hp = combat_state.max_shield_hp;
                    combat_state.signature_radius *= 1.30;
                }
                "Wolf-Rayet" => {
                    combat_state.max_armor_hp *= 1.30;
                    combat_state.armor_hp = combat_state.max_armor_hp;
                    combat_state.signature_radius *= 0.85;
                }
                "Black Hole" => {
                    new_ball.max_velocity *= 1.30;
                    new_ball.inertia *= 1.15;
                }
                "Cataclysmic Variable" => {
                    combat_state.max_capacitor *= 1.30;
                    combat_state.capacitor = combat_state.max_capacitor;
                }
                _ => {}
            }
            if let Some(pcs) = prev_cs {
                let s_frac = (pcs.shield_hp / pcs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                let a_frac = (pcs.armor_hp / pcs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                let h_frac = (pcs.structure_hp / pcs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                let c_frac = (pcs.capacitor / pcs.max_capacitor.max(1.0)).clamp(0.0, 1.0);
                combat_state.shield_hp = combat_state.max_shield_hp * s_frac;
                combat_state.armor_hp = combat_state.max_armor_hp * a_frac;
                combat_state.structure_hp = combat_state.max_structure_hp * h_frac;
                combat_state.capacitor = combat_state.max_capacitor * c_frac;
            }
            let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
            slim_meta.is_cloaked = true;
            let spawn_tick = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await.unwrap_or(0);
            record_session_change_timer(ctx, char_id, now_unix, dest_sys_id, spawn_tick, "wormhole_jump");

            let new_beyonce_id = match ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)), ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("destinationSystemID", PyRep::Integer(dest_sys_id as i64)), ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("wormholeID", PyRep::Integer(wh_item_id)), ("exitWormholeID", PyRep::Integer(exit_wh_id)),
                ("signatureID", PyRep::String(sig_id_str)), ("shipJumpMassKg", PyRep::Real(ship_mass)),
                ("propulsionMassAdditionKg", PyRep::Real(prop_mass_add)), ("remainingMassKg", PyRep::Real(remaining_mass)),
                ("maxJumpMassKg", PyRep::Real(max_jump_mass)), ("massStage", PyRep::String(mass_stage.into())),
                ("massStabilityBucket", PyRep::String(buckets.mass_stability_bucket.into())),
                ("massStabilityDescription", PyRep::String(buckets.mass_stability_description.into())),
                ("wormholeSize", PyRep::Real(wh_size)), ("lifetimeStage", PyRep::String(life_stage.into())),
                ("lifetimeBucket", PyRep::String(buckets.lifetime_bucket.into())),
                ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                ("wormholeAge", PyRep::Integer(wh_age)), ("collapsed", PyRep::Bool(is_collapsed)),
                ("replacementSignatureID", rep_sig_id.map(PyRep::String).unwrap_or(PyRep::None)),
                ("replacementWormholeID", rep_wh_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("replacementWormholeTypeID", rep_wh_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None)),
                ("wormholeClass", PyRep::Integer(dest_wh_class as i64)), ("isThera", PyRep::Bool(dest_is_thera)),
                ("isShattered", PyRep::Bool(dest_is_shattered)), ("delayedLocal", PyRep::Bool(dest_wh_class > 0)),
                ("environmentalEffect", PyRep::String(env_effect.into())),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}
