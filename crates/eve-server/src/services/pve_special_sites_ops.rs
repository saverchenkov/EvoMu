//! V24 SDE-backed helpers for Drifter Hives, Homefront Operations, Metaliminal Roaming Storms,
//! and Triglavian / EDENCOM sites (`feat:pve.drifter-hives`, `feat:pve.homefront-ops`,
//! `feat:pve.metaliminal-storm-movement`, `feat:pve.triglavian-sites`,
//! `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`,
//! `rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::pve_metaliminal_storms::{
    compute_storm_footprint, storm_move_interval_secs, storm_weather_beacons, RoamingStormRecord,
};
use crate::services::pve_special_sites::{
    next_spec_seq, spec_now_sec, AbyssalWeatherKind, DrifterHiveRecord, DrifterHiveRoom,
    PveSpecialSitesRegistry, TriglavianSiteRecord,
};
use eve_db::IskCents;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use std::collections::{HashMap, HashSet};

fn hf_ops_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_owned(),
        message: message.into(),
        client_error: None,
    }
}

/// Volume (m3) of the Warclone Blanks Vat Raid crate (`type_id = 77473`) in SDE.
pub const HOMEFRONT_RAID_CARGO_VOLUME_M3: f64 = 3_500.0;

/// Canonical Homefront Operation site archetypes grounded in V24 `archetype_id == 70` dungeons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HomefrontArchetype {
    Raid,
    EmergencyAid,
    MetaliminalMeteoroid,
    DreadAssault,
    AbyssalArtifactRecovery,
    SuspiciousSignal,
    SalvageResearch,
    StabilizeRift,
    TrafficStop,
}

/// Fleet role specialization within a Homefront Operation encounter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HomefrontRole {
    Dps,
    Logistics,
    Hauler,
    Miner,
    Hacker,
    CapBattery,
    Tackler,
    Neutralizer,
    Salvager,
    CargoScanner,
}

/// Acceleration-gate hull restriction tier (`FivePilotStandard` vs `ThreePilotFrigateDestroyer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomefrontShipPolicy {
    FivePilotStandard,
    ThreePilotFrigateDestroyer,
}

impl HomefrontArchetype {
    /// Classifies an SDE `archetype_id == 70` dungeon into its canonical operation archetype.
    pub fn from_dungeon(dungeon_id: u32, name: &str) -> Self {
        match dungeon_id {
            10_347 | 10_377..=10_379 => Self::Raid,
            10_238 | 10_373 | 10_376 | 10_380 => Self::EmergencyAid,
            10_312 | 10_387..=10_389 => Self::MetaliminalMeteoroid,
            10_320 | 10_381 | 10_383 | 10_384 => Self::DreadAssault,
            10_346 => Self::AbyssalArtifactRecovery,
            10_309 | 10_367..=10_369 => Self::SuspiciousSignal,
            10_713 => Self::SalvageResearch,
            10_714 => Self::StabilizeRift,
            10_705 => Self::TrafficStop,
            _ => {
                let lower = name.to_ascii_lowercase();
                if lower.contains("dread") {
                    Self::DreadAssault
                } else if lower.contains("emergency") || lower.contains("aid") {
                    Self::EmergencyAid
                } else if lower.contains("metaliminal") || lower.contains("meteoroid") || lower.contains("mining") {
                    Self::MetaliminalMeteoroid
                } else if lower.contains("abyssal") || lower.contains("artifact") {
                    Self::AbyssalArtifactRecovery
                } else if lower.contains("suspicious") || lower.contains("signal") {
                    Self::SuspiciousSignal
                } else if lower.contains("salvage") {
                    Self::SalvageResearch
                } else if lower.contains("rift") {
                    Self::StabilizeRift
                } else if lower.contains("traffic") {
                    Self::TrafficStop
                } else {
                    Self::Raid
                }
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raid => "Raid",
            Self::EmergencyAid => "Emergency Aid",
            Self::MetaliminalMeteoroid => "Metaliminal Meteoroid",
            Self::DreadAssault => "Dread Assault",
            Self::AbyssalArtifactRecovery => "Abyssal Artifact Recovery",
            Self::SuspiciousSignal => "Suspicious Signal",
            Self::SalvageResearch => "Salvage Research",
            Self::StabilizeRift => "Stabilize Rift",
            Self::TrafficStop => "Traffic Stop",
        }
    }

    /// Optimal fleet size per canonical EVE Homefront balance (5 for Viridian ops, 3 for Equinox ops).
    pub fn optimal_fleet_size(self) -> usize {
        match self {
            Self::SalvageResearch | Self::StabilizeRift | Self::TrafficStop => 3,
            _ => 5,
        }
    }

    /// Maximum participants eligible for shared payout (`2 * optimal_fleet_size`: 10 or 6).
    pub fn max_payout_pilots(self) -> usize {
        self.optimal_fleet_size() * 2
    }

    pub fn ship_policy(self) -> HomefrontShipPolicy {
        match self {
            Self::SalvageResearch | Self::StabilizeRift | Self::TrafficStop => {
                HomefrontShipPolicy::ThreePilotFrigateDestroyer
            }
            _ => HomefrontShipPolicy::FivePilotStandard,
        }
    }

    pub fn required_roles(self) -> &'static [HomefrontRole] {
        match self {
            Self::Raid => &[
                HomefrontRole::Dps,
                HomefrontRole::Tackler,
                HomefrontRole::Hauler,
                HomefrontRole::Logistics,
            ],
            Self::EmergencyAid => &[HomefrontRole::Logistics, HomefrontRole::Dps],
            Self::MetaliminalMeteoroid => &[
                HomefrontRole::Miner,
                HomefrontRole::Logistics,
                HomefrontRole::Hauler,
            ],
            Self::DreadAssault => &[
                HomefrontRole::CapBattery,
                HomefrontRole::Logistics,
                HomefrontRole::Dps,
            ],
            Self::AbyssalArtifactRecovery => &[
                HomefrontRole::Miner,
                HomefrontRole::Dps,
                HomefrontRole::Logistics,
            ],
            Self::SuspiciousSignal => &[
                HomefrontRole::Hacker,
                HomefrontRole::Dps,
                HomefrontRole::Logistics,
            ],
            Self::SalvageResearch => &[
                HomefrontRole::Salvager,
                HomefrontRole::Hacker,
                HomefrontRole::Dps,
            ],
            Self::StabilizeRift => &[HomefrontRole::Neutralizer, HomefrontRole::Dps],
            Self::TrafficStop => &[
                HomefrontRole::CargoScanner,
                HomefrontRole::Tackler,
                HomefrontRole::Dps,
            ],
        }
    }

    // DERIVED: canonical EVE Online Homefront operation timers (seconds).
    pub fn time_limit_secs(self) -> u64 {
        match self {
            Self::EmergencyAid => 600,
            Self::AbyssalArtifactRecovery => 1_800,
            _ => 1_200,
        }
    }

    // DERIVED: canonical stage count (waves / cycles / haulers / arrays) per operation.
    pub fn total_stages(self) -> u8 {
        match self {
            Self::Raid => 4,
            Self::EmergencyAid => 3,
            Self::MetaliminalMeteoroid => 1,
            Self::DreadAssault => 3,
            Self::AbyssalArtifactRecovery => 9,
            Self::SuspiciousSignal => 3,
            Self::SalvageResearch => 30,
            Self::StabilizeRift => 5,
            Self::TrafficStop => 6,
        }
    }

    // DERIVED: canonical per-pilot ISK payout at optimal fleet size (standard highsec, non-Exordium).
    pub fn canonical_base_pilot_isk(self) -> i64 {
        match self {
            Self::Raid | Self::EmergencyAid | Self::DreadAssault | Self::SuspiciousSignal => {
                15_000_000
            }
            Self::MetaliminalMeteoroid
            | Self::SalvageResearch
            | Self::StabilizeRift
            | Self::TrafficStop => 12_000_000,
            Self::AbyssalArtifactRecovery => 67_500_000,
        }
    }

    /// Canonical LP reward at optimal fleet size (`0` per EVE Homefront specification: rewards are ISK/ore/loot).
    pub fn canonical_base_pilot_lp(self) -> i64 {
        0
    }
}

/// Validated acceleration-gate eligibility summary for a pilot hull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomefrontShipEligibility {
    pub ship_type_id: TypeId,
    pub ship_group_id: u32,
    pub hull_class: &'static str,
    pub role_hints: Vec<HomefrontRole>,
}

/// Validates acceleration-gate ship restrictions from V24 SDE ship group, meta-group, tech level & race.
/// Enforces real Homefront restrictions: allows T1/Navy/Pirate/Mining frigates, T1/Navy/Pirate/Mining
/// destroyers (T1 destroyers only on 3-pilot sites), Basic T1 Empire cruisers, and T1 haulers (incl.
/// Squall & Noctis) on 5-pilot sites; strictly rejects T2, T3, Navy/Pirate cruisers, BCs, BS, Caps, Pods.
pub fn validate_homefront_ship_eligibility(
    sde: &SdeRegistry,
    ship_type_id: TypeId,
    archetype: HomefrontArchetype,
) -> Result<HomefrontShipEligibility, BridgeRpcError> {
    let ship = sde.types.get(&ship_type_id).ok_or_else(|| {
        hf_ops_err(
            "ShipRestrictedByGate",
            format!("Unknown hull type {ship_type_id} cannot enter Homefront acceleration gate"),
        )
    })?;
    if ship.category_id != 6 {
        return Err(hf_ops_err(
            "ShipRestrictedByGate",
            format!("Type {} ({}) is not a valid capsuleer ship", ship_type_id, ship.name),
        ));
    }

    let policy = archetype.ship_policy();
    let meta = ship.meta_group_id.unwrap_or(1);
    let tech = ship.tech_level.unwrap_or(1);

    if tech > 1 {
        return Err(hf_ops_err(
            "ShipRestrictedByGate",
            format!(
                "Homefront acceleration gate rejects Tech {tech} ship {} ({})",
                ship.name, ship_type_id
            ),
        ));
    }

    match ship.group_id {
        // Group 25: T1 / Navy / Pirate / Mining Frigates (`Venture` 32880, `Venture Consortium Issue` 89648).
        25 if matches!(meta, 0 | 1 | 4) => {
            let is_mining_frig = matches!(ship_type_id, 32_880 | 89_648)
                || ship.name.to_ascii_lowercase().contains("venture");
            let role_hints = if is_mining_frig {
                vec![HomefrontRole::Miner, HomefrontRole::Hauler]
            } else {
                vec![
                    HomefrontRole::Tackler,
                    HomefrontRole::Hacker,
                    HomefrontRole::Salvager,
                    HomefrontRole::CargoScanner,
                    HomefrontRole::Logistics,
                    HomefrontRole::Dps,
                ]
            };
            Ok(HomefrontShipEligibility {
                ship_type_id,
                ship_group_id: ship.group_id,
                hull_class: if is_mining_frig { "Mining Frigate" } else { "Frigate" },
                role_hints,
            })
        }
        // Group 420: T1 / Navy / Pirate / Mining Destroyers (`Pioneer` 89240 / 89647 / `Perseverance` 91174).
        420 if matches!(meta, 0 | 1 | 4) => {
            let is_mining_dest = matches!(ship_type_id, 89_240 | 89_647 | 91_174);
            let is_t1_empire_destroyer =
                meta == 1 && ship.published && matches!(ship.race_id, Some(1 | 2 | 4 | 8));
            if policy == HomefrontShipPolicy::ThreePilotFrigateDestroyer && !is_t1_empire_destroyer
            {
                return Err(hf_ops_err(
                    "ShipRestrictedByGate",
                    format!(
                        "3-pilot Homefront site {} allows only Tech 1 Empire Destroyers, rejecting {} ({})",
                        archetype.as_str(),
                        ship.name,
                        ship_type_id
                    ),
                ));
            }
            let role_hints = if is_mining_dest {
                vec![HomefrontRole::Miner, HomefrontRole::Dps]
            } else {
                vec![
                    HomefrontRole::Dps,
                    HomefrontRole::Neutralizer,
                    HomefrontRole::Salvager,
                    HomefrontRole::Tackler,
                ]
            };
            Ok(HomefrontShipEligibility {
                ship_type_id,
                ship_group_id: ship.group_id,
                hull_class: if is_mining_dest { "Mining Destroyer" } else { "Destroyer" },
                role_hints,
            })
        }
        // Group 26: Basic T1 Empire Cruisers (`type_list 322` or `meta == 1` + Empire race 1|2|4|8) on 5-pilot sites only.
        26 => {
            if policy != HomefrontShipPolicy::FivePilotStandard {
                return Err(hf_ops_err(
                    "ShipRestrictedByGate",
                    format!(
                        "3-pilot Homefront site {} restricts entry to Frigates and T1 Destroyers ({} rejected)",
                        archetype.as_str(),
                        ship.name
                    ),
                ));
            }
            let in_empire_cruiser_list = sde
                .type_lists
                .get(&322)
                .map(|tl| tl.included_type_ids.contains(&ship_type_id))
                .unwrap_or_else(|| {
                    meta == 1 && matches!(ship.race_id, Some(1 | 2 | 4 | 8))
                });
            if !in_empire_cruiser_list || meta != 1 {
                return Err(hf_ops_err(
                    "ShipRestrictedByGate",
                    format!(
                        "Homefront acceleration gate allows only Basic T1 Empire Cruisers, rejecting {} ({})",
                        ship.name, ship_type_id
                    ),
                ));
            }
            Ok(HomefrontShipEligibility {
                ship_type_id,
                ship_group_id: ship.group_id,
                hull_class: "Tech 1 Cruiser",
                role_hints: vec![
                    HomefrontRole::Dps,
                    HomefrontRole::Logistics,
                    HomefrontRole::CapBattery,
                    HomefrontRole::Hacker,
                ],
            })
        }
        // Group 28: T1 Haulers (incl. `Squall` 81008 & `Noctis` 2998; excludes `Primae` 2863 & special editions) on 5-pilot sites.
        28 if policy == HomefrontShipPolicy::FivePilotStandard
            && meta == 1
            && ship_type_id != 2_863 =>
        {
            Ok(HomefrontShipEligibility {
                ship_type_id,
                ship_group_id: ship.group_id,
                hull_class: if ship_type_id == 2_998 { "Salvage Hauler" } else { "Tech 1 Hauler" },
                role_hints: vec![HomefrontRole::Hauler, HomefrontRole::Salvager],
            })
        }
        other_group => Err(hf_ops_err(
            "ShipRestrictedByGate",
            format!(
                "Homefront site {} acceleration gate rejects {} (type {}, group {})",
                archetype.as_str(),
                ship.name,
                ship_type_id,
                other_group
            ),
        )),
    }
}

/// Resolved V24 SDE Homefront operation template.
#[derive(Debug, Clone)]
pub struct SdeHomefrontTemplate {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub difficulty: u32,
}

/// Complete V24 SDE-backed multi-role blueprint for a specific Homefront dungeon and system.
#[derive(Debug, Clone)]
pub struct HomefrontOperationBlueprint {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub archetype: HomefrontArchetype,
    pub difficulty: u32,
    pub sovereign_faction_id: u32,
    pub hostile_faction_id: u32,
    pub grants_combat_standing: bool,
    pub standings_faction_id: Option<u32>,
    pub optimal_fleet_size: usize,
    pub max_payout_pilots: usize,
    pub is_exordium_system: bool,
    pub base_pilot_isk: i64,
    pub base_pilot_lp: i64,
    pub abyssal_wave_payouts_isk: Vec<i64>,
    pub time_limit_secs: u64,
    pub total_stages: u8,
    pub required_roles: Vec<HomefrontRole>,
    pub dispatch_commander_type_id: TypeId,
    pub objective_structure_type_id: Option<TypeId>,
    pub support_entity_type_id: Option<TypeId>,
    pub loot_commodity_type_id: Option<TypeId>,
    pub escort_wave_type_ids: Vec<TypeId>,
    pub damage_profile: [f64; 4],
    pub ewar_profile: &'static str,
}

/// Resolves all V24 SDE Homefront Operations (`archetype_id == 70`) sorted by dungeon ID.
pub fn resolve_sde_homefront_templates(sde: &SdeRegistry) -> Vec<SdeHomefrontTemplate> {
    let ops_map = sde.operations().ok();
    let mut templates: Vec<SdeHomefrontTemplate> = sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id == Some(70))
        .map(|d| {
            let localized_name = ops_map
                .as_ref()
                .and_then(|m| m.get(&d.dungeon_id.to_string()))
                .and_then(|v| v.get("operationNameID").and_then(|id| id.as_u64()))
                .and_then(|msg_id| sde.localized_message_en_us(msg_id as u32))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| d.name.clone());
            SdeHomefrontTemplate {
                dungeon_id: d.dungeon_id,
                dungeon_name: localized_name,
                difficulty: d.difficulty.unwrap_or(2),
            }
        })
        .collect();
    templates.sort_by_key(|t| t.dungeon_id);
    templates
}

/// Resolves the sovereign Empire faction owning `sys_id` from constellation/region/station metadata.
pub fn resolve_homefront_sovereign_faction(sde: &SdeRegistry, sys_id: SolarSystemId) -> u32 {
    sde.solar_systems
        .get(&sys_id)
        .and_then(|s| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.faction_id))
                .or_else(|| {
                    s.stations
                        .iter()
                        .find_map(|st| st.owner_faction_id.map(|f| f as u32))
                })
        })
        .filter(|&f| matches!(f, 500_001 | 500_002 | 500_003 | 500_004))
        .unwrap_or(500_001)
}

/// Returns true if `sys_id` is a career/starter Exordium system with halved payouts (excluding trade hubs).
pub fn is_homefront_exordium_system(sde: &SdeRegistry, sys_id: SolarSystemId) -> bool {
    if matches!(
        sys_id,
        30_000_142 | 30_002_187 | 30_002_659 | 30_002_510 | 30_002_053
    ) {
        return false;
    }
    sde.schools
        .values()
        .any(|sc| sc.solar_system_id == sys_id && !matches!(sc.school_id, 23..=25))
}

/// Filters `archetype_id == 70` dungeons to regional + Empire-shared templates for `sys_id`
/// (with graceful fallback to all available templates for minimal synthetic unit-test SDEs).
pub fn resolve_sde_homefront_templates_for_system(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
) -> Vec<SdeHomefrontTemplate> {
    let all = resolve_sde_homefront_templates(sde);
    if all.len() <= 4 {
        return all;
    }
    let sov = resolve_homefront_sovereign_faction(sde, sys_id);
    let regional_templates: Vec<SdeHomefrontTemplate> = all
        .iter()
        .filter(|t| match t.dungeon_id {
            // Abyssal Artifact Recovery + Equinox 3-pilot ops spawn across all four empires
            10_346 | 10_705 | 10_713 | 10_714 => true,
            10_238 | 10_309 | 10_312 | 10_320 | 10_347 => sov == 500_003,
            10_373 | 10_367 | 10_387 | 10_381 | 10_377 => sov == 500_001,
            10_376 | 10_369 | 10_388 | 10_383 | 10_378 => sov == 500_004,
            10_380 | 10_368 | 10_389 | 10_384 | 10_379 => sov == 500_002,
            _ => false,
        })
        .cloned()
        .collect();
    if regional_templates.len() >= 2 {
        regional_templates
    } else {
        all
    }
}

fn pick_sde_combat_type(sde: &SdeRegistry, preferred: &[TypeId]) -> TypeId {
    for &tid in preferred {
        if let Some(t) = sde.types.get(&tid) {
            let sh = t
                .default_attributes
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(0.0);
            let ar = t
                .default_attributes
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(0.0);
            let hp = t
                .default_attributes
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(0.0);
            if sh > 0.0 && ar > 0.0 && hp > 0.0 && sh < 50_000.0 && ar < 50_000.0 && hp < 50_000.0 {
                return tid;
            }
        }
    }
    let mut cands: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| {
            matches!(t.group_id, 4569 | 4570 | 4571 | 4573 | 4577)
                && t.default_attributes
                    .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                && t.default_attributes
                    .get(&eve_sde::attr_ids::ARMOR_HP)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                && t.default_attributes
                    .get(&eve_sde::attr_ids::HP_STRUCT)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
        })
        .map(|t| t.type_id)
        .collect();
    cands.sort_unstable();
    cands.first().copied().unwrap_or_else(|| {
        sde.types
            .keys()
            .copied()
            .min()
            .unwrap_or(587)
    })
}

fn filter_present_types(sde: &SdeRegistry, ids: &[TypeId]) -> Vec<TypeId> {
    let out: Vec<TypeId> = ids
        .iter()
        .copied()
        .filter(|tid| sde.types.contains_key(tid))
        .collect();
    if out.is_empty() {
        vec![pick_sde_combat_type(sde, ids)]
    } else {
        out
    }
}

fn first_present_type(sde: &SdeRegistry, ids: &[TypeId]) -> Option<TypeId> {
    ids.iter()
        .copied()
        .find(|tid| sde.types.contains_key(tid))
        .or_else(|| ids.first().copied())
}

fn effective_faction_for_dungeon(dungeon_id: u32, sys_sov: u32) -> u32 {
    match dungeon_id {
        10_238 | 10_309 | 10_312 | 10_320 | 10_347 => 500_003,
        10_373 | 10_367 | 10_387 | 10_381 | 10_377 => 500_001,
        10_376 | 10_369 | 10_388 | 10_383 | 10_378 => 500_004,
        10_380 | 10_368 | 10_389 | 10_384 | 10_379 => 500_002,
        _ => sys_sov,
    }
}

/// Resolves the complete V24 SDE blueprint (commander, structures, haulers, Mutanite, escorts,
/// EWAR, damage profile, and Exordium-adjusted payouts) for a Homefront operation dungeon.
pub fn resolve_homefront_archetype_blueprint(
    sde: &SdeRegistry,
    dungeon_id: u32,
    dungeon_name: &str,
    sys_id: SolarSystemId,
    difficulty: u32,
) -> HomefrontOperationBlueprint {
    let sys_sov = resolve_homefront_sovereign_faction(sde, sys_id);
    let faction = effective_faction_for_dungeon(dungeon_id, sys_sov);
    let archetype = HomefrontArchetype::from_dungeon(dungeon_id, dungeon_name);
    let exordium = is_homefront_exordium_system(sde, sys_id);
    let scale_payout = |base: i64| if exordium { base / 2 } else { base };

    let base_pilot_isk = scale_payout(archetype.canonical_base_pilot_isk());
    let base_pilot_lp = scale_payout(archetype.canonical_base_pilot_lp());
    let abyssal_wave_payouts_isk = if archetype == HomefrontArchetype::AbyssalArtifactRecovery {
        vec![
            scale_payout(5_000_000),
            scale_payout(5_000_000),
            scale_payout(5_000_000),
            scale_payout(7_500_000),
            scale_payout(7_500_000),
            scale_payout(7_500_000),
            scale_payout(10_000_000),
            scale_payout(10_000_000),
            scale_payout(10_000_000),
        ]
    } else {
        Vec::new()
    };

    let (
        hostile_faction_id,
        grants_combat_standing,
        standings_faction_id,
        commander_cands,
        obj_struct_cands,
        support_cands,
        loot_cands,
        escort_cands,
        damage_profile,
        ewar_profile,
    ): (
        u32,
        bool,
        Option<u32>,
        Vec<TypeId>,
        Vec<TypeId>,
        Vec<TypeId>,
        Vec<TypeId>,
        Vec<TypeId>,
        [f64; 4],
        &'static str,
    ) = match archetype {
        HomefrontArchetype::Raid => match faction {
            500_003 => (
                500_012,
                true,
                Some(500_012),
                vec![77_066, 77_260],
                vec![77_179],
                vec![77_180],
                vec![77_473],
                vec![77_566, 77_635, 77_643, 77_595],
                [0.45, 0.35, 0.10, 0.10],
                "Energy Neutralizer / Weapon Disruption",
            ),
            500_001 => (
                500_010,
                true,
                Some(500_010),
                vec![77_505, 77_641],
                vec![77_295],
                vec![77_304],
                vec![77_474],
                vec![77_567, 77_636, 77_583, 77_596],
                [0.10, 0.45, 0.45, 0.00],
                "ECM",
            ),
            500_004 => (
                500_020,
                true,
                Some(500_020),
                vec![77_506, 77_640],
                vec![77_301],
                vec![77_305],
                vec![77_475],
                vec![77_564, 77_631, 77_580, 77_597],
                [0.00, 0.50, 0.50, 0.00],
                "Remote Sensor Dampening",
            ),
            _ => (
                500_011,
                false,
                None,
                vec![77_507, 77_642],
                vec![77_741],
                vec![77_303],
                vec![77_476],
                vec![77_565, 77_638, 77_581, 77_594],
                [0.15, 0.15, 0.25, 0.45],
                "Target Painter / ECM",
            ),
        },
        HomefrontArchetype::EmergencyAid => match faction {
            500_003 => (
                500_012,
                false,
                None,
                vec![77_643, 77_260],
                vec![77_126],
                vec![77_149],
                vec![],
                vec![77_258, 77_259, 77_671, 77_595],
                [0.50, 0.40, 0.05, 0.05],
                "Energy Neutralizer / Weapon Disruption",
            ),
            500_001 => (
                500_010,
                false,
                None,
                vec![77_644, 77_641],
                vec![77_287],
                vec![77_149],
                vec![],
                vec![77_558, 77_633, 77_672, 77_596],
                [0.10, 0.45, 0.45, 0.00],
                "ECM",
            ),
            500_004 => (
                500_020,
                false,
                None,
                vec![77_645, 77_640],
                vec![77_290],
                vec![77_149],
                vec![],
                vec![77_559, 77_632, 77_673, 77_597],
                [0.00, 0.50, 0.50, 0.00],
                "Stasis Webifier / Sensor Dampening",
            ),
            _ => (
                500_011,
                false,
                None,
                vec![77_646, 77_642],
                vec![77_292],
                vec![77_149],
                vec![],
                vec![77_560, 77_628, 77_674, 77_594],
                [0.10, 0.20, 0.20, 0.50],
                "Target Painter / ECM",
            ),
        },
        HomefrontArchetype::MetaliminalMeteoroid => match faction {
            500_003 => (
                500_012,
                false,
                None,
                vec![77_260, 77_643],
                vec![77_149],
                vec![77_118],
                vec![77_118],
                vec![77_256, 77_258, 77_261],
                [0.45, 0.35, 0.10, 0.10],
                "ECM",
            ),
            500_001 => (
                500_010,
                false,
                None,
                vec![77_641, 77_644],
                vec![77_149],
                vec![77_418],
                vec![77_418],
                vec![77_571, 77_537, 77_544],
                [0.10, 0.45, 0.45, 0.00],
                "ECM",
            ),
            500_004 => (
                500_020,
                false,
                None,
                vec![77_640, 77_645],
                vec![77_149],
                vec![77_419, 77_420],
                vec![77_419, 77_420],
                vec![77_572, 77_556, 77_546],
                [0.05, 0.50, 0.45, 0.00],
                "Stasis Webifier",
            ),
            _ => (
                500_011,
                false,
                None,
                vec![77_642, 77_646],
                vec![77_149],
                vec![77_421],
                vec![77_421],
                vec![77_573, 77_560, 77_547],
                [0.10, 0.25, 0.25, 0.40],
                "Target Painter",
            ),
        },
        HomefrontArchetype::DreadAssault => match faction {
            500_003 => (
                500_012,
                false,
                None,
                vec![77_543, 77_555, 77_260],
                vec![77_065],
                vec![77_019],
                vec![],
                vec![77_527, 77_877, 77_685, 77_595],
                [0.45, 0.35, 0.10, 0.10],
                "Dreadnought Energy Neutralizer / ECM / Weapon Disruption",
            ),
            500_001 => (
                500_010,
                false,
                None,
                vec![77_548, 77_544, 77_641],
                vec![77_251],
                vec![77_184],
                vec![],
                vec![77_530, 77_879, 77_683, 77_585],
                [0.10, 0.45, 0.45, 0.00],
                "Dreadnought Energy Neutralizer / ECM",
            ),
            500_004 => (
                500_020,
                false,
                None,
                vec![77_546, 77_549, 77_640],
                vec![77_252],
                vec![77_185],
                vec![],
                vec![77_531, 77_881, 77_682, 77_584],
                [0.05, 0.50, 0.45, 0.00],
                "Dreadnought Energy Neutralizer / ECM",
            ),
            _ => (
                500_011,
                false,
                None,
                vec![77_550, 77_547, 77_642],
                vec![77_253],
                vec![77_183],
                vec![],
                vec![77_533, 77_883, 77_684, 77_586],
                [0.10, 0.25, 0.25, 0.40],
                "Dreadnought Energy Neutralizer / ECM / Target Painter",
            ),
        },
        HomefrontArchetype::AbyssalArtifactRecovery => (
            500_026,
            false,
            None,
            vec![77_691, 77_648],
            vec![77_727],
            vec![77_524],
            vec![77_707, 77_708],
            vec![77_562, 77_620, 77_676, 77_696, 77_702],
            [0.05, 0.55, 0.05, 0.35],
            "Abyssal Warp Disruption Field / Remote Armor Logistics",
        ),
        HomefrontArchetype::SuspiciousSignal => match faction {
            500_003 => (
                500_002,
                false,
                None,
                vec![77_678, 77_642],
                vec![77_120],
                vec![77_121],
                vec![],
                vec![77_579, 77_614, 77_594, 77_554],
                [0.10, 0.20, 0.20, 0.50],
                "Target Painter",
            ),
            500_001 => (
                500_004,
                false,
                None,
                vec![77_668, 77_640],
                vec![77_195],
                vec![77_196],
                vec![],
                vec![77_572, 77_617, 77_589, 77_553],
                [0.00, 0.55, 0.45, 0.00],
                "Remote Sensor Dampening",
            ),
            500_004 => (
                500_001,
                false,
                None,
                vec![77_669, 77_641],
                vec![77_200],
                vec![77_197],
                vec![],
                vec![77_571, 77_616, 77_588, 77_545],
                [0.00, 0.45, 0.55, 0.00],
                "ECM",
            ),
            _ => (
                500_003,
                false,
                None,
                vec![77_671, 77_643],
                vec![77_199],
                vec![77_198],
                vec![],
                vec![77_570, 77_615, 77_587, 77_555],
                [0.50, 0.45, 0.05, 0.00],
                "Energy Neutralizer / Weapon Disruption",
            ),
        },
        HomefrontArchetype::SalvageResearch => (
            500_026,
            false,
            None,
            vec![77_647, 77_620],
            vec![83_590, 77_149],
            vec![81_438, 77_099],
            vec![81_352],
            vec![77_575, 77_592, 77_658],
            [0.05, 0.55, 0.05, 0.35],
            "ECM",
        ),
        HomefrontArchetype::StabilizeRift => (
            500_021,
            false,
            None,
            vec![77_656, 77_652],
            vec![77_149],
            vec![81_402, 77_121],
            vec![],
            vec![77_563, 77_621, 77_649, 77_698],
            [0.45, 0.45, 0.05, 0.05],
            "Energy Neutralizer / Cap Transfer",
        ),
        HomefrontArchetype::TrafficStop => (
            500_010,
            false,
            None,
            vec![77_567, 77_636],
            vec![77_149],
            vec![77_304],
            vec![81_383, 77_946, 77_947, 77_948, 77_949],
            vec![77_567, 77_636, 77_650],
            [0.45, 0.30, 0.20, 0.05],
            "Stasis Webifier / Combat Drones",
        ),
    };

    HomefrontOperationBlueprint {
        dungeon_id,
        dungeon_name: dungeon_name.to_string(),
        archetype,
        difficulty,
        sovereign_faction_id: faction,
        hostile_faction_id,
        grants_combat_standing,
        standings_faction_id,
        optimal_fleet_size: archetype.optimal_fleet_size(),
        max_payout_pilots: archetype.max_payout_pilots(),
        is_exordium_system: exordium,
        base_pilot_isk,
        base_pilot_lp,
        abyssal_wave_payouts_isk,
        time_limit_secs: archetype.time_limit_secs(),
        total_stages: archetype.total_stages(),
        required_roles: archetype.required_roles().to_vec(),
        dispatch_commander_type_id: pick_sde_combat_type(sde, &commander_cands),
        objective_structure_type_id: first_present_type(sde, &obj_struct_cands),
        support_entity_type_id: first_present_type(sde, &support_cands),
        loot_commodity_type_id: first_present_type(sde, &loot_cands),
        escort_wave_type_ids: filter_present_types(sde, &escort_cands),
        damage_profile,
        ewar_profile,
    }
}

/// Verified pilot interaction metrics inside a Homefront site used to gate payout eligibility.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HomefrontParticipantContribution {
    pub character_id: i64,
    pub entry_sequence: u64,
    pub damage_dealt: f64,
    pub repair_hp: f64,
    pub cap_transferred: f64,
    pub ore_mined_units: u32,
    pub cargo_delivered: u32,
    pub arrays_hacked: u32,
    pub objective_actions: u32,
}

impl HomefrontParticipantContribution {
    /// Canonical EVE threshold: >= 1,000 finite damage/rep/cap OR active role objective interaction.
    pub fn qualifies(&self) -> bool {
        (self.damage_dealt.is_finite() && self.damage_dealt >= 1_000.0)
            || (self.repair_hp.is_finite() && self.repair_hp >= 1_000.0)
            || (self.cap_transferred.is_finite() && self.cap_transferred >= 1_000.0)
            || self.ore_mined_units > 0
            || self.cargo_delivered > 0
            || self.arrays_hacked > 0
            || self.objective_actions > 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomefrontPilotPayout {
    pub character_id: i64,
    pub eligible: bool,
    pub isk: i64,
    pub isk_cents: IskCents,
    pub lp: i64,
    pub standing_delta: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomefrontFleetPayoutSummary {
    pub optimal_fleet_size: usize,
    pub max_payout_pilots: usize,
    pub eligible_pilot_count: usize,
    pub total_fleet_isk: i64,
    pub total_fleet_isk_cents: IskCents,
    pub total_fleet_lp: i64,
    pub payouts: Vec<HomefrontPilotPayout>,
}

/// Fixed-point integer `IskCents` payout scaling for Homefront Operations:
/// - `1 <= n < opt`: attenuates below optimal fleet size using `4^(opt - n) / 5^(opt - n)`.
/// - `n == opt`: full `1.0x` base payout.
/// - `opt < n <= 2*opt`: integer floor `(base_cents * opt) / n` so total fleet payout NEVER exceeds
///   `opt * base` (no +1/+2 ISK rounding overflow).
/// - `n == 0 || n > 2*opt`: zero payout.
pub fn scale_homefront_reward_cents(
    base_cents: IskCents,
    optimal_fleet_size: usize,
    qualified_count: usize,
) -> IskCents {
    let opt = optimal_fleet_size.max(1);
    let max_recipients = opt.saturating_mul(2);
    if qualified_count == 0 || qualified_count > max_recipients || base_cents.raw_cents() <= 0 {
        return IskCents::ZERO;
    }
    // DERIVED: integer under-optimal attenuation `(4/5)^(opt - n)` and floored `base * opt / n` dilution.
    let raw_cents = i128::from(base_cents.raw_cents());
    let scaled_cents = if qualified_count < opt {
        let deficit = u32::try_from(opt - qualified_count).unwrap_or(10);
        let num = 4_i128.checked_pow(deficit).unwrap_or(0);
        let den = 5_i128.checked_pow(deficit).unwrap_or(1);
        raw_cents.saturating_mul(num) / den
    } else if qualified_count == opt {
        raw_cents
    } else {
        raw_cents.saturating_mul(opt as i128) / (qualified_count as i128)
    };
    IskCents::from_raw_cents(scaled_cents.clamp(0, i64::MAX as i128) as i64)
}

fn scale_integer_pool_floor(base: i64, opt: usize, qualified_count: usize) -> i64 {
    if qualified_count == 0 || base <= 0 {
        return 0;
    }
    let b = i128::from(base);
    let scaled = if qualified_count < opt {
        let deficit = u32::try_from(opt - qualified_count).unwrap_or(10);
        let num = 4_i128.checked_pow(deficit).unwrap_or(0);
        let den = 5_i128.checked_pow(deficit).unwrap_or(1);
        b.saturating_mul(num) / den
    } else if qualified_count == opt {
        b
    } else {
        b.saturating_mul(opt as i128) / (qualified_count as i128)
    };
    scaled.clamp(0, i64::MAX as i128) as i64
}

/// Computes canonical Homefront payout shares across all contributors under the 5-pilot (or 3-pilot)
/// role cap: recipients are ordered deterministically by arrival/contribution `entry_sequence`
/// (with slice index fallback when `entry_sequence == 0`), avoiding lowest-`character_id` bias.
pub fn compute_homefront_fleet_payouts(
    base_pilot_isk: i64,
    base_pilot_lp: i64,
    optimal_fleet_size: usize,
    contributors: &[HomefrontParticipantContribution],
) -> HomefrontFleetPayoutSummary {
    let opt = optimal_fleet_size.max(1);
    let max_recipients = opt.saturating_mul(2);
    let base_isk = base_pilot_isk.max(0);
    let base_lp = base_pilot_lp.max(0);

    let mut seen = HashSet::new();
    let mut unique_entries: Vec<(usize, &HomefrontParticipantContribution)> = Vec::new();
    for (idx, entry) in contributors.iter().enumerate() {
        if entry.character_id <= 0 || !seen.insert(entry.character_id) {
            continue;
        }
        unique_entries.push((idx, entry));
    }

    // Order by arrival/contribution sequence (explicit > 0 sequences first, ascending; 0 sequences keep slice order).
    let mut ordered_qualified: Vec<(bool, u64, usize, i64)> = unique_entries
        .iter()
        .filter(|(_, e)| e.qualifies())
        .map(|&(idx, e)| (e.entry_sequence == 0, e.entry_sequence, idx, e.character_id))
        .collect();
    ordered_qualified.sort_unstable();

    let capped_qualified_count = ordered_qualified.len().min(max_recipients);
    let per_pilot_isk_cents = scale_homefront_reward_cents(
        IskCents::from_isk(base_isk),
        opt,
        capped_qualified_count,
    );
    let per_pilot_isk = per_pilot_isk_cents.raw_cents() / 100;
    let per_pilot_lp = scale_integer_pool_floor(base_lp, opt, capped_qualified_count);

    let paid_set: HashSet<i64> = ordered_qualified
        .into_iter()
        .take(max_recipients)
        .map(|(_, _, _, cid)| cid)
        .collect();

    let mut total_fleet_cents_raw: i64 = 0;
    let mut total_fleet_lp: i64 = 0;
    let payouts = unique_entries
        .into_iter()
        .map(|(_, e)| {
            let cid = e.character_id;
            if paid_set.contains(&cid) {
                total_fleet_cents_raw = total_fleet_cents_raw
                    .saturating_add(per_pilot_isk_cents.raw_cents());
                total_fleet_lp = total_fleet_lp.saturating_add(per_pilot_lp);
                HomefrontPilotPayout {
                    character_id: cid,
                    eligible: true,
                    isk: per_pilot_isk,
                    isk_cents: per_pilot_isk_cents,
                    lp: per_pilot_lp,
                    standing_delta: 0.0,
                }
            } else {
                HomefrontPilotPayout {
                    character_id: cid,
                    eligible: false,
                    isk: 0,
                    isk_cents: IskCents::ZERO,
                    lp: 0,
                    standing_delta: 0.0,
                }
            }
        })
        .collect();

    let total_fleet_isk_cents = IskCents::from_raw_cents(total_fleet_cents_raw);
    HomefrontFleetPayoutSummary {
        optimal_fleet_size: opt,
        max_payout_pilots: max_recipients,
        eligible_pilot_count: capped_qualified_count,
        total_fleet_isk: total_fleet_cents_raw / 100,
        total_fleet_isk_cents,
        total_fleet_lp,
        payouts,
    }
}

/// Computes live space-combat Homefront payout per pilot using exact fixed-point `IskCents`
/// scaling (`0.8^(opt-n)` under optimal, floored `base * opt / n` above optimal, and `0` above `2 * opt`).
pub fn compute_homefront_combat_pilot_isk(
    base_pilot_isk: i64,
    optimal_fleet_size: usize,
    fleet_count: usize,
) -> i64 {
    let eff = fleet_count.max(1);
    let cents = scale_homefront_reward_cents(
        IskCents::from_isk(base_pilot_isk.max(0)),
        optimal_fleet_size,
        eff,
    );
    cents.raw_cents() / 100
}

/// Seeds a single Drifter Hive for `target_sys_id` from V24 SDE (`#1542`).
pub(crate) fn init_drifter_hive_for_system_from_sde(
    sde: &SdeRegistry,
    reg: &mut PveSpecialSitesRegistry,
    target_sys_id: SolarSystemId,
) {
    let defs: &[(SolarSystemId, u32, &str, TypeId, &str)] = &[
        (31_000_001, 5648, "Barbican Hive", 34_557, "Barbican Element"),
        (31_000_002, 5649, "Vidette Hive", 34_558, "Vidette Element"),
        (31_000_003, 5650, "Conflux Hive", 34_559, "Conflux Element"),
        (31_000_004, 5651, "Redoubt Hive", 34_560, "Redoubt Element"),
        (31_000_005, 5642, "Sentinel Hive", 34_556, "Sentinel Element"),
    ];
    let Some(&(sys_id, did, fallback_dname, elem_tid, fallback_elem_name)) =
        defs.iter().find(|&&(sid, ..)| sid == target_sys_id)
    else {
        return;
    };
    if reg.drifter_hives.contains_key(&sys_id) {
        return;
    }

    let r1_a = sde.types.get(&47_958).map(|t| t.type_id).unwrap_or(47_958);
    let r1_b = sde.types.get(&47_959).map(|t| t.type_id).unwrap_or(47_959);
    let r2_a = sde.types.get(&47_956).map(|t| t.type_id).unwrap_or(47_956);
    let r2_b = sde.types.get(&47_957).map(|t| t.type_id).unwrap_or(47_957);

    let warden_tid = sde
        .types
        .get(&37_473)
        .map(|t| t.type_id)
        .unwrap_or(37_473);

    let dname = sde
        .dungeons
        .get(&did)
        .map(|d| d.name.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_dname.to_string());
    let elem_name = sde
        .types
        .get(&elem_tid)
        .map(|t| t.name.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_elem_name.to_string());

    let base_ball = 1_898_000_000 + ((did as i64) * 100);
    let warden_npc_ball_id = base_ball + 30;
    let rooms = vec![
        DrifterHiveRoom {
            room_index: 1,
            room_name: format!("{dname} — Exterior Perimeter"),
            npc_ball_ids: vec![base_ball + 11, base_ball + 12],
            npc_type_ids: vec![r1_a, r1_b],
            beacon_ball_id: base_ball + 10,
            cleared: false,
            destroyed_npc_ball_ids: Vec::new(),
        },
        DrifterHiveRoom {
            room_index: 2,
            room_name: format!("{dname} — Tactical Muster"),
            npc_ball_ids: vec![base_ball + 21, base_ball + 22],
            npc_type_ids: vec![r2_a, r2_b],
            beacon_ball_id: base_ball + 20,
            cleared: false,
            destroyed_npc_ball_ids: Vec::new(),
        },
        DrifterHiveRoom {
            room_index: 3,
            room_name: format!("{dname} — Inner Sanctuary"),
            npc_ball_ids: vec![warden_npc_ball_id],
            npc_type_ids: vec![warden_tid],
            beacon_ball_id: base_ball + 31,
            cleared: false,
            destroyed_npc_ball_ids: Vec::new(),
        },
    ];
    reg.drifter_hives.insert(
        sys_id,
        DrifterHiveRecord {
            site_id: format!("DRIFTER-HIVE-{did}"),
            dungeon_id: did,
            dungeon_name: dname,
            solar_system_id: sys_id,
            element_type_id: elem_tid,
            element_name: elem_name,
            current_room: 1,
            total_rooms: 3,
            warden_npc_ball_id,
            cleared: false,
            rooms,
        },
    );
}

/// Seeds all 5 Drifter Hives from V24 SDE `sde.dungeons` (`5642, 5648..=5651`), localized names,
/// Drifter NPC types (`sde.types`), and Antikythera Elements (`34556..=34560`).
pub fn init_drifter_hives_from_sde(sde: &SdeRegistry, reg: &mut PveSpecialSitesRegistry) {
    for sys_id in 31_000_001..=31_000_005 {
        init_drifter_hive_for_system_from_sde(sde, reg, sys_id);
    }
}

/// Ensures the full set of 8 roaming null-sec Metaliminal Storms (2 each of Gamma, Electrical,
/// Plasma/Firestorm, Exotic) is populated using V24 SDE null-sec systems and stargate topology.
pub fn ensure_full_roaming_storm_set(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    crate::services::pve_metaliminal_storms::init_roaming_storms(ctx, reg);
    if reg.roaming_storms.len() >= 8 {
        return;
    }
    let now_sec = spec_now_sec(reg);
    let weathers = [
        AbyssalWeatherKind::Gamma,
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Exotic,
    ];
    let mut used_regions: HashSet<u32> = reg
        .roaming_storms
        .iter()
        .filter_map(|s| {
            ctx.sde
                .solar_systems
                .get(&s.center_solar_system_id)
                .map(|sys| sys.region_id)
        })
        .collect();
    let mut null_candidates: Vec<(u32, SolarSystemId)> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            crate::services::pve_metaliminal_storms::is_nullsec_storm_system(
                &ctx.sde,
                s.solar_system_id,
            )
        })
        .map(|s| (s.region_id, s.solar_system_id))
        .collect();
    null_candidates.sort_unstable();

    let mut next_id: u32 = reg
        .roaming_storms
        .iter()
        .map(|s| s.storm_id)
        .max()
        .unwrap_or(0)
        + 1;

    for &weather in &weathers {
        let existing_count = reg
            .roaming_storms
            .iter()
            .filter(|s| s.weather == weather)
            .count();
        for _ in existing_count..2 {
            let center_opt = null_candidates
                .iter()
                .find(|&&(rid, _)| !used_regions.contains(&rid))
                .or_else(|| {
                    null_candidates.iter().find(|&&(_, sid)| {
                        !reg.roaming_storms
                            .iter()
                            .any(|s| s.center_solar_system_id == sid)
                    })
                })
                .copied();
            let Some((rid, center_sys)) = center_opt else {
                break;
            };
            used_regions.insert(rid);
            let (core, periphery) = compute_storm_footprint(ctx, center_sys);
            let sid = next_id;
            next_id += 1;
            reg.roaming_storms.push(RoamingStormRecord {
                storm_id: sid,
                weather,
                center_solar_system_id: center_sys,
                strong_core_systems: core,
                weak_periphery_systems: periphery,
                step_count: 0,
                next_move_at_sec: now_sec + storm_move_interval_secs(sid, 0),
                recent_centers: vec![center_sys],
            });
        }
    }
}

/// Broadcasts live ship dogma `OnAttributes` notifications to pilots in storm-affected systems
/// when roaming storm beacons are added or removed.
pub fn notify_storm_ship_dogma_updates(
    ctx: &ServerContext,
    affected_systems: &[SolarSystemId],
    effect_lists_by_sys: &HashMap<SolarSystemId, Vec<TypeId>>,
) {
    if affected_systems.is_empty() {
        return;
    }
    let affected_set: HashSet<SolarSystemId> = affected_systems.iter().copied().collect();
    let targets: Vec<(i64, i64, usize, bool)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
                    && affected_set.contains(&c.solar_system_id)
            })
            .map(|c| {
                let effects = effect_lists_by_sys
                    .get(&c.solar_system_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let is_strong = effects.iter().any(|&tid| {
                    [
                        storm_weather_beacons(AbyssalWeatherKind::Gamma).1,
                        storm_weather_beacons(AbyssalWeatherKind::Electrical).1,
                        storm_weather_beacons(AbyssalWeatherKind::Firestorm).1,
                        storm_weather_beacons(AbyssalWeatherKind::Exotic).1,
                    ]
                    .contains(&tid)
                });
                (c.character_id, c.active_ship_id, effects.len(), is_strong)
            })
            .collect()
    });

    for (cid, ship_id, storm_count, is_strong) in targets {
        let strength_mult = if storm_count == 0 {
            1.0
        } else if is_strong {
            1.25 * (storm_count as f64)
        } else {
            1.10 * (storm_count as f64)
        };
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnAttributes {
                changes: PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("metaliminalStormEffectScale".into()),
                    PyRep::keyval([("itemID", PyRep::Integer(ship_id))]),
                    PyRep::Real(strength_mult),
                ])]),
            },
        );
    }
}

/// Applies Triglavian / EDENCOM site rewards using EVE's asymptotic Social-skilled standing
/// formula (`new = cur + (10 - cur) * 0.015 * (1 + 0.05 * social_lvl)`), credits ISK + journal,
/// and emits `OnCharacterStandingChange` notification.
pub fn apply_triglavian_standings_and_isk(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    gain_faction: i64,
    lose_faction: i64,
    base_isk: i64,
    dungeon_name: &str,
) -> (Option<f64>, Option<(f64, f64)>) {
    let social_lvl = snap
        .skills
        .get(&char_id)
        .and_then(|skills| skills.get(&3355))
        .map(|s| s.level.clamp(0, 5))
        .unwrap_or(0);
    let social_mult = 1.0 + 0.05 * f64::from(social_lvl);

    let mut new_bal_isk = None;
    let mut standing_pair = None;
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        let g = ch.standings.entry(gain_faction).or_insert(0.0);
        let next_g = (*g + (10.0 - *g) * 0.015 * social_mult).clamp(-10.0, 10.0);
        *g = next_g;

        let l = ch.standings.entry(lose_faction).or_insert(0.0);
        let next_l = (*l - (10.0 + *l) * 0.015).clamp(-10.0, 10.0);
        *l = next_l;
        standing_pair = Some((next_g, next_l));

        ch.balance = ch
            .balance
            .checked_add(IskCents::from_isk(base_isk))
            .unwrap_or(ch.balance);
        let new_bal = ch.balance;
        new_bal_isk = Some(new_bal.to_wire_f64());
        snap.append_journal(
            char_id,
            "CorporateRewardPayout",
            IskCents::from_isk(base_isk),
            new_bal,
            format!("Triglavian site payout ({dungeon_name})"),
        );
    }
    (new_bal_isk, standing_pair)
}

/// Resolves Triglavian / EDENCOM site records using V24 SDE dungeon names & NPC types.
pub fn build_triglavian_sites_from_sde(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
    is_edencom: bool,
    empire_navy: (u32, u32, u32),
) -> Vec<TriglavianSiteRecord> {
    let base_inst = (sys_id as i64) * 100;
    let is_pochven = sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    let dng_name = |did: u32, default: &str| -> String {
        sde.dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let make_site = |inst, did: u32, fallback_name: &str, fac, isk, drop, boss_tid, esc_tids: Vec<u32>| {
        let esc_bids = (1..=esc_tids.len())
            .map(|i| 1_897_700_000 + inst * 10 + (i as i64))
            .collect();
        TriglavianSiteRecord {
            instance_id: inst,
            dungeon_id: did,
            dungeon_name: dng_name(did, fallback_name),
            solar_system_id: sys_id,
            faction_owner_id: fac,
            base_isk: isk,
            survey_db_drop: drop,
            completed: false,
            boss_npc_ball_id: 1_897_700_000 + inst,
            boss_type_id: boss_tid,
            escort_ball_ids: esc_bids,
            escort_type_ids: esc_tids,
            beacon_ball_id: 1_897_700_000 + inst * 10 + 9,
        }
    };

    if is_edencom {
        let (bs, bc, fr) = empire_navy;
        vec![
            make_site(
                base_inst + 1,
                8808,
                "EDENCOM Field Base",
                500_027,
                35_000_000,
                150,
                bs,
                vec![bc, fr],
            ),
            make_site(
                base_inst + 2,
                8792,
                "EDENCOM Forward Post",
                500_027,
                25_000_000,
                100,
                bc,
                vec![fr],
            ),
        ]
    } else if is_pochven {
        vec![
            make_site(
                base_inst + 1,
                8802,
                "Observatory Flashpoint",
                500_026,
                40_000_000,
                200,
                48_122,
                vec![48_086, 48_092],
            ),
            make_site(
                base_inst + 2,
                8742,
                "Incipient Drone Swarm",
                500_026,
                25_000_000,
                120,
                48_126,
                vec![48_088],
            ),
        ]
    } else {
        vec![
            make_site(
                base_inst + 1,
                8802,
                "Major Conduit",
                500_026,
                35_000_000,
                150,
                48_122,
                vec![48_086, 48_092],
            ),
            make_site(
                base_inst + 2,
                8742,
                "Minor Conduit",
                500_026,
                25_000_000,
                100,
                48_126,
                vec![48_088],
            ),
        ]
    }
}

/// Resolves extra Drifter Hive completion loot (`Antikythera Element` `34_575` + Sleeper blue loot).
pub fn allocate_drifter_hive_bonus_loot(
    reg: &mut PveSpecialSitesRegistry,
    elem_tid: TypeId,
) -> Vec<(i64, TypeId, i32)> {
    let elem_iid = 1_899_000_000 + next_spec_seq(reg);
    let blue_iid = 1_899_100_000 + next_spec_seq(reg);
    let antikythera_iid = 1_899_200_000 + next_spec_seq(reg);
    vec![
        (elem_iid, elem_tid, 1),
        (antikythera_iid, 34_575, 2),
        (blue_iid, 30_745, 5),
    ]
}

#[cfg(test)]
#[path = "pve_special_sites_ops_edges_tests.rs"]
mod pve_special_sites_ops_edges_tests;
