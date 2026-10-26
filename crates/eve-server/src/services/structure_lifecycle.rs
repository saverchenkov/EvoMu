//! `structure_lifecycle` — SDE-driven Upwell structure lifecycle engine: hull profiles,
//! deployment/anchoring/onlining timers, quantum-core gating, hourly service-module fuel burn,
//! low-power/abandoned power states, damage caps, repair timers, the reinforcement cycle,
//! destruction (killmail, core/fitting drop, automatic asset safety) and unanchor packaging.
//!
//! State is evaluated lazily from `ctx.now_unix()`: [`sync_structure`] advances every timer that
//! has elapsed. It runs before any structure RPC is handled (`station_structure::handle`).
//!
//! Static values come from SDE dogma attributes on the structure / module types. Timer lengths
//! that are not in the SDE are DERIVED from the EVE University wiki mirror in `~/ai/eve-wiki/`
//! (`articles/Vulnerability.md`, `articles/Anchoring_structures.md`,
//! `articles/Upwell_structure.md`, `articles/Quantum_Cores.md`); each constant cites its source.
#![forbid(unsafe_code)]
use super::structure_upwell_ops::{load_ext_state, save_ext_state, AssetSafetyWrapRecord, StructureExtendedState, StructureLifecycleState, ASSET_SAFETY_DELIVERY_DELAY_SECS};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_proto::BridgeRpcError;
use std::collections::{HashMap, HashSet, VecDeque};

// ---- SDE dogma attribute IDs (sde_dogma_attributes) ----
pub const ATTR_HP: u16 = 9; // hp (hull)
pub const ATTR_LOW_SLOTS: u16 = 12;
pub const ATTR_MED_SLOTS: u16 = 13;
pub const ATTR_HI_SLOTS: u16 = 14;
pub const ATTR_SHIELD_CAPACITY: u16 = 263;
pub const ATTR_ARMOR_HP: u16 = 265;
pub const ATTR_RIG_SLOTS: u16 = 1137;
pub const ATTR_RIG_SIZE: u16 = 1547;
pub const ATTR_DISALLOW_IN_HIGHSEC: u16 = 1970;
pub const ATTR_SHIELD_DAMAGE_LIMIT: u16 = 2034;
pub const ATTR_ARMOR_DAMAGE_LIMIT: u16 = 2035;
pub const ATTR_STRUCTURE_DAMAGE_LIMIT: u16 = 2036;
pub const ATTR_SERVICE_SLOTS: u16 = 2056;
pub const ATTR_SERVICE_FUEL_GROUP: u16 = 2108; // serviceModuleFuelConsumptionGroup
pub const ATTR_SERVICE_FUEL_PER_HOUR: u16 = 2109; // serviceModuleFuelAmount
pub const ATTR_SERVICE_FUEL_ONLINE: u16 = 2110; // serviceModuleFuelOnlineAmount
pub const ATTR_STRUCTURE_FULL_POWER_HP_MULT: u16 = 2743; // structureFullPowerStateHitpointMultiplier
pub const ATTR_SERVICE_FULL_POWER_HP_MULT: u16 = 2744; // serviceModuleFullPowerStateHitpointMultiplier
pub const ATTR_REQUIRES_DEED_TYPE: u16 = 3101; // structureRequiresDeedType (quantum core type)
pub const ATTR_PAUSE_SHIELD_REPAIR_DPS: u16 = 3354;
pub const ATTR_PAUSE_ARMOR_REPAIR_DPS: u16 = 3355;
pub const ATTR_PAUSE_HULL_REPAIR_DPS: u16 = 3356;
pub const ATTR_STRUCTURE_ROLE_BONUS: u16 = 2333; // structureRoleBonus (Citadel fuel reduction %)
pub const ATTR_SERVICE_ROLE_BONUS: u16 = 2339; // structureServiceRoleBonus (EC/Refinery service fuel reduction %)
pub const ATTR_ENG_MAT_BONUS: u16 = 2600; // strEngMatBonus
pub const ATTR_ENG_COST_BONUS: u16 = 2601; // strEngCostBonus
pub const ATTR_ENG_TIME_BONUS: u16 = 2602; // strEngTimeBonus
pub const ATTR_REACTION_TIME_MULT: u16 = 2721; // strReactionTimeMultiplier
pub const ATTR_REFINING_YIELD_BONUS: u16 = 2722; // strRefiningYieldBonus
/// SDE category of Upwell structures.
pub const CATEGORY_STRUCTURE: u32 = 65;
/// SDE type ID of Upwell Palatine Keepstar (cluster-wide limit of 1 active).
pub const PALATINE_KEEPSTAR_TYPE_ID: u32 = 40340;
/// SDE region ID of Pochven (Triglavian space).
pub const POCHVEN_REGION_ID: u32 = 10_000_070;

// ---- DERIVED timers (wiki citations) ----
/// DERIVED (Vulnerability.md "Deployment cycle"): initial anchoring timer, hull only, pausable.
pub const ANCHOR_VULNERABLE_SECS: u64 = 15 * 60;
/// DERIVED (Vulnerability.md / Anchoring_structures.md): invulnerable anchoring reinforcement.
pub const ANCHORING_SECS: u64 = 24 * 3_600;
/// DERIVED (Vulnerability.md): onlining timer after the quantum core is inserted.
pub const ONLINING_SECS: u64 = 15 * 60;
/// DERIVED (Vulnerability.md): repair timer for shield/armor damage.
pub const REPAIR_SHIELD_ARMOR_SECS: u64 = 15 * 60;
/// DERIVED (Vulnerability.md): repair timer for hull damage.
pub const REPAIR_HULL_SECS: u64 = 30 * 60;
/// DERIVED (Vulnerability.md "Structures in abandoned state (no fuel used for 7 days)").
pub const ABANDON_AFTER_LOW_POWER_SECS: u64 = 7 * 86_400;
/// DERIVED (Upwell_structure.md): minimum distance from stations, gates, belts, structures.
pub const MIN_ANCHOR_DISTANCE_M: f64 = 1_000_000.0;
/// Fuel is burned per hour (`serviceModuleFuelAmount` is an hourly amount).
const SECS_PER_HOUR: u64 = 3_600;
/// Celestial groups used for the anchoring distance rule (SDE `sde_groups`: 9 Asteroid Belt, 10 Stargate).
const ANCHOR_BLOCKING_CELESTIAL_GROUPS: [u32; 2] = [9, 10];

pub const STATE_ANCHOR_VULNERABLE: &str = "AnchorVulnerable";
pub const STATE_ANCHORING: &str = "Anchoring";
pub const STATE_NO_CORE: &str = "NoCore";
pub const STATE_ONLINING: &str = "Onlining";
pub const STATE_SHIELD_VULNERABLE: &str = "ShieldVulnerable";
pub const STATE_ARMOR_REINFORCED: &str = "ArmorReinforced";
pub const STATE_ARMOR_VULNERABLE: &str = "ArmorVulnerable";
pub const STATE_HULL_REINFORCED: &str = "HullReinforced";
pub const STATE_HULL_VULNERABLE: &str = "HullVulnerable";
pub const STATE_DESTROYED: &str = "Destroyed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HullClass {
    Medium,
    Large,
    XLarge,
    Flex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    /// Anchoring / no core / onlining: services cannot run yet.
    NotOperational,
    FullPower,
    LowPower,
    Abandoned,
}

impl PowerState {
    pub fn as_str(self) -> &'static str {
        match self {
            PowerState::NotOperational => "NotOperational",
            PowerState::FullPower => "FullPower",
            PowerState::LowPower => "LowPower",
            PowerState::Abandoned => "Abandoned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceClass {
    HighSec,
    LowNullSec,
    Wormhole,
    WarHq,
}

/// Hull profile read from the SDE dogma attributes of a category-65 type.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureProfile {
    pub type_id: u32,
    pub group_id: u32,
    pub hull_class: HullClass,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub shield_damage_cap: f64,
    pub armor_damage_cap: f64,
    pub hull_damage_cap: f64,
    pub pause_shield_dps: f64,
    pub pause_armor_dps: f64,
    pub pause_hull_dps: f64,
    pub core_type_id: Option<u32>,
    pub disallow_in_highsec: bool,
    pub high_slots: u32,
    pub mid_slots: u32,
    pub low_slots: u32,
    pub service_slots: u32,
    pub rig_slots: u32,
    pub full_power_hp_mult: f64,
    pub fuel_bonus_pct: f64,
    pub eng_mat_bonus_pct: f64,
    pub eng_cost_bonus_pct: f64,
    pub eng_time_bonus_pct: f64,
    pub reaction_time_mult: f64,
    pub refining_yield_bonus: f64,
}

pub fn structure_profile(ctx: &ServerContext, type_id: u32) -> Option<StructureProfile> {
    let t = ctx
        .sde
        .types
        .get(&type_id)
        .filter(|t| t.category_id == CATEGORY_STRUCTURE || matches!(type_id, 2233 | 3962))?;
    let a = |id: u16| t.default_attributes.get(&id).copied();
    // The Upwell hull class follows the SDE rig size (2 = medium, 3 = large, 4 = capital/XL);
    // types without rig slots (FLEX structures) have no rig size. Orbitals (2233/3962) use Medium
    // single-shield-reinforcement profile (`feat:pi.customs-office-gantry-deploy-reinforce`).
    let hull_class = match a(ATTR_RIG_SIZE).map(|v| v as i64) {
        Some(2) => HullClass::Medium,
        Some(3) => HullClass::Large,
        Some(4) => HullClass::XLarge,
        _ if matches!(type_id, 2233 | 3962) => HullClass::Medium,
        _ => HullClass::Flex,
    };
    Some(StructureProfile {
        type_id,
        group_id: t.group_id,
        hull_class,
        shield_hp: a(ATTR_SHIELD_CAPACITY).unwrap_or(0.0),
        armor_hp: a(ATTR_ARMOR_HP).unwrap_or(0.0),
        hull_hp: a(ATTR_HP).unwrap_or(0.0),
        shield_damage_cap: a(ATTR_SHIELD_DAMAGE_LIMIT).unwrap_or(f64::INFINITY),
        armor_damage_cap: a(ATTR_ARMOR_DAMAGE_LIMIT).unwrap_or(f64::INFINITY),
        hull_damage_cap: a(ATTR_STRUCTURE_DAMAGE_LIMIT).unwrap_or(f64::INFINITY),
        pause_shield_dps: a(ATTR_PAUSE_SHIELD_REPAIR_DPS).unwrap_or(0.0),
        pause_armor_dps: a(ATTR_PAUSE_ARMOR_REPAIR_DPS).unwrap_or(0.0),
        pause_hull_dps: a(ATTR_PAUSE_HULL_REPAIR_DPS).unwrap_or(0.0),
        core_type_id: a(ATTR_REQUIRES_DEED_TYPE).filter(|v| *v > 0.0).map(|v| v as u32),
        disallow_in_highsec: a(ATTR_DISALLOW_IN_HIGHSEC).unwrap_or(0.0) > 0.0,
        high_slots: a(ATTR_HI_SLOTS).unwrap_or(0.0) as u32,
        mid_slots: a(ATTR_MED_SLOTS).unwrap_or(0.0) as u32,
        low_slots: a(ATTR_LOW_SLOTS).unwrap_or(0.0) as u32,
        service_slots: a(ATTR_SERVICE_SLOTS).unwrap_or(0.0) as u32,
        rig_slots: a(ATTR_RIG_SLOTS).unwrap_or(0.0) as u32,
        full_power_hp_mult: a(ATTR_STRUCTURE_FULL_POWER_HP_MULT).unwrap_or(1.0),
        fuel_bonus_pct: a(ATTR_STRUCTURE_ROLE_BONUS).or_else(|| a(ATTR_SERVICE_ROLE_BONUS)).unwrap_or(0.0),
        eng_mat_bonus_pct: a(ATTR_ENG_MAT_BONUS).unwrap_or(0.0),
        eng_cost_bonus_pct: a(ATTR_ENG_COST_BONUS).unwrap_or(0.0),
        eng_time_bonus_pct: a(ATTR_ENG_TIME_BONUS).unwrap_or(0.0),
        reaction_time_mult: a(ATTR_REACTION_TIME_MULT).unwrap_or(1.0),
        refining_yield_bonus: a(ATTR_REFINING_YIELD_BONUS).unwrap_or(0.0),
    })
}

/// Security band as used by the reinforcement tables (J-space IDs start at 31,000,000).
pub fn space_class(ctx: &ServerContext, sys_id: u32) -> SpaceClass {
    if sys_id >= 31_000_000 {
        return SpaceClass::Wormhole;
    }
    let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sec >= 0.45 {
        SpaceClass::HighSec
    } else {
        SpaceClass::LowNullSec
    }
}

/// Fuel block groups accepted by service modules (SDE `serviceModuleFuelConsumptionGroup`).
pub fn service_fuel_groups(ctx: &ServerContext) -> HashSet<u32> {
    ctx.sde.types.values().filter_map(|t| t.default_attributes.get(&ATTR_SERVICE_FUEL_GROUP).map(|g| *g as u32)).filter(|g| *g > 0).collect()
}

fn is_service_slot(flag: InventoryFlag) -> bool {
    (164..=171).contains(&(flag as u16))
}

fn is_rig_slot(flag: InventoryFlag) -> bool {
    (92..=94).contains(&(flag as u16))
}

fn is_fitting_slot(flag: InventoryFlag) -> bool {
    matches!(flag as u16, 11..=34) || is_rig_slot(flag) || is_service_slot(flag)
}

/// Service modules fitted to the structure: `(item_id, fuel/hour, online fuel, full-power HP mult)`.
fn fitted_service_modules(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> Vec<(i64, u32, u32, f64)> {
    let mut out: Vec<_> = snap
        .items
        .values()
        .filter(|i| i.location_id == structure_id && is_service_slot(i.flag))
        .filter_map(|i| {
            let t = ctx.sde.types.get(&i.type_id)?;
            let per_hour = *t.default_attributes.get(&ATTR_SERVICE_FUEL_PER_HOUR)?;
            let online = t.default_attributes.get(&ATTR_SERVICE_FUEL_ONLINE).copied().unwrap_or(0.0);
            let mult = t.default_attributes.get(&ATTR_SERVICE_FULL_POWER_HP_MULT).copied().unwrap_or(1.0);
            Some((i.item_id, per_hour as u32, online as u32, mult))
        })
        .collect();
    out.sort_by_key(|m| m.0);
    out
}

fn online_service_modules(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> Vec<(i64, u32, u32, f64)> {
    fitted_service_modules(ctx, snap, structure_id).into_iter().filter(|m| snap.module_online_states.get(&m.0).copied().unwrap_or(false)).collect()
}

pub fn is_operational(state: &str) -> bool {
    !matches!(state, STATE_ANCHOR_VULNERABLE | STATE_ANCHORING | STATE_NO_CORE | STATE_ONLINING | STATE_DESTROYED)
}

/// Power state: full power with at least one online service module, low power otherwise,
/// abandoned after [`ABANDON_AFTER_LOW_POWER_SECS`] continuously in low power.
pub fn power_state(ctx: &ServerContext, snap: &WorldStateSnapshot, ext: &StructureExtendedState, st: &UpwellStructureRecord, now: u64) -> PowerState {
    if !is_operational(&st.state) {
        return PowerState::NotOperational;
    }
    if !online_service_modules(ctx, snap, st.structure_id).is_empty() {
        return PowerState::FullPower;
    }
    let since = ext.lifecycle.get(&st.structure_id).and_then(|l| l.low_power_since_unix);
    match since {
        Some(t) if now >= t.saturating_add(ABANDON_AFTER_LOW_POWER_SECS) => PowerState::Abandoned,
        _ => PowerState::LowPower,
    }
}

/// Effective maximum shield: the SDE shield capacity, multiplied in full power by the hull's
/// `structureFullPowerStateHitpointMultiplier` and the largest online module
/// `serviceModuleFullPowerStateHitpointMultiplier`. DERIVED reading of those attributes: low
/// power ("shield weak") uses the bare SDE shield capacity.
pub fn effective_max_shield(ctx: &ServerContext, snap: &WorldStateSnapshot, profile: &StructureProfile, structure_id: i64, power: PowerState) -> f64 {
    if power != PowerState::FullPower {
        return profile.shield_hp;
    }
    let module_mult = online_service_modules(ctx, snap, structure_id).iter().map(|m| m.3).fold(1.0_f64, f64::max);
    profile.shield_hp * profile.full_power_hp_mult * module_mult
}

/// Next occurrence of `hour` (UTC) at or after `earliest` — "next chosen hour N days later".
pub fn next_reinforce_exit(earliest: u64, hour: u64) -> u64 {
    let day_start = earliest - earliest % 86_400;
    let candidate = day_start + (hour % 24) * 3_600;
    if candidate >= earliest {
        candidate
    } else {
        candidate + 86_400
    }
}

/// DERIVED (Vulnerability.md): reinforcement exit jitter window half-width in seconds
/// (±1.5 h for Medium armor, ±3 h for Large/XL armor & hull, ±30 m for FLEX hull).
pub fn reinforce_jitter_window_secs(class: HullClass, is_armor: bool) -> u64 {
    match (class, is_armor) {
        (HullClass::Medium, true) => 5_400,
        (HullClass::Large | HullClass::XLarge, _) => 10_800,
        (HullClass::Flex, false) => 1_800,
        _ => 0,
    }
}

/// Applies `jitter_offset_secs` clamped to `[-max_jitter_secs, +max_jitter_secs]` and
/// clamped to the same UTC day `[day_start, day_start + 86_399]` so jitter never rolls
/// the exit time over to another UTC day (`feat:structures.reinforce-exit-time-rules`).
pub fn apply_reinforce_jitter(exit_unix: u64, jitter_offset_secs: i64, max_jitter_secs: u64) -> u64 {
    if max_jitter_secs == 0 || jitter_offset_secs == 0 {
        return exit_unix;
    }
    let max_j = max_jitter_secs as i64;
    let clamped_offset = jitter_offset_secs.clamp(-max_j, max_j);
    let day_start = exit_unix - (exit_unix % 86_400);
    let day_end = day_start + 86_399;
    let shifted = (exit_unix as i64).saturating_add(clamped_offset).max(0) as u64;
    shifted.clamp(day_start, day_end)
}

/// DERIVED (Vulnerability.md tables): minimum armor reinforcement length in seconds, or `None`
/// when the hull skips armor reinforcement.
pub fn armor_reinforcement_secs(class: HullClass, space: SpaceClass, power: PowerState) -> Option<u64> {
    if power == PowerState::Abandoned {
        return None;
    }
    match class {
        HullClass::Flex => None,
        HullClass::Medium => Some(match space {
            SpaceClass::WarHq => 24 * 3_600,      // 1.0 d
            SpaceClass::Wormhole => 60 * 3_600,   // 2.5 d
            SpaceClass::LowNullSec => 84 * 3_600, // 3.5 d
            SpaceClass::HighSec => 108 * 3_600,   // 4.5 d
        }),
        // Large/XL without an online service module (low power) skip armor reinforcement.
        HullClass::Large | HullClass::XLarge => (power == PowerState::FullPower).then_some(24 * 3_600),
    }
}

/// DERIVED (Vulnerability.md tables): minimum hull reinforcement length, `None` = hull goes
/// straight to vulnerable (medium and abandoned structures).
pub fn hull_reinforcement_secs(class: HullClass, space: SpaceClass, power: PowerState) -> Option<u64> {
    if power == PowerState::Abandoned {
        return None;
    }
    match class {
        HullClass::Medium => None,
        HullClass::Flex => Some(30 * 60),
        HullClass::Large | HullClass::XLarge => Some(match space {
            SpaceClass::WarHq => 24 * 3_600,      // 1.0 d
            SpaceClass::Wormhole => 36 * 3_600,   // 1.5 d
            SpaceClass::LowNullSec => 60 * 3_600, // 2.5 d
            SpaceClass::HighSec => 108 * 3_600,   // 4.5 d
        }),
    }
}

/// Reinforcement hour: the applied vulnerability window hour (set at anchoring, changed via
/// `structureVulnerability.UpdateReinforceTimesFromClient`).
fn reinforce_hour(snap: &WorldStateSnapshot, st: &UpwellStructureRecord) -> u64 {
    snap.structure_vulnerability_windows.get(&st.structure_id).map(|w| w.0).unwrap_or(0)
}

fn new_space_item(snap: &mut WorldStateSnapshot, type_id: u32, owner_id: i64, sys_id: u32, pos: [f64; 3], quantity: i32) -> i64 {
    snap.next_entity_id += 1;
    let id = snap.next_entity_id;
    snap.items.insert(
        id,
        InventoryItemRecord {
            item_id: id,
            type_id,
            owner_id,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity,
            singleton: quantity == 1,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        },
    );
    snap.item_positions.insert(id, pos);
    id
}

/// Nearest NPC station by stargate jumps (breadth-first over SDE jumps), restricted to
/// systems with security > 0.0 when starting outside high/low sec. DERIVED: the live server's
/// tie-breaking is unknown; ties are broken by lowest station ID.
pub fn nearest_npc_station(ctx: &ServerContext, origin: u32) -> Option<i64> {
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in ctx.sde.jumps.values() {
        adj.entry(j.from_solar_system_id).or_default().push(j.to_solar_system_id);
    }
    let origin_sec = ctx.sde.solar_systems.get(&origin).map(|s| s.security_status).unwrap_or(-1.0);
    let allowed = |sys: u32| origin_sec > 0.0 || ctx.sde.solar_systems.get(&sys).map(|s| s.security_status > 0.0).unwrap_or(false);
    let mut seen = HashSet::from([origin]);
    let mut frontier = VecDeque::from([origin]);
    while !frontier.is_empty() {
        let mut level: Vec<u32> = frontier.drain(..).collect();
        level.sort_unstable();
        let best = ctx.sde.stations.values().filter(|s| level.contains(&s.solar_system_id) && allowed(s.solar_system_id)).map(|s| s.station_id).min();
        if best.is_some() {
            return best;
        }
        for sys in level {
            for &n in adj.get(&sys).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(n) {
                    frontier.push_back(n);
                }
            }
        }
    }
    None
}

/// What happened to a structure's contents when it left space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisposalOutcome {
    pub killmail_id: Option<i64>,
    pub dropped_core_item_id: Option<i64>,
    pub packaged_item_id: Option<i64>,
    pub asset_safety_wrap_ids: Vec<i64>,
    pub dropped_item_ids: Vec<i64>,
    pub destroyed_item_ids: Vec<i64>,
}

/// Moves a structure's contents out: rigs are destroyed; in J-space everything else drops as
/// loot at the structure position; in K-space fitted modules drop/are destroyed (destruction)
/// or join asset safety (unanchor) and hangar contents go to per-owner asset safety wraps.
fn dispose_contents(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, st: &UpwellStructureRecord, destroyed: bool, now: u64, out: &mut DisposalOutcome) {
    let wormhole = space_class(ctx, st.solar_system_id) == SpaceClass::Wormhole;
    let mut contents: Vec<(i64, i64, InventoryFlag)> = snap.items.values().filter(|i| i.location_id == st.structure_id).map(|i| (i.item_id, i.owner_id, i.flag)).collect();
    contents.sort_by_key(|c| c.0);
    let mut fitted_index = 0usize;
    let mut to_safety: Vec<(i64, i64)> = Vec::new();
    for (iid, owner, flag) in contents {
        if is_rig_slot(flag) {
            snap.items.remove(&iid);
            out.destroyed_item_ids.push(iid);
            continue;
        }
        let fitted = is_fitting_slot(flag);
        // DERIVED: ships drop ~50% of fitted modules at random; structures here use a
        // deterministic alternation so outcomes are reproducible.
        let destroy_fitted = destroyed && fitted && {
            fitted_index += 1;
            fitted_index % 2 == 0
        };
        if destroy_fitted {
            snap.items.remove(&iid);
            snap.module_online_states.remove(&iid);
            out.destroyed_item_ids.push(iid);
        } else if wormhole || (destroyed && fitted) {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = i64::from(st.solar_system_id);
                it.flag = InventoryFlag::None;
            }
            snap.item_positions.insert(iid, st.position);
            snap.module_online_states.remove(&iid);
            out.dropped_item_ids.push(iid);
        } else {
            snap.module_online_states.remove(&iid);
            to_safety.push((iid, owner));
        }
    }
    if to_safety.is_empty() {
        return;
    }
    let station = nearest_npc_station(ctx, st.solar_system_id);
    let mut by_owner: HashMap<i64, Vec<i64>> = HashMap::new();
    for (iid, owner) in to_safety {
        by_owner.entry(owner).or_default().push(iid);
    }
    let mut owners: Vec<i64> = by_owner.keys().copied().collect();
    owners.sort_unstable();
    for owner in owners {
        let item_ids = by_owner.remove(&owner).unwrap_or_default();
        snap.next_entity_id += 1;
        let wrap_id = snap.next_entity_id;
        let mut value = 0.0;
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                value += ctx.sde.types.get(&it.type_id).map(|t| t.base_price).unwrap_or(0.0) * f64::from(it.quantity.max(1));
                it.location_id = wrap_id;
                it.flag = InventoryFlag::Locked;
            }
        }
        ext.asset_safety_wraps.insert(
            wrap_id,
            AssetSafetyWrapRecord {
                wrap_id,
                owner_id: owner,
                is_corp: snap.corporations.contains_key(&owner),
                source_structure_id: st.structure_id,
                source_structure_name: st.name.clone(),
                solar_system_id: st.solar_system_id,
                created_unix: now,
                deliverable_after_unix: now + ASSET_SAFETY_DELIVERY_DELAY_SECS,
                delivered_to_id: None,
                item_ids,
                estimated_value_isk: value,
                nearest_npc_station_id: station,
            },
        );
        out.asset_safety_wrap_ids.push(wrap_id);
    }
}

/// Destroys a structure: killmail, quantum core drop, fitting drop and automatic asset safety.
pub fn destroy_structure(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, attacker_char_id: i64, now: u64) -> DisposalOutcome {
    let mut out = DisposalOutcome::default();
    let Some(st) = snap.structures.get(&structure_id).cloned() else { return out };
    dispose_contents(ctx, snap, ext, &st, true, now, &mut out);
    // Quantum_Cores.md: cores always drop from destroyed structures that had them installed.
    if st.quantum_core_installed {
        if let Some(core) = structure_profile(ctx, st.type_id).and_then(|p| p.core_type_id) {
            out.dropped_core_item_id = Some(new_space_item(snap, core, st.owner_corp_id, st.solar_system_id, st.position, 1));
        }
    }
    snap.next_entity_id += 1;
    let kill_id = snap.next_entity_id;
    let value = ctx.sde.types.get(&st.type_id).map(|t| t.base_price).unwrap_or(0.0);
    // The victim of a structure kill is its owning corporation (stamped explicitly; there is no
    // victim pilot to resolve an affiliation from).
    let victim_alliance_id = snap.corporations.get(&st.owner_corp_id).and_then(|c| c.alliance_id);
    snap.record_killmail(KillmailRecord { kill_id, victim_character_id: st.owner_corp_id, attacker_character_id: attacker_char_id, solar_system_id: st.solar_system_id, ship_type_id: st.type_id, total_value_isk: value, victim_corp_id: st.owner_corp_id, victim_alliance_id, ..Default::default() }, now);
    out.killmail_id = Some(kill_id);
    if let Some(s) = snap.structures.get_mut(&structure_id) {
        s.state = STATE_DESTROYED.into();
        s.quantum_core_installed = false;
        s.shield_hp = 0.0;
        s.armor_hp = 0.0;
        s.hull_hp = 0.0;
    }
    ext.lifecycle.remove(&structure_id);
    ext.unanchoring_until_unix.remove(&structure_id);
    // feat:clones.clone-destroyed-structure: clones in destroyed structure are moved home (implants lost) per V24 rules.
    for clones in snap.jump_clones.values_mut() {
        for jc in clones.iter_mut() {
            if jc.location_id == structure_id {
                let home = snap
                    .characters
                    .get(&jc.character_id)
                    .map(|c| if c.home_station_id > 0 && c.home_station_id != structure_id { c.home_station_id } else { 60_003_760 })
                    .unwrap_or(60_003_760);
                jc.location_id = home;
                jc.implants.clear();
            }
        }
    }
    out
}

/// Completes decommissioning: the structure is packaged into an item in space at its position,
/// an installed core drops, rigs are destroyed and contents go to asset safety.
fn package_structure(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, now: u64) -> DisposalOutcome {
    let mut out = DisposalOutcome::default();
    let Some(st) = snap.structures.get(&structure_id).cloned() else { return out };
    dispose_contents(ctx, snap, ext, &st, false, now, &mut out);
    if st.quantum_core_installed {
        if let Some(core) = structure_profile(ctx, st.type_id).and_then(|p| p.core_type_id) {
            out.dropped_core_item_id = Some(new_space_item(snap, core, st.owner_corp_id, st.solar_system_id, st.position, 1));
        }
    }
    out.packaged_item_id = Some(new_space_item(snap, st.type_id, st.owner_corp_id, st.solar_system_id, st.position, 1));
    snap.structures.remove(&structure_id);
    ext.lifecycle.remove(&structure_id);
    ext.unanchoring_until_unix.remove(&structure_id);
    ext.descriptions.remove(&structure_id);
    snap.structure_vulnerability_windows.remove(&structure_id);
    out
}

/// A quantum core of the structure's SDE deed type located in the structure (core room).
fn installed_core_item(snap: &WorldStateSnapshot, st: &UpwellStructureRecord, core_type: u32) -> Option<i64> {
    let corp = st.owner_corp_id;
    let mut ids: Vec<i64> = snap
        .items
        .values()
        .filter(|i| i.location_id == st.structure_id && i.type_id == core_type && (i.owner_id == corp || snap.characters.get(&i.owner_id).map(|c| c.corporation_id == corp).unwrap_or(false)))
        .map(|i| i.item_id)
        .collect();
    ids.sort_unstable();
    ids.first().copied()
}

fn full_repair(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &StructureExtendedState, profile: &StructureProfile, structure_id: i64, now: u64) {
    let Some(st) = snap.structures.get(&structure_id).cloned() else { return };
    let power = power_state(ctx, snap, ext, &st, now);
    let max_shield = effective_max_shield(ctx, snap, profile, structure_id, power);
    if let Some(s) = snap.structures.get_mut(&structure_id) {
        s.shield_hp = max_shield;
        s.armor_hp = profile.armor_hp;
        s.hull_hp = profile.hull_hp;
        if matches!(s.state.as_str(), STATE_ARMOR_VULNERABLE | STATE_HULL_VULNERABLE) {
            s.state = STATE_SHIELD_VULNERABLE.into();
        }
    }
}

/// Burns hourly service-module fuel up to `now`; modules go offline when the bay runs dry.
fn burn_fuel(ctx: &ServerContext, snap: &mut WorldStateSnapshot, life: &mut StructureLifecycleState, structure_id: i64, now: u64) {
    let fuel_groups = service_fuel_groups(ctx);
    let bay_items: Vec<(i64, u32)> = snap
        .items
        .values()
        .filter(|i| i.location_id == structure_id && i.flag == InventoryFlag::SpecializedFuelBay && i.quantity > 0 && ctx.sde.types.get(&i.type_id).is_some_and(|t| fuel_groups.contains(&t.group_id)))
        .map(|i| (i.item_id, i.quantity as u32))
        .collect();
    for (iid, qty) in bay_items {
        snap.items.remove(&iid);
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.fuel_blocks = st.fuel_blocks.saturating_add(qty);
        }
    }
    // Onlining requirement (Anchoring_structures.md): a module only comes online when the fuel
    // bay holds its `serviceModuleFuelOnlineAmount`.
    let fuel_now = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
    for (iid, _, online_amount, _) in online_service_modules(ctx, snap, structure_id) {
        if !life.online_modules.contains(&iid) {
            if fuel_now >= online_amount {
                life.online_modules.push(iid);
            } else {
                snap.module_online_states.insert(iid, false);
            }
        }
    }
    life.online_modules.retain(|iid| snap.module_online_states.get(iid).copied().unwrap_or(false));
    let mut tick = life.last_fuel_tick_unix.unwrap_or(now);
    while tick + SECS_PER_HOUR <= now {
        tick += SECS_PER_HOUR;
        let online = online_service_modules(ctx, snap, structure_id);
        if online.is_empty() {
            continue;
        }
        let need: u32 = online.iter().map(|m| m.1).sum();
        let Some(st) = snap.structures.get_mut(&structure_id) else { return };
        if st.fuel_blocks >= need {
            st.fuel_blocks -= need;
        } else {
            for m in &online {
                snap.module_online_states.insert(m.0, false);
            }
            life.online_modules.clear();
            life.low_power_since_unix.get_or_insert(tick);
        }
    }
    life.last_fuel_tick_unix = Some(tick);
    if online_service_modules(ctx, snap, structure_id).is_empty() {
        life.low_power_since_unix.get_or_insert(now);
    } else {
        life.low_power_since_unix = None;
    }
}

/// Advances every elapsed lifecycle timer of one structure. Returns the disposal outcome when
/// the structure left space (unanchor completed).
pub fn sync_structure(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, now: u64) -> Option<DisposalOutcome> {
    let st = snap.structures.get(&structure_id).cloned()?;
    if st.state == STATE_DESTROYED {
        return None;
    }
    if ext.unanchoring_until_unix.get(&structure_id).is_some_and(|&until| now >= until) {
        return Some(package_structure(ctx, snap, ext, structure_id, now));
    }
    let profile = structure_profile(ctx, st.type_id)?;
    super::structure_upwell_ops::apply_pending_reinforce_schedule(snap, ext, structure_id, now);
    for _ in 0..16 {
        let st = snap.structures.get(&structure_id).cloned()?;
        let timer = st.reinforced_until_unix;
        let next = match st.state.as_str() {
            STATE_ANCHOR_VULNERABLE if now >= timer => Some((STATE_ANCHORING, timer + ANCHORING_SECS)),
            STATE_ANCHORING if now >= timer => Some(if profile.core_type_id.is_some() { (STATE_NO_CORE, 0) } else { (STATE_ONLINING, timer + ONLINING_SECS) }),
            STATE_NO_CORE => {
                let core_type = profile.core_type_id.unwrap_or(0);
                installed_core_item(snap, &st, core_type).map(|core_item| {
                    match snap.items.get_mut(&core_item) {
                        Some(it) if it.quantity > 1 => {
                            it.quantity -= 1;
                            it.version += 1;
                        }
                        _ => {
                            snap.items.remove(&core_item);
                        }
                    }
                    if let Some(s) = snap.structures.get_mut(&structure_id) {
                        s.quantum_core_installed = true;
                    }
                    (STATE_ONLINING, now + ONLINING_SECS)
                })
            }
            STATE_ONLINING if now >= timer => Some((STATE_SHIELD_VULNERABLE, 0)),
            STATE_ARMOR_REINFORCED if now >= timer => Some((STATE_ARMOR_VULNERABLE, 0)),
            STATE_HULL_REINFORCED if now >= timer => Some((STATE_HULL_VULNERABLE, 0)),
            _ => None,
        };
        let Some((state, until)) = next else { break };
        let leaving = st.state.clone();
        if let Some(s) = snap.structures.get_mut(&structure_id) {
            s.state = state.into();
            s.reinforced_until_unix = until;
        }
        let life = ext.lifecycle.entry(structure_id).or_default();
        let mut repair_now = false;
        match state {
            STATE_SHIELD_VULNERABLE => {
                life.last_fuel_tick_unix = Some(timer);
                life.low_power_since_unix = Some(timer);
                life.repair_until_unix = None;
                life.fight_started_unix = None;
                repair_now = true;
            }
            // Leaving a reinforcement starts a repair timer at the exit time.
            STATE_ARMOR_VULNERABLE if leaving == STATE_ARMOR_REINFORCED => {
                life.repair_until_unix = Some(timer + REPAIR_SHIELD_ARMOR_SECS);
                life.fight_started_unix = Some(timer);
            }
            STATE_HULL_VULNERABLE if leaving == STATE_HULL_REINFORCED => {
                life.repair_until_unix = Some(timer + REPAIR_HULL_SECS);
                life.fight_started_unix = Some(timer);
            }
            _ => {}
        }
        if repair_now {
            full_repair(ctx, snap, ext, &profile, structure_id, timer);
        }
    }
    let st = snap.structures.get(&structure_id).cloned()?;
    if is_operational(&st.state) {
        let mut life = ext.lifecycle.remove(&structure_id).unwrap_or_default();
        burn_fuel(ctx, snap, &mut life, structure_id, now);
        let repair_due = life.repair_until_unix.is_some_and(|t| now >= t);
        if repair_due {
            life.repair_until_unix = None;
            life.fight_started_unix = None;
        }
        ext.lifecycle.insert(structure_id, life);
        if repair_due {
            full_repair(ctx, snap, ext, &profile, structure_id, now);
        }
        // Low power clamps the shield to the weaker low-power maximum.
        let power = power_state(ctx, snap, ext, &st, now);
        let max_shield = effective_max_shield(ctx, snap, &profile, structure_id, power);
        if let Some(s) = snap.structures.get_mut(&structure_id) {
            s.shield_hp = s.shield_hp.min(max_shield);
        }
    } else if let Some(life) = ext.lifecycle.get_mut(&structure_id) {
        if life.repair_until_unix.is_some_and(|t| now >= t) {
            life.repair_until_unix = None;
            life.fight_started_unix = None;
            if let Some(s) = snap.structures.get_mut(&structure_id) {
                s.hull_hp = profile.hull_hp;
            }
        }
    }
    None
}

/// Syncs every structure (called before structure RPCs). Returns structures that left space.
pub fn sync_all_structures(ctx: &ServerContext) -> Vec<(i64, DisposalOutcome)> {
    let now = ctx.now_unix();
    let mut sov_claims: Vec<(u32, i64, i64, i64)> = Vec::new();
    let gone = ctx.db.with_mut_snapshot(|snap| {
        let mut ids: Vec<i64> = snap.structures.keys().copied().collect();
        ids.sort_unstable();
        let mut ext = load_ext_state(snap);
        let mut gone = Vec::new();
        for id in ids {
            if let Some(st) = snap.structures.get(&id).cloned() {
                if st.type_id == crate::services::sov_services::TYPE_SOVEREIGNTY_HUB
                    && st.state == STATE_ONLINING
                    && now >= st.reinforced_until_unix
                {
                    if let Some(s) = snap.structures.get_mut(&id) {
                        s.state = STATE_SHIELD_VULNERABLE.into();
                        s.reinforced_until_unix = 0;
                    }
                    if let Some(aid) = snap.corporations.get(&st.owner_corp_id).and_then(|c| c.alliance_id) {
                        sov_claims.push((st.solar_system_id, aid, st.owner_corp_id, id));
                    }
                    continue;
                }
            }
            if let Some(out) = sync_structure(ctx, snap, &mut ext, id, now) {
                gone.push((id, out));
            }
        }
        save_ext_state(snap, &ext);
        gone
    });
    for (sys_id, aid, corp_id, hub_id) in sov_claims {
        let _ = crate::services::sov_services::claim_system_sovereignty(ctx, sys_id, aid, corp_id, hub_id);
    }
    gone
}

/// Result of one weapon cycle against a structure.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageOutcome {
    pub applied_damage: f64,
    pub layer: &'static str,
    pub repair_paused: bool,
    pub state: String,
    pub reinforced_until_unix: u64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub destruction: Option<DisposalOutcome>,
}

/// Applies one weapon cycle (`raw_damage` over `cycle_secs`) to a structure: layer damage cap,
/// repair-timer pause threshold, reinforcement transitions and destruction.
pub fn apply_structure_damage(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, raw_damage: f64, cycle_secs: f64, attacker_char_id: i64, now: u64) -> Result<DamageOutcome, BridgeRpcError> {
    let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| BridgeRpcError { code: "StructureNotFound".into(), message: format!("Structure {structure_id} does not exist") })?;
    let profile = structure_profile(ctx, st.type_id).ok_or_else(|| BridgeRpcError { code: "InvalidStructureType".into(), message: format!("Type {} is not an Upwell structure", st.type_id) })?;
    let (layer, cap, pause) = match st.state.as_str() {
        STATE_SHIELD_VULNERABLE => ("shield", profile.shield_damage_cap, profile.pause_shield_dps),
        STATE_ARMOR_VULNERABLE => ("armor", profile.armor_damage_cap, profile.pause_armor_dps),
        STATE_HULL_VULNERABLE | STATE_ANCHOR_VULNERABLE | STATE_NO_CORE | STATE_ONLINING => ("hull", profile.hull_damage_cap, profile.pause_hull_dps),
        STATE_DESTROYED => return Err(BridgeRpcError { code: "StructureAlreadyDestroyed".into(), message: format!("Structure {structure_id} is already destroyed") }),
        other => return Err(BridgeRpcError { code: "StructureReinforcedInvulnerable".into(), message: format!("Structure {structure_id} is invulnerable ({other}) until {}", st.reinforced_until_unix) }),
    };
    let cycle = cycle_secs.max(0.1);
    let applied = crate::formulas::clamp_upwell_structure_damage(raw_damage, cap, cycle);
    let qualifying = applied / cycle >= pause;
    let power = power_state(ctx, snap, ext, &st, now);
    let space = if ext.war_hq_structures.contains(&structure_id) { SpaceClass::WarHq } else { space_class(ctx, st.solar_system_id) };
    let hour = reinforce_hour(snap, &st);
    let cycle_whole = cycle.ceil() as u64;
    let life = ext.lifecycle.entry(structure_id).or_default();
    if life.repair_until_unix.is_none() {
        life.fight_started_unix = Some(now);
    }
    let fight_start = life.fight_started_unix.unwrap_or(now);
    let jitter_offset = life.reinforce_jitter_secs;
    let repair_len = if layer == "hull" { REPAIR_HULL_SECS } else { REPAIR_SHIELD_ARMOR_SECS };
    // Damage starts the repair timer; damage at/above the pause threshold freezes it for the cycle.
    let mut repair_until = life.repair_until_unix.unwrap_or(now + repair_len);
    if qualifying {
        repair_until = now + cycle_whole + repair_until.saturating_sub(now);
    }
    life.repair_until_unix = Some(repair_until);
    let mut destruction = None;
    {
        let s = snap.structures.get_mut(&structure_id).ok_or_else(|| BridgeRpcError { code: "StructureNotFound".into(), message: "structure vanished".into() })?;
        s.last_damage_unix = now;
        // Anchoring/onlining timers are paused the same way while under qualifying fire.
        if qualifying && matches!(s.state.as_str(), STATE_ANCHOR_VULNERABLE | STATE_ONLINING) {
            s.reinforced_until_unix += cycle_whole;
        }
        match layer {
            "shield" => {
                s.shield_hp = (s.shield_hp - applied).max(0.0);
                if s.shield_hp <= 0.0 {
                    match armor_reinforcement_secs(profile.hull_class, space, power) {
                        Some(secs) => {
                            s.state = STATE_ARMOR_REINFORCED.into();
                            let base_exit = next_reinforce_exit(fight_start + secs, hour);
                            s.reinforced_until_unix = apply_reinforce_jitter(base_exit, jitter_offset, reinforce_jitter_window_secs(profile.hull_class, true));
                        }
                        None => s.state = STATE_ARMOR_VULNERABLE.into(),
                    }
                    life.repair_until_unix = if s.state == STATE_ARMOR_VULNERABLE { Some(now + REPAIR_SHIELD_ARMOR_SECS) } else { None };
                }
            }
            "armor" => {
                s.armor_hp = (s.armor_hp - applied).max(0.0);
                if s.armor_hp <= 0.0 {
                    match hull_reinforcement_secs(profile.hull_class, space, power) {
                        Some(secs) => {
                            s.state = STATE_HULL_REINFORCED.into();
                            let base_exit = if profile.hull_class == HullClass::Flex {
                                fight_start + secs
                            } else {
                                next_reinforce_exit(fight_start + secs, hour)
                            };
                            s.reinforced_until_unix = apply_reinforce_jitter(base_exit, jitter_offset, reinforce_jitter_window_secs(profile.hull_class, false));
                            life.repair_until_unix = None;
                        }
                        None => {
                            s.state = STATE_HULL_VULNERABLE.into();
                            life.repair_until_unix = Some(now + REPAIR_HULL_SECS);
                        }
                    }
                }
            }
            _ => {
                s.hull_hp = (s.hull_hp - applied).max(0.0);
            }
        }
    }
    if snap.structures.get(&structure_id).map(|s| s.hull_hp <= 0.0).unwrap_or(false) {
        destruction = Some(destroy_structure(ctx, snap, ext, structure_id, attacker_char_id, now));
    }
    let s = snap.structures.get(&structure_id).cloned().unwrap_or(st);
    Ok(DamageOutcome { applied_damage: applied, layer, repair_paused: qualifying, state: s.state, reinforced_until_unix: s.reinforced_until_unix, shield_hp: s.shield_hp, armor_hp: s.armor_hp, hull_hp: s.hull_hp, destruction })
}

/// Anchoring position rules: Pochven restriction, Palatine Keepstar cluster limit,
/// Keepstar/FLEX per-system limits, SDE `disallowInHighSec`, and the DERIVED 1,000 km minimum
/// distance from NPC stations, stargates, asteroid belts and other live structures.
pub fn check_anchor_position(ctx: &ServerContext, snap: &WorldStateSnapshot, profile: &StructureProfile, sys_id: u32, pos: glam::DVec3) -> Result<(), BridgeRpcError> {
    let region_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(0);
    if region_id == POCHVEN_REGION_ID {
        return Err(BridgeRpcError { code: "CannotAnchorInPochven".into(), message: format!("Upwell structures cannot be anchored in Pochven solar system {sys_id}") });
    }
    if profile.type_id == PALATINE_KEEPSTAR_TYPE_ID
        && snap.structures.values().any(|s| s.type_id == PALATINE_KEEPSTAR_TYPE_ID && s.state != STATE_DESTROYED)
    {
        return Err(BridgeRpcError { code: "PalatineKeepstarLimitReached".into(), message: "Only one Upwell Palatine Keepstar (40340) may be active in the cluster at any time".into() });
    }
    if profile.hull_class == HullClass::XLarge
        && profile.group_id == 1657
        && snap.structures.values().any(|s| s.solar_system_id == sys_id && s.state != STATE_DESTROYED && structure_profile(ctx, s.type_id).is_some_and(|p| p.hull_class == HullClass::XLarge && p.group_id == 1657))
    {
        return Err(BridgeRpcError { code: "KeepstarPerSystemLimit".into(), message: format!("Only one Keepstar-sized Citadel may be anchored in solar system {sys_id}") });
    }
    if profile.hull_class == HullClass::Flex
        && snap.structures.values().any(|s| s.solar_system_id == sys_id && s.state != STATE_DESTROYED && s.type_id == profile.type_id)
    {
        return Err(BridgeRpcError { code: "FlexStructurePerSystemLimit".into(), message: format!("Only one FLEX structure of type {} may be anchored in solar system {sys_id}", profile.type_id) });
    }
    if profile.disallow_in_highsec && space_class(ctx, sys_id) == SpaceClass::HighSec {
        return Err(BridgeRpcError { code: "CannotAnchorInHighSec".into(), message: format!("Structure type {} cannot be anchored in high-security system {sys_id}", profile.type_id) });
    }
    let too_close = |p: glam::DVec3| p.distance(pos) < MIN_ANCHOR_DISTANCE_M;
    let station_hit = ctx.sde.stations.values().any(|s| s.solar_system_id == sys_id && too_close(s.position));
    let celestial_hit = ctx.sde.celestials_in_system(sys_id).iter().any(|c| ANCHOR_BLOCKING_CELESTIAL_GROUPS.contains(&c.group_id) && too_close(c.position));
    let structure_hit = snap.structures.values().any(|s| s.solar_system_id == sys_id && s.state != STATE_DESTROYED && too_close(glam::DVec3::from_array(s.position)));
    if station_hit || celestial_hit || structure_hit {
        return Err(BridgeRpcError { code: "TooCloseToStructureOrStation".into(), message: format!("Anchoring position must be at least {} km from stations, stargates, asteroid belts and other structures", MIN_ANCHOR_DISTANCE_M / 1_000.0) });
    }
    Ok(())
}

/// Online service module type IDs fitted to `structure_id`.
pub fn online_service_type_ids(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> Vec<u32> {
    online_service_modules(ctx, snap, structure_id)
        .into_iter()
        .filter_map(|(iid, _, _, _)| snap.items.get(&iid).map(|it| it.type_id))
        .collect()
}

/// Validates that `structure_id` can be refitted right now (`feat:structures.structure-fitting`,
/// `feat:structures.fitting-state-locks`): must be in `ShieldVulnerable`, no active repair timer,
/// and at 100% shield/armor/hull HP.
pub fn validate_structure_refit(ctx: &ServerContext, snap: &WorldStateSnapshot, ext: &StructureExtendedState, structure_id: i64, now: u64) -> Result<(UpwellStructureRecord, StructureProfile), BridgeRpcError> {
    let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| BridgeRpcError { code: "StructureNotFound".into(), message: format!("Structure {structure_id} does not exist") })?;
    let profile = structure_profile(ctx, st.type_id).ok_or_else(|| BridgeRpcError { code: "InvalidStructureType".into(), message: format!("Type {} is not an Upwell structure", st.type_id) })?;
    if st.state != STATE_SHIELD_VULNERABLE {
        return Err(BridgeRpcError { code: "StructureFittingLockedInState".into(), message: format!("Structure {structure_id} cannot be refitted in state {}", st.state) });
    }
    if ext.lifecycle.get(&structure_id).and_then(|l| l.repair_until_unix).is_some_and(|until| now < until) {
        return Err(BridgeRpcError { code: "StructureRepairingCannotRefit".into(), message: format!("Structure {structure_id} cannot be refitted while its repair timer is active") });
    }
    let power = power_state(ctx, snap, ext, &st, now);
    let max_shield = effective_max_shield(ctx, snap, &profile, structure_id, power);
    if st.shield_hp + 1e-3 < max_shield || st.armor_hp + 1e-3 < profile.armor_hp || st.hull_hp + 1e-3 < profile.hull_hp {
        return Err(BridgeRpcError { code: "StructureDamagedCannotRefit".into(), message: format!("Structure {structure_id} must be at 100% shield, armor, and hull HP to refit") });
    }
    Ok((st, profile))
}
