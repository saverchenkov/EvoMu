//! V24 SDE-backed Sansha Incursion encounter composition, dynamic tale allocation,
//! and fleet/influence payout scaling (`feat:pve.incursion-payout`,
//! `notify:OnInfluenceUpdate`, `notify:OnTaleRemove`).

#![forbid(unsafe_code)]

use crate::services::pve_incursion_sites::IncursionNpcSpawn;
use crate::services::pve_special_sites::{next_spec_seq, ConstellationId, PveSpecialSitesRegistry};
use eve_sde::SdeRegistry;

/// Allocates a unique persisted world-event `tale_id` without hardcoding `800_000 + constellation_id`.
pub fn allocate_persistent_incursion_tale_id(
    reg: &mut PveSpecialSitesRegistry,
    constellation_id: ConstellationId,
) -> i64 {
    let seq = next_spec_seq(reg);
    let base = 810_000_i64 + seq;
    let collision = reg.incursions.values().any(|i| i.tale_id == base);
    if collision {
        base + i64::from(constellation_id) * 10
    } else {
        base
    }
}

/// Resolves Sansha's Nation encounter boss and escorts from V24 `sde.types` (`faction_id == 500_019`).
pub fn resolve_sansha_encounter_from_sde(
    sde: &SdeRegistry,
    boss_ball_id: i64,
    dungeon_id: u32,
    tier: &str,
) -> Option<(u32, Vec<IncursionNpcSpawn>)> {
    let mut battleships: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && t.faction_id == Some(500_019) && t.group_id == 570)
        .map(|t| t.type_id)
        .collect();
    battleships.sort_unstable();
    let mut cruisers: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && t.faction_id == Some(500_019) && t.group_id == 571)
        .map(|t| t.type_id)
        .collect();
    cruisers.sort_unstable();
    let mut frigates: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && t.faction_id == Some(500_019) && t.group_id == 572)
        .map(|t| t.type_id)
        .collect();
    frigates.sort_unstable();

    if battleships.is_empty() && cruisers.is_empty() && frigates.is_empty() {
        return None;
    }

    let bs0 = battleships.first().copied().unwrap_or(2845);
    let bs1 = battleships.get(1).copied().unwrap_or(bs0);
    let bs2 = battleships.get(2).copied().unwrap_or(bs1);
    let cr0 = cruisers.first().copied().unwrap_or(2936);
    let cr1 = cruisers.get(1).copied().unwrap_or(cr0);
    let fr0 = frigates.first().copied().unwrap_or(2909);
    let fr1 = frigates.get(1).copied().unwrap_or(fr0);
    let fr2 = frigates.get(2).copied().unwrap_or(fr1);

    let seed = usize::try_from(dungeon_id).unwrap_or(0);
    let esc_pos = [
        [-3_500.0, 1_200.0, 500.0],
        [3_200.0, -1_500.0, -800.0],
        [-1_800.0, 2_400.0, 1_100.0],
        [2_100.0, 1_800.0, -600.0],
        [-900.0, -800.0, 1_200.0],
        [850.0, 750.0, -950.0],
        [-600.0, 1_100.0, -700.0],
        [4_500.0, 2_000.0, 1_500.0],
        [1_200.0, -2_200.0, 800.0],
    ];

    match tier {
        "Mothership" | "Boss" => {
            let boss_tid = sde.types.get(&2192).map(|t| t.type_id).unwrap_or(2192);
            let pool = [bs0, bs1, cr0, cr1, fr0, fr1, fr2, bs2, fr0];
            let escorts = pool
                .iter()
                .enumerate()
                .map(|(i, &tid)| IncursionNpcSpawn {
                    npc_ball_id: boss_ball_id * 10 + ((i as i64) + 1),
                    type_id: tid,
                    offset: esc_pos[i],
                    destroyed: false,
                })
                .collect();
            Some((boss_tid, escorts))
        }
        "Headquarters" => {
            let boss_tid = battleships.get(seed % battleships.len().max(1)).copied().unwrap_or(bs0);
            let pool = [bs1, bs2, cr0, cr1, fr0, fr1, fr2];
            let escorts = pool
                .iter()
                .enumerate()
                .map(|(i, &tid)| IncursionNpcSpawn {
                    npc_ball_id: boss_ball_id * 10 + ((i as i64) + 1),
                    type_id: tid,
                    offset: esc_pos[i],
                    destroyed: false,
                })
                .collect();
            Some((boss_tid, escorts))
        }
        "Assault" => {
            let boss_tid = battleships
                .get((seed + 1) % battleships.len().max(1))
                .copied()
                .unwrap_or(bs1);
            let pool = [bs0, cr0, cr1, fr0, fr1];
            let escorts = pool
                .iter()
                .enumerate()
                .map(|(i, &tid)| IncursionNpcSpawn {
                    npc_ball_id: boss_ball_id * 10 + ((i as i64) + 1),
                    type_id: tid,
                    offset: esc_pos[i],
                    destroyed: false,
                })
                .collect();
            Some((boss_tid, escorts))
        }
        "Vanguard" => {
            let boss_tid = if dungeon_id == 5165 { bs1 } else { 3492 };
            let pool = [cr0, fr0, fr2];
            let escorts = pool
                .iter()
                .enumerate()
                .map(|(i, &tid)| IncursionNpcSpawn {
                    npc_ball_id: boss_ball_id * 10 + ((i as i64) + 1),
                    type_id: tid,
                    offset: esc_pos[i],
                    destroyed: false,
                })
                .collect();
            Some((boss_tid, escorts))
        }
        _ => {
            let boss_tid = sde.types.get(&3492).map(|t| t.type_id).unwrap_or(cr0);
            let pool = [cr0, cr1, fr0];
            let escorts = pool
                .iter()
                .enumerate()
                .map(|(i, &tid)| IncursionNpcSpawn {
                    npc_ball_id: boss_ball_id * 10 + ((i as i64) + 1),
                    type_id: tid,
                    offset: esc_pos[i],
                    destroyed: false,
                })
                .collect();
            Some((boss_tid, escorts))
        }
    }
}

/// Computes fleet + incursion state/influence payout multiplier for site completion.
pub fn compute_incursion_fleet_payout_scale(
    count: usize,
    min_pilots: usize,
    max_opt: usize,
    state: &str,
) -> f64 {
    let under_scale = if count < min_pilots.max(1) {
        (count as f64) / (min_pilots.max(1) as f64)
    } else {
        1.0
    };
    let over_scale = crate::services::pve_incursion_sites::incursion_overstack_ratio(
        count.saturating_sub(max_opt),
    );
    let state_mult = match state {
        "Withdrawing" => 1.15,
        "Mobilizing" => 1.05,
        _ => 1.0,
    };
    under_scale * over_scale * state_mult
}

/// Encodes an [`IncursionLPLogEntry`] into the EVE client `util.KeyVal` payload.
pub fn encode_incursion_lp_log_row(
    e: &crate::services::pve_incursion_sites::IncursionLPLogEntry,
) -> eve_proto::PyRep {
    eve_proto::PyRep::keyval([
        ("logID", eve_proto::PyRep::Integer(e.log_id)),
        ("siteID", eve_proto::PyRep::String(e.site_id.clone())),
        ("tier", eve_proto::PyRep::String(e.tier.clone())),
        ("constellationID", eve_proto::PyRep::Integer(e.constellation_id as i64)),
        ("solarSystemID", eve_proto::PyRep::Integer(e.solar_system_id as i64)),
        ("iskEarned", eve_proto::PyRep::Integer(e.isk_earned)),
        ("concordLpEarned", eve_proto::PyRep::Integer(e.concord_lp_earned)),
        ("fleetSize", eve_proto::PyRep::Integer(e.fleet_size as i64)),
        ("fleetScaleFactor", eve_proto::PyRep::Real(e.fleet_scale_factor)),
        ("timestamp", eve_proto::PyRep::Integer(e.timestamp_filetime)),
    ])
}

/// Filters and normalizes `UpdateIncursionLPLog` rows for `char_id`.
pub fn sync_incursion_lp_log_rows(
    reg: &mut crate::services::pve_special_sites::PveSpecialSitesRegistry,
    char_id: i64,
    args: &[eve_proto::PyRep],
    now_ft: i64,
) -> Vec<eve_proto::PyRep> {
    let site_filter = args.first().and_then(|a| a.as_str()).map(str::to_string);
    let cid_filter = args
        .first()
        .and_then(|a| a.as_i64())
        .and_then(|v| u32::try_from(v).ok());
    let Some(entries) = reg.incursion_lp_logs.get_mut(&char_id) else {
        return Vec::new();
    };
    let mut matched = Vec::new();
    for e in entries.iter_mut() {
        if e.timestamp_filetime <= 0 {
            e.timestamp_filetime = now_ft;
        }
        let keep = if let Some(ref sf) = site_filter {
            e.site_id == *sf
        } else if let Some(cf) = cid_filter {
            e.constellation_id == cf
        } else {
            true
        };
        if keep {
            matched.push(encode_incursion_lp_log_row(e));
        }
    }
    matched
}

#[cfg(test)]
#[path = "pve_incursion_sites_ops_edges_tests.rs"]
mod pve_incursion_sites_ops_edges_tests;
