//! `mining_cycle` — repeating mining-module cycle engine driven by the server clock (`MN-04`).
//!
//! `dogmaIM.Activate` on a fitted mining module validates the requirements and registers an
//! [`ActiveMiningCycle`]. Ore is delivered only when a cycle *completes* (wiki:Mining_yield
//! "Short-cycling": "the ore/ice/gas is only harvested at the end of the module's cycle"), so a
//! cycle is resolved by [`process_mining_cycles`], which the server's world tick calls every
//! second and which compares the cycle start against `ServerContext::clock()`.
//!
//! Every number comes from SDE dogma attributes evaluated through the fitting engine (`DG-1`
//! `modifierInfo`): skills, hull bonuses, upgrades, implants and charges all reach the module's
//! effective `miningAmount` (77), `duration` (73), `maxRange` (54), `capacitorNeed` (6),
//! `miningCritChance` (5967), `miningCritBonusYield` (5969), `miningWasteProbability` (3154) and
//! `miningWastedVolumeMultiplier` (3153), and the hull's effective hold capacities.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::InventoryFlag;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;

use crate::router::{NotificationTarget, ServerContext};

/// SDE dogma attribute IDs read by the cycle engine (names from `sde_dogma_attributes`).
mod attr {
    pub const CAPACITOR_NEED: u16 = 6;
    pub const CAPACITY: u16 = 38;
    pub const MAX_RANGE: u16 = 54;
    pub const DURATION: u16 = 73;
    pub const MINING_AMOUNT: u16 = 77;
    pub const REQUIRED_SKILL_1: u16 = 182;
    pub const REQUIRED_SKILL_2: u16 = 183;
    pub const REQUIRED_SKILL_3: u16 = 184;
    pub const GENERAL_MINING_HOLD: u16 = 1556;
    pub const SPECIAL_GAS_HOLD: u16 = 1557;
    pub const SPECIAL_ICE_HOLD: u16 = 3136;
    pub const WASTED_VOLUME_MULT: u16 = 3153;
    pub const WASTE_PROBABILITY: u16 = 3154;
    pub const CRIT_CHANCE: u16 = 5967;
    pub const HP: u16 = 9;
    pub const DAMAGE: u16 = 3;
    pub const CRYSTAL_VOLATILITY_CHANCE: u16 = 783;
    pub const CRYSTAL_VOLATILITY_DAMAGE: u16 = 784;
    pub const CRYSTALS_GET_DAMAGED: u16 = 786;
    pub const SPECIALIZATION_TYPE_LIST: u16 = 3148;
    pub const CRIT_BONUS_YIELD: u16 = 5969;
}

/// SDE `categoryID` of asteroid types (ore, ice and moon ore).
const ASTEROID_CATEGORY: u32 = 25;

/// What a mining module harvests, derived from its SDE default effect and required skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiningKind {
    /// `miningLaser` effect on a module whose required skills include the target's required skill.
    Asteroid,
    /// `miningClouds` effect (gas cloud harvesters).
    Gas,
}

/// Why a repeating mining cycle stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiningStopReason {
    HoldFull,
    AsteroidDepleted,
    TargetLost,
    OutOfRange,
    Warping,
    NotInSpace,
    Cloaked,
    CapacitorEmpty,
    ModuleOffline,
    Deactivated,
}

impl MiningStopReason {
    fn code(self) -> &'static str {
        match self {
            Self::HoldFull => "MiningHoldFull",
            Self::AsteroidDepleted => "AsteroidDepleted",
            Self::TargetLost => "TargetNotLocked",
            Self::OutOfRange => "TargetOutOfRange",
            Self::Warping => "CannotActivateWhileWarping",
            Self::NotInSpace => "NotInSpace",
            Self::Cloaked => "CannotActivateWhileCloaked",
            Self::CapacitorEmpty => "NotEnoughCapacitor",
            Self::ModuleOffline => "ModuleOffline",
            Self::Deactivated => "Deactivated",
        }
    }
}

/// One running mining module.
#[derive(Debug, Clone)]
pub struct ActiveMiningCycle {
    pub char_id: i64,
    pub ship_id: i64,
    pub module_id: i64,
    pub target_id: i64,
    pub solar_system_id: u32,
    pub kind: MiningKind,
    pub effect_name: String,
    /// Server time (ms) at which the current cycle started.
    pub cycle_start_ms: u64,
    /// Effective module `duration` (ms) captured when the cycle started.
    pub duration_ms: u64,
}

/// Result of one resolved (full or short) cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct MiningCycleOutcome {
    pub module_id: i64,
    pub target_id: i64,
    pub ore_type_id: u32,
    /// Fraction of a full cycle that was resolved (`1.0` for a completed cycle).
    pub cycle_fraction: f64,
    /// Effective `miningAmount` (m³) for a full cycle.
    pub mining_amount_m3: f64,
    /// Effective module `duration` (ms) of the resolved cycle.
    pub cycle_duration_ms: u64,
    /// Units removed from the asteroid by the yield itself.
    pub units_removed: i64,
    /// Extra units removed from the asteroid as residue (never delivered).
    pub residue_units: i64,
    /// Bonus units from a critical success (delivered, never removed from the asteroid).
    pub bonus_crit_units: i64,
    pub crit: bool,
    /// Units added to the hold (`units_removed + bonus_crit_units`, capped by free space).
    pub units_delivered: i64,
    pub destination_flag: InventoryFlag,
    pub asteroid_remaining: i64,
    /// Loaded crystal, and whether it matched the target ore (MN-06).
    pub crystal: Option<CrystalUse>,
    /// Volatility damage the crystal took this cycle (0 when the roll missed).
    pub crystal_damage: f64,
    pub crystal_shattered: bool,
    pub stop: Option<MiningStopReason>,
}

fn now_ms(ctx: &ServerContext) -> u64 {
    let clock = ctx.clock();
    clock.now_unix_secs() * 1000 + u64::from(clock.now_subsec_nanos() / 1_000_000)
}

fn required_skills(t: &InvType) -> Vec<u32> {
    [attr::REQUIRED_SKILL_1, attr::REQUIRED_SKILL_2, attr::REQUIRED_SKILL_3]
        .iter()
        .filter_map(|a| t.default_attributes.get(a).map(|v| v.round() as u32))
        .filter(|&s| s > 0)
        .collect()
}

fn effect_name(sde: &SdeRegistry, t: &InvType) -> Option<String> {
    t.default_effect_id.and_then(|e| sde.effects.get(&e)).map(|e| e.name.clone())
}

/// Returns the mining kind when `module` is a mining module (default effect `miningLaser` or
/// `miningClouds` in the SDE), else `None`.
pub fn mining_kind_of(sde: &SdeRegistry, module: &InvType) -> Option<MiningKind> {
    match effect_name(sde, module).as_deref() {
        Some("miningLaser") => Some(MiningKind::Asteroid),
        Some("miningClouds") => Some(MiningKind::Gas),
        _ => None,
    }
}

/// Whether `module` can harvest `target`.
///
/// Asteroid types carry their own `requiredSkill1` (Veldspar → Mining, Mercoxit → Deep Core
/// Mining, ice → Ice Harvesting); a `miningLaser` module may mine an asteroid only when that skill
/// is among the module's own required skills. Gas clouds are the non-asteroid types without a
/// required skill (DERIVED rule: SDE harvestable clouds carry no dogma attributes).
pub fn can_harvest(module: &InvType, kind: MiningKind, target: &InvType) -> bool {
    let target_skill = target.default_attributes.get(&attr::REQUIRED_SKILL_1).map(|v| v.round() as u32);
    match kind {
        MiningKind::Asteroid => {
            target.category_id == ASTEROID_CATEGORY
                && target_skill.is_some_and(|s| required_skills(module).contains(&s))
        }
        MiningKind::Gas => target.category_id != ASTEROID_CATEGORY && target_skill.is_none() && target.volume > 0.0,
    }
}

fn err(reason: MiningStopReason, message: String) -> BridgeRpcError {
    BridgeRpcError { code: reason.code().into(), message }
}

/// Live geometry/state used by the requirement checks.
struct CycleEnv {
    distance_m: f64,
    max_range_m: f64,
}

/// Fleet command-burst dbuffs on `ship_id` (MN-07). Every active dbuff whose SDE collection
/// (`sde_dbuff_collections`) lists a `locationRequiredSkillModifiers` entry for `attr_id` with a skill
/// the module requires scales the attribute by its `PostPercent` value (e.g. Mining Laser
/// Optimization: duration and capacitorNeed of modules requiring Mining/Ice/Gas Harvesting).
/// Values are aggregated per dbuff by the collection's `aggregateMode`. Applied on top of the
/// fitting engine's result as a separate modifier source (DERIVED ordering).
fn dbuff_multiplier(ctx: &ServerContext, ship_id: i64, module: &InvType, attr_id: u16) -> f64 {
    let skills = required_skills(module);
    let now = ctx.now_unix_secs();
    crate::services::inventory_fitting::get_aggregated_ship_dbuffs(ctx, ship_id, now)
        .into_iter()
        .filter_map(|(dbuff_id, (value, _))| {
            let coll = ctx.sde.dbuff_collections.get(&dbuff_id)?;
            let hits = coll
                .location_required_skill_modifiers
                .iter()
                .any(|m| m.attribute_id == attr_id && skills.contains(&m.skill_id));
            (hits && coll.operation == "PostPercent").then_some(1.0 + value / 100.0)
        })
        .product()
}

/// Effective module attribute for a mining cycle: fitting-engine value x active burst dbuffs.
fn module_attr(ctx: &ServerContext, fit: &eve_dogma::ShipFittingEngine, ship_id: i64, module_id: i64, attr_id: u16) -> f64 {
    let base = fit.get_module_effective_attribute(module_id, attr_id, &ctx.sde);
    let Some(module) = fit.modules.get(&module_id).and_then(|m| ctx.sde.types.get(&m.type_id)) else {
        return base;
    };
    base * dbuff_multiplier(ctx, ship_id, module, attr_id)
}

/// Checks every activation/continuation requirement for `cycle` (MN-04 "Requirements" and
/// "Interruptions"): ship in space and not warping/cloaked, module online, target asteroid present
/// with ore, target locked, and surface distance within the effective `maxRange`.
async fn check_requirements(ctx: &ServerContext, cycle: &ActiveMiningCycle) -> Result<CycleEnv, BridgeRpcError> {
    let in_space = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&cycle.char_id)
            .is_some_and(|c| c.station_id.is_none() && c.structure_id.is_none() && c.active_ship_id == cycle.ship_id)
    });
    if !in_space {
        return Err(err(MiningStopReason::NotInSpace, "Ship is not in space".into()));
    }
    let sys = ctx.systems.get_or_spawn(cycle.solar_system_id);
    let Some((_, ship_ball)) = sys.get_ball_snapshot(cycle.ship_id).await.unwrap_or(None) else {
        return Err(err(MiningStopReason::NotInSpace, format!("Ship {} has no ball in space", cycle.ship_id)));
    };
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(err(MiningStopReason::Warping, "Cannot mine while warping".into()));
    }
    if sys.get_combat_state(cycle.ship_id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
        return Err(err(MiningStopReason::Cloaked, "Cannot mine while cloaked".into()));
    }
    let fit = ctx.get_or_rebuild_ship_fitting(cycle.char_id, cycle.ship_id)?;
    let online = fit.modules.get(&cycle.module_id).is_some_and(|m| m.state != eve_dogma::ModuleState::Offline);
    if !online {
        return Err(err(MiningStopReason::ModuleOffline, format!("Module {} is not online", cycle.module_id)));
    }
    let Some(asteroid) = ctx.db.get_asteroid(cycle.target_id) else {
        return Err(err(MiningStopReason::TargetLost, format!("Target {} is not an asteroid", cycle.target_id)));
    };
    if asteroid.quantity <= 0 {
        return Err(err(MiningStopReason::AsteroidDepleted, format!("Asteroid {} is depleted", cycle.target_id)));
    }
    let locked = sys
        .get_target_locks(cycle.ship_id)
        .await
        .unwrap_or_default()
        .iter()
        .any(|l| l.target_id == cycle.target_id && l.locked);
    if !locked {
        return Err(err(MiningStopReason::TargetLost, format!("Target {} is not locked", cycle.target_id)));
    }
    // Module ranges are compared with the surface-to-surface distance (the overview distance).
    let ast_pos = DVec3::new(asteroid.position.0, asteroid.position.1, asteroid.position.2);
    let distance_m = ((ship_ball.position - ast_pos).length() - ship_ball.radius - asteroid.radius).max(0.0);
    let max_range_m = module_attr(ctx, &fit, cycle.ship_id, cycle.module_id, attr::MAX_RANGE);
    if distance_m > max_range_m {
        return Err(err(
            MiningStopReason::OutOfRange,
            format!("Asteroid is {distance_m:.1} m away (maxRange {max_range_m:.1} m)"),
        ));
    }
    Ok(CycleEnv { distance_m, max_range_m })
}

/// Deducts the module's effective `capacitorNeed` at a cycle start; fails when the ship has less.
async fn consume_cycle_capacitor(ctx: &ServerContext, cycle: &ActiveMiningCycle) -> Result<(), BridgeRpcError> {
    let fit = ctx.get_or_rebuild_ship_fitting(cycle.char_id, cycle.ship_id)?;
    let need = module_attr(ctx, &fit, cycle.ship_id, cycle.module_id, attr::CAPACITOR_NEED);
    let sys = ctx.systems.get_or_spawn(cycle.solar_system_id);
    let cs = match sys.get_combat_state(cycle.ship_id).await.ok().flatten() {
        Some(cs) => cs,
        None => {
            // No combat state yet on this ball: create it from the ship's dogma attributes (full
            // capacitor) so the first cycle is charged like every later one.
            let type_id = ctx.db.with_snapshot(|s| s.items.get(&cycle.ship_id).map(|i| i.type_id)).unwrap_or(0);
            let cs = ctx.build_ship_combat_state(cycle.char_id, cycle.ship_id, type_id);
            let _ = sys.set_combat_state(cycle.ship_id, cs.clone()).await;
            cs
        }
    };
    if cs.capacitor + 1e-9 < need {
        return Err(err(
            MiningStopReason::CapacitorEmpty,
            format!("Module needs {need} GJ, capacitor has {:.2} GJ", cs.capacitor),
        ));
    }
    let _ = sys.set_capacitor(cycle.ship_id, cs.capacitor - need).await;
    Ok(())
}

/// Picks the hold for `kind`/`ore` from the hull's effective capacities: the dedicated ice/gas
/// hold when the hull has one, else the general mining hold, else the cargo hold.
fn destination_hold(ctx: &ServerContext, fit: &eve_dogma::ShipFittingEngine, ore: &InvType) -> (InventoryFlag, f64) {
    let cap = |a: u16| fit.ship.get_attribute(a, &ctx.sde);
    let is_ice = ore.default_attributes.get(&attr::REQUIRED_SKILL_1).is_some_and(|&s| {
        ctx.sde.types.get(&(s.round() as u32)).is_some_and(|sk| sk.name == "Ice Harvesting")
    });
    let is_gas = ore.category_id != ASTEROID_CATEGORY;
    if is_ice && cap(attr::SPECIAL_ICE_HOLD) > 0.0 {
        return (InventoryFlag::SpecializedIceHold, cap(attr::SPECIAL_ICE_HOLD));
    }
    if is_gas && cap(attr::SPECIAL_GAS_HOLD) > 0.0 {
        return (InventoryFlag::SpecializedGasHold, cap(attr::SPECIAL_GAS_HOLD));
    }
    if cap(attr::GENERAL_MINING_HOLD) > 0.0 {
        return (InventoryFlag::SpecializedOreHold, cap(attr::GENERAL_MINING_HOLD));
    }
    (InventoryFlag::Cargo, cap(attr::CAPACITY))
}

fn units_for(volume_m3: f64, unit_volume: f64) -> i64 {
    ((volume_m3 / unit_volume) + 1e-9).floor().max(0.0) as i64
}

/// Loaded mining crystal and whether it applies to the current target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrystalUse {
    pub type_id: u32,
    pub matched: bool,
}

/// Fitting state for one cycle on `target_type`. A loaded crystal applies its dogma modifiers
/// (`miningCrystalsMiningAtributesAdjustments`, `specializationAsteroidDurationMultiplierEffect`)
/// only when the target is in its `specializationAsteroidTypeList`; otherwise the module mines as if
/// unloaded (wiki:Mining_crystals: "no benefit when mining another kind of ore").
fn cycle_fit(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    target_type: u32,
) -> Result<(eve_dogma::ShipFittingEngine, Option<CrystalUse>), BridgeRpcError> {
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let Some(charge) = fit.modules.get(&module_id).and_then(|m| m.charge_type_id) else {
        return Ok((fit, None));
    };
    let target = ctx.sde.types.get(&target_type);
    let matched = ctx
        .sde
        .types
        .get(&charge)
        .and_then(|c| c.default_attributes.get(&attr::SPECIALIZATION_TYPE_LIST))
        .and_then(|l| ctx.sde.type_lists.get(&(l.round() as u32)))
        .zip(target)
        .is_some_and(|(list, t)| list.contains(t));
    if !matched {
        if let Some(m) = fit.modules.get_mut(&module_id) {
            m.charge_type_id = None;
        }
        fit.recalculate_stats(&ctx.sde);
    }
    Ok((fit, Some(CrystalUse { type_id: charge, matched })))
}

/// wiki:Crystal_damage: every cycle (short cycles included) rolls the crystal's volatility chance;
/// a hit adds its volatility damage, and the crystal shatters when damage reaches its structure HP.
/// Returns `(damage_applied, shattered)`.
fn damage_crystal(ctx: &ServerContext, cycle: &ActiveMiningCycle, crystal: CrystalUse) -> (f64, bool) {
    let Some(ct) = ctx.sde.types.get(&crystal.type_id) else { return (0.0, false) };
    let a = |id: u16| ct.default_attributes.get(&id).copied().unwrap_or(0.0);
    if a(attr::CRYSTALS_GET_DAMAGED) < 0.5 || !ctx.mechanic_rng.roll(a(attr::CRYSTAL_VOLATILITY_CHANCE)) {
        return (0.0, false);
    }
    let dmg = a(attr::CRYSTAL_VOLATILITY_DAMAGE);
    let hp = a(attr::HP);
    let slot = ctx.db.with_snapshot(|s| s.items.get(&cycle.module_id).map(|m| m.flag));
    let shattered = ctx.db.with_mut_snapshot(|snap| {
        let Some(item) = snap
            .items
            .values_mut()
            .find(|i| i.location_id == cycle.ship_id && Some(i.flag) == slot && i.type_id == crystal.type_id)
        else {
            return false;
        };
        let total = item.dynamic_attributes.get(&attr::DAMAGE).copied().unwrap_or(0.0) + dmg;
        if total + 1e-9 < hp {
            item.dynamic_attributes.insert(attr::DAMAGE, total);
            item.version += 1;
            return false;
        }
        let id = item.item_id;
        if item.quantity > 1 {
            item.quantity -= 1;
            item.dynamic_attributes.remove(&attr::DAMAGE);
            item.version += 1;
        } else {
            snap.items.remove(&id);
        }
        true
    });
    if shattered {
        ctx.invalidate_ship_fitting(cycle.ship_id);
    }
    (dmg, shattered)
}

fn target_type_of(ctx: &ServerContext, target_id: i64) -> u32 {
    ctx.db.get_asteroid(target_id).map(|a| a.type_id).unwrap_or(0)
}

/// Resolves `fraction` of one cycle for `cycle`: moves ore from the asteroid into the hold, rolls
/// crit and residue with the server mechanic RNG, writes the ledger and sends `OnOreMined`.
async fn resolve_cycle(ctx: &ServerContext, cycle: &ActiveMiningCycle, fraction: f64) -> Result<MiningCycleOutcome, BridgeRpcError> {
    let Some(asteroid) = ctx.db.get_asteroid(cycle.target_id) else {
        return Err(err(MiningStopReason::TargetLost, format!("Target {} vanished", cycle.target_id)));
    };
    let Some(ore) = ctx.sde.types.get(&asteroid.type_id) else {
        return Err(err(MiningStopReason::TargetLost, format!("Unknown ore type {}", asteroid.type_id)));
    };
    let (fit, crystal) = cycle_fit(ctx, cycle.char_id, cycle.ship_id, cycle.module_id, asteroid.type_id)?;
    let eff = |a: u16| module_attr(ctx, &fit, cycle.ship_id, cycle.module_id, a);
    let amount_m3 = eff(attr::MINING_AMOUNT);
    let unit_volume = ore.volume.max(1e-6);
    let (flag, capacity) = destination_hold(ctx, &fit, ore);
    let used: f64 = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == cycle.ship_id && i.flag == flag)
            .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * f64::from(i.quantity))
            .sum()
    });
    let free_units = units_for((capacity - used).max(0.0), unit_volume);

    let wanted = units_for(amount_m3 * fraction, unit_volume);
    let mined = wanted.min(asteroid.quantity).min(free_units);
    // wiki:Mining "Critical yield": bonus = miningCritBonusYield x the cycle's yield, not removed
    // from the asteroid.
    let crit_chance = eff(attr::CRIT_CHANCE).clamp(0.0, 1.0);
    let crit = mined > 0 && crit_chance > 0.0 && ctx.mechanic_rng.roll(crit_chance);
    let bonus_crit_units = if crit { units_for(mined as f64 * eff(attr::CRIT_BONUS_YIELD), 1.0) } else { 0 };
    // wiki:Mining_yield "Residue": wasted units leave the asteroid but are not delivered.
    // `miningWasteProbability` (attr 3154, modified via SDE effect 1200 `ModAdd` from crystal attr
    // 3160 `crystalWasteProbabilityBonus`) is a percentage; `miningWastedVolumeMultiplier` (attr
    // 3153, base 1, modified via SDE effect 1200 operator `ModAdd` (2) from crystal attr 3159
    // `crystalWastedVolumeMultiplierBonus`: +18 -> 19x for Type C I, +28 -> 29x for Type C II).
    let waste_chance = (eff(attr::WASTE_PROBABILITY) / 100.0).clamp(0.0, 1.0);
    let residue_roll = mined > 0 && waste_chance > 0.0 && ctx.mechanic_rng.roll(waste_chance);
    let residue_units = if residue_roll {
        units_for(mined as f64 * eff(attr::WASTED_VOLUME_MULT), 1.0).min(asteroid.quantity - mined)
    } else {
        0
    };
    let delivered = (mined + bonus_crit_units).min(free_units);
    let (crystal_damage, crystal_shattered) = crystal.map(|c| damage_crystal(ctx, cycle, c)).unwrap_or((0.0, false));

    let remaining = ctx.db.with_mut_snapshot(|snap| {
        let mut left = 0;
        if let Some(a) = snap.asteroids.get_mut(&cycle.target_id) {
            a.quantity = (a.quantity - mined - residue_units).max(0);
            left = a.quantity;
        }
        if delivered > 0 {
            let existing = snap.items.values_mut().find(|i| {
                i.location_id == cycle.ship_id && i.flag == flag && i.type_id == asteroid.type_id && !i.singleton
            });
            if let Some(stack) = existing {
                stack.quantity = stack.quantity.saturating_add(i32::try_from(delivered).unwrap_or(i32::MAX));
                stack.version += 1;
            } else {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.items.insert(
                    id,
                    eve_db::InventoryItemRecord {
                        item_id: id,
                        type_id: asteroid.type_id,
                        owner_id: cycle.char_id,
                        location_id: cycle.ship_id,
                        flag,
                        quantity: i32::try_from(delivered).unwrap_or(i32::MAX),
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        }
        left
    });
    let due_unix = crate::system_actor::next_downtime_unix_secs(
        ctx.now_unix_secs(),
        ctx.systems.belt_respawn_interval_secs(),
        ctx.systems.downtime_utc_secs_of_day(),
    );
    ctx.systems
        .get_or_spawn(cycle.solar_system_id)
        .sync_asteroid_quantity(cycle.target_id, remaining.max(0) as u64, due_unix);
    if delivered > 0 || residue_units > 0 {
        let _ = ctx.db.record_mining_ledger_entry_with_waste(
            cycle.char_id,
            cycle.solar_system_id,
            asteroid.type_id,
            delivered,
            residue_units,
            ctx.now_unix_secs(),
        );
    }
    if delivered > 0 {
        crate::services::space_mining_harvest::dispatch_mining_gameplay_events(
            ctx,
            cycle.char_id,
            delivered as u64,
            cycle.solar_system_id,
            asteroid.type_id,
        );
    }
    // V24 `notifications/client/oreMinedNotificationAdapter.pyj` `OnOreMined(dataDict)`
    // reading keys `oreType`, `quantity_added`, `quantity_removed`, `hasRewards`,
    // `amountWasted`, `amountCritBonus`, `itemID`, `moduleItemID`.
    ctx.notify(
        NotificationTarget::Character(cycle.char_id),
        "OnOreMined",
        "charid",
        vec![PyRep::Dict(vec![
            (PyRep::String("oreType".into()), PyRep::Integer(i64::from(asteroid.type_id))),
            (PyRep::String("quantity_added".into()), PyRep::Integer(delivered)),
            (PyRep::String("quantity_removed".into()), PyRep::Integer(mined)),
            (PyRep::String("hasRewards".into()), PyRep::Bool(false)),
            (PyRep::String("amountWasted".into()), PyRep::Integer(residue_units)),
            (PyRep::String("amountCritBonus".into()), PyRep::Integer(bonus_crit_units)),
            (PyRep::String("itemID".into()), PyRep::Integer(cycle.target_id)),
            (PyRep::String("moduleItemID".into()), PyRep::Integer(cycle.module_id)),
        ])],
    );

    let free_after = free_units - delivered;
    let stop = if remaining == 0 {
        ctx.systems.get_or_spawn(cycle.solar_system_id).try_remove_ball(cycle.target_id);
        let ast_pos = DVec3::new(asteroid.position.0, asteroid.position.1, asteroid.position.2);
        clear_emptied_sites(ctx, cycle.solar_system_id, ast_pos);
        let _ = ctx.notify_asteroid_terminated(cycle.solar_system_id, cycle.target_id);
        Some(MiningStopReason::AsteroidDepleted)
    } else if free_after <= 0 {
        Some(MiningStopReason::HoldFull)
    } else {
        None
    };
    Ok(MiningCycleOutcome {
        module_id: cycle.module_id,
        target_id: cycle.target_id,
        ore_type_id: asteroid.type_id,
        cycle_fraction: fraction,
        mining_amount_m3: amount_m3,
        cycle_duration_ms: cycle.duration_ms,
        units_removed: mined,
        residue_units,
        bonus_crit_units,
        crit,
        units_delivered: delivered,
        destination_flag: flag,
        asteroid_remaining: remaining,
        crystal,
        crystal_damage,
        crystal_shattered,
        stop,
    })
}

/// Radius around a cosmic anomaly within which its asteroids are counted (MN-15 site rocks; DERIVED
/// grouping radius, not an SDE value).
const SITE_ROCK_RADIUS_M: f64 = 50_000.0;

/// After an asteroid empties, marks every uncleared anomaly near it whose rocks are all empty as
/// cleared and collapses the matching cosmic signature (MN-15 "despawns after it's cleared").
pub(crate) fn clear_emptied_sites(ctx: &ServerContext, sys_id: u32, ast_pos: DVec3) {
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        let rock_left_near = |snap: &eve_db::WorldStateSnapshot, center: DVec3| {
            snap.asteroids.values().any(|a| {
                a.solar_system_id == sys_id
                    && a.quantity > 0
                    && DVec3::new(a.position.0, a.position.1, a.position.2).distance(center) <= SITE_ROCK_RADIUS_M
            })
        };
        let mut cleared = Vec::new();
        let candidates: Vec<(usize, DVec3)> = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(|anoms| {
                anoms
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| !a.cleared && DVec3::from_array(a.position).distance(ast_pos) <= SITE_ROCK_RADIUS_M)
                    .map(|(i, a)| (i, DVec3::from_array(a.position)))
                    .collect()
            })
            .unwrap_or_default();
        for (idx, center) in candidates {
            if rock_left_near(snap, center) {
                continue;
            }
            if let Some(anom) = snap.cosmic_anomalies.get_mut(&sys_id).and_then(|v| v.get_mut(idx)) {
                anom.cleared = true;
                anom.remaining_volume = 0.0;
                anom.created_unix = now;
                cleared.push(anom.anomaly_id.clone());
            }
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            for s in sigs.iter_mut().filter(|s| cleared.contains(&s.signature_id)) {
                s.collapsed = true;
                s.signal_strength = 0.0;
            }
        }
    });
}

/// Marks the module inactive in the fitting engine and persisted state, and tells the client.
fn stop_module(ctx: &ServerContext, cycle: &ActiveMiningCycle) {
    ctx.mining_cycles.write().unwrap_or_else(|e| e.into_inner()).remove(&cycle.module_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cycle.module_id);
    });
    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(cycle.char_id, cycle.ship_id) {
        let _ = fit.deactivate_module(cycle.module_id, &ctx.sde);
        ctx.store_ship_fitting(cycle.ship_id, fit);
    }
    ctx.notify(
        NotificationTarget::Character(cycle.char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![PyRep::Integer(cycle.module_id), PyRep::String(cycle.effect_name.clone()), PyRep::Bool(false)],
    );
}

/// `dogmaIM.Activate` for a mining module: validates every requirement, consumes the first
/// cycle's capacitor and starts the repeating cycle.
pub async fn activate(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
    module_id: i64,
    target_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let module_type_id = ctx
        .db
        .with_snapshot(|s| s.items.get(&module_id).filter(|i| i.location_id == ship_id).map(|i| i.type_id))
        .ok_or_else(|| BridgeRpcError { code: "ModuleNotFound".into(), message: format!("Module {module_id} is not fitted") })?;
    let module = ctx.sde.types.get(&module_type_id).ok_or_else(|| BridgeRpcError {
        code: "ModuleNotFound".into(),
        message: format!("Unknown module type {module_type_id}"),
    })?;
    let kind = mining_kind_of(&ctx.sde, module).ok_or_else(|| BridgeRpcError {
        code: "NotAMiningModule".into(),
        message: format!("Module type {module_type_id} has no mining effect"),
    })?;
    if ctx.mining_cycles.read().unwrap_or_else(|e| e.into_inner()).contains_key(&module_id) {
        return Err(BridgeRpcError { code: "EffectAlreadyActive".into(), message: format!("Module {module_id} is already active") });
    }
    if let Some(asteroid) = ctx.db.get_asteroid(target_id) {
        let target_ok = ctx.sde.types.get(&asteroid.type_id).is_some_and(|t| can_harvest(module, kind, t));
        if !target_ok {
            return Err(BridgeRpcError {
                code: "TargetNotMinable".into(),
                message: format!("Module {module_type_id} cannot harvest type {}", asteroid.type_id),
            });
        }
    }
    let (fit, _) = cycle_fit(ctx, char_id, ship_id, module_id, target_type_of(ctx, target_id))?;
    let duration_ms = module_attr(ctx, &fit, ship_id, module_id, attr::DURATION).max(1.0) as u64;
    let cycle = ActiveMiningCycle {
        char_id,
        ship_id,
        module_id,
        target_id,
        solar_system_id,
        kind,
        effect_name: effect_name(&ctx.sde, module).unwrap_or_default(),
        cycle_start_ms: now_ms(ctx),
        duration_ms,
    };
    let env = check_requirements(ctx, &cycle).await?;
    consume_cycle_capacitor(ctx, &cycle).await?;

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let _ = fit.activate_module(module_id, false, &ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(module_id);
    });
    ctx.mining_cycles.write().unwrap_or_else(|e| e.into_inner()).insert(module_id, cycle.clone());
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![PyRep::Integer(module_id), PyRep::String(cycle.effect_name.clone()), PyRep::Bool(true)],
    );
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("targetID", PyRep::Integer(target_id)),
        ("active", PyRep::Bool(true)),
        ("cycleTimeMs", PyRep::Integer(duration_ms as i64)),
        ("distance", PyRep::Real(env.distance_m)),
        ("maxRange", PyRep::Real(env.max_range_m)),
    ]))
}

/// `dogmaIM.Deactivate` for a mining module. Mining modules may be short-cycled: the elapsed
/// fraction of the cycle is delivered (wiki:Mining_yield "Short-cycling"); ice yields whole units
/// only, so a short ice cycle delivers nothing. Returns `None` when `module_id` runs no mining
/// cycle, and an error (leaving the cycle running) when the caller does not own it.
pub async fn deactivate(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: Option<i64>,
    module_id: i64,
) -> Option<Result<Option<MiningCycleOutcome>, BridgeRpcError>> {
    let cycle = ctx.mining_cycles.read().unwrap_or_else(|e| e.into_inner()).get(&module_id).cloned()?;
    if cycle.char_id != char_id || Some(cycle.ship_id) != ship_id {
        return Some(Err(BridgeRpcError {
            code: "ModuleNotFound".into(),
            message: format!("Module {module_id} is not fitted to the caller's active ship"),
        }));
    }
    let elapsed = now_ms(ctx).saturating_sub(cycle.cycle_start_ms);
    let fraction = (elapsed as f64 / cycle.duration_ms as f64).clamp(0.0, 1.0);
    let outcome = if fraction > 0.0 && check_requirements(ctx, &cycle).await.is_ok() {
        resolve_cycle(ctx, &cycle, fraction).await.ok().map(|mut o| {
            o.stop = Some(MiningStopReason::Deactivated);
            o
        })
    } else {
        None
    };
    stop_module(ctx, &cycle);
    Some(Ok(outcome))
}

/// Advances every running mining cycle to the current server time. Interrupted cycles stop
/// without yield; each completed cycle delivers ore and, unless the hold is full or the asteroid
/// is depleted, starts the next cycle (paying its capacitor). Called by the world tick.
pub async fn process_mining_cycles(ctx: &ServerContext) -> Vec<MiningCycleOutcome> {
    let mut cycles: Vec<ActiveMiningCycle> =
        ctx.mining_cycles.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    cycles.sort_by_key(|c| c.module_id);
    let now = now_ms(ctx);
    let mut outcomes = Vec::new();
    for mut cycle in cycles {
        loop {
            if check_requirements(ctx, &cycle).await.is_err() {
                stop_module(ctx, &cycle);
                break;
            }
            if now < cycle.cycle_start_ms + cycle.duration_ms {
                ctx.mining_cycles.write().unwrap_or_else(|e| e.into_inner()).insert(cycle.module_id, cycle.clone());
                break;
            }
            let outcome = match resolve_cycle(ctx, &cycle, 1.0).await {
                Ok(o) => o,
                Err(_) => {
                    stop_module(ctx, &cycle);
                    break;
                }
            };
            let stop = outcome.stop;
            outcomes.push(outcome);
            if stop.is_some() {
                stop_module(ctx, &cycle);
                break;
            }
            cycle.cycle_start_ms += cycle.duration_ms;
            if let Ok((fit, _)) =
                cycle_fit(ctx, cycle.char_id, cycle.ship_id, cycle.module_id, target_type_of(ctx, cycle.target_id))
            {
                cycle.duration_ms =
                    module_attr(ctx, &fit, cycle.ship_id, cycle.module_id, attr::DURATION).max(1.0) as u64;
            }
            if consume_cycle_capacitor(ctx, &cycle).await.is_err() {
                if let Some(last) = outcomes.last_mut() {
                    last.stop = Some(MiningStopReason::CapacitorEmpty);
                }
                stop_module(ctx, &cycle);
                break;
            }
        }
    }
    outcomes
}

/// Running mining cycles for `ship_id`, keyed by module ID.
pub fn active_cycles_for_ship(ctx: &ServerContext, ship_id: i64) -> HashMap<i64, ActiveMiningCycle> {
    ctx.mining_cycles
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, c)| c.ship_id == ship_id)
        .map(|(k, v)| (*k, v.clone()))
        .collect()
}
