//! `industry_rigs` — Standup Structure Rig bonuses and security multipliers (`feat:industry.structure-rigs`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::InventoryFlag;

/// Computes Standup Structure Rig bonuses (`feat:industry.structure-rigs`, `feat:industry.structure-standup-rig-security-multipliers`):
/// - Manufacturing/Research/Invention rigs: High-Sec (`1.0x`), Low-Sec (`1.9x`), Null-Sec/WH (`2.1x`).
/// - Reaction rigs (`1933..=1939`, `2713`/`2714`, `1970` `disallowInHighSec`): High-Sec (`0.0x`), Low-Sec (`1.0x`), Null-Sec/WH (`1.1x`).
/// Returns `(rig_mat_bonus, rig_time_bonus, rig_cost_bonus, sec_multiplier)`.
pub fn resolve_structure_rig_bonuses_full(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> (f64, f64, f64, f64) {
    let sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let is_hs = sec >= 0.45 && solar_system_id < 31_000_000;
    let is_ls = sec > 0.0 && sec < 0.45 && solar_system_id < 31_000_000;
    let default_sec_mult = if is_hs {
        1.0
    } else if is_ls {
        1.9
    } else {
        2.1
    };

    let fitted_rigs: Vec<&eve_db::InventoryItemRecord> = snap
        .items
        .values()
        .filter(|it| {
            it.location_id == facility_id
                && matches!(
                    it.flag,
                    InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
                )
        })
        .collect();
    if fitted_rigs.is_empty() {
        return (0.0, 0.0, 0.0, default_sec_mult);
    }

    let norm_frac = |v: f64| -> f64 {
        let a = v.abs();
        if a > 1.0 { a / 100.0 } else { a }
    };

    let mut mat_bonus = 0.0_f64;
    let mut time_bonus = 0.0_f64;
    let mut cost_bonus = 0.0_f64;
    let mut active_sec_mult = default_sec_mult;

    for rig in fitted_rigs {
        let sde_type = ctx.sde.types.get(&rig.type_id);
        let is_t2 = sde_type
            .and_then(|t| t.default_attributes.get(&422).copied())
            .map(|tl| tl >= 2.0)
            .unwrap_or(false)
            || rig.dynamic_attributes.get(&422).copied().unwrap_or(1.0) >= 2.0;
        let is_reaction_rig = sde_type
            .map(|t| {
                (1933..=1939).contains(&t.group_id)
                    || t.default_attributes.get(&2713).copied().unwrap_or(0.0).abs() > 0.0
                    || t.default_attributes.get(&2714).copied().unwrap_or(0.0).abs() > 0.0
            })
            .unwrap_or(false);
        let disallow_hs = is_reaction_rig
            || sde_type
                .and_then(|t| t.default_attributes.get(&1970).copied())
                .unwrap_or(0.0)
                > 0.0;
        let rig_sec_mult = if is_hs {
            if disallow_hs {
                0.0
            } else {
                sde_type
                    .and_then(|t| t.default_attributes.get(&2355).copied())
                    .filter(|&v| v > 0.0)
                    .unwrap_or(1.0)
            }
        } else if is_ls {
            sde_type
                .and_then(|t| t.default_attributes.get(&2356).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(if is_reaction_rig { 1.0 } else { 1.9 })
        } else {
            sde_type
                .and_then(|t| t.default_attributes.get(&2357).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(if is_reaction_rig { 1.1 } else { 2.1 })
        };
        active_sec_mult = rig_sec_mult;

        let sde_mat = sde_type
            .and_then(|t| {
                t.default_attributes
                    .get(&2594)
                    .copied()
                    .filter(|v| v.abs() > 0.0)
                    .or_else(|| t.default_attributes.get(&2714).copied().filter(|v| v.abs() > 0.0))
            })
            .map(norm_frac);
        let sde_time = sde_type
            .and_then(|t| {
                t.default_attributes
                    .get(&2593)
                    .copied()
                    .filter(|v| v.abs() > 0.0)
                    .or_else(|| t.default_attributes.get(&2713).copied().filter(|v| v.abs() > 0.0))
            })
            .map(norm_frac);
        let sde_cost = sde_type
            .and_then(|t| t.default_attributes.get(&2595).copied().filter(|v| v.abs() > 0.0))
            .map(norm_frac);
        let has_any_sde_ind_attr = sde_mat.is_some() || sde_time.is_some() || sde_cost.is_some();

        let rig_mat = rig
            .dynamic_attributes
            .get(&2594)
            .or_else(|| rig.dynamic_attributes.get(&2714))
            .copied()
            .map(norm_frac)
            .or(sde_mat)
            .unwrap_or(if has_any_sde_ind_attr {
                0.0
            } else if is_t2 {
                0.024
            } else {
                0.02
            });
        let rig_time = rig
            .dynamic_attributes
            .get(&2593)
            .or_else(|| rig.dynamic_attributes.get(&2713))
            .copied()
            .map(norm_frac)
            .or(sde_time)
            .unwrap_or(if has_any_sde_ind_attr {
                0.0
            } else if is_t2 {
                0.24
            } else {
                0.20
            });
        let rig_cost = rig
            .dynamic_attributes
            .get(&2595)
            .copied()
            .map(norm_frac)
            .or(sde_cost)
            .unwrap_or(0.0);

        mat_bonus = mat_bonus.max((rig_mat * rig_sec_mult).clamp(0.0, 0.50));
        time_bonus = time_bonus.max((rig_time * rig_sec_mult).clamp(0.0, 0.80));
        cost_bonus = cost_bonus.max((rig_cost * rig_sec_mult).clamp(0.0, 0.50));
    }

    (mat_bonus, time_bonus, cost_bonus, active_sec_mult)
}

pub fn resolve_structure_rig_bonus(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> (f64, f64, f64) {
    let (mat, time, _, sec_mult) =
        resolve_structure_rig_bonuses_full(ctx, snap, facility_id, solar_system_id);
    (mat, time, sec_mult)
}
