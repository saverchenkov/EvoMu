//! V24 SDE-backed CONCORD Rogue Analysis Beacon (CRAB) wave composition, loot tables,
//! and Signal Interference Cost Modifier helpers (`feat:pve.crab-ceq-signal-interference`,
//! `feat:pve.crab-link-activation`, `notify:OnSolarsystemInterferenceChanged`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};

/// Resolves Rogue Drone NPC wave `TypeId`s for `wave_num` (`1..=3`) from V24 `sde.types`.
pub fn resolve_crab_wave_drone_types(
    sde: &SdeRegistry,
    beacon_type_id: TypeId,
    wave_num: u8,
) -> Vec<TypeId> {
    let count = match (beacon_type_id, wave_num) {
        (crate::services::pve_crab_beacons::TYPE_CARRIER_CRAB_BEACON, 1) => 4,
        (crate::services::pve_crab_beacons::TYPE_CARRIER_CRAB_BEACON, 2) => 5,
        (crate::services::pve_crab_beacons::TYPE_CARRIER_CRAB_BEACON, _) => 4,
        (_, 1) => 3,
        (_, 2) => 4,
        _ => 3,
    };

    // Prefer V24 CRAB Rogue Drone entities (`group_id == 4037`) or standard Rogue Drone NPCs (`801..=805`, `844`).
    let mut primary: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| t.published && t.group_id == 4037 && !t.name.contains("DO NOT TRANSLATE"))
        .map(|t| t.type_id)
        .collect();
    primary.sort_unstable();

    if primary.len() >= 10 {
        let start = match wave_num {
            1 => 0,
            2 => 3,
            _ => 7,
        };
        return primary.iter().cycle().skip(start).take(count).copied().collect();
    }

    let mut fallback: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            t.published
                && matches!(t.group_id, 801..=805 | 844)
                && !t.name.contains("DO NOT TRANSLATE")
        })
        .map(|t| t.type_id)
        .collect();
    fallback.sort_unstable();
    if fallback.is_empty() {
        fallback = vec![18_072, 18_074, 18_084, 23_505, 27_722];
    }
    let start = usize::from(wave_num.saturating_sub(1)) * 2;
    fallback.iter().cycle().skip(start).take(count).copied().collect()
}

/// Resolves V24 SDE Rogue Drone Survey Data & Mutaplasmid reward items for CRAB completion.
pub fn resolve_crab_completion_rewards(
    sde: &SdeRegistry,
    beacon_type_id: TypeId,
) -> Vec<(TypeId, i64)> {
    let data_qty = if beacon_type_id == crate::services::pve_crab_beacons::TYPE_CARRIER_CRAB_BEACON {
        10
    } else {
        15
    };
    let mut rewards = Vec::new();

    // Primary Rogue Drone Survey / Infestation Data (`60459` or Nexus Chips `33618..=33620`) verified in `sde.types`
    let data_type = [60_459u32, 33_619, 33_620, 33_618]
        .into_iter()
        .find(|tid| sde.types.contains_key(tid))
        .unwrap_or(60_459);
    rewards.push((data_type, data_qty));

    // Mutaplasmid drops from V24 `sde.types` (`group_id == 1964` or Radical mutaplasmids `60476`, `60477`)
    let mut mutas: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| t.published && t.group_id == 1964 && !t.name.contains("DO NOT TRANSLATE"))
        .map(|t| t.type_id)
        .collect();
    mutas.sort_unstable();
    let mut chosen_mutas: Vec<TypeId> = Vec::new();
    for preferred in [60_476_u32, 60_477] {
        if sde.types.contains_key(&preferred) {
            chosen_mutas.push(preferred);
        }
    }
    if chosen_mutas.len() < 2 && !mutas.is_empty() {
        let pick_idx = (beacon_type_id as usize) % mutas.len();
        if !chosen_mutas.contains(&mutas[pick_idx]) {
            chosen_mutas.push(mutas[pick_idx]);
        }
        if chosen_mutas.len() < 2 && mutas.len() > 1 {
            let second_idx = ((beacon_type_id as usize) + 1) % mutas.len();
            if !chosen_mutas.contains(&mutas[second_idx]) {
                chosen_mutas.push(mutas[second_idx]);
            }
        }
    }
    if chosen_mutas.is_empty() {
        chosen_mutas.extend_from_slice(&[60_476, 60_477]);
    }
    for m_tid in chosen_mutas {
        rewards.push((m_tid, 1));
    }

    rewards
}

/// Computes effective CEQ cost for a CRAB link scaled by the system's Signal Interference Cost Modifier.
///
/// At `0.0` system interference, cost equals base `CRAB_CEQ_COST` (`30`, permitting 3 links per 100 CEQ).
/// As system interference accumulates toward `1.0` (12/12 cap), the Interference Cost Modifier scales up
/// the per-link CEQ surcharge while still allowing 3 consecutive links per full 100-CEQ capacitor charge.
pub fn effective_crab_ceq_cost(base_cost: i64, system_interference_level: f64) -> (i64, f64) {
    let modifier = 1.0 + (system_interference_level.clamp(0.0, 1.0) * 0.10);
    let cost = ((base_cost as f64) * modifier).round().max(1.0) as i64;
    (cost, modifier)
}

/// Emits `OnSolarsystemInterferenceChanged` to all pilots in `sys_id` (both space and docked).
pub fn emit_interference_changed_clusterwide(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    level: f64,
    band: u8,
) {
    let local_state =
        crate::services::pve_special_sites::build_interference_state_keyval(sys_id, level, band);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarsystemInterferenceChanged",
        "solarsystemid",
        vec![PyRep::Integer(sys_id as i64), local_state.clone()],
    );
    let extra_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == sys_id
                    && (c.station_id.is_some() || c.structure_id.is_some())
            })
            .map(|c| c.character_id)
            .collect()
    });
    for cid in extra_chars {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSolarsystemInterferenceChanged",
            "solarsystemid",
            vec![PyRep::Integer(sys_id as i64), local_state.clone()],
        );
    }
}

/// Applies Signal Interference delta & decay for `solar_system_id`, returning `Some((level, band))` on change.
pub(super) fn apply_interference_activity(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    activity_delta: f64,
    decay_window_secs: f64,
) -> Option<(f64, u8)> {
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
    let delta = activity_delta.clamp(0.0, 1.0);
    let now_sec = ctx.now_unix();
    let (prev_level, level, old_band, new_band) =
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let entry = reg.interference_by_system.entry(solar_system_id).or_insert(
                crate::services::pve_special_sites::SystemInterferenceRecord {
                    solar_system_id,
                    interference_level: 0.0,
                    band: 1,
                    last_updated_sec: now_sec,
                },
            );
            let initial_level = entry.interference_level;
            if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
                let elapsed = now_sec - entry.last_updated_sec;
                let decay = (elapsed as f64) / decay_window_secs;
                entry.interference_level = (entry.interference_level - decay).max(0.0);
            }
            let prev = entry.band;
            if delta > 0.0 {
                entry.interference_level = (entry.interference_level + delta).clamp(0.0, 1.0);
            }
            entry.band = if entry.interference_level >= 0.70 {
                3
            } else if entry.interference_level >= 0.35 {
                2
            } else {
                1
            };
            entry.last_updated_sec = now_sec;
            (initial_level, entry.interference_level, prev, entry.band)
        });
    if (level - prev_level).abs() > 1e-9 || new_band != old_band {
        Some((level, new_band))
    } else {
        None
    }
}

#[cfg(test)]
#[path = "pve_crab_beacons_ops_edges_tests.rs"]
mod pve_crab_beacons_ops_edges_tests;
