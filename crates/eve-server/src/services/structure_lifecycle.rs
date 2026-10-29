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
use std::collections::{HashMap, HashSet};

// ---- SDE dogma attribute IDs (sde_dogma_attributes) ----
pub const ATTR_HP: u16 = 9; pub const ATTR_LOW_SLOTS: u16 = 12; pub const ATTR_MED_SLOTS: u16 = 13; pub const ATTR_HI_SLOTS: u16 = 14;
pub const ATTR_SHIELD_CAPACITY: u16 = 263; pub const ATTR_ARMOR_HP: u16 = 265; pub const ATTR_RIG_SLOTS: u16 = 1137; pub const ATTR_RIG_SIZE: u16 = 1547;
pub const ATTR_DISALLOW_IN_HIGHSEC: u16 = 1970; pub const ATTR_SHIELD_DAMAGE_LIMIT: u16 = 2034; pub const ATTR_ARMOR_DAMAGE_LIMIT: u16 = 2035; pub const ATTR_STRUCTURE_DAMAGE_LIMIT: u16 = 2036;
pub const ATTR_SERVICE_SLOTS: u16 = 2056; pub const ATTR_SERVICE_FUEL_GROUP: u16 = 2108; pub const ATTR_SERVICE_FUEL_PER_HOUR: u16 = 2109; pub const ATTR_SERVICE_FUEL_ONLINE: u16 = 2110;
pub const ATTR_STRUCTURE_FULL_POWER_HP_MULT: u16 = 2743; pub const ATTR_SERVICE_FULL_POWER_HP_MULT: u16 = 2744; pub const ATTR_REQUIRES_DEED_TYPE: u16 = 3101;
pub const ATTR_PAUSE_SHIELD_REPAIR_DPS: u16 = 3354; pub const ATTR_PAUSE_ARMOR_REPAIR_DPS: u16 = 3355; pub const ATTR_PAUSE_HULL_REPAIR_DPS: u16 = 3356;
pub const ATTR_STRUCTURE_ROLE_BONUS: u16 = 2333; pub const ATTR_SERVICE_ROLE_BONUS: u16 = 2339;
pub const ATTR_ENG_MAT_BONUS: u16 = 2600; pub const ATTR_ENG_COST_BONUS: u16 = 2601; pub const ATTR_ENG_TIME_BONUS: u16 = 2602;
pub const ATTR_REACTION_TIME_MULT: u16 = 2721; pub const ATTR_REFINING_YIELD_BONUS: u16 = 2722; pub const ATTR_TETHERING_RANGE: u16 = 2268;
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
/// DERIVED (Vulnerability.md:17,64,89): 30-minute hull repair window (15m shield/armor, 30m hull).
pub const REPAIR_HULL_SECS: u64 = 30 * 60;
/// DERIVED (Vulnerability.md "Structures in abandoned state (no fuel used for 7 days)").
pub const ABANDON_AFTER_LOW_POWER_SECS: u64 = 7 * 86_400;
/// Asset safety wraps auto-deliver to the nearest NPC station after 20 days (`feat:lifecycle.asset_safety-auto_delivery`).
pub const ASSET_SAFETY_AUTO_DELIVERY_SECS: u64 = 20 * 86_400;
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
        fuel_bonus_pct: a(ATTR_SERVICE_ROLE_BONUS).or_else(|| a(ATTR_STRUCTURE_ROLE_BONUS)).unwrap_or(0.0),
        eng_mat_bonus_pct: a(ATTR_ENG_MAT_BONUS).map(|v| (v - 1.0) * 100.0).unwrap_or(0.0),
        eng_cost_bonus_pct: a(ATTR_ENG_COST_BONUS).map(|v| (v - 1.0) * 100.0).unwrap_or(0.0),
        eng_time_bonus_pct: a(ATTR_ENG_TIME_BONUS).map(|v| (v - 1.0) * 100.0).unwrap_or(0.0),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFamily {
    Citadel,
    Engineering,
    Refinery,
    Other,
}

pub fn service_family_for_module(ctx: &ServerContext, type_id: u32) -> ServiceFamily {
    match ctx.sde.types.get(&type_id).map(|t| t.group_id) {
        Some(1321) => ServiceFamily::Citadel, // Structure Citadel Service Module (Market, Clone Bay)
        Some(1415 | 1416) => ServiceFamily::Engineering, // Structure Engineering & Research Service Modules
        Some(1322 | 1887) => ServiceFamily::Refinery, // Structure Resource Processing & Moon Drilling Modules
        _ => ServiceFamily::Other,
    }
}

pub fn structure_family(group_id: u32) -> ServiceFamily {
    match group_id {
        1657 => ServiceFamily::Citadel,
        1404 | 1876 => ServiceFamily::Engineering,
        1406 => ServiceFamily::Refinery,
        _ => ServiceFamily::Other,
    }
}

fn is_fitting_slot(flag: InventoryFlag) -> bool {
    matches!(flag as u16, 11..=34) || is_rig_slot(flag) || is_service_slot(flag)
}

/// Service modules fitted to the structure: `(item_id, type_id, fuel/hour, online fuel, full-power HP mult)`.
fn fitted_service_modules(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> Vec<(i64, u32, u32, u32, f64)> {
    let mut out: Vec<_> = snap
        .items
        .values()
        .filter(|i| i.location_id == structure_id && is_service_slot(i.flag))
        .filter_map(|i| {
            let t = ctx.sde.types.get(&i.type_id)?;
            let per_hour = *t.default_attributes.get(&ATTR_SERVICE_FUEL_PER_HOUR)?;
            let online = t.default_attributes.get(&ATTR_SERVICE_FUEL_ONLINE).copied().unwrap_or(0.0);
            let mult = t.default_attributes.get(&ATTR_SERVICE_FULL_POWER_HP_MULT).copied().unwrap_or(1.0);
            Some((i.item_id, i.type_id, per_hour as u32, online as u32, mult))
        })
        .collect();
    out.sort_by_key(|m| m.0);
    out
}

fn online_service_modules(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> Vec<(i64, u32, u32, u32, f64)> {
    if snap.structures.get(&structure_id).is_some_and(|st| {
        !st.quantum_core_installed && structure_profile(ctx, st.type_id).is_some_and(|p| p.core_type_id.is_some())
    }) {
        return Vec::new();
    }
    fitted_service_modules(ctx, snap, structure_id).into_iter().filter(|m| snap.module_online_states.get(&m.0).copied().unwrap_or(false)).collect()
}

pub fn is_operational(state: &str) -> bool {
    !matches!(state, STATE_ANCHOR_VULNERABLE | STATE_ANCHORING | STATE_NO_CORE | STATE_ONLINING | STATE_DESTROYED)
}

/// Power state: full power with at least one online service module, low power otherwise,
/// abandoned after [`ABANDON_AFTER_LOW_POWER_SECS`] continuously in low power (except Faction
/// Fortizars `47512..=47516`, which never enter the Abandoned state per `Asset_safety.md:68`).
pub fn power_state(ctx: &ServerContext, snap: &WorldStateSnapshot, ext: &StructureExtendedState, st: &UpwellStructureRecord, now: u64) -> PowerState {
    if !is_operational(&st.state) {
        return PowerState::NotOperational;
    }
    if !online_service_modules(ctx, snap, st.structure_id).is_empty() {
        return PowerState::FullPower;
    }
    if matches!(st.type_id, 47512..=47516) {
        return PowerState::LowPower;
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
    let module_mult = online_service_modules(ctx, snap, structure_id).iter().map(|m| m.4).fold(1.0_f64, f64::max);
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
/// `structureVulnerability.UpdateReinforceTimesFromClient`), falling back to the owning
/// corporation's default reinforcement hour (`feat:corporation.structure-reinforce-default`).
fn reinforce_hour(snap: &WorldStateSnapshot, st: &UpwellStructureRecord) -> u64 {
    snap.structure_vulnerability_windows
        .get(&st.structure_id)
        .map(|w| w.0)
        .or_else(|| snap.corp_governance.get(&st.owner_corp_id).map(|g| u64::from(g.reinforce_hour)))
        .unwrap_or(0)
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

const AU_METERS: f64 = 149_597_870_700.0;

/// Nearest NPC station by Euclidean distance in AU (`galactic_pos_ly`) within the allowed
/// security band (`high -> high` for `sec >= 0.45`, `low -> low` / `null -> low` for `sec < 0.45`),
/// tie-broken by lowest station ID (`feat:lifecycle.asset_safety-auto_delivery`).
pub fn nearest_npc_station(ctx: &ServerContext, origin: u32) -> Option<i64> {
    nearest_npc_station_from_pos_opt(ctx, origin, None)
}

pub fn nearest_npc_station_from_pos(ctx: &ServerContext, origin: u32, pos: glam::DVec3) -> Option<i64> {
    nearest_npc_station_from_pos_opt(ctx, origin, Some(pos))
}

fn nearest_npc_station_from_pos_opt(ctx: &ServerContext, origin: u32, pos_opt: Option<glam::DVec3>) -> Option<i64> {
    let origin_sys = ctx.sde.solar_systems.get(&origin);
    let origin_sec = origin_sys.map(|s| s.security_status).unwrap_or(-1.0);
    let origin_pos = origin_sys
        .map(|s| glam::DVec3::from_array(s.galactic_pos_ly))
        .unwrap_or(glam::DVec3::ZERO);
    let allowed = |sys: u32| -> bool {
        let Some(s) = ctx.sde.solar_systems.get(&sys) else {
            return false;
        };
        if origin_sec >= 0.45 {
            s.security_status >= 0.45
        } else {
            s.security_status > 0.0 && s.security_status < 0.45
        }
    };
    let mut best: Option<(f64, i64)> = None;
    for st in ctx.sde.stations.values() {
        if !allowed(st.solar_system_id) {
            continue;
        }
        let dist_au = if st.solar_system_id == origin {
            if let Some(pos) = pos_opt {
                pos.distance(st.position) / AU_METERS
            } else {
                0.0
            }
        } else if let Some(cand_sys) = ctx.sde.solar_systems.get(&st.solar_system_id) {
            origin_pos.distance(glam::DVec3::from_array(cand_sys.galactic_pos_ly)) * 63_241.077
        } else {
            continue;
        };
        match best {
            None => best = Some((dist_au, st.station_id)),
            Some((best_d, best_id)) => {
                if dist_au + 1e-9 < best_d
                    || ((dist_au - best_d).abs() <= 1e-9 && st.station_id < best_id)
                {
                    best = Some((dist_au, st.station_id));
                }
            }
        }
    }
    best.map(|(_, id)| id)
}

pub fn sync_asset_safety_wraps(ctx: &ServerContext) {
    super::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
}

/// What happened to a structure's contents when it left space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisposalOutcome {
    pub killmail_id: Option<i64>,
    pub wreck_item_id: Option<i64>,
    pub dropped_core_item_id: Option<i64>,
    pub packaged_item_id: Option<i64>,
    pub asset_safety_wrap_ids: Vec<i64>,
    pub dropped_item_ids: Vec<i64>,
    pub destroyed_item_ids: Vec<i64>,
}

/// Moves a structure's contents out: rigs are destroyed; in J-space or when an Abandoned
/// structure is destroyed (`feat:lifecycle.structure-abandoned_destroyed`) contents drop as loot
/// at the structure position; in non-abandoned K-space fitted modules drop/are destroyed
/// (destruction) or join asset safety (unanchor) and hangar contents go to per-owner wraps.
fn dispose_contents(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, st: &UpwellStructureRecord, destroyed: bool, now: u64, out: &mut DisposalOutcome) {
    crate::services::structure_acl_defense_rules::materialize_structure_fuel_for_disposal(ctx, snap, st.structure_id);
    let wormhole = space_class(ctx, st.solar_system_id) == SpaceClass::Wormhole;
    let abandoned = destroyed && power_state(ctx, snap, ext, st, now) == PowerState::Abandoned;
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
        } else if wormhole || abandoned || (destroyed && fitted) {
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
    let station = nearest_npc_station_from_pos(ctx, st.solar_system_id, glam::DVec3::from_array(st.position));
    let mut by_owner: HashMap<i64, Vec<i64>> = HashMap::new();
    for (iid, owner) in to_safety {
        by_owner.entry(owner).or_default().push(iid);
    }
    let mut owners: Vec<i64> = by_owner.keys().copied().collect();
    owners.sort_unstable();
    for owner in owners {
        let item_ids = by_owner.remove(&owner).unwrap_or_default();
        let mut all_ids_for_val = item_ids.clone();
        let mut frontier = item_ids.clone();
        while !frontier.is_empty() {
            let children: Vec<i64> = snap
                .items
                .values()
                .filter(|i| frontier.contains(&i.location_id) && !all_ids_for_val.contains(&i.item_id))
                .map(|i| i.item_id)
                .collect();
            all_ids_for_val.extend(children.iter().copied());
            frontier = children;
        }
        let value: f64 = all_ids_for_val
            .iter()
            .filter_map(|id| snap.items.get(id))
            .map(|it| ctx.sde.types.get(&it.type_id).map(|t| t.base_price).unwrap_or(0.0) * f64::from(it.quantity.max(1)))
            .sum();
        snap.next_entity_id += 1;
        let wrap_id = snap.next_entity_id;
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
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
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        out.asset_safety_wrap_ids.push(wrap_id);
    }
}

/// Removes the structure's ball, emits `OnStructureRemoved` / `OnStructureRemovedFromSpace` at
/// completion, and spawns balls at the structure position for dropped core, fittings, and
/// packaged structure item (`feat:lifecycle.structure-leave_space`).
fn emit_structure_left_space_and_spawn_drops(ctx: &ServerContext, snap: &WorldStateSnapshot, st: &UpwellStructureRecord, out: &DisposalOutcome) {
    let active_handle = ctx.systems.active_system_handles().into_iter().find(|(id, _)| *id == st.solar_system_id).map(|(_, h)| h);
    if let Some(handle) = active_handle {
        handle.try_remove_ball(st.structure_id);
        let pos = glam::DVec3::from_array(st.position);
        let alliance_id = snap.corporations.get(&st.owner_corp_id).and_then(|c| c.alliance_id).unwrap_or(0);
        for iid in out.wreck_item_id.into_iter().chain(out.dropped_core_item_id).chain(out.packaged_item_id).chain(out.dropped_item_ids.iter().copied()) {
            if let Some(it) = snap.items.get(&iid) {
                if it.location_id != i64::from(st.solar_system_id) {
                    continue;
                }
                let rad = ctx.sde.types.get(&it.type_id).map(|t| t.radius.max(10.0)).unwrap_or(25.0);
                let mass = ctx.sde.types.get(&it.type_id).map(|t| t.mass.max(1.0)).unwrap_or(1_000.0);
                handle.try_spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: iid, type_id: it.type_id, position: pos, velocity: glam::DVec3::ZERO,
                        max_velocity: 0.0, inertia: 1.0, mass, radius: rad, speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(it.type_id),
                    eve_destiny::SlimItemMeta {
                        owner_id: it.owner_id, char_id: None, corp_id: st.owner_corp_id, alliance_id,
                        group_id: ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(186),
                        category_id: ctx.sde.types.get(&it.type_id).map(|t| t.category_id).unwrap_or(65),
                        security_status: 0.0, is_cloaked: false,
                    },
                );
            }
        }
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    let mut sys_recips: Vec<(i64, Option<u64>)> = snap
        .characters
        .values()
        .filter(|c| c.solar_system_id == st.solar_system_id && snap.sessions.get(&c.user_id).is_some_and(|os| os.character_id == Some(c.character_id)))
        .map(|c| (c.character_id, Some(c.user_id)))
        .collect();
    sys_recips.sort_by_key(|(cid, _)| *cid);
    sys_recips.dedup_by_key(|(cid, _)| *cid);
    let mut corp_recips: Vec<(i64, Option<u64>)> = snap
        .characters
        .values()
        .filter(|c| c.corporation_id == st.owner_corp_id && snap.sessions.get(&c.user_id).is_some_and(|os| os.character_id == Some(c.character_id)))
        .map(|c| (c.character_id, Some(c.user_id)))
        .collect();
    corp_recips.sort_by_key(|(cid, _)| *cid);
    corp_recips.dedup_by_key(|(cid, _)| *cid);
    let mut queues = ctx.pending_notifications.write().unwrap_or_else(|e| e.into_inner());
    let sys_args = vec![eve_proto::PyRep::Integer(i64::from(st.solar_system_id)), eve_proto::PyRep::Integer(st.structure_id)];
    for (cid, uid) in sys_recips {
        let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(cid, uid, "OnStructureRemoved", "solarsystemid", sys_args.clone());
        let notif = crate::router::PendingMachoNotification {
            recipient_char_id: cid,
            event: "OnStructureRemoved".into(),
            id_type: "solarsystemid".into(),
            args: sys_args.clone(),
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        queues.entry(cid).or_default().push(notif.clone());
        let _ = ctx.notify_tx.send((cid, notif));
    }
    let corp_args = vec![eve_proto::PyRep::Integer(st.structure_id)];
    for (cid, uid) in corp_recips {
        let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(cid, uid, "OnStructureRemovedFromSpace", "corpid", corp_args.clone());
        let notif = crate::router::PendingMachoNotification {
            recipient_char_id: cid,
            event: "OnStructureRemovedFromSpace".into(),
            id_type: "corpid".into(),
            args: corp_args.clone(),
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        queues.entry(cid).or_default().push(notif.clone());
        let _ = ctx.notify_tx.send((cid, notif));
    }
    drop(queues);
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_structure_removed_with_snap(ctx, snap, st.solar_system_id, st.structure_id);
}

/// Maximum tethering distance from the structure center in meters:
/// hull collision radius + SDE attribute 2268 (`tetheringRange`).
pub fn tether_range_m(ctx: &ServerContext, st_type_id: u32) -> f64 {
    let ty = ctx.sde.types.get(&st_type_id);
    let radius = ty.map(|t| t.radius).unwrap_or(0.0);
    let tether_extra = ty
        .and_then(|t| t.default_attributes.get(&ATTR_TETHERING_RANGE).copied())
        .unwrap_or(10_000.0);
    radius + tether_extra
}

/// Destroys a structure: killmail, structure wreck, quantum core in wreck, and asset safety.
pub fn destroy_structure(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, attacker_char_id: i64, now: u64) -> DisposalOutcome {
    let mut out = DisposalOutcome::default();
    let Some(st) = snap.structures.get(&structure_id).cloned() else { return out };
    dispose_contents(ctx, snap, ext, &st, true, now, &mut out);
    let wreck_type_id = ctx.sde.wrecks.get(&st.type_id).copied().unwrap_or(26557);
    snap.next_entity_id += 1;
    let wreck_id = snap.next_entity_id;
    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wreck_id, now, false);
    snap.items.insert(wreck_id, InventoryItemRecord {
        item_id: wreck_id, type_id: wreck_type_id, owner_id: st.owner_corp_id,
        location_id: i64::from(st.solar_system_id), flag: InventoryFlag::None,
        quantity: 1, singleton: true, version: 1, ..Default::default()
    });
    snap.item_positions.insert(wreck_id, st.position);
    out.wreck_item_id = Some(wreck_id);
    if st.quantum_core_installed {
        if let Some(core) = structure_profile(ctx, st.type_id).and_then(|p| p.core_type_id) {
            snap.next_entity_id += 1;
            let core_item_id = snap.next_entity_id;
            snap.items.insert(core_item_id, InventoryItemRecord {
                item_id: core_item_id, type_id: core, owner_id: st.owner_corp_id,
                location_id: wreck_id, flag: InventoryFlag::Cargo,
                quantity: 1, singleton: false, version: 1, ..Default::default()
            });
            out.dropped_core_item_id = Some(core_item_id);
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
    ext.active_pilot_by_structure.remove(&structure_id); ext.previous_ship_by_structure.remove(&structure_id);
    // feat:clones.clone-destroyed-structure: clones in destroyed structure are moved home (implants lost) per V24 rules.
    relocate_structure_clones_to_home(ctx, snap, &st);
    emit_structure_left_space_and_spawn_drops(ctx, snap, &st, &out);
    out
}

pub fn resolve_char_school_or_nearest_station(ctx: &ServerContext, snap: &WorldStateSnapshot, char_id: i64, fallback_sys_id: u32) -> i64 {
    snap.characters
        .get(&char_id)
        .and_then(|c| {
            ctx.sde
                .schools
                .get(&c.school_id)
                .or_else(|| ctx.sde.schools.values().find(|s| s.race_id == c.race_id))
                .map(|s| s.station_id)
        })
        .or_else(|| nearest_npc_station(ctx, fallback_sys_id))
        .or_else(|| ctx.sde.stations.values().map(|s| s.station_id).min())
        .unwrap_or(0)
}

/// Relocates all jump clones installed in a structure to their character's home station with implants destroyed.
fn relocate_structure_clones_to_home(ctx: &ServerContext, snap: &mut WorldStateSnapshot, st: &UpwellStructureRecord) {
    let structure_id = st.structure_id;
    let char_ids: Vec<i64> = snap.jump_clones.keys().copied().collect();
    for cid in char_ids {
        let fallback = resolve_char_school_or_nearest_station(ctx, snap, cid, st.solar_system_id);
        let home = snap
            .characters
            .get(&cid)
            .map(|c| if c.home_station_id > 0 && c.home_station_id != structure_id { c.home_station_id } else { fallback })
            .unwrap_or(fallback);
        if let Some(clones) = snap.jump_clones.get_mut(&cid) {
            for jc in clones.iter_mut() {
                if jc.location_id == structure_id {
                    jc.location_id = home;
                    jc.implants.clear();
                }
            }
        }
    }
}

/// Syncs structures and clears an Upwell `home_station_id` if Clone Bay or ACL access is no longer valid before pod respawn.
pub fn prepare_home_station_respawn(ctx: &ServerContext, char_id: i64) {
    sync_all_structures(ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let Some(home_id) = snap.characters.get(&char_id).map(|c| c.home_station_id) else { return };
        if home_id >= 1_000_000_000_000 && validate_home_station_structure(ctx, snap, char_id, home_id).is_err() {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.home_station_id = 0;
            }
        }
    });
}

/// Completes decommissioning: the structure is packaged into an item in space at its position,
/// an installed core drops, rigs are destroyed and contents go to asset safety.
pub fn package_structure(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, now: u64) -> DisposalOutcome {
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
    ext.active_pilot_by_structure.remove(&structure_id); ext.previous_ship_by_structure.remove(&structure_id);
    ext.descriptions.remove(&structure_id);
    snap.structure_vulnerability_windows.remove(&structure_id);
    // feat:clones.clone-destroyed-structure: clones in unanchored/packaged structure are moved home (implants lost) per V24 rules.
    relocate_structure_clones_to_home(ctx, snap, &st);
    emit_structure_left_space_and_spawn_drops(ctx, snap, &st, &out);
    out
}

/// A quantum core of the structure's SDE deed type located in the structure (core room).
fn installed_core_item(snap: &WorldStateSnapshot, st: &UpwellStructureRecord, core_type: u32) -> Option<i64> {
    let corp = st.owner_corp_id;
    let mut ids: Vec<i64> = snap
        .items
        .values()
        .filter(|i| i.location_id == st.structure_id && (i.flag == InventoryFlag::StructureDeedBay || i.flag as u16 == 172 || i.flag as u16 == 180) && i.type_id == core_type && (i.owner_id == corp || snap.characters.get(&i.owner_id).map(|c| c.corporation_id == corp).unwrap_or(false)))
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
    use crate::services::structure_acl_defense_rules as rules;
    rules::sync_seeded_structure_fuel_bay(ctx, snap, structure_id);
    let first_init = life.last_fuel_tick_unix.is_none();
    let st_type_id = snap.structures.get(&structure_id).map(|s| s.type_id).unwrap_or(0);
    for (iid, mod_tid, _, _, _) in online_service_modules(ctx, snap, structure_id) {
        if !life.online_modules.contains(&iid) {
            if first_init {
                let avail = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
                if avail > 0 {
                    life.online_modules.push(iid);
                } else {
                    snap.module_online_states.insert(iid, false);
                }
            } else {
                let cost = rules::service_module_online_fuel_cost(ctx, st_type_id, mod_tid);
                if rules::consume_structure_fuel_blocks(ctx, snap, structure_id, cost) {
                    life.online_modules.push(iid);
                } else {
                    snap.module_online_states.insert(iid, false);
                }
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
        if !snap.structures.contains_key(&structure_id) {
            return;
        }
        let mut need = 0u32;
        for &(_, mod_tid, per_hour, _, _) in &online {
            let mod_mult = rules::service_module_fuel_multiplier(ctx, st_type_id, mod_tid);
            let mod_burn = (f64::from(per_hour) * mod_mult).round().max(1.0) as u32;
            need = need.saturating_add(mod_burn);
        }
        if !rules::consume_structure_fuel_blocks(ctx, snap, structure_id, need) {
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
    if !st.state.starts_with("Reinforced")
        && !matches!(
            st.state.as_str(),
            STATE_ARMOR_REINFORCED | STATE_HULL_REINFORCED | STATE_ARMOR_VULNERABLE | STATE_HULL_VULNERABLE
        )
        && st.reinforced_until_unix <= now
        && ext.unanchoring_until_unix.get(&structure_id).is_some_and(|&until| now >= until)
    {
        return Some(package_structure(ctx, snap, ext, structure_id, now));
    }
    let profile = structure_profile(ctx, st.type_id)?;
    super::structure_upwell_ops::apply_pending_reinforce_schedule(ctx, snap, ext, structure_id, now);
    super::structure_upwell_ops::apply_pending_profile_applications(ext, now);
    for _ in 0..16 {
        let st = snap.structures.get(&structure_id).cloned()?;
        let timer = st.reinforced_until_unix;
        let next = match st.state.as_str() {
            STATE_ANCHOR_VULNERABLE if now >= timer => Some((STATE_ANCHORING, timer + ANCHORING_SECS)),
            STATE_ANCHORING if now >= timer => Some(if profile.core_type_id.is_some() { (STATE_NO_CORE, 0) } else { (STATE_ONLINING, timer + ONLINING_SECS) }),
            STATE_NO_CORE => {
                if ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.region_id) == Some(POCHVEN_REGION_ID) {
                    None
                } else {
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
                life.shield_fight_started_unix = None;
                life.armor_reinforce_secs_applied = None;
                life.unjittered_reinforce_exit_unix = None;
                repair_now = true;
            }
            // Leaving a reinforcement starts a repair timer at the exit time while preserving
            // `shield_fight_started_unix` for multi-timer exit date calculation (`Vulnerability.md`).
            STATE_ARMOR_VULNERABLE if leaving == STATE_ARMOR_REINFORCED => {
                life.repair_until_unix = Some(timer + REPAIR_SHIELD_ARMOR_SECS);
                life.fight_started_unix = Some(life.unjittered_reinforce_exit_unix.take().unwrap_or(timer));
            }
            STATE_HULL_VULNERABLE if leaving == STATE_HULL_REINFORCED => {
                life.repair_until_unix = Some(timer + REPAIR_HULL_SECS);
                life.fight_started_unix = Some(life.unjittered_reinforce_exit_unix.take().unwrap_or(timer));
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
            life.shield_fight_started_unix = None;
            life.armor_reinforce_secs_applied = None;
            life.unjittered_reinforce_exit_unix = None;
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
            life.shield_fight_started_unix = None;
            life.armor_reinforce_secs_applied = None;
            life.unjittered_reinforce_exit_unix = None;
            if let Some(s) = snap.structures.get_mut(&structure_id) {
                s.hull_hp = profile.hull_hp;
            }
        }
    }
    None
}

/// Syncs every structure (called before structure RPCs). Returns structures that left space.
pub fn sync_all_structures(ctx: &ServerContext) -> Vec<(i64, DisposalOutcome)> {
    super::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
    if life.fight_started_unix.is_none() {
        life.fight_started_unix = Some(now);
    }
    if layer == "shield" && life.shield_fight_started_unix.is_none() {
        life.shield_fight_started_unix = Some(now);
    }
    let fight_start = life.fight_started_unix.unwrap_or(now);
    let is_deploying = matches!(st.state.as_str(), STATE_ANCHOR_VULNERABLE | STATE_ONLINING);
    if is_deploying {
        life.repair_until_unix = None;
    } else {
        let repair_len = if layer == "hull" { REPAIR_HULL_SECS } else { REPAIR_SHIELD_ARMOR_SECS };
        let mut repair_until = life.repair_until_unix.unwrap_or(now + repair_len);
        if qualifying {
            repair_until = now + cycle_whole + repair_until.saturating_sub(now);
        }
        life.repair_until_unix = Some(repair_until);
    }
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
                    let shield_anchor = life.shield_fight_started_unix.unwrap_or(fight_start);
                    match armor_reinforcement_secs(profile.hull_class, space, power) {
                        Some(secs) => {
                            s.state = STATE_ARMOR_REINFORCED.into();
                            life.armor_reinforce_secs_applied = Some(secs);
                            let max_j = reinforce_jitter_window_secs(profile.hull_class, true);
                            let j = if max_j > 0 {
                                let roll = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5);
                                ((roll * 2.0 - 1.0) * (max_j as f64)).round() as i64
                            } else { 0 };
                            life.reinforce_jitter_secs = j;
                            let base_exit = next_reinforce_exit((shield_anchor + secs).max(now), hour);
                            life.unjittered_reinforce_exit_unix = Some(base_exit);
                            s.reinforced_until_unix = apply_reinforce_jitter(base_exit, j, max_j).max(now + 60);
                        }
                        None => {
                            s.state = STATE_ARMOR_VULNERABLE.into();
                            life.armor_reinforce_secs_applied = None;
                            life.unjittered_reinforce_exit_unix = None;
                            life.fight_started_unix = Some(now);
                        }
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
                            let max_j = reinforce_jitter_window_secs(profile.hull_class, false);
                            let j = if max_j > 0 {
                                let roll = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5);
                                ((roll * 2.0 - 1.0) * (max_j as f64)).round() as i64
                            } else { 0 };
                            life.reinforce_jitter_secs = j;
                            let earliest_hull = if let Some(armor_secs) = life.armor_reinforce_secs_applied {
                                let t_shield = life.shield_fight_started_unix.unwrap_or(fight_start);
                                (t_shield + armor_secs + secs).max(now)
                            } else {
                                fight_start.max(now) + secs
                            };
                            let base_exit = next_reinforce_exit(earliest_hull, hour);
                            life.unjittered_reinforce_exit_unix = Some(base_exit);
                            s.reinforced_until_unix = apply_reinforce_jitter(base_exit, j, max_j).max(now + 60);
                            life.repair_until_unix = None;
                        }
                        None => {
                            s.state = STATE_HULL_VULNERABLE.into();
                            life.unjittered_reinforce_exit_unix = None;
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
    if snap
        .structures
        .get(&structure_id)
        .is_some_and(|s| matches!(s.state.as_str(), STATE_ARMOR_REINFORCED | STATE_ARMOR_VULNERABLE | STATE_HULL_REINFORCED | STATE_HULL_VULNERABLE))
    {
        ext.unanchoring_until_unix.remove(&structure_id);
    }
    if snap.structures.get(&structure_id).map(|s| s.hull_hp <= 0.0).unwrap_or(false) {
        destruction = Some(destroy_structure(ctx, snap, ext, structure_id, attacker_char_id, now));
    }
    let s = snap.structures.get(&structure_id).cloned().unwrap_or(st);
    Ok(DamageOutcome { applied_damage: applied, layer, repair_paused: qualifying, state: s.state, reinforced_until_unix: s.reinforced_until_unix, shield_hp: s.shield_hp, armor_hp: s.armor_hp, hull_hp: s.hull_hp, destruction })
}

/// Applies a typed 4-channel weapon cycle (`em`, `thermal`, `kinetic`, `explosive`) after SDE layer
/// resonance reduction (`feat:structures.repair-timer-pause-threshold`).
#[allow(clippy::too_many_arguments)]
pub fn apply_structure_damage_typed(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    ext: &mut StructureExtendedState,
    structure_id: i64,
    em: f64,
    thermal: f64,
    kinetic: f64,
    explosive: f64,
    cycle_secs: f64,
    attacker_char_id: i64,
    now: u64,
) -> Result<DamageOutcome, BridgeRpcError> {
    let st = snap.structures.get(&structure_id).ok_or_else(|| BridgeRpcError { code: "StructureNotFound".into(), message: format!("Structure {structure_id} does not exist") })?;
    let attrs = ctx.sde.types.get(&st.type_id).map(|t| &t.default_attributes);
    let (em_a, th_a, ki_a, ex_a) = match st.state.as_str() {
        STATE_SHIELD_VULNERABLE => (271u16, 274u16, 273u16, 272u16),
        STATE_ARMOR_VULNERABLE => (267, 270, 269, 268),
        _ => (113, 110, 109, 111),
    };
    let res = |aid: u16| attrs.and_then(|m| m.get(&aid).copied()).unwrap_or(1.0);
    let post_resist = em.max(0.0) * res(em_a) + thermal.max(0.0) * res(th_a) + kinetic.max(0.0) * res(ki_a) + explosive.max(0.0) * res(ex_a);
    apply_structure_damage(ctx, snap, ext, structure_id, post_resist, cycle_secs, attacker_char_id, now)
}

/// Records that `char_id` completed a Data Analyzer hack on `structure_id` (`feat:structures.reinforce-hour-hack-reveal`).
pub fn record_structure_hack_reveal(ctx: &ServerContext, char_id: i64, structure_id: i64) -> Option<(u8, u8, Option<(u8, u8, u64)>)> {
    let info = ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get(&structure_id)?.clone();
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        if caller_corp != 0 && caller_corp == st.owner_corp_id {
            return None;
        }
        let mut ext = load_ext_state(snap);
        ext.hacked_reinforce_reveals.entry(structure_id).or_default().insert(char_id);
        let pending = ext.pending_reinforce_schedule.get(&structure_id).copied();
        save_ext_state(snap, &ext);
        let (h, d) = snap.structure_vulnerability_windows.get(&structure_id).copied().unwrap_or((18, 5));
        Some((st.solar_system_id, h as u8, d as u8, pending))
    })?;
    let hack_tuple = match info.3 {
        Some((nh, nd, at)) => eve_proto::PyRep::Tuple(vec![eve_proto::PyRep::Integer(info.2 as i64), eve_proto::PyRep::Integer(info.1 as i64), eve_proto::PyRep::Integer(nd as i64), eve_proto::PyRep::Integer(nh as i64), eve_proto::PyRep::Integer(at as i64)]),
        None => eve_proto::PyRep::Tuple(vec![eve_proto::PyRep::Integer(info.2 as i64), eve_proto::PyRep::Integer(info.1 as i64), eve_proto::PyRep::None, eve_proto::PyRep::None, eve_proto::PyRep::None]),
    };
    ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnStructureHacked", "charid", vec![eve_proto::PyRep::Integer(structure_id), hack_tuple]);
    Some((info.1, info.2, info.3))
}

/// Anchoring position rules: Pochven restriction, Palatine Keepstar cluster limit,
/// Keepstar/FLEX per-system limits, SDE `disallowInHighSec`, and the DERIVED 1,000 km minimum
/// distance from NPC stations, stargates, asteroid belts and other live structures.
pub fn check_anchor_position(ctx: &ServerContext, snap: &WorldStateSnapshot, profile: &StructureProfile, sys_id: u32, pos: glam::DVec3) -> Result<(), BridgeRpcError> {
    if !pos.is_finite() || pos.abs().max_element() > 1.0e14 {
        return Err(BridgeRpcError { code: "InvalidAnchorCoordinates".into(), message: "Anchoring position coordinates must be finite and within solar system bounds".into() });
    }
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
        .map(|(_, tid, _, _, _)| tid)
        .collect()
}

/// Validates that `structure_id` can be set as `char_id`'s home station (`feat:structures.service-clone-bay`).
pub fn validate_home_station_structure(ctx: &ServerContext, snap: &WorldStateSnapshot, char_id: i64, structure_id: i64) -> Result<(), BridgeRpcError> {
    let st = snap.structures.get(&structure_id).filter(|s| s.state != STATE_DESTROYED).ok_or_else(|| BridgeRpcError {
        code: "StationNotFound".into(),
        message: format!("Station or structure {structure_id} does not exist"),
    })?;
    if ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.region_id) == Some(POCHVEN_REGION_ID) {
        return Err(BridgeRpcError { code: "CannotSetMedicalCloneInPochven".into(), message: "A medical clone cannot be set in Pochven".into() });
    }
    if !super::structure_upwell_ops::evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
        return Err(BridgeRpcError { code: "CloneBayAccessDenied".into(), message: format!("Character {char_id} lacks access to structure {structure_id}") });
    }
    if !super::structure_upwell_ops::is_structure_service_online(ctx, snap, structure_id, 2)
        && !super::structure_upwell_ops::is_structure_service_online(ctx, snap, structure_id, 9)
    {
        return Err(BridgeRpcError { code: "CloneBayServiceOffline".into(), message: format!("Structure {structure_id} does not have an online Clone Bay service") });
    }
    Ok(())
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
