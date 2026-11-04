//! `industry_rigs` — Standup Structure Rig bonuses and security multipliers (`feat:industry.structure-rigs`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::InventoryFlag;
use eve_proto::BridgeRpcError;

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

pub fn structure_sde_industry_bonuses(ctx: &ServerContext, type_id: eve_sde::TypeId) -> (f64, f64, f64) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return (1.0, 1.0, 1.0);
    };
    let mat_mod = t.default_attributes.get(&2600).copied().unwrap_or(1.0);
    let time_mod = t
        .default_attributes
        .get(&2602)
        .copied()
        .or_else(|| t.default_attributes.get(&2721).copied())
        .unwrap_or(1.0);
    let cost_mod = t.default_attributes.get(&2601).copied().unwrap_or(1.0);
    (mat_mod, time_mod, cost_mod)
}

pub fn resolve_facility_modifiers_and_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<(eve_sde::TypeId, i64, u32, u32, f64, f64, f64)> {
    if let Some(st) = snap.structures.get(&facility_id) {
        if st.state == "Destroyed" {
            return None;
        }
        let region_id = ctx
            .sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|sys| sys.region_id)
            .unwrap_or(10000002);
        let (base_mat_mod, base_time_mod, cost_mod) =
            structure_sde_industry_bonuses(ctx, st.type_id);
        let (_, rig_time, rig_cost, _) =
            resolve_structure_rig_bonuses_full(ctx, snap, facility_id, st.solar_system_id);
        let eff_time_mod = base_time_mod * (1.0 - rig_time);
        let eff_cost_mod = cost_mod * (1.0 - rig_cost);
        return Some((
            st.type_id,
            st.owner_corp_id,
            st.solar_system_id,
            region_id,
            base_mat_mod,
            eff_time_mod,
            eff_cost_mod,
        ));
    }
    if let Some(it) = snap.items.get(&facility_id) {
        if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836) {
            let sys_id = if (30_000_000..40_000_000).contains(&it.location_id) {
                it.location_id as u32
            } else {
                30_000_142
            };
            let region_id = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|sys| sys.region_id)
                .unwrap_or(10000002);
            let (base_mat_mod, base_time_mod, cost_mod) =
                structure_sde_industry_bonuses(ctx, it.type_id);
            let (_, rig_time, rig_cost, _) =
                resolve_structure_rig_bonuses_full(ctx, snap, facility_id, sys_id);
            let eff_time_mod = base_time_mod * (1.0 - rig_time);
            let eff_cost_mod = cost_mod * (1.0 - rig_cost);
            return Some((
                it.type_id,
                it.owner_id,
                sys_id,
                region_id,
                base_mat_mod,
                eff_time_mod,
                eff_cost_mod,
            ));
        }
    }
    for sys in ctx.sde.solar_systems.values() {
        if let Some(station) = sys.stations.iter().find(|s| s.station_id == facility_id) {
            return Some((
                station.station_type_id,
                station.owner_corporation_id,
                sys.solar_system_id,
                sys.region_id,
                1.0,
                1.0,
                1.0,
            ));
        }
    }
    None
}

static SDE_ADJUSTED_PRICES: std::sync::RwLock<Option<std::collections::HashMap<eve_sde::TypeId, f64>>> =
    std::sync::RwLock::new(None);

/// Overrides or sets market adjusted prices cache (`feat:industry.eiv-adjusted-prices`).
pub fn set_sde_adjusted_prices(prices: std::collections::HashMap<eve_sde::TypeId, f64>) {
    *SDE_ADJUSTED_PRICES.write().unwrap() = Some(prices);
}

/// Sets an individual type's adjusted price in the cache (`feat:industry.eiv-adjusted-prices`).
pub fn set_sde_adjusted_price(type_id: eve_sde::TypeId, price: f64) {
    let mut guard = SDE_ADJUSTED_PRICES.write().unwrap();
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    map.insert(type_id, price);
}

/// Clears cached market adjusted prices (`feat:industry.eiv-adjusted-prices`).
pub fn clear_sde_adjusted_prices() {
    *SDE_ADJUSTED_PRICES.write().unwrap() = None;
}

/// Resolves a material's adjusted price for EIV calculation (`feat:industry.eiv-adjusted-prices`):
/// 1. Cached adjusted prices from `sde_adjusted_prices` / market price cache.
/// 2. 28-day clamped EMA over `snap.market_history` for `type_id` (matching `/markets/prices/`).
/// 3. SDE `base_price` fallback (or `10.0` if unset).
pub fn resolve_material_adjusted_price(
    ctx: &ServerContext,
    snap: Option<&eve_db::WorldStateSnapshot>,
    type_id: eve_sde::TypeId,
) -> f64 {
    if let Ok(guard) = SDE_ADJUSTED_PRICES.read() {
        if let Some(map) = guard.as_ref() {
            if let Some(&price) = map.get(&type_id) {
                if price > 0.0 {
                    return price;
                }
            }
        }
    }

    let calc_rolling = |s: &eve_db::WorldStateSnapshot| -> Option<f64> {
        let mut days: Vec<&eve_db::MarketHistoryDayRecord> = s
            .market_history
            .iter()
            .filter(|((_, tid), _)| *tid == type_id)
            .flat_map(|(_, v)| v.iter())
            .filter(|h| h.average > 0.0)
            .collect();
        if !days.is_empty() {
            days.sort_by(|a, b| a.date.cmp(&b.date));
            let recent_28 = &days[days.len().saturating_sub(28)..];
            let mut rolling_adj = recent_28[0].average;
            for d in &recent_28[1..] {
                let clamped = d.average.clamp(rolling_adj * 0.5, rolling_adj * 2.0);
                rolling_adj = (rolling_adj * 27.0 + clamped) / 28.0;
            }
            if rolling_adj > 0.0 {
                return Some(rolling_adj);
            }
        }
        None
    };

    let maybe_price = match snap {
        Some(s) => calc_rolling(s),
        None => ctx.db.with_snapshot(calc_rolling),
    };

    if let Some(price) = maybe_price {
        return price;
    }

    ctx.sde
        .types
        .get(&type_id)
        .map(|t| if t.base_price > 0.0 { t.base_price } else { 10.0 })
        .unwrap_or(10.0)
}

/// Validates capital and supercapital construction restrictions (`feat:industry.capital-construction-restrictions`).
pub fn validate_capital_construction_restrictions(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bp: &eve_sde::BlueprintInfo,
    activity_id: u8,
    fac_sys_id: u32,
    fac_type_id: eve_sde::TypeId,
    station_id: i64,
) -> Result<(), BridgeRpcError> {
    if activity_id != 1 {
        return Ok(());
    }
    let prod_grp = ctx
        .sde
        .types
        .get(&bp.product_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let fac_sec = ctx
        .sde
        .solar_systems
        .get(&fac_sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    if matches!(prod_grp, 659 | 30) {
        if fac_sec > 0.0 || fac_sys_id >= 31_000_000 {
            return Err(BridgeRpcError {
                code: "SupercapitalRequiresSovereignNullSec".into(),
                message: format!(
                    "Supercapitals require sovereign Null-Sec (system {fac_sys_id} sec {fac_sec:.2})"
                ),
            });
        }
        let has_sov = snap
            .system_states
            .get(&fac_sys_id)
            .and_then(|s| s.sov_alliance_id)
            .is_some();
        let has_supercap_upgrade = snap.items.values().any(|i| {
            i.location_id == fac_sys_id as i64
                && matches!(i.type_id, 81623 | 2009)
                && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
        });
        if !has_sov || !has_supercap_upgrade {
            return Err(BridgeRpcError {
                code: "SupercapitalRequiresSovereignNullSec".into(),
                message: format!(
                    "System {fac_sys_id} requires active sovereignty and an online Supercapital Construction Facilities upgrade"
                ),
            });
        }
        let has_supercap_shipyard = fac_type_id == 35827
            && snap.items.values().any(|i| {
                i.location_id == station_id
                    && (164..=171).contains(&(i.flag as u16))
                    && i.type_id == 35877
                    && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
            });
        if !has_supercap_shipyard {
            return Err(BridgeRpcError {
                code: "SupercapitalRequiresSotiyoSupercapitalShipyard".into(),
                message: format!(
                    "Supercapitals require a Sotiyo (35827) with an online Standup Supercapital Shipyard I (35877) at {station_id}"
                ),
            });
        }
    } else if matches!(prod_grp, 485 | 4594 | 547 | 5120 | 1538 | 883)
        && bp.product_type_id != 28606
    {
        if ctx.sde.solar_systems.get(&fac_sys_id).map(|s| s.region_id) == Some(10_000_070) {
            return Err(BridgeRpcError::user_error("CapitalConstructionForbiddenInPochven"));
        }
        if fac_sec >= 0.45 && fac_sys_id < 31_000_000 {
            return Err(BridgeRpcError {
                code: "CapitalConstructionForbiddenInHighSec".into(),
                message: format!(
                    "Capital ship construction is forbidden in High-Sec (system {fac_sys_id} sec {fac_sec:.2})"
                ),
            });
        }
        let has_cap_shipyard = matches!(fac_type_id, 35826 | 35827 | 35833 | 35834)
            && snap.items.values().any(|i| {
                i.location_id == station_id
                    && (164..=171).contains(&(i.flag as u16))
                    && matches!(i.type_id, 35881 | 35877)
                    && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
            });
        if !has_cap_shipyard {
            return Err(BridgeRpcError {
                code: "FacilityNotCapitalCapable".into(),
                message: format!(
                    "Facility {station_id} (type {fac_type_id}) requires an online Standup Capital Shipyard I (35881)"
                ),
            });
        }
    }
    Ok(())
}

