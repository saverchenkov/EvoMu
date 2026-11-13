//! `industry_rigs` — Standup Structure Rig bonuses and security multipliers (`feat:industry.structure-rigs`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::InventoryFlag;
use eve_proto::BridgeRpcError;

/// Resolves the output product `TypeId` produced by `bp` under `activity_id` (`1..=11`).
pub fn resolve_blueprint_activity_product(
    bp: &eve_sde::BlueprintInfo,
    activity_id: u8,
) -> Option<eve_sde::TypeId> {
    let prod_from_act = match activity_id {
        9 | 11 => bp
            .activities
            .get("reaction")
            .or_else(|| bp.activities.get("9")),
        8 => bp
            .activities
            .get("invention")
            .or_else(|| bp.activities.get("8")),
        _ => bp
            .activities
            .get("manufacturing")
            .or_else(|| bp.activities.get("1")),
    }
    .and_then(|a| a.products.first())
    .map(|p| p.type_id)
    .filter(|&tid| tid != 0);
    prod_from_act.or_else(|| (bp.product_type_id != 0).then_some(bp.product_type_id))
}

/// Computes Standup Structure Rig bonuses (`feat:industry.structure-rigs`, `feat:industry.structure-standup-rig-security-multipliers`)
/// using the SDE industry tables (`industry_activity_modifier_sources.static` + `industry_activity_target_filters.static` +
/// `sde_dogma_effects`):
/// - Evaluates `(activity_id, product_type_id)` against each fitted rig's modifier entries and target filters,
///   so specialized rigs (e.g. Ammunition ME, Basic Small Ship ME, Biochemical Reactor ME) grant bonuses ONLY
///   to matching activities and product categories/groups, and non-industry rigs grant `0.0`.
/// - Manufacturing/Research/Invention rigs: High-Sec (`1.0x`), Low-Sec (`1.9x`), Null-Sec/WH (`2.1x`).
/// - Reaction rigs (`1933..=1939`, `2713`/`2714`, `1970` `disallowInHighSec`): High-Sec (`0.0x`), Low-Sec (`1.0x`), Null-Sec/WH (`1.1x`).
/// Resolved structure industry rig bonuses (`mat_bonus`, `time_bonus`, `cost_bonus`, `sec_multiplier`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StructureRigBonuses {
    pub mat_bonus: f64,
    pub time_bonus: f64,
    pub cost_bonus: f64,
    pub sec_multiplier: f64,
}

/// Legacy 3-field view of structure industry rig bonuses (`mat_bonus`, `time_bonus`, `sec_multiplier`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StructureRigBonus {
    pub mat_bonus: f64,
    pub time_bonus: f64,
    pub sec_multiplier: f64,
}

/// Resolved Upwell structure hull industry modifiers (`material_modifier`, `time_modifier`, `cost_modifier`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StructureIndustryBonuses {
    pub material_modifier: f64,
    pub time_modifier: f64,
    pub cost_modifier: f64,
}

/// Resolved facility metadata and effective industry modifiers for a job/facility lookup.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FacilityModifiersAndSystem {
    pub type_id: eve_sde::TypeId,
    pub owner_corp_id: i64,
    pub solar_system_id: u32,
    pub region_id: u32,
    pub material_modifier: f64,
    pub time_modifier: f64,
    pub cost_modifier: f64,
}

/// - Outpost Conversion rigs (`group_id == 1984`): flat `1.0x` security multiplier.
/// Returns `StructureRigBonuses { mat_bonus, time_bonus, cost_bonus, sec_multiplier }`.
pub fn resolve_structure_rig_bonuses_for_job(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
    activity_id: Option<u8>,
    product_type_id: Option<eve_sde::TypeId>,
) -> StructureRigBonuses {
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
        return StructureRigBonuses {
            mat_bonus: 0.0,
            time_bonus: 0.0,
            cost_bonus: 0.0,
            sec_multiplier: default_sec_mult,
        };
    }

    let norm_frac = |v: f64| -> f64 {
        let a = v.abs();
        if a > 1.0 { a / 100.0 } else { a }
    };

    let req_act_key = match activity_id {
        Some(aid) => match ctx.sde.industry_activity_key(aid) {
            Some(k) => Some(k),
            None => {
                return StructureRigBonuses {
                    mat_bonus: 0.0,
                    time_bonus: 0.0,
                    cost_bonus: 0.0,
                    sec_multiplier: default_sec_mult,
                };
            }
        },
        None => None,
    };

    let mut mat_bonus = 0.0_f64;
    let mut time_bonus = 0.0_f64;
    let mut cost_bonus = 0.0_f64;
    let mut active_sec_mult = default_sec_mult;

    for rig in fitted_rigs {
        let sde_type = ctx.sde.types.get(&rig.type_id);
        let is_outpost_conversion_rig = sde_type.is_some_and(|t| t.group_id == 1984);
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
        } else if is_outpost_conversion_rig {
            1.0
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

        let mut rig_mat = 0.0_f64;
        let mut rig_time = 0.0_f64;
        let mut rig_cost = 0.0_f64;

        if let (Some(rig_t), Some(mod_sources)) = (
            sde_type,
            ctx.sde.industry_modifier_sources.get(&rig.type_id),
        ) {
            for (act_key, kind_map) in mod_sources {
                if let Some(req_k) = req_act_key {
                    if act_key != req_k {
                        continue;
                    }
                }
                let eval_kind = |kind: &str| -> f64 {
                    kind_map
                        .get(kind)
                        .into_iter()
                        .flat_map(|entries| entries.iter())
                        .filter(|e| {
                            ctx.sde
                                .industry_target_filter_matches(e.filter_id, product_type_id)
                        })
                        .filter_map(|e| {
                            ctx.sde.rig_dogma_source_attribute(
                                rig_t,
                                Some(&rig.dynamic_attributes),
                                e.dogma_attribute_id,
                            )
                        })
                        .filter(|v| v.abs() > 0.0)
                        .map(norm_frac)
                        .fold(0.0_f64, f64::max)
                };
                rig_mat = rig_mat.max(eval_kind("material"));
                rig_time = rig_time.max(eval_kind("time"));
                rig_cost = rig_cost.max(eval_kind("cost"));
            }
        } else {
            // Synthetic unit-test rig types not indexed in `industry_modifier_sources`:
            // honor only explicit industry Dogma attributes (`2593..=2595`, `2713..=2714`) with
            // zero hardcoded T1/T2 percentage fallbacks.
            let allow_eng = matches!(req_act_key, None | Some("manufacturing" | "research_time" | "research_material" | "copying" | "invention"));
            let allow_react = matches!(req_act_key, None | Some("reaction"));
            let read_explicit = |aid: u16| -> Option<f64> {
                rig.dynamic_attributes
                    .get(&aid)
                    .copied()
                    .or_else(|| sde_type.and_then(|t| t.default_attributes.get(&aid).copied()))
                    .filter(|v| v.abs() > 0.0)
                    .map(norm_frac)
            };
            if allow_eng {
                rig_mat = rig_mat.max(read_explicit(2594).unwrap_or(0.0));
                rig_time = rig_time.max(read_explicit(2593).unwrap_or(0.0));
                rig_cost = rig_cost.max(read_explicit(2595).unwrap_or(0.0));
            }
            if allow_react {
                rig_mat = rig_mat.max(read_explicit(2714).unwrap_or(0.0));
                rig_time = rig_time.max(read_explicit(2713).unwrap_or(0.0));
            }
        }

        mat_bonus = mat_bonus.max((rig_mat * rig_sec_mult).clamp(0.0, 0.50));
        time_bonus = time_bonus.max((rig_time * rig_sec_mult).clamp(0.0, 0.80));
        cost_bonus = cost_bonus.max((rig_cost * rig_sec_mult).clamp(0.0, 0.50));
    }

    StructureRigBonuses {
        mat_bonus,
        time_bonus,
        cost_bonus,
        sec_multiplier: active_sec_mult,
    }
}

pub fn resolve_structure_rig_bonuses_full(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> StructureRigBonuses {
    resolve_structure_rig_bonuses_for_job(ctx, snap, facility_id, solar_system_id, None, None)
}

pub fn resolve_structure_rig_bonus(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> StructureRigBonus {
    let full = resolve_structure_rig_bonuses_full(ctx, snap, facility_id, solar_system_id);
    StructureRigBonus {
        mat_bonus: full.mat_bonus,
        time_bonus: full.time_bonus,
        sec_multiplier: full.sec_multiplier,
    }
}

/// Resolves Upwell structure hull role bonuses (`2600` ME, `2602`/`2721` TE, `2601` job cost)
/// scoped to `activity_id` via `industry_activity_modifier_sources.static` (so e.g. Tatara's `-25%`
/// reaction time modifier `2721` applies to Reactions and not Manufacturing).
pub fn structure_sde_industry_bonuses_for_activity(
    ctx: &ServerContext,
    type_id: eve_sde::TypeId,
    activity_id: Option<u8>,
) -> StructureIndustryBonuses {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return StructureIndustryBonuses {
            material_modifier: 1.0,
            time_modifier: 1.0,
            cost_modifier: 1.0,
        };
    };
    if let Some(mod_sources) = ctx.sde.industry_modifier_sources.get(&type_id) {
        if let Some(aid) = activity_id {
            let Some(act_key) = ctx.sde.industry_activity_key(aid) else {
                return StructureIndustryBonuses {
                    material_modifier: 1.0,
                    time_modifier: 1.0,
                    cost_modifier: 1.0,
                };
            };
            let Some(kind_map) = mod_sources.get(act_key) else {
                return StructureIndustryBonuses {
                    material_modifier: 1.0,
                    time_modifier: 1.0,
                    cost_modifier: 1.0,
                };
            };
            let read_hull_mod = |kind: &str| -> f64 {
                kind_map
                    .get(kind)
                    .and_then(|entries| entries.first())
                    .and_then(|e| t.default_attributes.get(&e.dogma_attribute_id).copied())
                    .unwrap_or(1.0)
            };
            return StructureIndustryBonuses {
                material_modifier: read_hull_mod("material"),
                time_modifier: read_hull_mod("time"),
                cost_modifier: read_hull_mod("cost"),
            };
        }
    }
    let allow_react = matches!(activity_id, None | Some(9 | 11));
    let allow_eng = !matches!(activity_id, Some(9 | 11));
    let mat_mod = if allow_eng {
        t.default_attributes.get(&2600).copied().unwrap_or(1.0)
    } else {
        1.0
    };
    let time_mod = if allow_eng {
        t.default_attributes
            .get(&2602)
            .copied()
            .or_else(|| {
                allow_react
                    .then(|| t.default_attributes.get(&2721).copied())
                    .flatten()
            })
            .unwrap_or(1.0)
    } else {
        t.default_attributes.get(&2721).copied().unwrap_or(1.0)
    };
    let cost_mod = if allow_eng {
        t.default_attributes.get(&2601).copied().unwrap_or(1.0)
    } else {
        1.0
    };
    StructureIndustryBonuses {
        material_modifier: mat_mod,
        time_modifier: time_mod,
        cost_modifier: cost_mod,
    }
}

pub fn structure_sde_industry_bonuses(
    ctx: &ServerContext,
    type_id: eve_sde::TypeId,
) -> StructureIndustryBonuses {
    structure_sde_industry_bonuses_for_activity(ctx, type_id, None)
}

pub fn resolve_facility_modifiers_and_system_for_job(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    activity_id: Option<u8>,
    product_type_id: Option<eve_sde::TypeId>,
) -> Option<FacilityModifiersAndSystem> {
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
        let hull = structure_sde_industry_bonuses_for_activity(ctx, st.type_id, activity_id);
        let rigs = resolve_structure_rig_bonuses_for_job(
            ctx,
            snap,
            facility_id,
            st.solar_system_id,
            activity_id,
            product_type_id,
        );
        let eff_time_mod = hull.time_modifier * (1.0 - rigs.time_bonus);
        let eff_cost_mod = hull.cost_modifier * (1.0 - rigs.cost_bonus);
        return Some(FacilityModifiersAndSystem {
            type_id: st.type_id,
            owner_corp_id: st.owner_corp_id,
            solar_system_id: st.solar_system_id,
            region_id,
            material_modifier: hull.material_modifier,
            time_modifier: eff_time_mod,
            cost_modifier: eff_cost_mod,
        });
    }
    if let Some(it) = snap.items.get(&facility_id) {
        if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836 | 47512..=47516) {
            let sys_id = if (30_000_000..40_000_000).contains(&it.location_id) {
                it.location_id as u32
            } else {
                snap.structures
                    .get(&facility_id)
                    .or_else(|| snap.structures.get(&it.location_id))
                    .map(|s| s.solar_system_id)
                    .or_else(|| ctx.sde.location_solar_system(it.location_id))
                    .or_else(|| snap.characters.get(&it.owner_id).map(|c| c.solar_system_id))
                    .or_else(|| snap.characters.values().find(|c| c.corporation_id == it.owner_id).map(|c| c.solar_system_id))
                    .or_else(|| snap.characters.values().next().map(|c| c.solar_system_id))
                    .unwrap_or(0)
            };
            let region_id = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|sys| sys.region_id)
                .unwrap_or(0);
            let hull = structure_sde_industry_bonuses_for_activity(ctx, it.type_id, activity_id);
            let rigs = resolve_structure_rig_bonuses_for_job(
                ctx,
                snap,
                facility_id,
                sys_id,
                activity_id,
                product_type_id,
            );
            let eff_time_mod = hull.time_modifier * (1.0 - rigs.time_bonus);
            let eff_cost_mod = hull.cost_modifier * (1.0 - rigs.cost_bonus);
            return Some(FacilityModifiersAndSystem {
                type_id: it.type_id,
                owner_corp_id: it.owner_id,
                solar_system_id: sys_id,
                region_id,
                material_modifier: hull.material_modifier,
                time_modifier: eff_time_mod,
                cost_modifier: eff_cost_mod,
            });
        }
    }
    if let Some(station) = ctx.sde.stations.get(&facility_id) {
        return Some(FacilityModifiersAndSystem {
            type_id: station.station_type_id,
            owner_corp_id: station.owner_corporation_id,
            solar_system_id: station.solar_system_id,
            region_id: station.region_id,
            material_modifier: 1.0,
            time_modifier: 1.0,
            cost_modifier: 1.0,
        });
    }
    for sys in ctx.sde.solar_systems.values() {
        if let Some(station) = sys.stations.iter().find(|s| s.station_id == facility_id) {
            return Some(FacilityModifiersAndSystem {
                type_id: station.station_type_id,
                owner_corp_id: station.owner_corporation_id,
                solar_system_id: sys.solar_system_id,
                region_id: sys.region_id,
                material_modifier: 1.0,
                time_modifier: 1.0,
                cost_modifier: 1.0,
            });
        }
    }
    None
}

pub fn resolve_facility_modifiers_and_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<FacilityModifiersAndSystem> {
    resolve_facility_modifiers_and_system_for_job(ctx, snap, facility_id, None, None)
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

/// Computes all active rolling market adjusted prices (`feat:industry.eiv-adjusted-prices`, `feat:esi.market.prices`)
/// from the explicit adjusted-price cache and 28-day clamped rolling EMA over executed `snap.market_history`.
pub fn collect_live_market_adjusted_prices(
    snap: &eve_db::WorldStateSnapshot,
) -> std::collections::HashMap<eve_sde::TypeId, f64> {
    let mut hist_by_type: std::collections::HashMap<
        eve_sde::TypeId,
        Vec<&eve_db::MarketHistoryDayRecord>,
    > = std::collections::HashMap::new();
    for ((_reg_id, tid), days) in &snap.market_history {
        hist_by_type
            .entry(*tid)
            .or_default()
            .extend(days.iter().filter(|h| h.average > 0.0));
    }
    let mut out = std::collections::HashMap::with_capacity(hist_by_type.len());
    for (tid, mut days) in hist_by_type {
        if days.is_empty() {
            continue;
        }
        days.sort_by(|a, b| a.date.cmp(&b.date));
        let recent_28 = &days[days.len().saturating_sub(28)..];
        let mut rolling_adj = recent_28[0].average;
        for d in &recent_28[1..] {
            let clamped = d.average.clamp(rolling_adj * 0.5, rolling_adj * 2.0);
            rolling_adj = (rolling_adj * 27.0 + clamped) / 28.0;
        }
        if rolling_adj > 0.0 {
            out.insert(tid, rolling_adj);
        }
    }
    if let Ok(guard) = SDE_ADJUSTED_PRICES.read() {
        if let Some(map) = guard.as_ref() {
            for (&tid, &price) in map {
                if price > 0.0 {
                    out.insert(tid, price);
                }
            }
        }
    }
    out
}

/// Resolves a type's live rolling market adjusted price if trade history or cache exists (`None` if untraded).
pub fn compute_market_adjusted_price_from_snap(
    snap: &eve_db::WorldStateSnapshot,
    type_id: eve_sde::TypeId,
) -> Option<f64> {
    if let Ok(guard) = SDE_ADJUSTED_PRICES.read() {
        if let Some(map) = guard.as_ref() {
            if let Some(&price) = map.get(&type_id) {
                if price > 0.0 {
                    return Some(price);
                }
            }
        }
    }
    let mut days: Vec<&eve_db::MarketHistoryDayRecord> = snap
        .market_history
        .iter()
        .filter(|((_, tid), _)| *tid == type_id)
        .flat_map(|(_, v)| v.iter())
        .filter(|h| h.average > 0.0)
        .collect();
    if days.is_empty() {
        return None;
    }
    days.sort_by(|a, b| a.date.cmp(&b.date));
    let recent_28 = &days[days.len().saturating_sub(28)..];
    let mut rolling_adj = recent_28[0].average;
    for d in &recent_28[1..] {
        let clamped = d.average.clamp(rolling_adj * 0.5, rolling_adj * 2.0);
        rolling_adj = (rolling_adj * 27.0 + clamped) / 28.0;
    }
    (rolling_adj > 0.0).then_some(rolling_adj)
}

/// Resolves a material's adjusted price for EIV calculation (`feat:industry.eiv-adjusted-prices`):
/// 1. Cached adjusted prices from `sde_adjusted_prices` / market price cache.
/// 2. 28-day clamped EMA over `snap.market_history` for `type_id` (shared with `/markets/prices/` and insurance).
/// 3. SDE `base_price` baseline when no live trades have executed yet.
pub fn resolve_material_adjusted_price(
    ctx: &ServerContext,
    snap: Option<&eve_db::WorldStateSnapshot>,
    type_id: eve_sde::TypeId,
) -> f64 {
    let maybe_price = match snap {
        Some(s) => compute_market_adjusted_price_from_snap(s, type_id),
        None => ctx
            .db
            .with_snapshot(|s| compute_market_adjusted_price_from_snap(s, type_id)),
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

/// Computes ship insurance reference value incorporating live rolling market adjusted prices
/// of the ship's ME-0 manufacturing material basket (shared with `/markets/prices` and blueprint EIV).
pub fn compute_ship_insurance_reference_value_with_prices(
    sde: &eve_sde::SdeRegistry,
    live_prices: &std::collections::HashMap<eve_sde::TypeId, f64>,
    ship_type_id: eve_sde::TypeId,
) -> f64 {
    let base_hull_price = crate::formulas::compute_ship_insurance_reference_value(sde, ship_type_id);
    if live_prices.is_empty() {
        return base_hull_price;
    }
    if let Some(bp) = sde
        .blueprints
        .values()
        .filter(|b| b.product_type_id == ship_type_id)
        .min_by_key(|b| b.blueprint_type_id)
    {
        let mats: Vec<(eve_sde::TypeId, u64)> = if !bp.materials.is_empty() {
            bp.materials.iter().map(|m| (m.type_id, m.quantity)).collect()
        } else {
            bp.activities
                .get("manufacturing")
                .or_else(|| bp.activities.get("1"))
                .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
                .unwrap_or_default()
        };
        if !mats.is_empty() && mats.iter().any(|(tid, _)| live_prices.contains_key(tid)) {
            let mut base_sum = 0.0_f64;
            let mut live_sum = 0.0_f64;
            for (tid, qty) in mats {
                let b_px = sde
                    .types
                    .get(&tid)
                    .map(|t| if t.base_price > 0.0 { t.base_price } else { 10.0 })
                    .unwrap_or(10.0);
                let l_px = live_prices.get(&tid).copied().unwrap_or(b_px);
                base_sum += (qty as f64) * b_px;
                live_sum += (qty as f64) * l_px;
            }
            if base_sum > 0.0 && live_sum > 0.0 {
                return if base_hull_price > 0.0 {
                    base_hull_price * (live_sum / base_sum)
                } else {
                    live_sum
                };
            }
        }
    }
    if let Some(&hull_adj) = live_prices.get(&ship_type_id) {
        if hull_adj > 0.0 {
            return hull_adj;
        }
    }
    base_hull_price
}

pub fn compute_ship_insurance_reference_value_for_ctx(
    ctx: &ServerContext,
    ship_type_id: eve_sde::TypeId,
) -> f64 {
    ctx.db.with_snapshot(|snap| {
        let live_prices = collect_live_market_adjusted_prices(snap);
        compute_ship_insurance_reference_value_with_prices(&ctx.sde, &live_prices, ship_type_id)
    })
}

pub fn is_station_container_type(type_id: eve_sde::TypeId) -> bool {
    matches!(
        type_id,
        23 | 3293
            | 3296
            | 3297
            | 3465..=3467
            | 11488
            | 11489
            | 17363..=17368
            | 24445
            | 33003..=33011
            | 41567
    )
}

pub fn is_valid_industry_output_flag(
    flag: eve_db::InventoryFlag,
    is_item_container: bool,
    use_corp: bool,
) -> bool {
    if is_item_container {
        return matches!(
            flag,
            eve_db::InventoryFlag::Unlocked | eve_db::InventoryFlag::Hangar
        );
    }
    match flag {
        eve_db::InventoryFlag::Hangar | eve_db::InventoryFlag::Deliveries => true,
        eve_db::InventoryFlag::CorpDeliveries
        | eve_db::InventoryFlag::CorpSAG1
        | eve_db::InventoryFlag::CorpSAG2
        | eve_db::InventoryFlag::CorpSAG3
        | eve_db::InventoryFlag::CorpSAG4
        | eve_db::InventoryFlag::CorpSAG5
        | eve_db::InventoryFlag::CorpSAG6
        | eve_db::InventoryFlag::CorpSAG7 => use_corp,
        _ => false,
    }
}

pub fn is_industry_loc_at_facility(
    snap: &eve_db::WorldStateSnapshot,
    loc_id: i64,
    station_id: i64,
    _docked_id: i64,
    char_id: i64,
    use_corp: bool,
    job_owner_id: i64,
) -> bool {
    if loc_id == station_id {
        return true;
    }
    let Some(parent) = snap.items.get(&loc_id) else {
        return false;
    };
    if parent.owner_id != char_id && !(use_corp && parent.owner_id == job_owner_id) {
        return false;
    }
    if parent.type_id == 60
        || matches!(
            parent.flag,
            eve_db::InventoryFlag::Impounded | eve_db::InventoryFlag::Locked
        )
    {
        return false;
    }
    if parent.location_id == station_id {
        return true;
    }
    snap.items.get(&parent.location_id).is_some_and(|gp| {
        (gp.owner_id == char_id || (use_corp && gp.owner_id == job_owner_id))
            && gp.type_id != 60
            && !matches!(
                gp.flag,
                eve_db::InventoryFlag::Impounded | eve_db::InventoryFlag::Locked
            )
            && gp.location_id == station_id
    })
}

pub fn resolve_corp_office_at_facility(
    snap: &eve_db::WorldStateSnapshot,
    use_corp: bool,
    job_owner_id: i64,
    station_id: i64,
) -> Option<i64> {
    if !use_corp {
        return None;
    }
    snap.corp_governance
        .get(&job_owner_id)
        .and_then(|g| g.offices.get(&station_id).copied())
        .filter(|id| snap.items.contains_key(id))
        .or_else(|| {
            snap.items
                .values()
                .find(|i| {
                    i.owner_id == job_owner_id && i.location_id == station_id && i.type_id == 27
                })
                .map(|i| i.item_id)
        })
}

pub fn resolve_industry_delivery_target(
    snap: &eve_db::WorldStateSnapshot,
    job: &eve_db::IndustryJobRecord,
    meta: &crate::services::industry_pi::IndustryJobCostMeta,
    char_id: i64,
    out_owner_id: i64,
    fallback_loc: i64,
) -> (i64, eve_db::InventoryFlag) {
    let safe_fallback = if fallback_loc == job.facility_id {
        job.facility_id
    } else {
        match snap.items.get(&fallback_loc) {
            Some(it)
                if (it.owner_id == char_id
                    || (meta.use_corp && it.owner_id == out_owner_id))
                    && it.type_id != 60
                    && !matches!(
                        it.flag,
                        eve_db::InventoryFlag::Impounded | eve_db::InventoryFlag::Locked
                    )
                    && is_industry_loc_at_facility(
                        snap,
                        it.location_id,
                        job.facility_id,
                        job.facility_id,
                        char_id,
                        meta.use_corp,
                        out_owner_id,
                    ) =>
            {
                if it.type_id == 27 {
                    job.facility_id
                } else if is_station_container_type(it.type_id) && it.singleton {
                    fallback_loc
                } else {
                    job.facility_id
                }
            }
            _ => job.facility_id,
        }
    };
    let raw_target = if job.output_location_id > 0 {
        job.output_location_id
    } else if meta.output_location_id > 0 {
        meta.output_location_id
    } else {
        safe_fallback
    };
    let target_loc = if raw_target == job.facility_id {
        job.facility_id
    } else {
        match snap.items.get(&raw_target) {
            Some(it)
                if (it.owner_id == char_id
                    || (meta.use_corp && it.owner_id == out_owner_id))
                    && it.type_id != 60
                    && !matches!(
                        it.flag,
                        eve_db::InventoryFlag::Impounded | eve_db::InventoryFlag::Locked
                    )
                    && is_industry_loc_at_facility(
                        snap,
                        it.location_id,
                        job.facility_id,
                        job.facility_id,
                        char_id,
                        meta.use_corp,
                        out_owner_id,
                    ) =>
            {
                if it.type_id == 27 {
                    job.facility_id
                } else if is_station_container_type(it.type_id) && it.singleton {
                    raw_target
                } else {
                    safe_fallback
                }
            }
            _ => safe_fallback,
        }
    };
    let is_item_container = snap
        .items
        .get(&target_loc)
        .is_some_and(|it| is_station_container_type(it.type_id) && it.singleton);
    let default_flag = if is_item_container {
        eve_db::InventoryFlag::Unlocked
    } else if meta.use_corp && out_owner_id != char_id {
        crate::services::industry_pi::restore_blueprint_flag(out_owner_id, meta)
    } else {
        eve_db::InventoryFlag::Hangar
    };
    let target_flag = match meta.output_flag {
        Some(f) if is_valid_industry_output_flag(f, is_item_container, meta.use_corp && out_owner_id != char_id) => {
            if is_item_container {
                eve_db::InventoryFlag::Unlocked
            } else {
                f
            }
        }
        _ => default_flag,
    };
    (target_loc, target_flag)
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
    let (prod_cat, prod_grp) = ctx
        .sde
        .types
        .get(&bp.product_type_id)
        .map(|t| (t.category_id, t.group_id))
        .unwrap_or((0, 0));
    let service_accepts_ship = |mod_tid: eve_sde::TypeId, fallback_ok: bool| -> bool {
        let lines = ctx
            .sde
            .installation_assembly_lines_for_activity(mod_tid, 1);
        if lines.is_empty() {
            fallback_ok
        } else {
            lines.iter().any(|l| l.accepts_product(prod_cat, prod_grp))
        }
    };
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
                client_error: None,
            });
        }
        let has_sov = snap
            .system_states
            .get(&fac_sys_id)
            .and_then(|s| s.sov_alliance_id)
            .is_some();
        let is_mod_online = |i: &eve_db::InventoryItemRecord| -> bool {
            snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
        };
        let has_supercap_upgrade = snap.items.values().any(|i| {
            matches!(i.type_id, 81623 | 2009)
                && is_mod_online(i)
                && (i.location_id == fac_sys_id as i64
                    || snap.structures.get(&i.location_id).is_some_and(|st| {
                        st.solar_system_id == fac_sys_id && st.state != "Destroyed"
                    }))
        });
        if !has_sov || !has_supercap_upgrade {
            return Err(BridgeRpcError {
                code: "SupercapitalRequiresSovereignNullSec".into(),
                message: format!(
                    "System {fac_sys_id} requires active sovereignty and an online Supercapital Construction Facilities upgrade"
                ),
                client_error: None,
            });
        }
        let has_supercap_shipyard = fac_type_id == 35827
            && snap.items.values().any(|i| {
                i.location_id == station_id
                    && (164..=171).contains(&(i.flag as u16))
                    && service_accepts_ship(i.type_id, i.type_id == 35877)
                    && is_mod_online(i)
            });
        if !has_supercap_shipyard {
            return Err(BridgeRpcError {
                code: "SupercapitalRequiresSotiyoSupercapitalShipyard".into(),
                message: format!(
                    "Supercapitals require a Sotiyo (35827) with an online Standup Supercapital Shipyard I (35877) at {station_id}"
                ),
                client_error: None,
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
                client_error: None,
            });
        }
        let has_cap_shipyard = matches!(fac_type_id, 35826 | 35827 | 35833 | 35834)
            && snap.items.values().any(|i| {
                i.location_id == station_id
                    && (164..=171).contains(&(i.flag as u16))
                    && service_accepts_ship(i.type_id, matches!(i.type_id, 35881 | 35877))
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                    && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
            });
        if !has_cap_shipyard {
            return Err(BridgeRpcError {
                code: "FacilityNotCapitalCapable".into(),
                message: format!(
                    "Facility {station_id} (type {fac_type_id}) requires an online Standup Capital Shipyard I (35881)"
                ),
                client_error: None,
            });
        }
    }
    Ok(())
}

/// Computes the Process Time Value (PTV) EIV multiplier across `runs` starting from `start_step`
/// (`me_level` for ME research, `te_level / 2` for TE research):
/// - Manufacturing / Reactions (`1 | 9`): `runs`
/// - Copying / Invention (`5 | 8`): `0.02 * runs`
/// - ME / TE Research (`3 | 4`): `0.02 * sum(LVL_SECS[start_step..start_step + runs] / 105.0)`
pub fn compute_activity_ptv_multiplier(activity_id: u8, start_step: usize, runs: u32) -> f64 {
    const LVL_SECS: [u64; 10] = [105, 250, 595, 1_414, 3_360, 8_000, 19_000, 45_255, 107_700, 256_000];
    match activity_id {
        3 | 4 => {
            let s = start_step.min(10);
            let e = (s + runs as usize).min(10);
            let sum_secs: u64 = LVL_SECS[s..e].iter().sum();
            0.02 * (sum_secs as f64) / 105.0
        }
        5 | 8 => 0.02 * (runs as f64),
        _ => runs as f64,
    }
}

#[cfg(test)]
#[path = "industry_rigs_tests.rs"]
mod industry_rigs_tests;


