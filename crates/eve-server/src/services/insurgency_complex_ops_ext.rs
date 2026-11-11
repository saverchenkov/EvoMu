//! V24 SDE-backed Havoc Insurgency complex defenders, Moderate Battlecruiser dogma
//! application (`dbuff_id = 2544`), and Laundering / War HQ reward crate resolution
//! (`feat:pve.insurgency-laundering`, `feat:pve.insurgency-mining-ambush`,
//! `feat:pve.insurgency-outpost-raid`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::insurgency_complex_ops::{
    is_battlecruiser_group, OutpostRaidSize, MODERATE_OUTPOST_BC_SKILL_DBUFF_ID,
    TYPE_ANGEL_LAUNDERING_FACILITY, TYPE_FEDERATION_CI_OUTPOST_HUB, TYPE_GURISTAS_LAUNDERING_FACILITY,
    TYPE_IMPERIAL_CI_OUTPOST_HUB, TYPE_STATE_CI_OUTPOST_HUB,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};

/// Resolves V24 SDE NPC `TypeId` for an Outpost Raid defender matching faction and site size.
pub fn resolve_sde_outpost_defender_type(
    sde: &SdeRegistry,
    faction_id: u32,
    size: OutpostRaidSize,
    fallback: TypeId,
) -> TypeId {
    let target_groups: &[u32] = match size {
        OutpostRaidSize::Small => &[551, 562, 572, 602, 811, 1465],
        OutpostRaidSize::Medium => &[553, 564, 571, 603, 812, 1466],
        OutpostRaidSize::Moderate => &[554, 565, 571, 603, 812, 1466],
        OutpostRaidSize::LargeAdv => &[550, 560, 570, 604, 813, 1467],
    };
    let mut cands: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.faction_id == Some(faction_id)
                && target_groups.contains(&t.group_id)
                && t.default_attributes
                    .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
        })
        .map(|t| t.type_id)
        .collect();
    cands.sort_unstable();
    cands.first().copied().unwrap_or(fallback)
}

/// Returns the V24 SDE Reward Crate `TypeId` granted upon Laundering or War HQ Counter-Insurgency
/// hand-in so pilots always receive the matching SDE crate even outside itemTrader recipe calls.
pub fn resolve_laundering_reward_crate_type(sde: &SdeRegistry, trader_type_id: TypeId) -> TypeId {
    let preferred = match trader_type_id {
        TYPE_GURISTAS_LAUNDERING_FACILITY => 78_867,
        TYPE_ANGEL_LAUNDERING_FACILITY => 78_872,
        TYPE_FEDERATION_CI_OUTPOST_HUB => 78_873,
        TYPE_IMPERIAL_CI_OUTPOST_HUB => 78_874,
        TYPE_STATE_CI_OUTPOST_HUB => 79_117,
        _ => 79_118,
    };
    if sde.types.contains_key(&preferred) {
        preferred
    } else {
        78_867
    }
}

/// Emits live ship dogma `OnAttributes` update for Battlecruisers entering or leaving a Moderate
/// Insurgency Outpost pocket (`dbuff_id = 2544`: `+20%` per-level Battlecruiser skill bonus).
pub fn notify_moderate_outpost_bc_bonus(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_group_id: u32,
    active: bool,
) {
    if !is_battlecruiser_group(ship_group_id) {
        return;
    }
    let bc_skill_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .map(|skills| {
                [12_097_u32, 12_099, 12_098, 12_096, 33_096]
                    .iter()
                    .filter_map(|sid| skills.get(sid).map(|r| r.level))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    });
    let modified_attrs: usize = ctx
        .sde
        .dbuff_collections
        .get(&MODERATE_OUTPOST_BC_SKILL_DBUFF_ID)
        .map(|d| d.item_modifiers.len() + d.location_modifiers.len())
        .unwrap_or(0);
    let base_pct = if modified_attrs > 0 { 20.0 } else { 20.0 };
    let effective_bonus_pct = if active {
        base_pct * f64::from(bc_skill_lvl.max(1)) / 5.0
    } else {
        0.0
    };
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAttributes",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::Integer(ship_id),
            PyRep::Dict(vec![
                (
                    PyRep::String("moderateOutpostDbuffID".into()),
                    PyRep::Integer(if active {
                        i64::from(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID)
                    } else {
                        0
                    }),
                ),
                (
                    PyRep::String("battlecruiserRoleBonusPct".into()),
                    PyRep::Real(effective_bonus_pct),
                ),
            ]),
        )])],
    );
}

/// Emits militia LP change notification after a Laundering / War HQ Counter-Insurgency hand-in.
pub fn notify_insurgency_laundering_reward(
    ctx: &ServerContext,
    char_id: i64,
    war_faction_id: i64,
    sys_id: SolarSystemId,
) {
    let total_lp = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&war_faction_id).copied())
            .unwrap_or(0)
    });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLPChange",
        "charid",
        vec![
            PyRep::Integer(war_faction_id),
            PyRep::Integer(total_lp),
            PyRep::Integer(i64::from(sys_id)),
        ],
    );
}

/// Credits militia LP and insurgency contribution across aligned on-grid pilots.
pub fn reward_aligned_insurgency_pilots(
    ctx: &ServerContext,
    campaign_id: i64,
    sys_id: SolarSystemId,
    pilots: &[(i64, i64, i64, u32, bool)],
    shared_lp: i64,
    contrib_pts: i64,
    suppression_scaled: bool,
) {
    use crate::services::fw_gameplay::record_insurgency_contribution;
    use crate::services::insurgency_complex_ops::credit_militia_lp;
    use crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier;
    for &(cid, _, wf, _, _) in pilots {
        let mult = if suppression_scaled {
            insurgency_suppression_empire_lp_multiplier(ctx, sys_id, wf)
        } else {
            1.0
        };
        let lp = ((shared_lp as f64) * mult).round() as i64;
        credit_militia_lp(ctx, cid, wf, lp);
        record_insurgency_contribution(ctx, campaign_id, cid, wf, contrib_pts);
    }
}

/// Clears moderate battlecruiser dogma fits on Outpost Raid capture.
pub fn clear_moderate_outpost_fits(
    ctx: &ServerContext,
    prev_occ: &[(i64, i64)],
    bc_list: &[(i64, i64, u32)],
) {
    if !prev_occ.is_empty() {
        let mut fits = ctx.ship_fittings.write().unwrap_or_else(|e| e.into_inner());
        for (_, shid) in prev_occ {
            fits.remove(shid);
        }
        for &(cid, shid, sg) in bc_list {
            notify_moderate_outpost_bc_bonus(ctx, cid, shid, sg, false);
        }
    }
}

/// Collects aligned, uncloaked on-grid pilots within `range_m` of `anchor` for insurgency sites.
pub async fn collect_aligned_pilots_on_grid(
    ctx: &ServerContext,
    sys_id: u32,
    anchor: glam::DVec3,
    range_m: f64,
    allow_pirate_fac: Option<i64>,
    opposing_empire_for_pirate: Option<i64>,
) -> Vec<(i64, i64, i64, u32, bool)> {
    let candidates: Vec<(i64, i64, i64, u32, bool)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter_map(|c| {
                if c.solar_system_id != sys_id
                    || c.station_id.is_some()
                    || c.structure_id.is_some()
                    || c.active_ship_id <= 0
                {
                    return None;
                }
                let wf = c.war_faction_id?;
                let allowed = allow_pirate_fac == Some(wf)
                    || opposing_empire_for_pirate
                        .is_some_and(|pf| crate::services::insurgency_complex_ops::is_warzone_opposing_empire(pf, wf));
                if !allowed {
                    return None;
                }
                let stid = snap.items.get(&c.active_ship_id).map(|i| i.type_id).unwrap_or(0);
                if stid == 0 || stid == 670 {
                    return None;
                }
                let grp = ctx.sde.types.get(&stid).map(|t| t.group_id).unwrap_or(0);
                let has_wcs = snap.items.values().any(|it| {
                    it.location_id == c.active_ship_id
                        && (11..=19).contains(&(it.flag as u16))
                        && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 315)
                });
                Some((c.character_id, c.active_ship_id, wf, grp, has_wcs))
            })
            .collect()
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut out = Vec::new();
    for (cid, shid, wf, grp, has_wcs) in candidates {
        if sys_handle.get_combat_state(shid).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
            continue;
        }
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(shid).await {
            if !matches!(b.mode, eve_destiny::MovementMode::Warp { .. }) && b.position.distance(anchor) <= range_m {
                out.push((cid, shid, wf, grp, has_wcs));
            }
        }
    }
    out.sort_by_key(|&(cid, _, _, _, _)| cid);
    out
}

#[cfg(test)]
#[path = "insurgency_complex_ops_ext_edges_tests.rs"]
mod insurgency_complex_ops_ext_edges_tests;
