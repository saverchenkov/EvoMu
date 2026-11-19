//! `pve_homefront_sites` — Persisted V24 Homefront Operations runtime (`DungeonRuntimeStore`),
//! outer beacon + acceleration gate separation, multi-role archetype state machines,
//! live space/combat/mining/hacking/salvage/scanner/clock progression, and atomic whole-cent payouts.
//!
// parity: feat:pve.homefront-ops
// parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances

#![forbid(unsafe_code)]

use crate::router::notify_registry::{OnAccountChange, OnLPChange};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{
    with_combat_ephemeral, with_mut_combat_ephemeral, ExtendedNpcEntity,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats;
use crate::services::pve_special_sites::PveSpecialSitesRegistry;
use crate::services::pve_special_sites_ops::{
    compute_homefront_fleet_payouts, resolve_homefront_archetype_blueprint,
    resolve_sde_homefront_templates_for_system, validate_homefront_ship_eligibility,
    HomefrontArchetype, HomefrontFleetPayoutSummary, HomefrontOperationBlueprint,
    HomefrontParticipantContribution, HomefrontPilotPayout, HomefrontRole,
    HomefrontShipEligibility, HOMEFRONT_RAID_CARGO_VOLUME_M3,
};
use crate::services::space_nav_movement_guard::{AccelerationGateKind, MovementIntent};
use crate::system_actor::{AsteroidState, SystemHandle, SystemRegistry};
use eve_db::{
    AsteroidRecord, HomefrontArchetypeKind, HomefrontBlueprintRecord, HomefrontContributionRecord,
    HomefrontProgressRecord, HomefrontRoleKind, HomefrontRuntimeRecord,
    HomefrontShipEligibilityRecord, HomefrontSiteRecord, InventoryFlag, InventoryItemRecord,
    IskCents, WorldRepository, WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

// DERIVED: V24 SDE object typeIDs & gate geometry for Homefront Operations.
const HOMEFRONT_BEACON_TYPE_ID: TypeId = 77_149;
const HOMEFRONT_ACCEL_GATE_TYPE_ID: TypeId = 17_831;
const HOMEFRONT_CARGO_CONTAINER_TYPE_ID: TypeId = 23;
const HOMEFRONT_GATE_ACTIVATION_RANGE_M: f64 = 2_500.0;
const HOMEFRONT_POCKET_Z_OFFSET_M: f64 = 1_000_000.0;
const HOMEFRONT_POCKET_LANDING_Y_M: f64 = -1_500.0;
const HOMEFRONT_WARP_ARRIVAL_RADIUS_M: f64 = 50_000.0;
const HOMEFRONT_AAR_ROCKS_PER_WAVE: usize = 12;
const HOMEFRONT_AAR_UNITS_PER_ROCK: u32 = 27;
const HOMEFRONT_AAR_MUTANITE_TYPE_ID: TypeId = 77_524;
const HOMEFRONT_AAR_BASILEUS_TYPE_ID: TypeId = 77_702;
const HOMEFRONT_AAR_CACHE_TYPE_ID: TypeId = 77_727;
const HOMEFRONT_AAR_CACHE_WRECK_TYPE_ID: TypeId = 84_467;
const HOMEFRONT_SALVAGE_WRECK_TYPE_ID: TypeId = 81_438;
const HOMEFRONT_SALVAGE_NPC_OWNER_ID: i64 = 1_000_125;
pub const HOMEFRONT_SALVAGE_DATA_TYPE_ID: TypeId = 81_352;
const HOMEFRONT_SALVAGE_WERPOST_TYPE_ID: TypeId = 83_590;
const HOMEFRONT_MIN_SALVAGE_BONUS_PCT: f64 = 20.0;
const HOMEFRONT_TRAFFIC_CONTRABAND_TYPE_ID: TypeId = 81_383;
const HOMEFRONT_MUTANITE_UNIT_VOLUME_M3: f64 = 4.0;
const HOMEFRONT_RAID_CRATE_TYPE_ID: TypeId = 77_473;
const HOMEFRONT_SIGNAL_VULN_WINDOW_SECS: u64 = 90;
const HOMEFRONT_TRAFFIC_TIMER_SECS: u64 = 540;
const HOMEFRONT_MIN_SKILL_CONTRIBUTION: f64 = 1_000.0;

pub const HOMEFRONT_SYSTEMS: &[SolarSystemId] =
    &[30_000_142_u32, 30_002_187, 30_002_659, 30_002_510];

/// Archetype-specific multi-role progression state for an active Homefront site instance.
#[derive(Debug, Clone, PartialEq)]
pub enum HomefrontArchetypeProgress {
    Raid {
        haulers_spawned: u8,
        haulers_destroyed: u8,
        haulers_escaped: u8,
        active_hauler_ball_id: Option<i64>,
        active_hauler_tackled: bool,
        cargo_dropped: u8,
        cargo_delivered: u8,
        required_deliveries: u8,
    },
    EmergencyAid {
        protectee_ball_id: i64,
        protectee_hp: f64,
        protectee_max_hp: f64,
        survival_elapsed_secs: u64,
        required_survival_secs: u64,
        waves_repelled: u8,
    },
    MetaliminalMeteoroid {
        meteoroid_ball_id: i64,
        mutanite_type_id: TypeId,
        remaining_units: u32,
        total_units: u32,
        total_residue_units: u32,
        ore_mined_by_fleet: u32,
    },
    DreadAssault {
        dread_ball_id: i64,
        base_ball_id: i64,
        capacitor_gj: f64,
        capacitor_target_gj: f64,
        siege_cycles_completed: u8,
        target_siege_cycles: u8,
        in_siege_mode: bool,
        siege_remaining_secs: u64,
        enemy_base_hp: f64,
        enemy_base_max_hp: f64,
    },
    AbyssalArtifactRecovery {
        current_wave: u8,
        total_waves: u8,
        mutanite_units_remaining: u32,
        wave_hostiles_remaining: u8,
        warp_bubble_active: bool,
        caches_recovered: u32,
        waves_paid: u8,
        extraction_window_secs: u64,
    },
    SuspiciousSignal {
        relay_ball_id: i64,
        array_ball_ids: Vec<i64>,
        arrays_hacked: [bool; 3],
        vulnerability_window_secs: u64,
        relay_hp: f64,
        relay_max_hp: f64,
    },
    SalvageResearch {
        wrecks_salvaged: u8,
        total_wrecks: u8,
        werpost_activated: bool,
    },
    StabilizeRift {
        arrays_cap_pct: [f64; 5],
        held_low_secs: u64,
        required_hold_secs: u64,
    },
    TrafficStop {
        smugglers_destroyed: u8,
        required_smugglers: u8,
        smugglers_escaped: u8,
        civilians_harmed: u8,
    },
}

/// Live runtime state projection for an active Homefront Operation instance.
#[derive(Debug, Clone)]
pub struct HomefrontRuntimeState {
    pub blueprint: HomefrontOperationBlueprint,
    pub solar_system_id: SolarSystemId,
    pub instance_id: i64,
    pub site_position: DVec3,
    pub pocket_position: DVec3,
    pub beacon_ball_id: i64,
    pub outer_gate_ball_id: i64,
    pub dispatch_boss_ball_id: i64,
    pub started: bool,
    pub pocket_spawned: bool,
    pub started_at_sec: u64,
    pub expires_at_sec: u64,
    pub completed: bool,
    pub failed: bool,
    pub failure_reason: Option<String>,
    pub spawned_ball_ids: Vec<i64>,
    pub entered_pilots: HashMap<i64, HomefrontShipEligibility>,
    pub contributions: BTreeMap<i64, HomefrontParticipantContribution>,
    pub progress: HomefrontArchetypeProgress,
}

impl HomefrontRuntimeState {
    pub fn is_objective_complete(&self) -> bool {
        if self.failed {
            return false;
        }
        match &self.progress {
            HomefrontArchetypeProgress::Raid {
                cargo_delivered,
                required_deliveries,
                ..
            } => *cargo_delivered >= *required_deliveries,
            HomefrontArchetypeProgress::EmergencyAid {
                protectee_hp,
                survival_elapsed_secs,
                required_survival_secs,
                ..
            } => *protectee_hp > 0.0 && *survival_elapsed_secs >= *required_survival_secs,
            HomefrontArchetypeProgress::MetaliminalMeteoroid {
                remaining_units, ..
            } => *remaining_units == 0,
            HomefrontArchetypeProgress::DreadAssault {
                siege_cycles_completed,
                target_siege_cycles,
                enemy_base_hp,
                ..
            } => *siege_cycles_completed >= *target_siege_cycles && *enemy_base_hp <= 0.0,
            HomefrontArchetypeProgress::AbyssalArtifactRecovery {
                current_wave,
                total_waves,
                mutanite_units_remaining,
                warp_bubble_active,
                ..
            } => {
                *current_wave >= *total_waves
                    && *mutanite_units_remaining == 0
                    && !*warp_bubble_active
            }
            HomefrontArchetypeProgress::SuspiciousSignal { relay_hp, .. } => *relay_hp <= 0.0,
            HomefrontArchetypeProgress::SalvageResearch {
                wrecks_salvaged,
                total_wrecks,
                ..
            } => *wrecks_salvaged >= *total_wrecks,
            HomefrontArchetypeProgress::StabilizeRift {
                held_low_secs,
                required_hold_secs,
                ..
            } => *held_low_secs >= *required_hold_secs,
            HomefrontArchetypeProgress::TrafficStop {
                smugglers_destroyed,
                required_smugglers,
                smugglers_escaped,
                civilians_harmed,
            } => {
                *smugglers_destroyed >= *required_smugglers
                    && *smugglers_escaped < 5
                    && *civilians_harmed <= 2
            }
        }
    }
}

fn hf_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

/// Updates `is_tethered` on a Suspicious Signal relay via the `SystemHandle` actor API.
fn set_homefront_relay_tethered(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    relay_id: i64,
    tethered: bool,
) {
    if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
        let _ = sys_h.try_update_combat_state(relay_id, |cs| {
            cs.is_tethered = tethered;
        });
    }
}

/// Applies direct shield/armor/structure damage to a Homefront space entity via `SystemHandle`
/// and removes its ball if structure HP reaches zero.
fn apply_homefront_direct_damage_in_space(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    target_id: i64,
    dmg: f64,
) {
    if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
        let mut destroyed = false;
        sys_h.try_update_combat_state(target_id, |cs| {
            let mut rem = dmg;
            let ds = rem.min(cs.shield_hp);
            cs.shield_hp -= ds;
            rem -= ds;
            let da = rem.min(cs.armor_hp);
            cs.armor_hp -= da;
            rem -= da;
            cs.structure_hp = (cs.structure_hp - rem).max(0.0);
            destroyed = cs.structure_hp <= 0.0;
        });
        if destroyed {
            sys_h.try_remove_destroyed_ball(target_id);
        }
    }
}

fn validate_finite_non_negative(name: &str, val: f64) -> Result<(), BridgeRpcError> {
    if !val.is_finite() || val < 0.0 {
        return Err(hf_err(
            "InvalidArgument",
            format!("{name} must be a finite non-negative number"),
        ));
    }
    Ok(())
}

fn validate_finite_unit_fraction(name: &str, val: f64) -> Result<(), BridgeRpcError> {
    if !val.is_finite() || val < 0.0 || val > 1.0 {
        return Err(hf_err(
            "InvalidArgument",
            format!("{name} must be a finite fraction in [0.0, 1.0]"),
        ));
    }
    Ok(())
}

fn arch_to_kind(arch: HomefrontArchetype) -> HomefrontArchetypeKind {
    match arch {
        HomefrontArchetype::Raid => HomefrontArchetypeKind::Raid,
        HomefrontArchetype::EmergencyAid => HomefrontArchetypeKind::EmergencyAid,
        HomefrontArchetype::MetaliminalMeteoroid => HomefrontArchetypeKind::MetaliminalMeteoroid,
        HomefrontArchetype::DreadAssault => HomefrontArchetypeKind::DreadAssault,
        HomefrontArchetype::AbyssalArtifactRecovery => {
            HomefrontArchetypeKind::AbyssalArtifactRecovery
        }
        HomefrontArchetype::SuspiciousSignal => HomefrontArchetypeKind::SuspiciousSignal,
        HomefrontArchetype::SalvageResearch => HomefrontArchetypeKind::SalvageResearch,
        HomefrontArchetype::StabilizeRift => HomefrontArchetypeKind::StabilizeRift,
        HomefrontArchetype::TrafficStop => HomefrontArchetypeKind::TrafficStop,
    }
}

fn kind_to_arch(kind: HomefrontArchetypeKind) -> HomefrontArchetype {
    match kind {
        HomefrontArchetypeKind::Raid => HomefrontArchetype::Raid,
        HomefrontArchetypeKind::EmergencyAid => HomefrontArchetype::EmergencyAid,
        HomefrontArchetypeKind::MetaliminalMeteoroid => HomefrontArchetype::MetaliminalMeteoroid,
        HomefrontArchetypeKind::DreadAssault => HomefrontArchetype::DreadAssault,
        HomefrontArchetypeKind::AbyssalArtifactRecovery => {
            HomefrontArchetype::AbyssalArtifactRecovery
        }
        HomefrontArchetypeKind::SuspiciousSignal => HomefrontArchetype::SuspiciousSignal,
        HomefrontArchetypeKind::SalvageResearch => HomefrontArchetype::SalvageResearch,
        HomefrontArchetypeKind::StabilizeRift => HomefrontArchetype::StabilizeRift,
        HomefrontArchetypeKind::TrafficStop => HomefrontArchetype::TrafficStop,
    }
}

fn role_to_kind(role: HomefrontRole) -> HomefrontRoleKind {
    match role {
        HomefrontRole::Dps => HomefrontRoleKind::Dps,
        HomefrontRole::Logistics => HomefrontRoleKind::Logistics,
        HomefrontRole::Hauler => HomefrontRoleKind::Hauler,
        HomefrontRole::Miner => HomefrontRoleKind::Miner,
        HomefrontRole::Hacker => HomefrontRoleKind::Hacker,
        HomefrontRole::CapBattery => HomefrontRoleKind::CapBattery,
        HomefrontRole::Tackler => HomefrontRoleKind::Tackler,
        HomefrontRole::Neutralizer => HomefrontRoleKind::Neutralizer,
        HomefrontRole::Salvager => HomefrontRoleKind::Salvager,
        HomefrontRole::CargoScanner => HomefrontRoleKind::CargoScanner,
    }
}

fn kind_to_role(kind: HomefrontRoleKind) -> HomefrontRole {
    match kind {
        HomefrontRoleKind::Dps => HomefrontRole::Dps,
        HomefrontRoleKind::Logistics => HomefrontRole::Logistics,
        HomefrontRoleKind::Hauler => HomefrontRole::Hauler,
        HomefrontRoleKind::Miner => HomefrontRole::Miner,
        HomefrontRoleKind::Hacker => HomefrontRole::Hacker,
        HomefrontRoleKind::CapBattery => HomefrontRole::CapBattery,
        HomefrontRoleKind::Tackler => HomefrontRole::Tackler,
        HomefrontRoleKind::Neutralizer => HomefrontRole::Neutralizer,
        HomefrontRoleKind::Salvager => HomefrontRole::Salvager,
        HomefrontRoleKind::CargoScanner => HomefrontRole::CargoScanner,
    }
}

fn bp_to_record(bp: &HomefrontOperationBlueprint) -> HomefrontBlueprintRecord {
    HomefrontBlueprintRecord {
        dungeon_id: bp.dungeon_id,
        dungeon_name: bp.dungeon_name.clone(),
        archetype: arch_to_kind(bp.archetype),
        difficulty: bp.difficulty,
        sovereign_faction_id: bp.sovereign_faction_id,
        hostile_faction_id: bp.hostile_faction_id,
        grants_combat_standing: bp.grants_combat_standing,
        standings_faction_id: bp.standings_faction_id,
        optimal_fleet_size: bp.optimal_fleet_size,
        max_payout_pilots: bp.max_payout_pilots,
        is_exordium_system: bp.is_exordium_system,
        base_pilot_isk: bp.base_pilot_isk,
        base_pilot_lp: bp.base_pilot_lp,
        abyssal_wave_payouts_isk: bp.abyssal_wave_payouts_isk.clone(),
        time_limit_secs: bp.time_limit_secs,
        total_stages: bp.total_stages,
        required_roles: bp.required_roles.iter().copied().map(role_to_kind).collect(),
        dispatch_commander_type_id: bp.dispatch_commander_type_id,
        objective_structure_type_id: bp.objective_structure_type_id,
        support_entity_type_id: bp.support_entity_type_id,
        loot_commodity_type_id: bp.loot_commodity_type_id,
        escort_wave_type_ids: bp.escort_wave_type_ids.clone(),
        damage_profile: bp.damage_profile,
        ewar_profile: bp.ewar_profile.to_string(),
    }
}

fn static_ewar_profile(label: &str) -> &'static str {
    match label {
        "Energy Neutralizer / Weapon Disruption" => "Energy Neutralizer / Weapon Disruption",
        "ECM" => "ECM",
        "Remote Sensor Dampening" => "Remote Sensor Dampening",
        "Target Painter / ECM" => "Target Painter / ECM",
        "Stasis Webifier / Sensor Dampening" => "Stasis Webifier / Sensor Dampening",
        "Stasis Webifier" => "Stasis Webifier",
        "Target Painter" => "Target Painter",
        "Dreadnought Energy Neutralizer / ECM / Weapon Disruption" => {
            "Dreadnought Energy Neutralizer / ECM / Weapon Disruption"
        }
        "Dreadnought Energy Neutralizer / ECM" => "Dreadnought Energy Neutralizer / ECM",
        "Dreadnought Energy Neutralizer / ECM / Target Painter" => {
            "Dreadnought Energy Neutralizer / ECM / Target Painter"
        }
        "Abyssal Warp Disruption Field / Remote Armor Logistics" => {
            "Abyssal Warp Disruption Field / Remote Armor Logistics"
        }
        "Energy Neutralizer / Cap Transfer" => "Energy Neutralizer / Cap Transfer",
        "Stasis Webifier / Combat Drones" => "Stasis Webifier / Combat Drones",
        _ => "Homefront EWAR",
    }
}

fn record_to_bp(rec: &HomefrontBlueprintRecord) -> HomefrontOperationBlueprint {
    HomefrontOperationBlueprint {
        dungeon_id: rec.dungeon_id,
        dungeon_name: rec.dungeon_name.clone(),
        archetype: kind_to_arch(rec.archetype),
        difficulty: rec.difficulty,
        sovereign_faction_id: rec.sovereign_faction_id,
        hostile_faction_id: rec.hostile_faction_id,
        grants_combat_standing: rec.grants_combat_standing,
        standings_faction_id: rec.standings_faction_id,
        optimal_fleet_size: rec.optimal_fleet_size,
        max_payout_pilots: rec.max_payout_pilots,
        is_exordium_system: rec.is_exordium_system,
        base_pilot_isk: rec.base_pilot_isk,
        base_pilot_lp: rec.base_pilot_lp,
        abyssal_wave_payouts_isk: rec.abyssal_wave_payouts_isk.clone(),
        time_limit_secs: rec.time_limit_secs,
        total_stages: rec.total_stages,
        required_roles: rec.required_roles.iter().copied().map(kind_to_role).collect(),
        dispatch_commander_type_id: rec.dispatch_commander_type_id,
        objective_structure_type_id: rec.objective_structure_type_id,
        support_entity_type_id: rec.support_entity_type_id,
        loot_commodity_type_id: rec.loot_commodity_type_id,
        escort_wave_type_ids: rec.escort_wave_type_ids.clone(),
        damage_profile: rec.damage_profile,
        ewar_profile: static_ewar_profile(&rec.ewar_profile),
    }
}

fn elig_to_record(e: &HomefrontShipEligibility) -> HomefrontShipEligibilityRecord {
    HomefrontShipEligibilityRecord {
        ship_type_id: e.ship_type_id,
        ship_group_id: e.ship_group_id,
        hull_class: e.hull_class.to_string(),
        role_hints: e.role_hints.iter().copied().map(role_to_kind).collect(),
    }
}

fn static_hull_class(label: &str) -> &'static str {
    match label {
        "Frigate" => "Frigate",
        "Mining Frigate" => "Mining Frigate",
        "Destroyer" => "Destroyer",
        "Mining Destroyer" => "Mining Destroyer",
        "Basic T1 Cruiser" => "Basic T1 Cruiser",
        "Tech 1 Cruiser" => "Tech 1 Cruiser",
        "Hauler" => "Hauler",
        "Salvage Hauler" => "Salvage Hauler",
        "Tech 1 Hauler" => "Tech 1 Hauler",
        _ => "AllowedHull",
    }
}

fn record_to_elig(rec: &HomefrontShipEligibilityRecord) -> HomefrontShipEligibility {
    HomefrontShipEligibility {
        ship_type_id: rec.ship_type_id,
        ship_group_id: rec.ship_group_id,
        hull_class: static_hull_class(&rec.hull_class),
        role_hints: rec.role_hints.iter().copied().map(kind_to_role).collect(),
    }
}

fn contrib_to_public(c: &HomefrontContributionRecord) -> HomefrontParticipantContribution {
    HomefrontParticipantContribution {
        character_id: c.character_id,
        entry_sequence: c.entry_sequence,
        damage_dealt: c.damage_dealt,
        repair_hp: c.repair_hp,
        cap_transferred: c.cap_transferred,
        ore_mined_units: c.ore_mined_units,
        cargo_delivered: c.cargo_delivered,
        arrays_hacked: c.arrays_hacked,
        objective_actions: c.objective_actions,
    }
}

fn progress_record_to_public(p: &HomefrontProgressRecord) -> HomefrontArchetypeProgress {
    match p {
        HomefrontProgressRecord::Raid {
            haulers_spawned,
            haulers_destroyed,
            haulers_escaped,
            active_hauler_ball_id,
            active_hauler_tackled,
            cargo_dropped,
            cargo_delivered,
            required_deliveries,
            ..
        } => HomefrontArchetypeProgress::Raid {
            haulers_spawned: *haulers_spawned,
            haulers_destroyed: *haulers_destroyed,
            haulers_escaped: *haulers_escaped,
            active_hauler_ball_id: *active_hauler_ball_id,
            active_hauler_tackled: *active_hauler_tackled,
            cargo_dropped: *cargo_dropped,
            cargo_delivered: *cargo_delivered,
            required_deliveries: *required_deliveries,
        },
        HomefrontProgressRecord::EmergencyAid {
            protectee_ball_id,
            protectee_hp,
            protectee_max_hp,
            survival_elapsed_secs,
            required_survival_secs,
            waves_repelled,
        } => HomefrontArchetypeProgress::EmergencyAid {
            protectee_ball_id: *protectee_ball_id,
            protectee_hp: *protectee_hp,
            protectee_max_hp: *protectee_max_hp,
            survival_elapsed_secs: *survival_elapsed_secs,
            required_survival_secs: *required_survival_secs,
            waves_repelled: *waves_repelled,
        },
        HomefrontProgressRecord::MetaliminalMeteoroid {
            meteoroid_ball_id,
            mutanite_type_id,
            remaining_units,
            total_units,
            total_residue_units,
            ore_mined_by_fleet,
        } => HomefrontArchetypeProgress::MetaliminalMeteoroid {
            meteoroid_ball_id: *meteoroid_ball_id,
            mutanite_type_id: *mutanite_type_id,
            remaining_units: *remaining_units,
            total_units: *total_units,
            total_residue_units: *total_residue_units,
            ore_mined_by_fleet: *ore_mined_by_fleet,
        },
        HomefrontProgressRecord::DreadAssault {
            dread_ball_id,
            base_ball_id,
            capacitor_gj,
            capacitor_target_gj,
            siege_cycles_completed,
            target_siege_cycles,
            in_siege_mode,
            siege_remaining_secs,
            enemy_base_hp,
            enemy_base_max_hp,
        } => HomefrontArchetypeProgress::DreadAssault {
            dread_ball_id: *dread_ball_id,
            base_ball_id: *base_ball_id,
            capacitor_gj: *capacitor_gj,
            capacitor_target_gj: *capacitor_target_gj,
            siege_cycles_completed: *siege_cycles_completed,
            target_siege_cycles: *target_siege_cycles,
            in_siege_mode: *in_siege_mode,
            siege_remaining_secs: *siege_remaining_secs,
            enemy_base_hp: *enemy_base_hp,
            enemy_base_max_hp: *enemy_base_max_hp,
        },
        HomefrontProgressRecord::AbyssalArtifactRecovery {
            current_wave,
            total_waves,
            mutanite_units_remaining,
            wave_hostiles_remaining,
            warp_bubble_active,
            caches_recovered,
            waves_paid,
            extraction_window_secs,
            ..
        } => HomefrontArchetypeProgress::AbyssalArtifactRecovery {
            current_wave: *current_wave,
            total_waves: *total_waves,
            mutanite_units_remaining: *mutanite_units_remaining,
            wave_hostiles_remaining: *wave_hostiles_remaining,
            warp_bubble_active: *warp_bubble_active,
            caches_recovered: *caches_recovered,
            waves_paid: *waves_paid,
            extraction_window_secs: *extraction_window_secs,
        },
        HomefrontProgressRecord::SuspiciousSignal {
            relay_ball_id,
            array_ball_ids,
            arrays_hacked,
            vulnerability_window_secs,
            relay_hp,
            relay_max_hp,
        } => HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id: *relay_ball_id,
            array_ball_ids: array_ball_ids.clone(),
            arrays_hacked: *arrays_hacked,
            vulnerability_window_secs: *vulnerability_window_secs,
            relay_hp: *relay_hp,
            relay_max_hp: *relay_max_hp,
        },
        HomefrontProgressRecord::SalvageResearch {
            wrecks_salvaged,
            total_wrecks,
            werpost_activated,
            ..
        } => HomefrontArchetypeProgress::SalvageResearch {
            wrecks_salvaged: *wrecks_salvaged,
            total_wrecks: *total_wrecks,
            werpost_activated: *werpost_activated,
        },
        HomefrontProgressRecord::StabilizeRift {
            arrays_cap_pct,
            held_low_secs,
            required_hold_secs,
            ..
        } => HomefrontArchetypeProgress::StabilizeRift {
            arrays_cap_pct: *arrays_cap_pct,
            held_low_secs: *held_low_secs,
            required_hold_secs: *required_hold_secs,
        },
        HomefrontProgressRecord::TrafficStop {
            smugglers_destroyed,
            required_smugglers,
            smugglers_escaped,
            civilians_harmed,
            ..
        } => HomefrontArchetypeProgress::TrafficStop {
            smugglers_destroyed: *smugglers_destroyed,
            required_smugglers: *required_smugglers,
            smugglers_escaped: *smugglers_escaped,
            civilians_harmed: *civilians_harmed,
        },
    }
}

fn record_to_runtime_state(rec: &HomefrontRuntimeRecord) -> HomefrontRuntimeState {
    HomefrontRuntimeState {
        blueprint: record_to_bp(&rec.blueprint),
        solar_system_id: rec.solar_system_id,
        instance_id: rec.instance_id,
        site_position: DVec3::from_array(rec.site_position),
        pocket_position: DVec3::from_array(rec.pocket_position),
        beacon_ball_id: rec.outer_beacon_ball_id,
        outer_gate_ball_id: rec.outer_gate_ball_id,
        dispatch_boss_ball_id: rec.dispatch_boss_ball_id,
        started: rec.started,
        pocket_spawned: rec.pocket_spawned,
        started_at_sec: rec.started_at_sec,
        expires_at_sec: rec.expires_at_sec,
        completed: rec.completed,
        failed: rec.failed,
        failure_reason: rec.failure_reason.clone(),
        spawned_ball_ids: rec.spawned_ball_ids.clone(),
        entered_pilots: rec
            .entered_pilots
            .iter()
            .map(|(&cid, e)| (cid, record_to_elig(e)))
            .collect(),
        contributions: rec
            .contributions
            .iter()
            .map(|(&cid, c)| (cid, contrib_to_public(c)))
            .collect(),
        progress: progress_record_to_public(&rec.progress),
    }
}

fn runtime_record_objective_complete(rec: &HomefrontRuntimeRecord) -> bool {
    record_to_runtime_state(rec).is_objective_complete()
}

fn initial_progress_record(
    bp: &HomefrontOperationBlueprint,
    instance_id: i64,
    alloc_id: &mut impl FnMut() -> i64,
) -> HomefrontProgressRecord {
    let base_obj = 1_897_800_000 + instance_id.saturating_mul(100);
    match bp.archetype {
        HomefrontArchetype::Raid => HomefrontProgressRecord::Raid {
            haulers_spawned: 1,
            haulers_destroyed: 0,
            haulers_escaped: 0,
            active_hauler_ball_id: Some(base_obj + 10),
            active_hauler_tackled: false,
            active_hauler_escape_at_sec: 0,
            freighter_ball_id: base_obj + 11,
            delivery_depot_ball_id: alloc_id(),
            dropped_crate_ball_ids: Vec::new(),
            cargo_dropped: 0,
            cargo_delivered: 0,
            required_deliveries: 4,
        },
        HomefrontArchetype::EmergencyAid => HomefrontProgressRecord::EmergencyAid {
            protectee_ball_id: base_obj + 12,
            protectee_hp: 120_000.0,
            protectee_max_hp: 120_000.0,
            survival_elapsed_secs: 0,
            required_survival_secs: 600,
            waves_repelled: 0,
        },
        HomefrontArchetype::MetaliminalMeteoroid => {
            HomefrontProgressRecord::MetaliminalMeteoroid {
                meteoroid_ball_id: alloc_id(),
                mutanite_type_id: bp.loot_commodity_type_id.unwrap_or(77_118),
                remaining_units: 5_000,
                total_units: 5_000,
                total_residue_units: 0,
                ore_mined_by_fleet: 0,
            }
        }
        HomefrontArchetype::DreadAssault => HomefrontProgressRecord::DreadAssault {
            dread_ball_id: base_obj + 13,
            base_ball_id: base_obj + 14,
            capacitor_gj: 0.0,
            capacitor_target_gj: 30_000.0,
            siege_cycles_completed: 0,
            target_siege_cycles: 3,
            in_siege_mode: false,
            siege_remaining_secs: 0,
            enemy_base_hp: 600_000.0,
            enemy_base_max_hp: 600_000.0,
        },
        HomefrontArchetype::AbyssalArtifactRecovery => {
            let rock_ball_ids: Vec<i64> =
                (0..HOMEFRONT_AAR_ROCKS_PER_WAVE).map(|_| alloc_id()).collect();
            HomefrontProgressRecord::AbyssalArtifactRecovery {
                current_wave: 1,
                total_waves: 9,
                rock_ball_ids,
                wave_hostile_ball_ids: Vec::new(),
                weather_kind: ((instance_id.unsigned_abs() % 5) as u8),
                setup_remaining_secs: 90,
                wave_time_remaining_secs: 180,
                evacuation_active: false,
                basileus_spawned: false,
                cache_ball_id: None,
                mutanite_units_remaining: (HOMEFRONT_AAR_ROCKS_PER_WAVE as u32)
                    * HOMEFRONT_AAR_UNITS_PER_ROCK,
                wave_hostiles_remaining: 4,
                warp_bubble_active: true,
                caches_recovered: 0,
                waves_paid: 0,
                waves_paid_mask: 0,
                extraction_window_secs: 0,
            }
        }
        HomefrontArchetype::SuspiciousSignal => HomefrontProgressRecord::SuspiciousSignal {
            relay_ball_id: base_obj + 15,
            array_ball_ids: vec![base_obj + 60, base_obj + 61, base_obj + 62],
            arrays_hacked: [false; 3],
            vulnerability_window_secs: 0,
            relay_hp: 350_000.0,
            relay_max_hp: 350_000.0,
        },
        HomefrontArchetype::SalvageResearch => HomefrontProgressRecord::SalvageResearch {
            werpost_ball_id: base_obj + 16,
            wreck_ball_ids: (0..30).map(|_| alloc_id()).collect(),
            wrecks_salvaged: 0,
            total_wrecks: 30,
            werpost_activated: false,
        },
        HomefrontArchetype::StabilizeRift => HomefrontProgressRecord::StabilizeRift {
            array_ball_ids: vec![
                base_obj + 65,
                base_obj + 66,
                base_obj + 67,
                base_obj + 68,
                base_obj + 69,
            ],
            arrays_cap_pct: [1.0; 5],
            held_low_secs: 0,
            required_hold_secs: 60,
        },
        HomefrontArchetype::TrafficStop => HomefrontProgressRecord::TrafficStop {
            station_ball_id: base_obj + 17,
            active_hauler_ball_ids: Vec::new(),
            smuggler_ball_ids: Vec::new(),
            civilian_ball_ids: Vec::new(),
            scanned_hauler_ball_ids: Vec::new(),
            survival_elapsed_secs: 0,
            required_survival_secs: HOMEFRONT_TRAFFIC_TIMER_SECS,
            smugglers_destroyed: 0,
            required_smugglers: 6,
            smugglers_escaped: 0,
            civilians_harmed: 0,
        },
    }
}

fn build_runtime_record_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    site: &HomefrontSiteRecord,
    slot_idx: usize,
) -> HomefrontRuntimeRecord {
    let bp = resolve_homefront_archetype_blueprint(
        sde,
        site.dungeon_id,
        &site.dungeon_name,
        site.solar_system_id,
        2,
    );
    let site_pos = [((slot_idx as f64) + 1.0) * 1.2e10, 0.0, 4.0e9];
    let pocket_pos = [site_pos[0], site_pos[1], site_pos[2] + HOMEFRONT_POCKET_Z_OFFSET_M];
    let base_obj = 1_897_800_000 + site.instance_id.saturating_mul(100);
    let mut alloc_id = || {
        snap.next_entity_id += 1;
        snap.next_entity_id
    };
    let progress = initial_progress_record(&bp, site.instance_id, &mut alloc_id);
    HomefrontRuntimeRecord {
        blueprint: bp_to_record(&bp),
        solar_system_id: site.solar_system_id,
        instance_id: site.instance_id,
        site_position: site_pos,
        pocket_position: pocket_pos,
        outer_beacon_ball_id: base_obj,
        outer_gate_ball_id: base_obj + 1,
        dispatch_boss_ball_id: site.boss_npc_ball_id,
        started: false,
        pocket_spawned: false,
        started_at_sec: 0,
        expires_at_sec: 0,
        last_tick_sec: 0,
        completed: site.completed,
        failed: false,
        failure_reason: None,
        next_entry_sequence: 0,
        spawned_ball_ids: Vec::new(),
        entered_pilots: BTreeMap::new(),
        contributions: BTreeMap::new(),
        progress,
    }
}

fn resolve_runtime_record_in_snap<'a>(
    snap: &'a mut WorldStateSnapshot,
    sde: &SdeRegistry,
    instance_id: i64,
) -> Option<&'a mut HomefrontRuntimeRecord> {
    if !snap.dungeon_runtime.homefront_runtimes.contains_key(&instance_id) {
        let found = snap
            .pve_special_sites
            .homefront_sites
            .values()
            .find_map(|list| {
                list.iter()
                    .enumerate()
                    .find(|(_, r)| r.instance_id == instance_id)
                    .map(|(slot_idx, r)| (slot_idx, r.clone()))
            });
        if let Some((slot_idx, site_rec)) = found {
            let built = build_runtime_record_in_snap(snap, sde, &site_rec, slot_idx);
            snap.dungeon_runtime
                .homefront_runtimes
                .insert(instance_id, built);
        }
    }
    snap.dungeon_runtime.homefront_runtimes.get_mut(&instance_id)
}

fn resolve_site_record_completed_in_snap(snap: &WorldStateSnapshot, instance_id: i64) -> bool {
    snap.pve_special_sites
        .homefront_sites
        .values()
        .flatten()
        .find(|s| s.instance_id == instance_id)
        .is_some_and(|s| s.completed)
}

/// Seeds up to 2 high-sec Homefront Operations for `sys_id` using V24 `archetype_id == 70` templates
/// and archetype/faction blueprints.
pub fn ensure_homefront_sites_for_system_locked(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
) {
    if reg.homefront_sites.contains_key(&sys_id) {
        return;
    }
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec < 0.45 || sys_id >= 31_000_000 {
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    let templates = resolve_sde_homefront_templates_for_system(&ctx.sde, sys_id);
    if templates.is_empty() {
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    let rng = crate::rng::MechanicRng::seeded(
        (sys_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ctx.now_unix(),
    );
    let idx1 = (rng.try_next_u64().unwrap_or(sys_id as u64) as usize) % templates.len();
    let idx2 = if templates.len() > 1 {
        let mut step = 1 + ((rng.try_next_u64().unwrap_or(1) as usize) % (templates.len() - 1));
        let arch1 = HomefrontArchetype::from_dungeon(
            templates[idx1].dungeon_id,
            &templates[idx1].dungeon_name,
        );
        for offset in 0..(templates.len() - 1) {
            let cand = (idx1 + ((step + offset) % (templates.len() - 1)).max(1)) % templates.len();
            if HomefrontArchetype::from_dungeon(
                templates[cand].dungeon_id,
                &templates[cand].dungeon_name,
            ) != arch1
            {
                step = (cand + templates.len() - idx1) % templates.len();
                break;
            }
        }
        (idx1 + step) % templates.len()
    } else {
        idx1
    };

    let base_inst = (sys_id as i64) * 1000;
    let t1 = &templates[idx1];
    let t2 = &templates[idx2];
    let bp1 = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        t1.dungeon_id,
        &t1.dungeon_name,
        sys_id,
        t1.difficulty,
    );
    let bp2 = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        t2.dungeon_id,
        &t2.dungeon_name,
        sys_id,
        t2.difficulty,
    );

    reg.homefront_sites.insert(
        sys_id,
        vec![
            HomefrontSiteRecord {
                instance_id: base_inst + 1,
                dungeon_id: t1.dungeon_id,
                dungeon_name: t1.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: bp1.optimal_fleet_size,
                base_pilot_isk: bp1.base_pilot_isk,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 1),
                boss_type_id: bp1.dispatch_commander_type_id,
            },
            HomefrontSiteRecord {
                instance_id: base_inst + 2,
                dungeon_id: t2.dungeon_id,
                dungeon_name: t2.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: bp2.optimal_fleet_size,
                base_pilot_isk: bp2.base_pilot_isk,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 2),
                boss_type_id: bp2.dispatch_commander_type_id,
            },
        ],
    );
}

pub fn init_homefront_sites(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    for &sys_id in HOMEFRONT_SYSTEMS {
        ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
    }
}

/// Spawns ONLY the outer Homefront beacon (`77149`) and acceleration gate (`17831`) on the warp-in grid.
pub fn spawn_homefront_outer_entry_sync(ctx: &ServerContext, rec: &HomefrontRuntimeRecord) {
    let sys_h = ctx.systems.get_or_spawn(rec.solar_system_id);
    let pos = DVec3::from_array(rec.site_position);
    let tethered_cs = || CombatState {
        is_tethered: true,
        shield_hp: 100_000.0,
        max_shield_hp: 100_000.0,
        armor_hp: 100_000.0,
        max_armor_hp: 100_000.0,
        structure_hp: 100_000.0,
        max_structure_hp: 100_000.0,
        ..CombatState::default()
    };
    let view = sys_h.view();
    if !view.balls.contains_key(&rec.outer_beacon_ball_id) {
        ctx.systems
            .claim_special_site_ball(rec.solar_system_id, rec.outer_beacon_ball_id);
        sys_h.try_spawn_ball_with_combat(
            Ball {
                id: rec.outer_beacon_ball_id,
                type_id: HOMEFRONT_BEACON_TYPE_ID,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            tethered_cs(),
        );
    }
    if !view.balls.contains_key(&rec.outer_gate_ball_id) {
        ctx.systems
            .claim_special_site_ball(rec.solar_system_id, rec.outer_gate_ball_id);
        sys_h.try_spawn_ball_with_combat(
            Ball {
                id: rec.outer_gate_ball_id,
                type_id: HOMEFRONT_ACCEL_GATE_TYPE_ID,
                position: pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 5_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            tethered_cs(),
        );
    }
}

/// Ensures outer beacon + acceleration gate exist in space for seeded Homefront sites,
/// without spawning pocket NPCs or objectives directly in space.
pub fn ensure_homefront_outer_entries_sync(
    ctx: &ServerContext,
    target_sys: Option<SolarSystemId>,
    target_inst: Option<i64>,
) {
    let candidates: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .pve_special_sites
            .homefront_sites
            .iter()
            .filter(|(&sid, _)| target_sys.is_none_or(|t| t == sid))
            .flat_map(|(_, list)| list.iter())
            .filter(|s| !s.completed && target_inst.is_none_or(|t| t == s.instance_id))
            .map(|s| s.instance_id)
            .collect();
        for r in snap.dungeon_runtime.homefront_runtimes.values() {
            if !r.completed
                && !r.failed
                && target_sys.is_none_or(|t| t == r.solar_system_id)
                && target_inst.is_none_or(|t| t == r.instance_id)
                && !ids.contains(&r.instance_id)
            {
                ids.push(r.instance_id);
            }
        }
        ids
    });
    if candidates.is_empty() {
        return;
    }
    let resolved = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<Vec<HomefrontRuntimeRecord>, BridgeRpcError> {
            let mut recs = Vec::new();
            for inst_id in candidates {
                if let Some(rec) = resolve_runtime_record_in_snap(snap, &ctx.sde, inst_id) {
                    if !rec.completed && !rec.failed {
                        recs.push(rec.clone());
                    }
                }
            }
            Ok(recs)
        },
    );
    if let Ok(recs) = resolved {
        for rec in recs {
            spawn_homefront_outer_entry_sync(ctx, &rec);
        }
    }
}

/// Spawns or activates a specific V24 SDE Homefront `dungeon_id` (`archetype_id == 70`) in `sys_id`
/// for live space operations, spawning ONLY the outer beacon + acceleration gate.
pub fn spawn_homefront_site_for_dungeon(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    dungeon_id: u32,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec < 0.45 || sys_id >= 31_000_000 {
        return Err(hf_err(
            "InvalidHomefrontSecurity",
            format!("Homefront operations require High-Security empire space (sys {sys_id})"),
        ));
    }
    let dungeon = ctx.sde.dungeons.get(&dungeon_id).ok_or_else(|| {
        hf_err(
            "UnknownDungeon",
            format!("Unknown Homefront dungeon ID {dungeon_id}"),
        )
    })?;
    if dungeon.archetype_id != Some(70) {
        return Err(hf_err(
            "InvalidDungeonArchetype",
            format!("Dungeon {dungeon_id} is not an archetype 70 Homefront Operation"),
        ));
    }

    let name = ctx
        .sde
        .operations()
        .ok()
        .and_then(|m| m.get(&dungeon_id.to_string()).cloned())
        .and_then(|v| v.get("operationNameID").and_then(|id| id.as_u64()))
        .and_then(|msg_id| ctx.sde.localized_message_en_us(msg_id as u32))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| dungeon.name.clone());

    let bp = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        dungeon_id,
        &name,
        sys_id,
        dungeon.difficulty.unwrap_or(2),
    );
    let now_seed = ctx.now_unix();
    let rec = ctx.db.try_with_mut_snapshot(|snap| -> Result<HomefrontRuntimeRecord, BridgeRpcError> {
        ensure_homefront_sites_for_system_locked_with_seed(ctx, &mut snap.pve_special_sites, sys_id, now_seed);
        let list = snap.pve_special_sites.homefront_sites.entry(sys_id).or_default();
        let next_slot = list.len() + 1;
        let instance_id = (sys_id as i64) * 1000 + (next_slot as i64);
        let site_rec = HomefrontSiteRecord {
            instance_id,
            dungeon_id,
            dungeon_name: bp.dungeon_name.clone(),
            solar_system_id: sys_id,
            optimal_fleet_size: bp.optimal_fleet_size,
            base_pilot_isk: bp.base_pilot_isk,
            completed: false,
            reward_paid_isk: 0,
            boss_npc_ball_id: 1_897_700_000 + instance_id,
            boss_type_id: bp.dispatch_commander_type_id,
        };
        list.push(site_rec.clone());
        let rt_rec = build_runtime_record_in_snap(snap, &ctx.sde, &site_rec, next_slot.saturating_sub(1));
        snap.dungeon_runtime
            .homefront_runtimes
            .insert(instance_id, rt_rec.clone());
        Ok(rt_rec)
    })?;

    spawn_homefront_outer_entry_sync(ctx, &rec);
    Ok(record_to_runtime_state(&rec))
}

fn ensure_homefront_sites_for_system_locked_with_seed(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
    _seed: u64,
) {
    ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
}

fn register_homefront_npc_entity(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ball_id: i64,
    type_id: TypeId,
    faction_id: u32,
    archetype_tag: &str,
    spawn_pos: DVec3,
    dmg: [f64; 4],
    total_dps: f64,
    orbit_range: f64,
    optimal_range: f64,
) {
    let damage_vector = DamageVector::new(
        dmg[0] * total_dps,
        dmg[1] * total_dps,
        dmg[2] * total_dps,
        dmg[3] * total_dps,
    );
    let ent = ExtendedNpcEntity {
        ball_id,
        type_id,
        faction_id,
        archetype: archetype_tag.to_string(),
        solar_system_id: sys_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range,
        orbit_range,
        damage_vector,
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 1.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: spawn_pos,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    };
    with_mut_combat_ephemeral(ctx, |rt| {
        rt.npc_entities.insert(ball_id, ent);
    });
}

fn spawn_ball_db_with_claim(
    systems: &SystemRegistry,
    sys_h: &SystemHandle,
    sys_id: SolarSystemId,
    ball: Ball,
    cs: CombatState,
) {
    let id = ball.id;
    systems.claim_special_site_ball(sys_id, id);
    sys_h.try_spawn_ball_with_combat(ball, cs);
}

fn spawn_ball_sync_with_claim(
    ctx: &ServerContext,
    sys_h: &SystemHandle,
    sys_id: SolarSystemId,
    ball: Ball,
    cs: CombatState,
) {
    spawn_ball_db_with_claim(&ctx.systems, sys_h, sys_id, ball, cs);
}

fn spawn_asteroid_db_with_record(
    systems: &SystemRegistry,
    db: &WorldRepository,
    sys_h: &SystemHandle,
    sys_id: SolarSystemId,
    asteroid_id: i64,
    type_id: TypeId,
    units: u32,
    pos: DVec3,
    radius: f64,
) {
    systems.claim_special_site_ball(sys_id, asteroid_id);
    sys_h.try_spawn_asteroid(
        AsteroidState {
            asteroid_id,
            belt_id: 0,
            type_id,
            initial_units: u64::from(units),
            remaining_units: u64::from(units),
            unit_volume: HOMEFRONT_MUTANITE_UNIT_VOLUME_M3,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: asteroid_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 500_000.0,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        snap.asteroids.insert(
            asteroid_id,
            AsteroidRecord {
                asteroid_id,
                type_id,
                solar_system_id: sys_id,
                belt_id: 0,
                quantity: i64::from(units),
                initial_quantity: i64::from(units),
                position: (pos.x, pos.y, pos.z),
                radius,
            },
        );
        Ok(())
    });
}

fn spawn_asteroid_sync_with_record(
    ctx: &ServerContext,
    sys_h: &SystemHandle,
    sys_id: SolarSystemId,
    asteroid_id: i64,
    type_id: TypeId,
    units: u32,
    pos: DVec3,
    radius: f64,
) {
    spawn_asteroid_db_with_record(
        &ctx.systems,
        &ctx.db,
        sys_h,
        sys_id,
        asteroid_id,
        type_id,
        units,
        pos,
        radius,
    );
}

/// Materializes a Homefront scene item into snapshot state via the tracked transient item API.
fn insert_scene_item_in_snap(snap: &mut WorldStateSnapshot, item: InventoryItemRecord) {
    let id = item.item_id;
    snap.insert_transient_item(item);
    let _ = snap.materialize_transient_item(id);
}

fn spawn_container_item_in_snap(
    snap: &mut WorldStateSnapshot,
    sys_id: SolarSystemId,
    container_id: i64,
    owner_id: i64,
    pos: [f64; 3],
    now: u64,
    contents: &[(TypeId, u64)],
) {
    insert_scene_item_in_snap(
        snap,
        InventoryItemRecord {
            item_id: container_id,
            type_id: HOMEFRONT_CARGO_CONTAINER_TYPE_ID,
            owner_id,
            location_id: sys_id as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    snap.item_positions.insert(container_id, pos);
    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, container_id, now, true);
    for &(item_tid, qty) in contents {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        insert_scene_item_in_snap(
            snap,
            InventoryItemRecord {
                item_id,
                type_id: item_tid,
                owner_id,
                location_id: container_id,
                flag: InventoryFlag::Cargo,
                quantity: i32::try_from(qty).unwrap_or(i32::MAX),
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    }
}

/// Spawns the inner pocket entities behind the acceleration gate (`pocket_position`), updating
/// `spawned_ball_ids` and `pocket_spawned = true` in DB.
fn spawn_pocket_entities_sync(ctx: &ServerContext, instance_id: i64) {
    let rec_opt = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime.homefront_runtimes.get(&instance_id).cloned()
    });
    let Some(mut rec) = rec_opt else {
        return;
    };
    if rec.pocket_spawned {
        return;
    }

    let sys_id = rec.solar_system_id;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let pos = DVec3::from_array(rec.pocket_position);
    let bp = record_to_bp(&rec.blueprint);
    let base_obj = 1_897_800_000 + rec.instance_id.saturating_mul(100);
    let now = ctx.now_unix();
    let mut spawned = vec![rec.outer_beacon_ball_id, rec.outer_gate_ball_id];

    spawn_homefront_outer_entry_sync(ctx, &rec);

    // 1. Dispatch commander NPC inside pocket
    let boss_id = rec.dispatch_boss_ball_id;
    let boss_tid = bp.dispatch_commander_type_id;
    let boss_pos = pos + DVec3::new(0.0, 0.0, 2_500.0);
    spawn_ball_sync_with_claim(
        ctx,
        &sys_h,
        sys_id,
        Ball {
            id: boss_id,
            type_id: boss_tid,
            position: boss_pos,
            velocity: DVec3::ZERO,
            max_velocity: 260.0,
            inertia: 0.6,
            mass: 12_000_000.0,
            radius: 180.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(boss_tid),
    );
    register_homefront_npc_entity(
        ctx,
        sys_id,
        boss_id,
        boss_tid,
        bp.hostile_faction_id,
        "HomefrontHostile",
        boss_pos,
        bp.damage_profile,
        140.0,
        4_500.0,
        25_000.0,
    );
    spawned.push(boss_id);

    // 2. Escort wave NPCs inside pocket
    let escort_tag = if bp.archetype == HomefrontArchetype::EmergencyAid {
        "HomefrontProtecteeTarget"
    } else {
        "HomefrontHostile"
    };
    let mut escort_ids = Vec::with_capacity(bp.escort_wave_type_ids.len());
    for (idx, &esc_tid) in bp.escort_wave_type_ids.iter().enumerate() {
        let esc_id = base_obj + 20 + (idx as i64);
        let offset = DVec3::new(3_000.0 + (idx as f64) * 1_500.0, -2_000.0, 0.0);
        let esc_pos = pos + offset;
        spawn_ball_sync_with_claim(
            ctx,
            &sys_h,
            sys_id,
            Ball {
                id: esc_id,
                type_id: esc_tid,
                position: esc_pos,
                velocity: DVec3::ZERO,
                max_velocity: 260.0,
                inertia: 0.5,
                mass: 2_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(esc_tid),
        );
        register_homefront_npc_entity(
            ctx,
            sys_id,
            esc_id,
            esc_tid,
            bp.hostile_faction_id,
            escort_tag,
            esc_pos,
            bp.damage_profile,
            90.0,
            3_500.0,
            20_000.0,
        );
        spawned.push(esc_id);
        escort_ids.push(esc_id);
    }

    let ally_target_opt = match &rec.progress {
        HomefrontProgressRecord::EmergencyAid {
            protectee_ball_id, ..
        } => Some(*protectee_ball_id),
        HomefrontProgressRecord::DreadAssault { dread_ball_id, .. } => Some(*dread_ball_id),
        _ => None,
    };
    if let Some(ally_id) = ally_target_opt {
        with_mut_combat_ephemeral(ctx, |rt| {
            for nid in std::iter::once(boss_id).chain(escort_ids.iter().copied()) {
                if let Some(npc) = rt.npc_entities.get_mut(&nid) {
                    npc.aggression_threat.entry(ally_id).or_insert(500.0);
                    npc.threat_table.entry(ally_id).or_insert(500.0);
                }
            }
        });
    }

    // 3. Archetype-specific pocket objectives
    match &mut rec.progress {
        HomefrontProgressRecord::Raid {
            active_hauler_ball_id,
            active_hauler_escape_at_sec,
            freighter_ball_id,
            delivery_depot_ball_id,
            ..
        } => {
            *active_hauler_escape_at_sec = now.saturating_add(90);
            if let Some(hauler_id) = *active_hauler_ball_id {
                let hauler_tid = bp.objective_structure_type_id.unwrap_or(77_192);
                let h_pos = pos + DVec3::new(4_000.0, 0.0, 2_000.0);
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: hauler_id,
                        type_id: hauler_tid,
                        position: h_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 180.0,
                        inertia: 0.8,
                        mass: 15_000_000.0,
                        radius: 260.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(hauler_tid),
                );
                spawned.push(hauler_id);
            }
            let freighter_tid = bp.support_entity_type_id.unwrap_or(HOMEFRONT_BEACON_TYPE_ID);
            let f_pos = pos + DVec3::new(-4_000.0, -4_000.0, 0.0);
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *freighter_ball_id,
                    type_id: freighter_tid,
                    position: f_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 600.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_tethered: true,
                    shield_hp: 200_000.0,
                    max_shield_hp: 200_000.0,
                    armor_hp: 200_000.0,
                    max_armor_hp: 200_000.0,
                    structure_hp: 200_000.0,
                    max_structure_hp: 200_000.0,
                    ..CombatState::default()
                },
            );
            spawned.push(*freighter_ball_id);

            let depot_id = *delivery_depot_ball_id;
            let depot_pos = pos + DVec3::new(-4_000.0, -5_500.0, 0.0);
            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                spawn_container_item_in_snap(
                    snap,
                    sys_id,
                    depot_id,
                    0,
                    depot_pos.to_array(),
                    now,
                    &[],
                );
                Ok(())
            });
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: depot_id,
                    type_id: HOMEFRONT_CARGO_CONTAINER_TYPE_ID,
                    position: depot_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 2_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_tethered: true,
                    structure_hp: 5_000.0,
                    max_structure_hp: 5_000.0,
                    ..CombatState::default()
                },
            );
            spawned.push(depot_id);
        }
        HomefrontProgressRecord::EmergencyAid {
            protectee_ball_id,
            protectee_max_hp,
            ..
        } => {
            let p_tid = bp.support_entity_type_id.unwrap_or(77_196);
            let p_pos = pos + DVec3::new(0.0, 0.0, 0.0);
            let each_layer = *protectee_max_hp / 3.0;
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *protectee_ball_id,
                    type_id: p_tid,
                    position: p_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 10.0,
                    inertia: 1.0,
                    mass: 25_000_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: each_layer,
                    max_shield_hp: each_layer,
                    armor_hp: each_layer,
                    max_armor_hp: each_layer,
                    structure_hp: each_layer,
                    max_structure_hp: each_layer,
                    is_tethered: false,
                    ..CombatState::default()
                },
            );
            spawned.push(*protectee_ball_id);
        }
        HomefrontProgressRecord::MetaliminalMeteoroid {
            meteoroid_ball_id,
            mutanite_type_id,
            total_units,
            ..
        } => {
            spawn_asteroid_sync_with_record(
                ctx,
                &sys_h,
                sys_id,
                *meteoroid_ball_id,
                *mutanite_type_id,
                *total_units,
                pos + DVec3::new(0.0, 0.0, 3_000.0),
                450.0,
            );
            spawned.push(*meteoroid_ball_id);
        }
        HomefrontProgressRecord::DreadAssault {
            dread_ball_id,
            base_ball_id,
            enemy_base_max_hp,
            ..
        } => {
            let d_tid = bp.support_entity_type_id.unwrap_or(77_197);
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *dread_ball_id,
                    type_id: d_tid,
                    position: pos + DVec3::new(-4_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 40.0,
                    inertia: 1.0,
                    mass: 90_000_000.0,
                    radius: 900.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 150_000.0,
                    max_shield_hp: 150_000.0,
                    armor_hp: 150_000.0,
                    max_armor_hp: 150_000.0,
                    structure_hp: 150_000.0,
                    max_structure_hp: 150_000.0,
                    capacitor: 0.0,
                    max_capacitor: 120_000.0,
                    capacitor_recharge_time_ms: 1.0e12,
                    is_tethered: false,
                    ..CombatState::default()
                },
            );
            spawned.push(*dread_ball_id);

            let b_tid = bp.objective_structure_type_id.unwrap_or(77_195);
            let each_layer = *enemy_base_max_hp / 3.0;
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *base_ball_id,
                    type_id: b_tid,
                    position: pos + DVec3::new(8_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 200_000_000.0,
                    radius: 1_500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: each_layer,
                    max_shield_hp: each_layer,
                    armor_hp: each_layer,
                    max_armor_hp: each_layer,
                    structure_hp: each_layer,
                    max_structure_hp: each_layer,
                    is_tethered: false,
                    ..CombatState::default()
                },
            );
            spawned.push(*base_ball_id);
        }
        HomefrontProgressRecord::AbyssalArtifactRecovery { rock_ball_ids, .. } => {
            sys_h.try_add_mobile_warp_disruptor(base_obj + 9, pos, 150_000.0);
            for (idx, &rock_id) in rock_ball_ids.iter().enumerate() {
                let angle = (idx as f64) * std::f64::consts::TAU
                    / (HOMEFRONT_AAR_ROCKS_PER_WAVE as f64);
                let rock_pos = pos + DVec3::new(angle.cos() * 7_500.0, 0.0, angle.sin() * 7_500.0);
                spawn_asteroid_sync_with_record(
                    ctx,
                    &sys_h,
                    sys_id,
                    rock_id,
                    HOMEFRONT_AAR_MUTANITE_TYPE_ID,
                    HOMEFRONT_AAR_UNITS_PER_ROCK,
                    rock_pos,
                    120.0,
                );
                spawned.push(rock_id);
            }
        }
        HomefrontProgressRecord::SuspiciousSignal {
            relay_ball_id,
            array_ball_ids,
            relay_max_hp,
            ..
        } => {
            let r_tid = bp.objective_structure_type_id.unwrap_or(77_199);
            let each_layer = *relay_max_hp / 3.0;
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *relay_ball_id,
                    type_id: r_tid,
                    position: pos + DVec3::new(0.0, 2_500.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: each_layer,
                    max_shield_hp: each_layer,
                    armor_hp: each_layer,
                    max_armor_hp: each_layer,
                    structure_hp: each_layer,
                    max_structure_hp: each_layer,
                    is_tethered: true,
                    ..CombatState::default()
                },
            );
            spawned.push(*relay_ball_id);

            let arr_tid = bp.support_entity_type_id.unwrap_or(77_198);
            for (idx, &arr_id) in array_ball_ids.iter().enumerate() {
                let offset = DVec3::new((idx as f64 - 1.0) * 12_000.0, -5_000.0, 0.0);
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: arr_id,
                        type_id: arr_tid,
                        position: pos + offset,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 5_000_000.0,
                        radius: 300.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        is_tethered: true,
                        shield_hp: 50_000.0,
                        max_shield_hp: 50_000.0,
                        armor_hp: 50_000.0,
                        max_armor_hp: 50_000.0,
                        structure_hp: 50_000.0,
                        max_structure_hp: 50_000.0,
                        ..CombatState::default()
                    },
                );
                spawned.push(arr_id);
            }
        }
        HomefrontProgressRecord::SalvageResearch {
            werpost_ball_id,
            wreck_ball_ids,
            ..
        } => {
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *werpost_ball_id,
                    type_id: HOMEFRONT_SALVAGE_WERPOST_TYPE_ID,
                    position: pos + DVec3::new(0.0, 4_000.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 20_000_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_tethered: true,
                    shield_hp: 100_000.0,
                    max_shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    max_armor_hp: 100_000.0,
                    structure_hp: 100_000.0,
                    max_structure_hp: 100_000.0,
                    ..CombatState::default()
                },
            );
            spawned.push(*werpost_ball_id);

            let wreck_positions: Vec<(i64, DVec3)> = wreck_ball_ids
                .iter()
                .enumerate()
                .map(|(idx, &wid)| {
                    let ring = (idx / 10) as f64;
                    let ang = ((idx % 10) as f64) * std::f64::consts::TAU / 10.0;
                    let wpos = pos
                        + DVec3::new(
                            ang.cos() * (5_000.0 + ring * 2_500.0),
                            (idx as f64 - 15.0) * 150.0,
                            ang.sin() * (5_000.0 + ring * 2_500.0),
                        );
                    (wid, wpos)
                })
                .collect();

            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                for &(wid, wpos) in &wreck_positions {
                    insert_scene_item_in_snap(
                        snap,
                        InventoryItemRecord {
                            item_id: wid,
                            type_id: HOMEFRONT_SALVAGE_WRECK_TYPE_ID,
                            owner_id: HOMEFRONT_SALVAGE_NPC_OWNER_ID,
                            location_id: sys_id as i64,
                            flag: InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    snap.item_positions.insert(wid, wpos.to_array());
                    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                        snap, wid, now, true,
                    );
                }
                Ok(())
            });

            for (wid, wpos) in wreck_positions {
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: wid,
                        type_id: HOMEFRONT_SALVAGE_WRECK_TYPE_ID,
                        position: wpos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 10_000.0,
                        radius: 60.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        structure_hp: 1_500.0,
                        max_structure_hp: 1_500.0,
                        ..CombatState::default()
                    },
                );
                spawned.push(wid);
            }
        }
        HomefrontProgressRecord::StabilizeRift { array_ball_ids, .. } => {
            let arr_tid = bp.support_entity_type_id.unwrap_or(81_402);
            for (idx, &arr_id) in array_ball_ids.iter().enumerate() {
                let ang = (idx as f64) * std::f64::consts::TAU / 5.0;
                let a_pos = pos + DVec3::new(ang.cos() * 8_000.0, 0.0, ang.sin() * 8_000.0);
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: arr_id,
                        type_id: arr_tid,
                        position: a_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 10_000_000.0,
                        radius: 350.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 150_000.0,
                        max_shield_hp: 150_000.0,
                        armor_hp: 150_000.0,
                        max_armor_hp: 150_000.0,
                        structure_hp: 150_000.0,
                        max_structure_hp: 150_000.0,
                        capacitor: 4_000.0,
                        max_capacitor: 4_000.0,
                        capacitor_recharge_time_ms: 120_000.0,
                        is_tethered: false,
                        ..CombatState::default()
                    },
                );
                spawned.push(arr_id);
            }
        }
        HomefrontProgressRecord::TrafficStop {
            station_ball_id,
            active_hauler_ball_ids,
            smuggler_ball_ids,
            civilian_ball_ids,
            ..
        } => {
            let st_tid = bp.objective_structure_type_id.unwrap_or(77_304);
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: *station_ball_id,
                    type_id: st_tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 80_000_000.0,
                    radius: 800.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_tethered: true,
                    shield_hp: 250_000.0,
                    max_shield_hp: 250_000.0,
                    armor_hp: 250_000.0,
                    max_armor_hp: 250_000.0,
                    structure_hp: 250_000.0,
                    max_structure_hp: 250_000.0,
                    ..CombatState::default()
                },
            );
            spawned.push(*station_ball_id);

            let hauler_tid = 649_u32; // Industrial hauler
            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                for idx in 0..6 {
                    snap.next_entity_id += 1;
                    let hid = snap.next_entity_id;
                    active_hauler_ball_ids.push(hid);
                    smuggler_ball_ids.push(hid);
                    snap.next_entity_id += 1;
                    let cargo_id = snap.next_entity_id;
                    insert_scene_item_in_snap(
                        snap,
                        InventoryItemRecord {
                            item_id: cargo_id,
                            type_id: HOMEFRONT_TRAFFIC_CONTRABAND_TYPE_ID,
                            owner_id: i64::from(bp.hostile_faction_id),
                            location_id: hid,
                            flag: InventoryFlag::Cargo,
                            quantity: 1,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    let _ = idx;
                }
                for _ in 0..3 {
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    active_hauler_ball_ids.push(cid);
                    civilian_ball_ids.push(cid);
                }
                Ok(())
            });
            for (idx, &hid) in active_hauler_ball_ids.iter().enumerate() {
                let h_pos = pos + DVec3::new((idx as f64 - 4.0) * 2_500.0, 2_000.0, -3_000.0);
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: hid,
                        type_id: hauler_tid,
                        position: h_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 140.0,
                        inertia: 0.9,
                        mass: 12_000_000.0,
                        radius: 220.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 10_000.0,
                        max_shield_hp: 10_000.0,
                        armor_hp: 8_000.0,
                        max_armor_hp: 8_000.0,
                        structure_hp: 8_000.0,
                        max_structure_hp: 8_000.0,
                        is_tethered: false,
                        ..CombatState::default()
                    },
                );
                spawned.push(hid);
            }
        }
    }

    rec.pocket_spawned = true;
    rec.spawned_ball_ids = spawned;
    let progress_snapshot = rec.progress.clone();
    let spawned_snapshot = rec.spawned_ball_ids.clone();
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        if let Some(live) = snap.dungeon_runtime.homefront_runtimes.get_mut(&instance_id) {
            live.pocket_spawned = true;
            live.spawned_ball_ids = spawned_snapshot;
            live.progress = progress_snapshot;
        }
        Ok(())
    });
}

/// Validates acceleration-gate ship restrictions, proximity in space, and movement
/// guards; spawns the inner pocket behind the gate and admits the pilot.
pub async fn activate_homefront_acceleration_gate(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontShipEligibility, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(hf_err(
            "ShipDocked",
            "Cannot activate Homefront acceleration gate while docked",
        ));
    }

    let (sys_id, ship_id, ship_type_id, gate_ball_id, pocket_pos, dungeon_id, char_sys_opt) =
        ctx.db.with_snapshot(|snap| -> Result<_, BridgeRpcError> {
            let sid = session
                .ship_id
                .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id))
                .ok_or_else(|| hf_err("NoActiveShip", "Pilot must be in an active ship"))?;
            let stid = snap
                .items
                .get(&sid)
                .map(|i| i.type_id)
                .ok_or_else(|| hf_err("NoActiveShip", "Active ship item not found"))?;
            let char_sys = snap
                .characters
                .get(&char_id)
                .map(|c| c.solar_system_id)
                .filter(|&id| id > 0);
            let rec = snap
                .dungeon_runtime
                .homefront_runtimes
                .get(&instance_id)
                .ok_or_else(|| {
                    hf_err(
                        "HomefrontSiteNotFound",
                        format!("Unknown Homefront site instance {instance_id}"),
                    )
                })?;
            Ok((
                rec.solar_system_id,
                sid,
                stid,
                rec.outer_gate_ball_id,
                DVec3::from_array(rec.pocket_position),
                rec.blueprint.dungeon_id,
                char_sys,
            ))
        })?;

    let sess_sys = session
        .solar_system_id
        .or(char_sys_opt)
        .ok_or_else(|| hf_err("NotInSpace", "Action requires an active solar system"))?;
    if sess_sys != sys_id {
        return Err(hf_err(
            "WrongSolarSystem",
            format!("Site {instance_id} is in solar system {sys_id}, not {sess_sys}"),
        ));
    }

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let (_, sb) = sys_h
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            hf_err(
                "ShipNotInSpace",
                format!("Ship {ship_id} is not present in solar system {sys_id}"),
            )
        })?;
    let (_, gb) = sys_h
        .get_ball_snapshot(gate_ball_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            hf_err(
                "GateNotFound",
                format!("Homefront acceleration gate {gate_ball_id} is not present in solar system {sys_id}"),
            )
        })?;
    ctx.validate_ship_movement(
        char_id,
        ship_id,
        MovementIntent::AccelerationGate {
            sys_id,
            kind: AccelerationGateKind::Dungeon,
        },
    )
    .await?;
    let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
    if surface_dist > HOMEFRONT_GATE_ACTIVATION_RANGE_M {
        return Err(hf_err(
            "OutsideGateActivationRange",
            format!(
                "Ship is {surface_dist:.1} m from Homefront acceleration gate {gate_ball_id} (must be <= {HOMEFRONT_GATE_ACTIVATION_RANGE_M:.0} m)"
            ),
        ));
    }

    let now = ctx.now_unix();
    let elig = ctx.db.try_with_mut_snapshot(|snap| -> Result<HomefrontShipEligibility, BridgeRpcError> {
        let site_done = resolve_site_record_completed_in_snap(snap, instance_id);
        let rec = resolve_runtime_record_in_snap(snap, &ctx.sde, instance_id).ok_or_else(|| {
            hf_err(
                "HomefrontSiteNotFound",
                format!("Unknown Homefront site instance {instance_id}"),
            )
        })?;
        if rec.completed || site_done {
            return Err(hf_err(
                "SiteAlreadyCompleted",
                format!("Homefront operation {instance_id} is already completed"),
            ));
        }
        if rec.failed {
            return Err(failed_site_err(rec));
        }
        let arch = kind_to_arch(rec.blueprint.archetype);
        let elig = validate_homefront_ship_eligibility(&ctx.sde, ship_type_id, arch)?;
        if !rec.started {
            rec.started = true;
            rec.started_at_sec = now;
            rec.last_tick_sec = now;
            rec.expires_at_sec = now.saturating_add(rec.blueprint.time_limit_secs);
        }
        rec.entered_pilots.insert(char_id, elig_to_record(&elig));
        if !rec.contributions.contains_key(&char_id) {
            rec.next_entry_sequence = rec.next_entry_sequence.saturating_add(1);
            rec.contributions.insert(
                char_id,
                HomefrontContributionRecord {
                    character_id: char_id,
                    entry_sequence: rec.next_entry_sequence,
                    ..Default::default()
                },
            );
        }
        Ok(elig)
    })?;

    spawn_pocket_entities_sync(ctx, instance_id);

    let idx_offset = (char_id.unsigned_abs() % 7) as f64 * 300.0;
    let dest = pocket_pos + DVec3::new(idx_offset, HOMEFRONT_POCKET_LANDING_Y_M, 0.0);
    let _ = sys_h.set_ball_position(ship_id, dest).await;
    let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        snap.item_positions.insert(ship_id, dest.to_array());
        Ok(())
    });
    let _ = dungeon_id;

    Ok(elig)
}

/// Handles `beyonce.ActivateAccelerationGate(gateID)` when targeting a Homefront outer gate.
pub async fn activate_homefront_gate_by_ball_id(
    ctx: &ServerContext,
    session: &mut SessionState,
    sys_id: SolarSystemId,
    gate_ball_id: i64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let inst_id_opt = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .find(|r| {
                r.solar_system_id == sys_id
                    && r.outer_gate_ball_id == gate_ball_id
                    && !r.completed
                    && !r.failed
            })
            .map(|r| r.instance_id)
    });
    let instance_id = inst_id_opt?;
    let ship_id = match session.ship_id {
        Some(sid) => sid,
        None => return Some(Err(hf_err("NoActiveShip", "Pilot has no active ship"))),
    };
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    if sys_h.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
        return Some(Err(hf_err("ShipNotInSpace", "Active ship is not in space")));
    }
    Some(
        activate_homefront_acceleration_gate(ctx, session, instance_id)
            .await
            .map(|elig| {
                PyRep::keyval([
                    ("instanceID", PyRep::Integer(instance_id)),
                    ("gateBallID", PyRep::Integer(gate_ball_id)),
                    ("hullClass", PyRep::String(elig.hull_class.to_string())),
                ])
            }),
    )
}

pub fn get_homefront_runtime_state(
    ctx: &ServerContext,
    instance_id: i64,
) -> Option<HomefrontRuntimeState> {
    ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .get(&instance_id)
            .map(record_to_runtime_state)
    })
}

pub fn get_homefront_runtime_record(
    ctx: &ServerContext,
    instance_id: i64,
) -> Option<HomefrontRuntimeRecord> {
    ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime.homefront_runtimes.get(&instance_id).cloned()
    })
}

fn failed_site_err(rec: &HomefrontRuntimeRecord) -> BridgeRpcError {
    let code = if rec
        .failure_reason
        .as_deref()
        .is_some_and(|r| r.contains("operation timer"))
    {
        "OperationTimedOut"
    } else {
        "OperationFailed"
    };
    hf_err(
        code,
        rec.failure_reason
            .clone()
            .unwrap_or_else(|| "Homefront operation failed".into()),
    )
}

fn verify_active_pilot_and_timeout_in_snap<'a>(
    snap: &'a mut WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    instance_id: i64,
    now: u64,
) -> Result<&'a mut HomefrontRuntimeRecord, Result<HomefrontRuntimeState, BridgeRpcError>> {
    let site_done = resolve_site_record_completed_in_snap(snap, instance_id);
    let Some(rec) = resolve_runtime_record_in_snap(snap, sde, instance_id) else {
        return Err(Err(hf_err(
            "HomefrontSiteNotFound",
            format!("Unknown Homefront site instance {instance_id}"),
        )));
    };
    if rec.completed || site_done {
        rec.completed = true;
        return Err(Err(hf_err(
            "SiteAlreadyCompleted",
            format!("Homefront operation {instance_id} is already completed"),
        )));
    }
    if rec.failed {
        return Err(Err(failed_site_err(rec)));
    }
    if !rec.started || !rec.entered_pilots.contains_key(&char_id) {
        return Err(Err(hf_err(
            "GateNotEntered",
            "Pilot must pass the Homefront acceleration gate first",
        )));
    }
    if rec.expires_at_sec > 0 && now >= rec.expires_at_sec {
        rec.failed = true;
        rec.failure_reason = Some(format!(
            "Homefront {} exceeded {}-second operation timer",
            rec.blueprint.dungeon_name, rec.blueprint.time_limit_secs
        ));
        return Err(Err(hf_err(
            "OperationTimedOut",
            rec.failure_reason.clone().unwrap_or_default(),
        )));
    }
    Ok(rec)
}

fn with_verified_active_pilot<F>(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    f: F,
) -> Result<HomefrontRuntimeState, BridgeRpcError>
where
    F: FnOnce(&mut WorldStateSnapshot, i64, u64) -> Result<HomefrontRuntimeState, BridgeRpcError>,
{
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<Result<HomefrontRuntimeState, BridgeRpcError>, BridgeRpcError> {
            match verify_active_pilot_and_timeout_in_snap(snap, &ctx.sde, char_id, instance_id, now)
            {
                Ok(_) => Ok(f(snap, char_id, now)),
                Err(inner_err) => Ok(inner_err),
            }
        },
    )?;
    res
}

fn ensure_contribution_fields<'a>(
    contributions: &'a mut BTreeMap<i64, HomefrontContributionRecord>,
    next_entry_sequence: &mut u64,
    char_id: i64,
) -> &'a mut HomefrontContributionRecord {
    if !contributions.contains_key(&char_id) {
        *next_entry_sequence = next_entry_sequence.saturating_add(1);
        contributions.insert(
            char_id,
            HomefrontContributionRecord {
                character_id: char_id,
                entry_sequence: *next_entry_sequence,
                ..Default::default()
            },
        );
    }
    contributions.get_mut(&char_id).expect("inserted above")
}

fn ensure_contribution<'a>(
    rec: &'a mut HomefrontRuntimeRecord,
    char_id: i64,
) -> &'a mut HomefrontContributionRecord {
    ensure_contribution_fields(&mut rec.contributions, &mut rec.next_entry_sequence, char_id)
}

fn active_ship_id_for_pilot(snap: &WorldStateSnapshot, session: &SessionState, char_id: i64) -> Option<i64> {
    session
        .ship_id
        .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id))
}

/// Validates pilot has a fitted ONLINE mining module (`HiSlot0..7`, `group_id 54|464|483`)
/// OR a launched Mining Drone (`group_id == 101`) in space.
fn validate_pilot_mining_capability(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    _ship_type_id: TypeId,
) -> Result<(), BridgeRpcError> {
    let has_online_miner_module = ctx.db.with_snapshot(|snap| {
        let Some(ship_id) = active_ship_id_for_pilot(snap, session, char_id) else {
            return false;
        };
        snap.items.values().any(|item| {
            item.location_id == ship_id
                && (27..=34).contains(&(item.flag as u16))
                && snap.module_online_states.get(&item.item_id).copied() != Some(false)
                && ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .is_some_and(|t| matches!(t.group_id, 54 | 464 | 483))
        })
    });
    if has_online_miner_module {
        return Ok(());
    }
    let has_mining_drone = with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .any(|d| d.owner_char_id == char_id && d.group_id == 101)
    });
    if has_mining_drone {
        return Ok(());
    }
    Err(hf_err(
        "MiningModuleRequired",
        "Requires a fitted online Mining Laser / Strip Miner or launched Mining Drone",
    ))
}

/// Computes pilot's salvager module + Salvaging skill + salvage tackle rig bonus.
fn validate_pilot_salvage_capability(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> Result<f64, BridgeRpcError> {
    let (mod_bonus_opt, skill_bonus, rig_bonus) = ctx.db.with_snapshot(|snap| {
        let ship_id = active_ship_id_for_pilot(snap, session, char_id).unwrap_or(0);
        let mod_bonus = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && (11..=34).contains(&(i.flag as u16))
                    && snap.module_online_states.get(&i.item_id).copied() != Some(false)
            })
            .filter_map(|i| ctx.sde.types.get(&i.type_id))
            .filter(|t| t.group_id == 1122 || t.default_attributes.contains_key(&902))
            .filter_map(|t| {
                t.default_attributes
                    .get(&902)
                    .copied()
                    .or_else(|| t.default_attributes.get(&901).copied())
            })
            .reduce(f64::max);

        let sk_map = snap.skills.get(&char_id);
        let lv = sk_map
            .and_then(|m| m.get(&25_863).or_else(|| m.get(&3_438)))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);

        let r_bonus: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(
                        i.flag,
                        InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
                    )
            })
            .filter_map(|i| ctx.sde.types.get(&i.type_id))
            .filter_map(|t| {
                t.default_attributes
                    .get(&901)
                    .copied()
                    .or_else(|| t.default_attributes.get(&902).copied())
            })
            .sum();

        (mod_bonus, 5.0 * lv, r_bonus)
    });

    let drone_bonus = if mod_bonus_opt.is_none() {
        with_combat_ephemeral(ctx, |rt| {
            rt.active_drones_in_space
                .values()
                .any(|d| d.owner_char_id == char_id && d.type_id == 32_787)
        })
        .then_some(3.0)
    } else {
        None
    };

    let base_bonus = mod_bonus_opt.or(drone_bonus).ok_or_else(|| {
        hf_err(
            "SalvagerModuleRequired",
            "Requires a fitted online Salvager module or launched Salvage Drone",
        )
    })?;
    let total_bonus = base_bonus + skill_bonus + rig_bonus;
    if total_bonus < HOMEFRONT_MIN_SALVAGE_BONUS_PCT {
        return Err(hf_err(
            "InsufficientSalvageBonus",
            format!(
                "Salvage Research wrecks require >= {HOMEFRONT_MIN_SALVAGE_BONUS_PCT:.0}% combined salvager/skill/rig access bonus (pilot has {total_bonus:.1}%)"
            ),
        ));
    }
    Ok(total_bonus)
}

fn validate_fitted_online_data_analyzer(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    resolve_fitted_analyzer_virus_stats(ctx, session, char_id, false)?;
    let has_online_data = ctx.db.with_snapshot(|snap| {
        let Some(ship_id) = active_ship_id_for_pilot(snap, session, char_id) else {
            return false;
        };
        snap.items.values().any(|i| {
            i.location_id == ship_id
                && (11..=34).contains(&(i.flag as u16))
                && snap.module_online_states.get(&i.item_id).copied() != Some(false)
                && ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                    let is_relic = t.default_attributes.get(&183) == Some(&13_278.0)
                        || t.default_attributes.contains_key(&1331)
                        || t.name.contains("Relic");
                    (t.group_id == 538 || t.default_attributes.get(&1909).copied().unwrap_or(0.0) > 0.0)
                        && !is_relic
                })
        })
    });
    if !has_online_data {
        return Err(hf_err(
            "AnalyzerModuleOffline",
            "Fitted Data Analyzer must be online",
        ));
    }
    Ok(())
}

/// Records fleet DPS and Logistics contribution for any Homefront archetype.
pub fn record_homefront_combat_and_logistics(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    damage_dealt: f64,
    repair_hp: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("damage_dealt", damage_dealt)?;
    validate_finite_non_negative("repair_hp", repair_hp)?;
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let c = ensure_contribution(rec, char_id);
        c.damage_dealt += damage_dealt;
        c.repair_hp += repair_hp;
        Ok(record_to_runtime_state(rec))
    })
}

/// Raid archetype step: requires tackling the industrial hauler (`applied_tackle == true`); untackled
/// haulers warp off (`TargetEscaped` persisted in DB BEFORE returning `Err`).
pub fn raid_tackle_and_destroy_hauler(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    applied_tackle: bool,
    damage_dealt: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("damage_dealt", damage_dealt)?;
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::Raid {
                haulers_spawned,
                haulers_destroyed,
                haulers_escaped,
                active_hauler_ball_id,
                active_hauler_tackled,
                cargo_dropped,
                required_deliveries,
                ..
            } => {
                if !applied_tackle {
                    *haulers_escaped = haulers_escaped.saturating_add(1);
                    *active_hauler_tackled = false;
                    *haulers_spawned = haulers_spawned.saturating_add(1);
                    *active_hauler_ball_id =
                        Some(1_897_800_000 + instance_id * 100 + 30 + i64::from(*haulers_spawned));
                    return Err(hf_err(
                        "TargetEscaped",
                        "Untackled Raid industrial hauler warped out without dropping cargo",
                    ));
                }
                *active_hauler_tackled = true;
                *haulers_destroyed = haulers_destroyed.saturating_add(1);
                *cargo_dropped = cargo_dropped.saturating_add(1);
                if *haulers_destroyed < *required_deliveries {
                    *haulers_spawned = haulers_spawned.saturating_add(1);
                    *active_hauler_ball_id =
                        Some(1_897_800_000 + instance_id * 100 + 30 + i64::from(*haulers_spawned));
                    *active_hauler_tackled = false;
                } else {
                    *active_hauler_ball_id = None;
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Hauler tackle/kill applies only to Raid operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.damage_dealt += damage_dealt;
        c.objective_actions += 1;
        Ok(record_to_runtime_state(rec))
    })
}

/// Raid archetype cargo delivery step: enforces ship cargo capacity `>= 3,500 m3`.
pub fn raid_deliver_cargo_to_freighter(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let ship_id = active_ship_id_for_pilot(snap, session, char_id);
        let ship_tid = ship_id
            .and_then(|sid| snap.items.get(&sid))
            .map(|i| i.type_id)
            .ok_or_else(|| {
                hf_err("NoActiveShip", "Pilot must be piloting a ship to deliver Raid cargo")
            })?;
        let ship_type = ctx.sde.types.get(&ship_tid).ok_or_else(|| {
            hf_err("UnknownShipType", format!("Unknown ship type {ship_tid}"))
        })?;
        let fleet_hangar = ship_type.default_attributes.get(&912).copied().unwrap_or(0.0);
        let general_fleet = ship_type.default_attributes.get(&1559).copied().unwrap_or(0.0);
        let eff_cargo = ship_type.capacity.max(fleet_hangar).max(general_fleet);
        if eff_cargo < HOMEFRONT_RAID_CARGO_VOLUME_M3 {
            return Err(hf_err(
                "CargoCapacityTooSmall",
                format!(
                    "{} has {eff_cargo:.0} m3 hold (< {HOMEFRONT_RAID_CARGO_VOLUME_M3:.0} m3 Raid crate); bring a T1 Hauler/Squall",
                    ship_type.name
                ),
            ));
        }

        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::Raid {
                cargo_dropped,
                cargo_delivered,
                required_deliveries,
                ..
            } => {
                if *cargo_delivered >= *cargo_dropped {
                    return Err(hf_err(
                        "NoCargoAvailable",
                        "No dropped Raid hauler cargo crate available to deliver",
                    ));
                }
                if *cargo_delivered < *required_deliveries {
                    *cargo_delivered = cargo_delivered.saturating_add(1);
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Cargo delivery applies only to Raid operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.cargo_delivered += 1;
        Ok(record_to_runtime_state(rec))
    })
}

/// Emergency Aid archetype defense step (`repair_hp` vs `incoming_hostile_damage` over `elapsed_secs`).
pub fn emergency_aid_advance_defense(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    repair_hp: f64,
    incoming_hostile_damage: f64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("repair_hp", repair_hp)?;
    validate_finite_non_negative("incoming_hostile_damage", incoming_hostile_damage)?;
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::EmergencyAid {
                protectee_hp,
                protectee_max_hp,
                survival_elapsed_secs,
                required_survival_secs,
                waves_repelled,
                ..
            } => {
                let next_hp =
                    (*protectee_hp - incoming_hostile_damage + repair_hp).clamp(0.0, *protectee_max_hp);
                *protectee_hp = next_hp;
                if next_hp <= 0.0 {
                    rec.failed = true;
                    rec.failure_reason =
                        Some("Emergency Aid allied vessel was destroyed".into());
                    return Err(hf_err(
                        "OperationFailed",
                        "Emergency Aid allied vessel was destroyed",
                    ));
                }
                *survival_elapsed_secs =
                    survival_elapsed_secs.saturating_add(elapsed_secs).min(*required_survival_secs);
                let wave_stage = (*survival_elapsed_secs / 200).min(3) as u8;
                *waves_repelled = (*waves_repelled).max(wave_stage);
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Protectee defense applies only to Emergency Aid operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.repair_hp += repair_hp;
        c.objective_actions += 1;
        Ok(record_to_runtime_state(rec))
    })
}

/// Metaliminal Meteoroid mining step: requires fitted online mining laser or launched mining drone.
pub fn metaliminal_mine_mutanite(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    yield_units: u32,
    residue_units: u32,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_tid = ctx
        .db
        .with_snapshot(|snap| {
            active_ship_id_for_pilot(snap, session, char_id)
                .and_then(|sid| snap.items.get(&sid))
                .map(|i| i.type_id)
        })
        .ok_or_else(|| hf_err("NoActiveShip", "Pilot must be piloting a ship"))?;
    validate_pilot_mining_capability(ctx, session, char_id, ship_tid)?;

    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let mined_actual;
        match &mut rec.progress {
            HomefrontProgressRecord::MetaliminalMeteoroid {
                remaining_units,
                total_residue_units,
                ore_mined_by_fleet,
                ..
            } => {
                mined_actual = yield_units.min(*remaining_units);
                *remaining_units -= mined_actual;
                let res_actual = residue_units.min(*remaining_units);
                *remaining_units -= res_actual;
                *ore_mined_by_fleet += mined_actual;
                *total_residue_units += res_actual;
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Mutanite asteroid mining applies only to Metaliminal Meteoroid operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.ore_mined_units += mined_actual;
        Ok(record_to_runtime_state(rec))
    })
}

/// Dread Assault capacitor transfer step (capped by Siege lockout when already sieged).
pub fn dread_assault_transfer_capacitor(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    cap_transferred_gj: f64,
    hostile_neut_drain_gj: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("cap_transferred_gj", cap_transferred_gj)?;
    validate_finite_non_negative("hostile_neut_drain_gj", hostile_neut_drain_gj)?;
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::DreadAssault {
                capacitor_gj,
                capacitor_target_gj,
                in_siege_mode,
                siege_remaining_secs,
                ..
            } => {
                if *in_siege_mode {
                    return Err(hf_err(
                        "DreadInSiegeMode",
                        "Allied Dreadnought cannot receive remote capacitor while Siege is active",
                    ));
                }
                *capacitor_gj = (*capacitor_gj + cap_transferred_gj - hostile_neut_drain_gj)
                    .clamp(0.0, *capacitor_target_gj);
                if *capacitor_gj >= *capacitor_target_gj {
                    *in_siege_mode = true;
                    *siege_remaining_secs = 90;
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Dreadnought capacitor transfer applies only to Dread Assault operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.cap_transferred += cap_transferred_gj;
        c.objective_actions += 1;
        Ok(record_to_runtime_state(rec))
    })
}

/// Dread Assault Siege volley progression step (`90s` Siege cycle destroys 1/3 of enemy base HP).
pub fn dread_assault_advance_siege(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    with_verified_active_pilot(ctx, session, instance_id, |snap, _, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::DreadAssault {
                capacitor_gj,
                siege_cycles_completed,
                target_siege_cycles,
                in_siege_mode,
                siege_remaining_secs,
                enemy_base_hp,
                enemy_base_max_hp,
                ..
            } => {
                if !*in_siege_mode {
                    return Err(hf_err(
                        "DreadNotInSiegeMode",
                        "Transfer capacitor to target before Dreadnought enters Siege cycle",
                    ));
                }
                if elapsed_secs >= *siege_remaining_secs {
                    *siege_remaining_secs = 0;
                    *in_siege_mode = false;
                    *capacitor_gj = 0.0;
                    *siege_cycles_completed = siege_cycles_completed.saturating_add(1);
                    let chunk = *enemy_base_max_hp / f64::from((*target_siege_cycles).max(1));
                    *enemy_base_hp = (*enemy_base_hp - chunk).max(0.0);
                } else {
                    *siege_remaining_secs -= elapsed_secs;
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Siege advancement applies only to Dread Assault operations",
                ));
            }
        }
        Ok(record_to_runtime_state(rec))
    })
}

fn aar_wave_duration_secs(wave: u8) -> u64 {
    match wave {
        1..=3 => 180,
        4..=6 => 150,
        _ => 120,
    }
}

/// Abyssal Artifact Recovery wave progression: clears Mutanite (`12 x 27 = 324` units/wave); clears
/// the warp bubble immediately when `mutanite_units_remaining == 0` regardless of hostiles.
pub async fn abyssal_clear_wave(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    mined_units: u32,
    hostile_damage: f64,
    recover_cache: bool,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("hostile_damage", hostile_damage)?;
    let (_, char_id) = ServerContext::require_character(session)?;
    if mined_units > 0 {
        let ship_tid = ctx
            .db
            .with_snapshot(|snap| {
                active_ship_id_for_pilot(snap, session, char_id)
                    .and_then(|sid| snap.items.get(&sid))
                    .map(|i| i.type_id)
            })
            .ok_or_else(|| {
                hf_err(
                    "NoActiveShip",
                    "Pilot must be piloting a ship to mine Admixti Mutanite",
                )
            })?;
        validate_pilot_mining_capability(ctx, session, char_id, ship_tid)?;
    }

    let now = ctx.now_unix();
    let mut disperse_bubble = false;
    let mut respawn_bubble = false;
    let mut sys_id_out = 0;
    let mut site_pos_out = DVec3::ZERO;
    let (state, emitted_wave_payouts) = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<
            Result<
                (
                    HomefrontRuntimeState,
                    Option<Vec<(HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>)>>,
                ),
                BridgeRpcError,
            >,
            BridgeRpcError,
        > {
            let mut cleared_wave_opt: Option<u8> = None;
            {
                let rec = match verify_active_pilot_and_timeout_in_snap(
                    snap,
                    &ctx.sde,
                    char_id,
                    instance_id,
                    now,
                ) {
                    Ok(rec) => rec,
                    Err(Ok(timed_out_state)) => return Ok(Ok((timed_out_state, None))),
                    Err(Err(e)) => return Ok(Err(e)),
                };
                sys_id_out = rec.solar_system_id;
                site_pos_out = DVec3::from_array(rec.pocket_position);
                let c = ensure_contribution(rec, char_id);
                c.ore_mined_units += mined_units;
                c.damage_dealt += hostile_damage;
                if recover_cache {
                    c.objective_actions += 1;
                }
                match &mut rec.progress {
                    HomefrontProgressRecord::AbyssalArtifactRecovery {
                        current_wave,
                        total_waves,
                        mutanite_units_remaining,
                        wave_hostiles_remaining,
                        warp_bubble_active,
                        caches_recovered,
                        wave_time_remaining_secs,
                        evacuation_active,
                        extraction_window_secs,
                        ..
                    } => {
                        *mutanite_units_remaining =
                            mutanite_units_remaining.saturating_sub(mined_units);
                        if *mutanite_units_remaining == 0 && *warp_bubble_active {
                            *warp_bubble_active = false;
                            disperse_bubble = true;
                        }
                        if hostile_damage >= HOMEFRONT_MIN_SKILL_CONTRIBUTION {
                            *wave_hostiles_remaining = 0;
                        }
                        if recover_cache {
                            *caches_recovered += 1;
                        }
                        if *mutanite_units_remaining == 0 && *wave_hostiles_remaining == 0 {
                            cleared_wave_opt = Some(*current_wave);
                            if *current_wave < *total_waves {
                                *current_wave += 1;
                                *mutanite_units_remaining = (HOMEFRONT_AAR_ROCKS_PER_WAVE as u32)
                                    * HOMEFRONT_AAR_UNITS_PER_ROCK;
                                *wave_hostiles_remaining = 3 + *current_wave / 2;
                                *wave_time_remaining_secs = aar_wave_duration_secs(*current_wave);
                                *warp_bubble_active = true;
                                respawn_bubble = true;
                            } else {
                                *evacuation_active = true;
                                *extraction_window_secs = 60;
                            }
                        }
                    }
                    _ => {
                        return Ok(Err(hf_err(
                            "InvalidArchetypeAction",
                            "Abyssal wave progression applies only to Abyssal Artifact Recovery",
                        )));
                    }
                }
            }
            let emitted = cleared_wave_opt
                .and_then(|wave_num| pay_aar_wave_in_snap(snap, instance_id, wave_num));
            let state_out = snap
                .dungeon_runtime
                .homefront_runtimes
                .get(&instance_id)
                .map(record_to_runtime_state)
                .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
            Ok(Ok((state_out, emitted)))
        },
    )??;

    if let Some(emitted) = emitted_wave_payouts {
        emit_payout_notifications(ctx, &emitted);
    }
    if sys_id_out > 0 {
        let sys_h = ctx.systems.get_or_spawn(sys_id_out);
        let bubble_id = 1_897_800_000 + instance_id.saturating_mul(100) + 9;
        if disperse_bubble {
            let _ = sys_h.remove_warp_bubble(bubble_id).await;
        }
        if respawn_bubble {
            let _ = sys_h
                .add_warp_bubble(bubble_id, site_pos_out, 150_000.0)
                .await;
        }
    }
    Ok(state)
}

/// Suspicious Signal array hack: requires fitted online Data Analyzer and sets a `90s`
/// vulnerability window when all 3 arrays are hacked (`is_tethered = false` in `Ballpark`).
pub fn suspicious_signal_hack_array(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    array_index: usize,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    if array_index >= 3 {
        return Err(hf_err(
            "InvalidArrayIndex",
            format!("Suspicious Signal array index must be 0..3, got {array_index}"),
        ));
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    validate_fitted_online_data_analyzer(ctx, session, char_id)?;

    let mut clear_relay_tether: Option<(SolarSystemId, i64)> = None;
    let out = with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let sys_id = rec.solar_system_id;
        match &mut rec.progress {
            HomefrontProgressRecord::SuspiciousSignal {
                relay_ball_id,
                arrays_hacked,
                vulnerability_window_secs,
                ..
            } => {
                arrays_hacked[array_index] = true;
                if arrays_hacked.iter().all(|&h| h) {
                    *vulnerability_window_secs = HOMEFRONT_SIGNAL_VULN_WINDOW_SECS;
                    clear_relay_tether = Some((sys_id, *relay_ball_id));
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Array hacking applies only to Suspicious Signal operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.arrays_hacked += 1;
        Ok(record_to_runtime_state(rec))
    })?;

    if let Some((sys_id, relay_id)) = clear_relay_tether {
        set_homefront_relay_tethered(ctx, sys_id, relay_id, false);
    }
    Ok(out)
}

/// Suspicious Signal relay attack: rejects damage while `vulnerability_window_secs == 0`, re-arms
/// all 3 arrays and restores `is_tethered = true` if the 90s window expires before relay death.
pub fn suspicious_signal_attack_relay(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    damage: f64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("damage", damage)?;
    let mut retether_relay: Option<(SolarSystemId, i64)> = None;
    let out = with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let sys_id = rec.solar_system_id;
        match &mut rec.progress {
            HomefrontProgressRecord::SuspiciousSignal {
                relay_ball_id,
                arrays_hacked,
                vulnerability_window_secs,
                relay_hp,
                ..
            } => {
                if !arrays_hacked.iter().all(|&h| h) || *vulnerability_window_secs == 0 {
                    return Err(hf_err(
                        "StructureInvulnerable",
                        "Relay structure is invulnerable until all 3 Data Arrays are hacked",
                    ));
                }
                *relay_hp = (*relay_hp - damage).max(0.0);
                if *relay_hp > 0.0 && elapsed_secs >= *vulnerability_window_secs {
                    *vulnerability_window_secs = 0;
                    *arrays_hacked = [false; 3];
                    retether_relay = Some((sys_id, *relay_ball_id));
                } else {
                    *vulnerability_window_secs =
                        vulnerability_window_secs.saturating_sub(elapsed_secs);
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Relay attack applies only to Suspicious Signal operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.damage_dealt += damage;
        Ok(record_to_runtime_state(rec))
    })?;

    if let Some((sys_id, relay_id)) = retether_relay {
        set_homefront_relay_tethered(ctx, sys_id, relay_id, true);
    }
    Ok(out)
}

/// Salvage Research processing step: enforces online Salvager/Drone AND `>= 20%` combined access bonus.
pub fn salvage_research_process_wreck(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    wrecks_count: u8,
    activate_werpost: bool,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if wrecks_count > 0 {
        validate_pilot_salvage_capability(ctx, session, char_id)?;
    }
    if activate_werpost {
        validate_fitted_online_data_analyzer(ctx, session, char_id)?;
    }
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let delta;
        match &mut rec.progress {
            HomefrontProgressRecord::SalvageResearch {
                wreck_ball_ids,
                wrecks_salvaged,
                total_wrecks,
                werpost_activated,
                ..
            } => {
                delta = wrecks_count.min(total_wrecks.saturating_sub(*wrecks_salvaged));
                *wrecks_salvaged = wrecks_salvaged.saturating_add(delta);
                let drop_n = usize::from(delta).min(wreck_ball_ids.len());
                wreck_ball_ids.drain(0..drop_n);
                if activate_werpost {
                    *werpost_activated = true;
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Wreck salvage applies only to Salvage Research operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.objective_actions += u32::from(delta);
        if activate_werpost {
            c.arrays_hacked += 1;
        }
        Ok(record_to_runtime_state(rec))
    })
}

/// Stabilize Rift neutralizer step: requires all 5 Destabilizing Arrays `< 20%` capacitor held continuously.
pub fn stabilize_rift_neutralize_arrays(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    array_cap_drain: [f64; 5],
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    for (i, &v) in array_cap_drain.iter().enumerate() {
        validate_finite_unit_fraction(&format!("array_cap_drain[{i}]"), v)?;
    }
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        let mut drained_any = false;
        match &mut rec.progress {
            HomefrontProgressRecord::StabilizeRift {
                arrays_cap_pct,
                held_low_secs,
                required_hold_secs,
                ..
            } => {
                for (pct, drain) in arrays_cap_pct.iter_mut().zip(array_cap_drain.iter()) {
                    if *drain > 0.0 {
                        drained_any = true;
                    }
                    *pct = (*pct - *drain).clamp(0.0, 1.0);
                }
                if arrays_cap_pct.iter().all(|&p| p < 0.20) {
                    *held_low_secs = held_low_secs.saturating_add(elapsed_secs).min(*required_hold_secs);
                } else {
                    *held_low_secs = 0;
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Array neutralization applies only to Stabilize Rift operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        let total_drain_pct: f64 = array_cap_drain.iter().copied().sum();
        c.cap_transferred += total_drain_pct * 500.0;
        if drained_any {
            c.objective_actions += 1;
        }
        Ok(record_to_runtime_state(rec))
    })
}

/// Traffic Stop intercept step: persists collateral failure if `civilians_harmed >= 3`.
pub fn traffic_stop_scan_and_intercept(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    smugglers_caught: u8,
    civilians_attacked: u8,
    damage_dealt: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    validate_finite_non_negative("damage_dealt", damage_dealt)?;
    with_verified_active_pilot(ctx, session, instance_id, |snap, char_id, _| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&instance_id)
            .ok_or_else(|| hf_err("HomefrontSiteNotFound", "Site missing"))?;
        match &mut rec.progress {
            HomefrontProgressRecord::TrafficStop {
                smugglers_destroyed,
                required_smugglers,
                civilians_harmed,
                ..
            } => {
                *smugglers_destroyed = smugglers_destroyed
                    .saturating_add(smugglers_caught)
                    .min(*required_smugglers);
                *civilians_harmed = civilians_harmed.saturating_add(civilians_attacked);
                if *civilians_harmed >= 3 {
                    rec.failed = true;
                    rec.failure_reason = Some(
                        "Traffic Stop failed due to excessive civilian collateral damage".into(),
                    );
                    return Err(hf_err(
                        "OperationFailed",
                        "Traffic Stop failed: >= 3 civilian haulers harmed",
                    ));
                }
            }
            _ => {
                return Err(hf_err(
                    "InvalidArchetypeAction",
                    "Convoy interception applies only to Traffic Stop operations",
                ));
            }
        }
        let c = ensure_contribution(rec, char_id);
        c.damage_dealt += damage_dealt;
        c.objective_actions += u32::from(smugglers_caught);
        Ok(record_to_runtime_state(rec))
    })
}

fn despawn_homefront_balls_db(
    systems: &SystemRegistry,
    sys_id: SolarSystemId,
    instance_id: i64,
    balls: &[i64],
) {
    let sys_h = systems.get_or_spawn(sys_id);
    for bid in balls {
        sys_h.try_remove_ball(*bid);
    }
    sys_h.try_remove_warp_bubble(1_897_800_000 + instance_id.saturating_mul(100) + 9);
}

fn despawn_homefront_balls_sync(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    balls: &[i64],
) {
    with_mut_combat_ephemeral(ctx, |rt| {
        for bid in balls {
            rt.npc_entities.remove(bid);
        }
    });
    despawn_homefront_balls_db(&ctx.systems, sys_id, instance_id, balls);
}

fn emit_payout_notifications(
    ctx: &ServerContext,
    payouts: &[(HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>)],
) {
    for (payout, new_balance, lp_change_opt) in payouts {
        if !payout.eligible {
            continue;
        }
        if payout.isk_cents.raw_cents() > 0 {
            ctx.emit_typed(
                NotificationTarget::Character(payout.character_id),
                OnAccountChange {
                    account_key: "cash".into(),
                    owner_id: payout.character_id,
                    balance: new_balance.to_wire_f64(),
                },
            );
        }
        if let Some((issuer_corp_id, lp_before, lp_after)) = *lp_change_opt {
            ctx.emit_typed(
                NotificationTarget::Character(payout.character_id),
                OnLPChange {
                    change_type: "homefront_payout".into(),
                    issuer_corp_id,
                    lp_before,
                    lp_after,
                },
            );
        }
    }
}

fn credit_payouts_in_snap(
    snap: &mut WorldStateSnapshot,
    dname: &str,
    issuer_corp_id: i64,
    summary: &HomefrontFleetPayoutSummary,
    ratio_num: usize,
    ratio_den: usize,
) -> Vec<(HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>)> {
    let mut emitted = Vec::new();
    for payout in &summary.payouts {
        if !payout.eligible {
            continue;
        }
        let credit_cents = if ratio_num == ratio_den || ratio_den == 0 {
            payout.isk_cents
        } else {
            let scaled = (payout.isk_cents.raw_cents() as i128) * (ratio_num as i128)
                / (ratio_den as i128);
            IskCents::from_raw_cents(scaled as i64)
        };
        let mut new_bal = IskCents::ZERO;
        let mut lp_delta = None;
        let mut append_journal_entry = false;
        if let Some(ch) = snap.characters.get_mut(&payout.character_id) {
            if credit_cents.raw_cents() > 0 {
                ch.balance = ch.balance.checked_add(credit_cents).unwrap_or(ch.balance);
                new_bal = ch.balance;
                append_journal_entry = true;
            }
            if payout.lp > 0 {
                let before = ch.loyalty_points.get(&issuer_corp_id).copied().unwrap_or(0);
                let after = before.saturating_add(payout.lp);
                ch.loyalty_points.insert(issuer_corp_id, after);
                lp_delta = Some((issuer_corp_id, before, after));
            }
        }
        if append_journal_entry {
            snap.append_journal(
                payout.character_id,
                "CorporateRewardPayout",
                credit_cents,
                new_bal,
                format!("Homefront Operation Payout ({dname})"),
            );
        }
        let mut adjusted = payout.clone();
        adjusted.isk_cents = credit_cents;
        adjusted.isk = credit_cents.raw_cents() / 100;
        emitted.push((adjusted, new_bal, lp_delta));
    }
    emitted
}

type SettledPayoutRow = (HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>);

fn settle_homefront_completion_atomic_db(
    db: &WorldRepository,
    sde: &SdeRegistry,
    systems: &SystemRegistry,
    instance_id: i64,
    require_pilot_entered: Option<i64>,
) -> Result<(HomefrontFleetPayoutSummary, SolarSystemId, Vec<i64>, Vec<SettledPayoutRow>), BridgeRpcError> {
    let settled = db.try_with_mut_snapshot(
        |snap| -> Result<
            Result<
                (HomefrontFleetPayoutSummary, SolarSystemId, Vec<i64>, Vec<SettledPayoutRow>),
                BridgeRpcError,
            >,
            BridgeRpcError,
        > {
            let site_done = resolve_site_record_completed_in_snap(snap, instance_id);
            let (site_sys, base_isk, base_lp, opt_fleet, raw_contribs) = {
                let Some(rec) = resolve_runtime_record_in_snap(snap, sde, instance_id) else {
                    return Ok(Err(hf_err(
                        "HomefrontSiteNotFound",
                        format!("Unknown Homefront site instance {instance_id}"),
                    )));
                };
                if rec.completed || site_done {
                    rec.completed = true;
                    return Ok(Err(hf_err(
                        "SiteAlreadyCompleted",
                        format!("Homefront operation {instance_id} is already completed"),
                    )));
                }
                if rec.failed {
                    return Ok(Err(failed_site_err(rec)));
                }
                if let Some(cid) = require_pilot_entered {
                    if !rec.started || !rec.entered_pilots.contains_key(&cid) {
                        return Ok(Err(hf_err(
                            "GateNotEntered",
                            "Pilot must pass the Homefront acceleration gate first",
                        )));
                    }
                }
                if !runtime_record_objective_complete(rec) {
                    return Ok(Err(hf_err(
                        "ObjectivesIncomplete",
                        format!(
                            "Cannot complete Homefront {:?} ({instance_id}): role objectives not yet satisfied",
                            rec.blueprint.archetype
                        ),
                    )));
                }
                (
                    rec.solar_system_id,
                    rec.blueprint.base_pilot_isk,
                    rec.blueprint.base_pilot_lp,
                    rec.blueprint.optimal_fleet_size,
                    rec.contributions
                        .values()
                        .map(contrib_to_public)
                        .collect::<Vec<_>>(),
                )
            };

            let contributors: Vec<HomefrontParticipantContribution> = raw_contribs
                .into_iter()
                .filter(|c| {
                    snap.characters.get(&c.character_id).is_none_or(|ch| {
                        ch.solar_system_id == site_sys
                            && ch.station_id.is_none()
                            && ch.structure_id.is_none()
                    })
                })
                .collect();
            let summary = compute_homefront_fleet_payouts(
                base_isk,
                base_lp,
                opt_fleet,
                &contributors,
            );
            if summary.eligible_pilot_count == 0 {
                return Ok(Err(hf_err(
                    "NoEligibleContributors",
                    "No pilot met the minimum contribution threshold for Homefront payout",
                )));
            }

            let rec = snap
                .dungeon_runtime
                .homefront_runtimes
                .get_mut(&instance_id)
                .ok_or_else(|| {
                    hf_err(
                        "HomefrontSiteNotFound",
                        format!("Unknown Homefront site instance {instance_id}"),
                    )
                })?;

            // Compute unpaid share delta for Abyssal Artifact Recovery waves already paid live
            let (unpaid_num, unpaid_den) = match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    total_waves,
                    waves_paid,
                    waves_paid_mask,
                    ..
                } if (*waves_paid_mask != 0 || *waves_paid > 0)
                    && !rec.blueprint.abyssal_wave_payouts_isk.is_empty() =>
                {
                    let total_isk: i64 = rec.blueprint.abyssal_wave_payouts_isk.iter().sum();
                    let paid_isk: i64 = if *waves_paid_mask != 0 {
                        rec.blueprint
                            .abyssal_wave_payouts_isk
                            .iter()
                            .take(usize::from(*total_waves))
                            .enumerate()
                            .filter(|&(idx, _)| (*waves_paid_mask & (1_u16 << idx)) != 0)
                            .map(|(_, &isk)| isk)
                            .sum()
                    } else {
                        rec.blueprint
                            .abyssal_wave_payouts_isk
                            .iter()
                            .take(usize::from((*waves_paid).min(*total_waves)))
                            .sum()
                    };
                    let rem = total_isk.saturating_sub(paid_isk);
                    (
                        usize::try_from(rem.max(0)).unwrap_or(0),
                        usize::try_from(total_isk.max(1)).unwrap_or(1),
                    )
                }
                _ => (1, 1),
            };

            rec.completed = true;
            let sys_id = rec.solar_system_id;
            let balls = rec.spawned_ball_ids.clone();
            let dname = rec.blueprint.dungeon_name.clone();
            let issuer_corp = i64::from(rec.blueprint.sovereign_faction_id);
            let rep_per_pilot = summary
                .payouts
                .iter()
                .find(|p| p.eligible)
                .map(|p| p.isk)
                .unwrap_or(0);

            if let Some(list) = snap.pve_special_sites.homefront_sites.get_mut(&sys_id) {
                if let Some(srec) = list.iter_mut().find(|s| s.instance_id == instance_id) {
                    srec.completed = true;
                    srec.reward_paid_isk = rep_per_pilot;
                }
            }

            let emitted =
                credit_payouts_in_snap(snap, &dname, issuer_corp, &summary, unpaid_num, unpaid_den);
            Ok(Ok((summary, sys_id, balls, emitted)))
        },
    )??;

    let (summary, sys_id, balls, emitted) = settled;
    despawn_homefront_balls_db(systems, sys_id, instance_id, &balls);
    Ok((summary, sys_id, balls, emitted))
}

fn settle_homefront_completion_atomic_sync(
    ctx: &ServerContext,
    instance_id: i64,
    require_pilot_entered: Option<i64>,
) -> Result<(HomefrontFleetPayoutSummary, SolarSystemId, Vec<i64>), BridgeRpcError> {
    let (summary, sys_id, balls, emitted) = settle_homefront_completion_atomic_db(
        &ctx.db,
        &ctx.sde,
        &ctx.systems,
        instance_id,
        require_pilot_entered,
    )?;
    with_mut_combat_ephemeral(ctx, |rt| {
        for bid in &balls {
            rt.npc_entities.remove(bid);
        }
    });
    emit_payout_notifications(ctx, &emitted);
    Ok((summary, sys_id, balls))
}

/// Verifies all multi-role operation objectives atomically inside one `try_with_mut_snapshot`
/// transaction, pays whole cents + emits `OnAccountChange`/`OnLPChange`, and despawns pocket entities.
pub async fn complete_homefront_operation_and_pay(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontFleetPayoutSummary, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (summary, _, _) = settle_homefront_completion_atomic_sync(ctx, instance_id, Some(char_id))?;
    Ok(summary)
}

fn pay_aar_wave_in_snap(
    snap: &mut WorldStateSnapshot,
    instance_id: i64,
    wave_num: u8,
) -> Option<Vec<(HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>)>> {
    if wave_num == 0 || u32::from(wave_num) > u16::BITS {
        return None;
    }
    let rec = snap.dungeon_runtime.homefront_runtimes.get_mut(&instance_id)?;
    if rec.completed || rec.failed {
        return None;
    }
    let wave_idx = usize::from(wave_num - 1);
    let wave_bit = 1_u16 << wave_idx;
    let wave_base_isk = rec
        .blueprint
        .abyssal_wave_payouts_isk
        .get(wave_idx)
        .copied()
        .unwrap_or(0);
    if wave_base_isk <= 0 {
        return None;
    }
    if let HomefrontProgressRecord::AbyssalArtifactRecovery {
        waves_paid_mask,
        waves_paid,
        ..
    } = &rec.progress
    {
        if (*waves_paid_mask & wave_bit) != 0
            || (*waves_paid_mask == 0 && *waves_paid >= wave_num)
        {
            return None;
        }
    } else {
        return None;
    }

    let site_sys = rec.solar_system_id;
    let contributors: Vec<HomefrontParticipantContribution> = rec
        .contributions
        .values()
        .filter(|c| {
            snap.characters.get(&c.character_id).is_none_or(|ch| {
                ch.solar_system_id == site_sys
                    && ch.station_id.is_none()
                    && ch.structure_id.is_none()
            })
        })
        .map(contrib_to_public)
        .collect();
    let summary = compute_homefront_fleet_payouts(
        wave_base_isk,
        0,
        rec.blueprint.optimal_fleet_size,
        &contributors,
    );
    if summary.eligible_pilot_count == 0 {
        return None;
    }
    let dname = format!("{} Wave {}", rec.blueprint.dungeon_name, wave_num);
    let issuer = i64::from(rec.blueprint.sovereign_faction_id);
    if let HomefrontProgressRecord::AbyssalArtifactRecovery {
        waves_paid,
        waves_paid_mask,
        ..
    } = &mut rec.progress
    {
        if *waves_paid_mask == 0 && *waves_paid > 0 {
            *waves_paid_mask = 1_u16
                .checked_shl(u32::from(*waves_paid))
                .unwrap_or(0)
                .wrapping_sub(1);
        }
        *waves_paid_mask |= wave_bit;
        *waves_paid = waves_paid_mask.count_ones() as u8;
    }
    let emitted = credit_payouts_in_snap(snap, &dname, issuer, &summary, 1, 1);
    Some(emitted)
}

fn pay_aar_wave_live_if_needed(ctx: &ServerContext, instance_id: i64, wave_num: u8) {
    let res = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<
            Option<Vec<(HomefrontPilotPayout, IskCents, Option<(i64, i64, i64)>)>>,
            BridgeRpcError,
        > { Ok(pay_aar_wave_in_snap(snap, instance_id, wave_num)) },
    );
    if let Ok(Some(emitted)) = res {
        emit_payout_notifications(ctx, &emitted);
    }
}

fn try_settle_completed_homefront_live(ctx: &ServerContext, instance_id: i64) {
    let _ = settle_homefront_completion_atomic_sync(ctx, instance_id, None);
}

fn spawn_next_aar_wave_db(
    systems: &SystemRegistry,
    db: &WorldRepository,
    sde: &SdeRegistry,
    instance_id: i64,
    next_wave: u8,
) -> Vec<(SolarSystemId, i64, TypeId, DVec3)> {
    let mut allocated_ids: Vec<i64> = Vec::new();
    let info = db.try_with_mut_snapshot(
        |snap| -> Result<Option<(SolarSystemId, DVec3, Vec<TypeId>, Vec<i64>)>, BridgeRpcError> {
            let mut ids = Vec::with_capacity(HOMEFRONT_AAR_ROCKS_PER_WAVE);
            for _ in 0..HOMEFRONT_AAR_ROCKS_PER_WAVE {
                snap.next_entity_id += 1;
                ids.push(snap.next_entity_id);
            }
            let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&instance_id) else {
                return Ok(None);
            };
            let sys_id = rec.solar_system_id;
            let pos = DVec3::from_array(rec.pocket_position);
            let esc = rec.blueprint.escort_wave_type_ids.clone();
            let mut esc_ids = Vec::with_capacity(esc.len());
            for _ in 0..esc.len() {
                snap.next_entity_id += 1;
                esc_ids.push(snap.next_entity_id);
            }
            if let HomefrontProgressRecord::AbyssalArtifactRecovery {
                current_wave,
                rock_ball_ids,
                wave_hostile_ball_ids,
                mutanite_units_remaining,
                wave_hostiles_remaining,
                warp_bubble_active,
                wave_time_remaining_secs,
                ..
            } = &mut rec.progress
            {
                *current_wave = next_wave;
                *rock_ball_ids = ids.clone();
                *wave_hostile_ball_ids = esc_ids.clone();
                *mutanite_units_remaining =
                    (HOMEFRONT_AAR_ROCKS_PER_WAVE as u32) * HOMEFRONT_AAR_UNITS_PER_ROCK;
                *wave_hostiles_remaining = esc_ids.len() as u8;
                *warp_bubble_active = true;
                *wave_time_remaining_secs = aar_wave_duration_secs(next_wave);
            }
            rec.spawned_ball_ids.extend(ids.iter().copied());
            rec.spawned_ball_ids.extend(esc_ids.iter().copied());
            allocated_ids = ids;
            Ok(Some((sys_id, pos, esc, esc_ids)))
        },
    );

    let mut spawned_escorts = Vec::new();
    if let Ok(Some((sys_id, pos, esc_tids, esc_ids))) = info {
        let sys_h = systems.get_or_spawn(sys_id);
        let bubble_id = 1_897_800_000 + instance_id.saturating_mul(100) + 9;
        sys_h.try_add_mobile_warp_disruptor(bubble_id, pos, 150_000.0);
        for (idx, rock_id) in allocated_ids.into_iter().enumerate() {
            let angle = (idx as f64) * std::f64::consts::TAU
                / (HOMEFRONT_AAR_ROCKS_PER_WAVE as f64);
            spawn_asteroid_db_with_record(
                systems,
                db,
                &sys_h,
                sys_id,
                rock_id,
                HOMEFRONT_AAR_MUTANITE_TYPE_ID,
                HOMEFRONT_AAR_UNITS_PER_ROCK,
                pos + DVec3::new(angle.cos() * 7_500.0, 0.0, angle.sin() * 7_500.0),
                120.0,
            );
        }
        for (idx, (eid, etid)) in esc_ids.into_iter().zip(esc_tids).enumerate() {
            let epos = pos + DVec3::new(3_500.0 + (idx as f64) * 1_200.0, -2_000.0, 0.0);
            spawn_ball_db_with_claim(
                systems,
                &sys_h,
                sys_id,
                Ball {
                    id: eid,
                    type_id: etid,
                    position: epos,
                    velocity: DVec3::ZERO,
                    max_velocity: 260.0,
                    inertia: 0.5,
                    mass: 2_000_000.0,
                    radius: 120.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                build_type_combat_state_from_sde(sde, etid),
            );
            spawned_escorts.push((sys_id, eid, etid, epos));
        }
    }
    spawned_escorts
}

fn spawn_next_aar_wave_in_space(ctx: &ServerContext, instance_id: i64, next_wave: u8) {
    let spawned = spawn_next_aar_wave_db(&ctx.systems, &ctx.db, &ctx.sde, instance_id, next_wave);
    for (sys_id, eid, etid, epos) in spawned {
        register_homefront_npc_entity(
            ctx,
            sys_id,
            eid,
            etid,
            500_026,
            "HomefrontHostile",
            epos,
            [0.25, 0.25, 0.25, 0.25],
            100.0,
            3_500.0,
            20_000.0,
        );
    }
}

/// Hook for warping onto a Homefront Operations bookmark/anomaly: spawns ONLY outer entry.
pub async fn on_homefront_warp_landed(
    ctx: &ServerContext,
    _char_id: i64,
    sys_id: SolarSystemId,
    dest_pos: DVec3,
) -> bool {
    let matched_opt = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<Option<HomefrontRuntimeRecord>, BridgeRpcError> {
            let list = snap
                .pve_special_sites
                .homefront_sites
                .get(&sys_id)
                .cloned()
                .unwrap_or_default();
            for (slot_idx, site_rec) in list.iter().enumerate() {
                if site_rec.completed {
                    continue;
                }
                let expected = DVec3::new(((slot_idx as f64) + 1.0) * 1.2e10, 0.0, 4.0e9);
                if dest_pos.distance(expected) <= HOMEFRONT_WARP_ARRIVAL_RADIUS_M {
                    let rt = resolve_runtime_record_in_snap(snap, &ctx.sde, site_rec.instance_id)
                        .cloned();
                    return Ok(rt);
                }
            }
            for rt in snap.dungeon_runtime.homefront_runtimes.values() {
                if rt.solar_system_id == sys_id
                    && !rt.completed
                    && !rt.failed
                    && dest_pos.distance(DVec3::from_array(rt.site_position))
                        <= HOMEFRONT_WARP_ARRIVAL_RADIUS_M
                {
                    return Ok(Some(rt.clone()));
                }
            }
            Ok(None)
        },
    );

    if let Ok(Some(rec)) = matched_opt {
        spawn_homefront_outer_entry_sync(ctx, &rec);
        return true;
    }
    false
}

/// Checks if a Homefront dungeon instance is present for `pve_special_sites::complete_special_dungeon_instance`.
pub fn is_homefront_pve_completion_target(
    ctx: &ServerContext,
    _sys_id: SolarSystemId,
    instance_id: i64,
) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .contains_key(&instance_id)
            || snap
                .pve_special_sites
                .homefront_sites
                .values()
                .flatten()
                .any(|s| s.instance_id == instance_id)
    })
}

/// Replaces legacy single-boss shortcut completion in `complete_special_dungeon_instance`.
pub fn complete_homefront_from_special_dungeon(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: SolarSystemId,
    instance_id: i64,
) -> Result<(Vec<(i64, i64, IskCents)>, HomefrontFleetPayoutSummary), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let site_check = ctx.db.try_with_mut_snapshot(
        |snap| -> Result<Result<(SolarSystemId, String), BridgeRpcError>, BridgeRpcError> {
            let Some(rec) = resolve_runtime_record_in_snap(snap, &ctx.sde, instance_id) else {
                return Ok(Err(hf_err(
                    "HomefrontSiteNotFound",
                    format!("Unknown Homefront site instance {instance_id}"),
                )));
            };
            Ok(Ok((rec.solar_system_id, rec.blueprint.dungeon_name.clone())))
        },
    )??;
    if site_check.0 != sys_id {
        return Err(hf_err(
            "NotInSiteSystem",
            format!("Must be in system {} to complete {}", site_check.0, site_check.1),
        ));
    }
    let (summary, _, _) = settle_homefront_completion_atomic_sync(ctx, instance_id, Some(char_id))?;
    let mut payouts = Vec::new();
    ctx.db.with_snapshot(|snap| {
        for p in &summary.payouts {
            if p.eligible && p.isk_cents.raw_cents() > 0 {
                if let Some(ch) = snap.characters.get(&p.character_id) {
                    payouts.push((p.character_id, p.isk, ch.balance));
                }
            }
        }
    });
    Ok((payouts, summary))
}

/// Synchronous weapon/drone/NPC damage tracking for Homefront NPCs and structures.
pub fn on_homefront_combat_damage_event_sync(
    ctx: &ServerContext,
    attacker_char_opt: Option<i64>,
    _attacker_ball_id: i64,
    target_ball_id: i64,
    damage_dealt: f64,
) {
    if !damage_dealt.is_finite() || damage_dealt <= 0.0 {
        return;
    }
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if rec.completed || rec.failed || !rec.spawned_ball_ids.contains(&target_ball_id) {
                continue;
            }
            if attacker_char_opt.is_some_and(|cid| !rec.entered_pilots.contains_key(&cid)) {
                continue;
            }
            if let Some(cid) = attacker_char_opt {
                ensure_contribution(rec, cid).damage_dealt += damage_dealt;
            }
            match &mut rec.progress {
                HomefrontProgressRecord::EmergencyAid {
                    protectee_ball_id,
                    protectee_hp,
                    ..
                } if *protectee_ball_id == target_ball_id => {
                    *protectee_hp = (*protectee_hp - damage_dealt).max(0.0);
                }
                HomefrontProgressRecord::SuspiciousSignal {
                    relay_ball_id,
                    arrays_hacked,
                    vulnerability_window_secs,
                    relay_hp,
                    ..
                } if *relay_ball_id == target_ball_id => {
                    if arrays_hacked.iter().all(|&h| h) && *vulnerability_window_secs > 0 {
                        *relay_hp = (*relay_hp - damage_dealt).max(0.0);
                    }
                }
                HomefrontProgressRecord::DreadAssault {
                    base_ball_id,
                    enemy_base_hp,
                    in_siege_mode,
                    ..
                } if *base_ball_id == target_ball_id && *in_siege_mode => {
                    *enemy_base_hp = (*enemy_base_hp - damage_dealt).max(0.0);
                }
                _ => {}
            }
            break;
        }
        Ok(())
    });
}

/// Live weapon/drone damage tracking for Homefront NPCs and structures.
pub async fn on_homefront_combat_damage_event(
    ctx: &ServerContext,
    attacker_char_opt: Option<i64>,
    attacker_ball_id: i64,
    target_ball_id: i64,
    damage_dealt: f64,
) {
    on_homefront_combat_damage_event_sync(
        ctx,
        attacker_char_opt,
        attacker_ball_id,
        target_ball_id,
        damage_dealt,
    );
}

/// Live entity destruction handler called by `pve_special_sites_combat`.
/// Returns `None` if `ball_id` is not part of an active Homefront site, or `Some(account_changes)`
/// (`(character_id, new_balance_isk)`) when handled so the caller emits `OnAccountChange`.
pub async fn on_homefront_entity_destroyed_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    sys_id: SolarSystemId,
    ball_id: i64,
    killer_char_opt: Option<i64>,
) -> Option<Vec<(i64, f64)>> {
    enum PostKillAction {
        None,
        DropRaidCrate {
            crate_id: i64,
            crate_pos: DVec3,
            next_hauler_opt: Option<(i64, TypeId, DVec3)>,
        },
        SettleComplete(i64),
        SettleFail(i64, Vec<i64>),
        SpawnNextAarWave(i64, u8),
        DropAarCacheWreck(i64, DVec3),
    }

    let now = db.clock().now_unix_secs();

    let action_res = db.try_with_mut_snapshot(
        |snap| -> Result<Option<PostKillAction>, BridgeRpcError> {
            let target_inst = snap
                .dungeon_runtime
                .homefront_runtimes
                .values()
                .find(|r| {
                    r.solar_system_id == sys_id
                        && !r.completed
                        && !r.failed
                        && killer_char_opt.is_none_or(|cid| r.entered_pilots.contains_key(&cid))
                        && (r.spawned_ball_ids.contains(&ball_id)
                            || r.dispatch_boss_ball_id == ball_id)
                })
                .map(|r| r.instance_id);
            let Some(inst_id) = target_inst else {
                return Ok(None);
            };
            snap.next_entity_id += 1;
            let extra_id = snap.next_entity_id;

            let sys_h_opt = systems.get_handle(sys_id);
            let was_scrambled = sys_h_opt
                .as_ref()
                .is_some_and(|h| h.is_ball_scrambled_sync(ball_id));
            let ball_pos = sys_h_opt
                .as_ref()
                .and_then(|h| h.get_ball_position_sync(ball_id));

            let rec = snap
                .dungeon_runtime
                .homefront_runtimes
                .get_mut(&inst_id)
                .expect("checked above");
            if let Some(cid) = killer_char_opt {
                let in_space_sys = snap.characters.get(&cid).is_none_or(|ch| {
                    ch.solar_system_id == sys_id
                        && ch.station_id.is_none()
                        && ch.structure_id.is_none()
                });
                if in_space_sys && rec.entered_pilots.contains_key(&cid) {
                    let c = ensure_contribution(rec, cid);
                    c.damage_dealt += HOMEFRONT_MIN_SKILL_CONTRIBUTION;
                }
            }
            let loot_tid = rec
                .blueprint
                .loot_commodity_type_id
                .unwrap_or(HOMEFRONT_RAID_CRATE_TYPE_ID);
            let hauler_tid = rec
                .blueprint
                .objective_structure_type_id
                .unwrap_or(77_192);
            let pocket_pos = DVec3::from_array(rec.pocket_position);

            let mut post = PostKillAction::None;
            let mut add_crate_to_snap: Option<(i64, [f64; 3])> = None;

            match &mut rec.progress {
                HomefrontProgressRecord::Raid {
                    haulers_spawned,
                    haulers_destroyed,
                    haulers_escaped,
                    active_hauler_ball_id,
                    active_hauler_tackled,
                    active_hauler_escape_at_sec,
                    dropped_crate_ball_ids,
                    cargo_dropped,
                    required_deliveries,
                    ..
                } if *active_hauler_ball_id == Some(ball_id) => {
                    if !*active_hauler_tackled && !was_scrambled {
                        *haulers_escaped = haulers_escaped.saturating_add(1);
                    } else {
                        *haulers_destroyed = haulers_destroyed.saturating_add(1);
                        *cargo_dropped = cargo_dropped.saturating_add(1);
                        let crate_pos = ball_pos.unwrap_or(pocket_pos + DVec3::new(3_500.0, 0.0, 0.0));
                        dropped_crate_ball_ids.push(extra_id);
                        rec.spawned_ball_ids.push(extra_id);
                        add_crate_to_snap = Some((extra_id, crate_pos.to_array()));

                        let next_hauler = if *haulers_destroyed < *required_deliveries {
                            *haulers_spawned = haulers_spawned.saturating_add(1);
                            let hid = 1_897_800_000
                                + inst_id * 100
                                + 30
                                + i64::from(*haulers_spawned);
                            *active_hauler_ball_id = Some(hid);
                            *active_hauler_tackled = false;
                            *active_hauler_escape_at_sec = now.saturating_add(90);
                            rec.spawned_ball_ids.push(hid);
                            Some((
                                hid,
                                hauler_tid,
                                pocket_pos + DVec3::new(4_000.0, 0.0, 2_000.0),
                            ))
                        } else {
                            *active_hauler_ball_id = None;
                            None
                        };
                        post = PostKillAction::DropRaidCrate {
                            crate_id: extra_id,
                            crate_pos,
                            next_hauler_opt: next_hauler,
                        };
                    }
                }
                HomefrontProgressRecord::EmergencyAid {
                    protectee_ball_id,
                    protectee_hp,
                    ..
                } if *protectee_ball_id == ball_id => {
                    *protectee_hp = 0.0;
                    rec.failed = true;
                    rec.failure_reason =
                        Some("Emergency Aid allied vessel was destroyed".into());
                    post = PostKillAction::SettleFail(inst_id, rec.spawned_ball_ids.clone());
                }
                HomefrontProgressRecord::SuspiciousSignal {
                    relay_ball_id,
                    arrays_hacked,
                    relay_hp,
                    ..
                } if *relay_ball_id == ball_id => {
                    if arrays_hacked.iter().all(|&h| h) {
                        *relay_hp = 0.0;
                        post = PostKillAction::SettleComplete(inst_id);
                    }
                }
                HomefrontProgressRecord::DreadAssault {
                    dread_ball_id,
                    base_ball_id,
                    enemy_base_hp,
                    ..
                } => {
                    if *dread_ball_id == ball_id {
                        rec.failed = true;
                        rec.failure_reason = Some("Allied Dreadnought was destroyed".into());
                        post = PostKillAction::SettleFail(inst_id, rec.spawned_ball_ids.clone());
                    } else if *base_ball_id == ball_id {
                        *enemy_base_hp = 0.0;
                        if runtime_record_objective_complete(rec) {
                            post = PostKillAction::SettleComplete(inst_id);
                        }
                    }
                }
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    total_waves,
                    wave_hostile_ball_ids,
                    wave_hostiles_remaining,
                    mutanite_units_remaining,
                    evacuation_active,
                    basileus_spawned,
                    cache_ball_id,
                    caches_recovered,
                    ..
                } => {
                    if *cache_ball_id == Some(ball_id) {
                        *cache_ball_id = None;
                        *caches_recovered += 1;
                        let cpos = ball_pos.unwrap_or(pocket_pos);
                        post = PostKillAction::DropAarCacheWreck(extra_id, cpos);
                    } else if wave_hostile_ball_ids.contains(&ball_id) {
                        wave_hostile_ball_ids.retain(|&id| id != ball_id);
                        *wave_hostiles_remaining = wave_hostile_ball_ids.len() as u8;
                        if *mutanite_units_remaining == 0
                            && *wave_hostiles_remaining == 0
                            && *current_wave < *total_waves
                        {
                            post = PostKillAction::SpawnNextAarWave(inst_id, *current_wave + 1);
                        } else if (!*evacuation_active
                            || (*basileus_spawned && cache_ball_id.is_none()))
                            && runtime_record_objective_complete(rec)
                        {
                            post = PostKillAction::SettleComplete(inst_id);
                        }
                    }
                }
                HomefrontProgressRecord::TrafficStop {
                    active_hauler_ball_ids,
                    smuggler_ball_ids,
                    civilian_ball_ids,
                    smugglers_destroyed,
                    required_smugglers,
                    civilians_harmed,
                    ..
                } => {
                    active_hauler_ball_ids.retain(|&id| id != ball_id);
                    if smuggler_ball_ids.contains(&ball_id) {
                        smuggler_ball_ids.retain(|&id| id != ball_id);
                        *smugglers_destroyed =
                            smugglers_destroyed.saturating_add(1).min(*required_smugglers);
                        if runtime_record_objective_complete(rec) {
                            post = PostKillAction::SettleComplete(inst_id);
                        }
                    } else if civilian_ball_ids.contains(&ball_id) {
                        civilian_ball_ids.retain(|&id| id != ball_id);
                        *civilians_harmed = civilians_harmed.saturating_add(1);
                        if *civilians_harmed >= 3 {
                            rec.failed = true;
                            rec.failure_reason = Some(
                                "Traffic Stop failed due to civilian collateral damage".into(),
                            );
                            post = PostKillAction::SettleFail(inst_id, rec.spawned_ball_ids.clone());
                        }
                    }
                }
                _ => {}
            }

            if let Some((crate_id, pos_arr)) = add_crate_to_snap {
                spawn_container_item_in_snap(snap, sys_id, crate_id, 0, pos_arr, now, &[(loot_tid, 1)]);
            }
            if let PostKillAction::DropAarCacheWreck(wreck_id, cpos) = post {
                insert_scene_item_in_snap(
                    snap,
                    InventoryItemRecord {
                        item_id: wreck_id,
                        type_id: HOMEFRONT_AAR_CACHE_WRECK_TYPE_ID,
                        owner_id: 0,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                snap.item_positions.insert(wreck_id, cpos.to_array());
            }

            Ok(Some(post))
        },
    );

    let Ok(Some(post)) = action_res else {
        return None;
    };
    let sys_h = systems.get_or_spawn(sys_id);
    let mut account_changes = Vec::new();
    match post {
        PostKillAction::None => {}
        PostKillAction::DropRaidCrate {
            crate_id,
            crate_pos,
            next_hauler_opt,
        } => {
            spawn_ball_db_with_claim(
                systems,
                &sys_h,
                sys_id,
                Ball {
                    id: crate_id,
                    type_id: HOMEFRONT_CARGO_CONTAINER_TYPE_ID,
                    position: crate_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 2_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    structure_hp: 1_500.0,
                    max_structure_hp: 1_500.0,
                    ..CombatState::default()
                },
            );
            if let Some((nh_id, nh_tid, nh_pos)) = next_hauler_opt {
                spawn_ball_db_with_claim(
                    systems,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: nh_id,
                        type_id: nh_tid,
                        position: nh_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 180.0,
                        inertia: 0.8,
                        mass: 15_000_000.0,
                        radius: 260.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    build_type_combat_state_from_sde(sde, nh_tid),
                );
            }
        }
        PostKillAction::SettleComplete(inst_id) => {
            if let Ok((_, _, _, emitted)) =
                settle_homefront_completion_atomic_db(db, sde, systems, inst_id, None)
            {
                for (payout, new_balance, _) in emitted {
                    if payout.eligible && payout.isk_cents.raw_cents() > 0 {
                        account_changes.push((payout.character_id, new_balance.to_wire_f64()));
                    }
                }
            }
        }
        PostKillAction::SettleFail(inst_id, balls) => {
            despawn_homefront_balls_db(systems, sys_id, inst_id, &balls);
        }
        PostKillAction::SpawnNextAarWave(inst_id, next_w) => {
            let _ = spawn_next_aar_wave_db(systems, db, sde, inst_id, next_w);
        }
        PostKillAction::DropAarCacheWreck(wreck_id, wpos) => {
            spawn_ball_db_with_claim(
                systems,
                &sys_h,
                sys_id,
                Ball {
                    id: wreck_id,
                    type_id: HOMEFRONT_AAR_CACHE_WRECK_TYPE_ID,
                    position: wpos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 5_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    structure_hp: 1_500.0,
                    max_structure_hp: 1_500.0,
                    ..CombatState::default()
                },
            );
        }
    }
    Some(account_changes)
}

/// Live ore extraction hook called by `space_entity_mining` after extracting Mutanite.
pub async fn on_homefront_ore_mined(
    ctx: &ServerContext,
    char_id: i64,
    _ship_id: i64,
    sys_id: SolarSystemId,
    asteroid_id: i64,
    _ore_type_id: TypeId,
    mined_units: u64,
    wasted_units: u64,
    remaining_rock_units: u64,
) {
    let mut wave_to_pay: Option<(i64, u8, bool)> = None;
    let mut settle_inst: Option<i64> = None;

    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let mut matched_id = None;
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if rec.solar_system_id != sys_id
                || !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
            {
                continue;
            }
            match &mut rec.progress {
                HomefrontProgressRecord::MetaliminalMeteoroid {
                    meteoroid_ball_id,
                    remaining_units,
                    total_residue_units,
                    ore_mined_by_fleet,
                    ..
                } if *meteoroid_ball_id == asteroid_id => {
                    *remaining_units = u32::try_from(remaining_rock_units).unwrap_or(0);
                    *ore_mined_by_fleet =
                        ore_mined_by_fleet.saturating_add(u32::try_from(mined_units).unwrap_or(u32::MAX));
                    *total_residue_units =
                        total_residue_units.saturating_add(u32::try_from(wasted_units).unwrap_or(u32::MAX));
                    ensure_contribution_fields(
                        &mut rec.contributions,
                        &mut rec.next_entry_sequence,
                        char_id,
                    )
                    .ore_mined_units += u32::try_from(mined_units).unwrap_or(u32::MAX);
                    if *remaining_units == 0 {
                        settle_inst = Some(rec.instance_id);
                    }
                    matched_id = Some(rec.instance_id);
                    break;
                }
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    total_waves,
                    rock_ball_ids,
                    mutanite_units_remaining,
                    warp_bubble_active,
                    evacuation_active,
                    extraction_window_secs,
                    ..
                } if rock_ball_ids.contains(&asteroid_id) => {
                    let removed = u32::try_from(mined_units.saturating_add(wasted_units))
                        .unwrap_or(u32::MAX);
                    *mutanite_units_remaining = mutanite_units_remaining.saturating_sub(removed);
                    if remaining_rock_units == 0 {
                        rock_ball_ids.retain(|&id| id != asteroid_id);
                        if rock_ball_ids.is_empty() {
                            *mutanite_units_remaining = 0;
                        }
                    }
                    ensure_contribution_fields(
                        &mut rec.contributions,
                        &mut rec.next_entry_sequence,
                        char_id,
                    )
                    .ore_mined_units += u32::try_from(mined_units).unwrap_or(u32::MAX);
                    if *mutanite_units_remaining == 0 {
                        let bubble_was = *warp_bubble_active;
                        *warp_bubble_active = false;
                        if *current_wave >= *total_waves && !*evacuation_active {
                            *evacuation_active = true;
                            *extraction_window_secs = 60;
                        }
                        wave_to_pay = Some((rec.instance_id, *current_wave, bubble_was));
                    }
                    matched_id = Some(rec.instance_id);
                    break;
                }
                _ => {}
            }
        }
        let _ = matched_id;
        Ok(())
    });

    if let Some((inst_id, wnum, bubble_was)) = wave_to_pay {
        if bubble_was {
            let sys_h = ctx.systems.get_or_spawn(sys_id);
            let _ = sys_h
                .remove_warp_bubble(1_897_800_000 + inst_id.saturating_mul(100) + 9)
                .await;
        }
        pay_aar_wave_live_if_needed(ctx, inst_id, wnum);
    }
    if let Some(inst_id) = settle_inst {
        try_settle_completed_homefront_live(ctx, inst_id);
    }
}

/// Identifies a Homefront hackable object (`SuspiciousSignal` Data Arrays or `SalvageResearch` Werpost).
/// Returns `Some((is_relic=false, site_difficulty))` if matched.
pub fn is_homefront_hackable_structure(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ball_id: i64,
) -> Option<(bool, i64)> {
    ctx.db.with_snapshot(|snap| {
        for rec in snap.dungeon_runtime.homefront_runtimes.values() {
            if rec.solar_system_id != sys_id || rec.completed || rec.failed {
                continue;
            }
            match &rec.progress {
                HomefrontProgressRecord::SuspiciousSignal {
                    array_ball_ids,
                    arrays_hacked,
                    ..
                } if array_ball_ids.iter().position(|&id| id == ball_id).is_some_and(|idx| {
                    !arrays_hacked.get(idx).copied().unwrap_or(false)
                }) => {
                    return Some((false, i64::from(rec.blueprint.difficulty.max(1))));
                }
                HomefrontProgressRecord::SalvageResearch {
                    werpost_ball_id,
                    werpost_activated,
                    ..
                } if *werpost_ball_id == ball_id && !*werpost_activated => {
                    return Some((false, i64::from(rec.blueprint.difficulty.max(1))));
                }
                _ => {}
            }
        }
        None
    })
}

/// Live hacking completion hook called by `pve_hacking_sites` when minigame ends with `Won`.
pub async fn on_homefront_structure_hacked(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    container_id: i64,
) -> bool {
    let mut clear_relay: Option<(SolarSystemId, i64)> = None;
    let handled = ctx.db.try_with_mut_snapshot(|snap| -> Result<bool, BridgeRpcError> {
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if rec.solar_system_id != sys_id
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
            {
                continue;
            }
            match &mut rec.progress {
                HomefrontProgressRecord::SuspiciousSignal {
                    relay_ball_id,
                    array_ball_ids,
                    arrays_hacked,
                    vulnerability_window_secs,
                    ..
                } => {
                    if let Some(idx) = array_ball_ids.iter().position(|&id| id == container_id) {
                        if idx < 3 {
                            arrays_hacked[idx] = true;
                        }
                        if arrays_hacked.iter().all(|&h| h) {
                            *vulnerability_window_secs = HOMEFRONT_SIGNAL_VULN_WINDOW_SECS;
                            clear_relay = Some((sys_id, *relay_ball_id));
                        }
                        ensure_contribution(rec, char_id).arrays_hacked += 1;
                        return Ok(true);
                    }
                }
                HomefrontProgressRecord::SalvageResearch {
                    werpost_ball_id,
                    werpost_activated,
                    ..
                } if *werpost_ball_id == container_id => {
                    *werpost_activated = true;
                    let c = ensure_contribution(rec, char_id);
                    c.arrays_hacked += 1;
                    c.objective_actions += 1;
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(false)
    });

    if let Some((sid, relay_id)) = clear_relay {
        if let Some(sys_h) = ctx.systems.get_handle(sid) {
            let _ = sys_h
                .modify_combat_state(relay_id, |cs| {
                    cs.is_tethered = false;
                })
                .await;
        }
    }
    handled.unwrap_or(false)
}

/// Validates Salvage Research gate entry and `>= 20%` combined salvager access bonus in `drones_salvage_tractor`.
pub fn validate_homefront_salvage_attempt(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    wreck_id: i64,
    mod_bonus: f64,
    skill_bonus: f64,
    rig_bonus: f64,
) -> Result<bool, BridgeRpcError> {
    let match_state = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .find_map(|rec| {
                if rec.solar_system_id == sys_id
                    && !rec.completed
                    && !rec.failed
                    && matches!(
                        &rec.progress,
                        HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. }
                            if wreck_ball_ids.contains(&wreck_id)
                    )
                {
                    Some(rec.entered_pilots.contains_key(&char_id))
                } else {
                    None
                }
            })
    });
    let Some(pilot_entered) = match_state else {
        return Ok(false);
    };
    if !pilot_entered {
        return Err(hf_err(
            "GateNotEntered",
            "Pilot has not entered the Homefront acceleration gate",
        ));
    }
    let pilot_bonus = mod_bonus + skill_bonus + rig_bonus;
    if pilot_bonus < HOMEFRONT_MIN_SALVAGE_BONUS_PCT {
        return Err(hf_err(
            "InsufficientAccessChance",
            format!(
                "Salvage Research wrecks require >= {HOMEFRONT_MIN_SALVAGE_BONUS_PCT:.0}% combined salvager bonus (got {pilot_bonus:.1}%)"
            ),
        ));
    }
    Ok(true)
}

/// Live wreck salvage completion hook called by `drones_salvage_tractor` (returns `true` if handled).
pub async fn on_homefront_wreck_salvaged(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    wreck_id: i64,
) -> bool {
    let mut complete_inst = None;
    let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<bool, BridgeRpcError> {
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if rec.solar_system_id != sys_id
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
            {
                continue;
            }
            if let HomefrontProgressRecord::SalvageResearch {
                wreck_ball_ids,
                wrecks_salvaged,
                total_wrecks,
                ..
            } = &mut rec.progress
            {
                if wreck_ball_ids.contains(&wreck_id) {
                    wreck_ball_ids.retain(|&id| id != wreck_id);
                    *wrecks_salvaged = wrecks_salvaged.saturating_add(1).min(*total_wrecks);
                    ensure_contribution_fields(
                        &mut rec.contributions,
                        &mut rec.next_entry_sequence,
                        char_id,
                    )
                    .objective_actions += 1;
                    if *wrecks_salvaged >= *total_wrecks {
                        complete_inst = Some(rec.instance_id);
                    }
                    return Ok(true);
                }
            }
        }
        Ok(false)
    });
    if let Some(inst_id) = complete_inst {
        try_settle_completed_homefront_live(ctx, inst_id);
    }
    res.unwrap_or(false)
}

/// Live Cargo Scanner hook called by `fitting_scanners_superweapons`.
pub async fn on_homefront_cargo_scanned(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    target_id: i64,
    cargo_list: &[(i32, i64)],
) {
    let has_contraband = cargo_list
        .iter()
        .any(|&(tid, qty)| tid as u32 == HOMEFRONT_TRAFFIC_CONTRABAND_TYPE_ID && qty > 0);
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if rec.solar_system_id != sys_id
                || !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
            {
                continue;
            }
            if let HomefrontProgressRecord::TrafficStop {
                active_hauler_ball_ids,
                scanned_hauler_ball_ids,
                ..
            } = &mut rec.progress
            {
                if active_hauler_ball_ids.contains(&target_id) {
                    if !scanned_hauler_ball_ids.contains(&target_id) {
                        scanned_hauler_ball_ids.push(target_id);
                    }
                    let c = ensure_contribution(rec, char_id);
                    c.objective_actions += if has_contraband { 2 } else { 1 };
                    return Ok(());
                }
            }
        }
        Ok(())
    });
}

/// Live item move hook called when cargo items are transferred into a Raid delivery depot.
pub async fn on_homefront_item_moved(
    ctx: &ServerContext,
    char_id: i64,
    dest_id: i64,
    item_type_id: TypeId,
    quantity: u64,
) {
    if quantity == 0 {
        return;
    }
    let mut complete_inst = None;
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
            {
                continue;
            }
            let loot_tid = rec
                .blueprint
                .loot_commodity_type_id
                .unwrap_or(HOMEFRONT_RAID_CRATE_TYPE_ID);
            if item_type_id != loot_tid {
                continue;
            }
            if let HomefrontProgressRecord::Raid {
                delivery_depot_ball_id,
                cargo_delivered,
                required_deliveries,
                ..
            } = &mut rec.progress
            {
                if *delivery_depot_ball_id == dest_id {
                    let add = u8::try_from(quantity).unwrap_or(u8::MAX);
                    *cargo_delivered = cargo_delivered.saturating_add(add);
                    ensure_contribution_fields(
                        &mut rec.contributions,
                        &mut rec.next_entry_sequence,
                        char_id,
                    )
                    .cargo_delivered += u32::from(add);
                    if *cargo_delivered >= *required_deliveries {
                        complete_inst = Some(rec.instance_id);
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    });
    if let Some(inst_id) = complete_inst {
        try_settle_completed_homefront_live(ctx, inst_id);
    }
}

/// Live inventory-movement hook invoked via `pve_dungeon_runtime::on_dungeon_item_looted` whenever a pilot
/// transfers an item between space containers / ship cargo (verifies depot deliveries in `snap.items`).
pub async fn on_homefront_inventory_event(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    source_or_dest_id: i64,
    item_type_id: TypeId,
) {
    let mut complete_insts: Vec<i64> = Vec::new();
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let mut depot_counts: Vec<(i64, u8)> = Vec::new();
        for rec in snap.dungeon_runtime.homefront_runtimes.values() {
            if !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
                || sys_id_opt.is_some_and(|sid| rec.solar_system_id != sid)
            {
                continue;
            }
            if let HomefrontProgressRecord::Raid {
                delivery_depot_ball_id,
                ..
            } = &rec.progress
            {
                let loot_tid = rec
                    .blueprint
                    .loot_commodity_type_id
                    .unwrap_or(HOMEFRONT_RAID_CRATE_TYPE_ID);
                if *delivery_depot_ball_id == source_or_dest_id && item_type_id == loot_tid {
                    let count: i64 = snap
                        .items
                        .values()
                        .filter(|it| {
                            it.location_id == *delivery_depot_ball_id
                                && it.type_id == loot_tid
                                && it.quantity > 0
                                && rec.entered_pilots.contains_key(&it.owner_id)
                        })
                        .map(|it| i64::from(it.quantity))
                        .sum();
                    depot_counts.push((
                        rec.instance_id,
                        u8::try_from(count.max(0)).unwrap_or(u8::MAX),
                    ));
                }
            }
        }
        for (inst_id, delivered_in_depot) in depot_counts {
            if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) {
                if let HomefrontProgressRecord::Raid {
                    cargo_delivered,
                    required_deliveries,
                    ..
                } = &mut rec.progress
                {
                    if delivered_in_depot > *cargo_delivered {
                        let delta = delivered_in_depot - *cargo_delivered;
                        *cargo_delivered = delivered_in_depot;
                        ensure_contribution_fields(
                            &mut rec.contributions,
                            &mut rec.next_entry_sequence,
                            char_id,
                        )
                        .cargo_delivered += u32::from(delta);
                    } else if delivered_in_depot < *cargo_delivered {
                        *cargo_delivered = delivered_in_depot;
                    }
                    if *cargo_delivered >= *required_deliveries {
                        complete_insts.push(inst_id);
                    }
                }
            }
        }
        Ok(())
    });
    for inst_id in complete_insts {
        try_settle_completed_homefront_live(ctx, inst_id);
    }
}

/// Synchronous capacitor transfer / energy neutralizer hook (DreadAssault & StabilizeRift).
pub fn on_homefront_cap_warfare_event_sync(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    target_id: i64,
    amount_gj: f64,
    is_positive_transfer: bool,
) {
    if !amount_gj.is_finite() || amount_gj <= 0.0 {
        return;
    }
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let resolved_sys_opt = sys_id_opt
            .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id));
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
                || resolved_sys_opt.is_some_and(|sid| rec.solar_system_id != sid)
            {
                continue;
            }
            match &mut rec.progress {
                HomefrontProgressRecord::DreadAssault {
                    dread_ball_id,
                    capacitor_gj,
                    capacitor_target_gj,
                    in_siege_mode,
                    siege_remaining_secs,
                    ..
                } if *dread_ball_id == target_id && is_positive_transfer && !*in_siege_mode => {
                    *capacitor_gj = (*capacitor_gj + amount_gj).clamp(0.0, *capacitor_target_gj);
                    if *capacitor_gj >= *capacitor_target_gj {
                        *in_siege_mode = true;
                        *siege_remaining_secs = 90;
                    }
                    ensure_contribution(rec, char_id).cap_transferred += amount_gj;
                    return Ok(());
                }
                HomefrontProgressRecord::StabilizeRift {
                    array_ball_ids,
                    arrays_cap_pct,
                    ..
                } if !is_positive_transfer => {
                    if let Some(idx) = array_ball_ids.iter().position(|&id| id == target_id) {
                        if idx < 5 {
                            let drain_pct = (amount_gj / 4_000.0).clamp(0.0, 1.0);
                            arrays_cap_pct[idx] = (arrays_cap_pct[idx] - drain_pct).clamp(0.0, 1.0);
                        }
                        let c = ensure_contribution(rec, char_id);
                        c.cap_transferred += amount_gj;
                        c.objective_actions += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });
}

/// Live capacitor transfer / energy neutralizer hook (DreadAssault & StabilizeRift).
pub async fn on_homefront_cap_warfare_event(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    target_id: i64,
    amount_gj: f64,
    is_positive_transfer: bool,
) {
    on_homefront_cap_warfare_event_sync(
        ctx,
        char_id,
        Some(sys_id),
        target_id,
        amount_gj,
        is_positive_transfer,
    );
}

/// Synchronous Remote Repair hook (EmergencyAid protectee & DreadAssault Dreadnought).
pub fn on_homefront_remote_repair_event_sync(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    target_id: i64,
    repaired_hp: f64,
) {
    if !repaired_hp.is_finite() || repaired_hp <= 0.0 {
        return;
    }
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let resolved_sys_opt = sys_id_opt
            .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id));
        for rec in snap.dungeon_runtime.homefront_runtimes.values_mut() {
            if !rec.started
                || rec.completed
                || rec.failed
                || !rec.entered_pilots.contains_key(&char_id)
                || resolved_sys_opt.is_some_and(|sid| rec.solar_system_id != sid)
            {
                continue;
            }
            match &mut rec.progress {
                HomefrontProgressRecord::EmergencyAid {
                    protectee_ball_id,
                    protectee_hp,
                    protectee_max_hp,
                    ..
                } if *protectee_ball_id == target_id => {
                    *protectee_hp = (*protectee_hp + repaired_hp).min(*protectee_max_hp);
                    ensure_contribution(rec, char_id).repair_hp += repaired_hp;
                    return Ok(());
                }
                HomefrontProgressRecord::DreadAssault { dread_ball_id, .. }
                    if *dread_ball_id == target_id =>
                {
                    ensure_contribution(rec, char_id).repair_hp += repaired_hp;
                    return Ok(());
                }
                _ => {}
            }
        }
        Ok(())
    });
}

/// Live Remote Repair hook (EmergencyAid protectee & DreadAssault Dreadnought).
pub async fn on_homefront_remote_repair_event(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    target_id: i64,
    repaired_hp: f64,
) {
    on_homefront_remote_repair_event_sync(ctx, char_id, Some(sys_id), target_id, repaired_hp);
}

/// Synchronous 1 Hz / `advance_pve_special_clock` Homefront tick across all systems.
pub fn tick_homefront_operations_sync(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let active_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .filter(|r| r.started && !r.completed && !r.failed)
            .map(|r| r.instance_id)
            .collect()
    });
    if active_ids.is_empty() {
        return;
    }

    for inst_id in active_ids {
        tick_single_homefront_instance_sync(ctx, inst_id, now);
    }
}

/// Synchronous 1 Hz per-system Homefront tick called from `combat_npc_ai::step_extended_npc_ai_sync`.
pub fn tick_homefront_operations_for_system_sync(ctx: &ServerContext, sys_id: SolarSystemId) {
    let now = ctx.now_unix();
    let active_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .filter(|r| r.solar_system_id == sys_id && r.started && !r.completed && !r.failed)
            .map(|r| r.instance_id)
            .collect()
    });
    if active_ids.is_empty() {
        return;
    }

    for inst_id in active_ids {
        tick_single_homefront_instance_sync(ctx, inst_id, now);
    }
}

fn tick_single_homefront_instance_sync(ctx: &ServerContext, inst_id: i64, now: u64) {
    #[derive(Default)]
    struct SideEffects {
        settle_complete: bool,
        despawn_fail: Option<(SolarSystemId, Vec<i64>)>,
        retether_relay: Option<(SolarSystemId, i64)>,
        spawn_aar_wave: Option<u8>,
        spawn_basileus: Option<(SolarSystemId, DVec3)>,
        werpost_attack: Option<(SolarSystemId, i64, f64)>,
        dread_siege_dmg: Option<(SolarSystemId, i64, f64)>,
        respawn_raid_hauler: Option<(SolarSystemId, i64, TypeId, DVec3, i64)>,
    }

    let snapshot_pre = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime.homefront_runtimes.get(&inst_id).cloned()
    });
    let Some(pre) = snapshot_pre else {
        return;
    };
    let sys_h_opt = ctx.systems.get_handle(pre.solar_system_id);

    // Sample live ballpark CombatState outside DB write lock (strict lock order!)
    let mut live_dread_cap: Option<f64> = None;
    let mut live_rift_caps: Option<[f64; 5]> = None;
    let mut live_protectee_hp: Option<f64> = None;
    let mut live_raid_scrambled = false;
    let mut live_hostile_target: Option<i64> = None;

    if let Some(sys_h) = sys_h_opt.as_ref() {
        let view = sys_h.view();
        match &pre.progress {
            HomefrontProgressRecord::DreadAssault { dread_ball_id, .. } => {
                if let Some(cs) = view.combat_states.get(dread_ball_id) {
                    live_dread_cap = Some(cs.capacitor);
                }
            }
            HomefrontProgressRecord::StabilizeRift { array_ball_ids, .. } => {
                let mut caps = [1.0_f64; 5];
                let mut any_ball = false;
                for (i, aid) in array_ball_ids.iter().enumerate().take(5) {
                    if let Some(cs) = view.combat_states.get(aid) {
                        caps[i] = if cs.max_capacitor > 0.0 {
                            (cs.capacitor / cs.max_capacitor).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        any_ball = true;
                    }
                }
                if any_ball {
                    live_rift_caps = Some(caps);
                }
            }
            HomefrontProgressRecord::EmergencyAid { protectee_ball_id, .. } => {
                if let Some(cs) = view.combat_states.get(protectee_ball_id) {
                    live_protectee_hp = Some(cs.shield_hp + cs.armor_hp + cs.structure_hp);
                }
            }
            HomefrontProgressRecord::Raid {
                active_hauler_ball_id: Some(hid),
                ..
            } => {
                live_raid_scrambled = sys_h.is_ball_scrambled_sync(*hid);
            }
            HomefrontProgressRecord::SalvageResearch {
                werpost_activated: true,
                ..
            } => {
                live_hostile_target = pre
                    .spawned_ball_ids
                    .iter()
                    .copied()
                    .find(|bid| *bid == pre.dispatch_boss_ball_id && view.balls.contains_key(bid));
            }
            _ => {}
        }
    }

    let fx_res = ctx.db.try_with_mut_snapshot(|snap| -> Result<SideEffects, BridgeRpcError> {
        let mut fx = SideEffects::default();
        let (raid_depot_count, raid_last_pilot_opt) = snap
            .dungeon_runtime
            .homefront_runtimes
            .get(&inst_id)
            .and_then(|rec| match &rec.progress {
                HomefrontProgressRecord::Raid {
                    delivery_depot_ball_id,
                    ..
                } => {
                    let loot_tid = rec
                        .blueprint
                        .loot_commodity_type_id
                        .unwrap_or(HOMEFRONT_RAID_CRATE_TYPE_ID);
                    let mut total: i64 = 0;
                    let mut owner_opt = None;
                    for it in snap.items.values() {
                        if it.location_id == *delivery_depot_ball_id
                            && it.type_id == loot_tid
                            && it.quantity > 0
                            && rec.entered_pilots.contains_key(&it.owner_id)
                        {
                            total += i64::from(it.quantity);
                            if owner_opt.is_none() {
                                owner_opt = Some(it.owner_id);
                            }
                        }
                    }
                    Some((
                        u8::try_from(total.max(0)).unwrap_or(u8::MAX),
                        owner_opt,
                    ))
                }
                _ => None,
            })
            .unwrap_or((0, None));
        let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) else {
            return Ok(fx);
        };
        if !rec.started || rec.completed || rec.failed {
            return Ok(fx);
        }
        let dt = now.saturating_sub(rec.last_tick_sec.max(rec.started_at_sec));
        rec.last_tick_sec = now;

        if rec.expires_at_sec > 0 && now >= rec.expires_at_sec {
            rec.failed = true;
            rec.failure_reason = Some(format!(
                "Homefront {} exceeded {}-second operation timer",
                rec.blueprint.dungeon_name, rec.blueprint.time_limit_secs
            ));
            fx.despawn_fail = Some((rec.solar_system_id, rec.spawned_ball_ids.clone()));
            return Ok(fx);
        }

        if let HomefrontProgressRecord::Raid {
            cargo_delivered,
            required_deliveries,
            ..
        } = &mut rec.progress
        {
            if raid_depot_count > *cargo_delivered {
                let delta = raid_depot_count - *cargo_delivered;
                *cargo_delivered = raid_depot_count;
                if let Some(pid) = raid_last_pilot_opt {
                    ensure_contribution_fields(
                        &mut rec.contributions,
                        &mut rec.next_entry_sequence,
                        pid,
                    )
                    .cargo_delivered += u32::from(delta);
                }
                if *cargo_delivered >= *required_deliveries {
                    fx.settle_complete = true;
                }
            }
        }
        if dt == 0 {
            return Ok(fx);
        }

        let sys_id = rec.solar_system_id;
        let pocket_pos = DVec3::from_array(rec.pocket_position);
        let hauler_tid = rec
            .blueprint
            .objective_structure_type_id
            .unwrap_or(77_192);

        match &mut rec.progress {
            HomefrontProgressRecord::Raid {
                haulers_spawned,
                haulers_escaped,
                active_hauler_ball_id,
                active_hauler_tackled,
                active_hauler_escape_at_sec,
                ..
            } => {
                if live_raid_scrambled {
                    *active_hauler_tackled = true;
                }
                if let Some(old_hid) = *active_hauler_ball_id {
                    if !*active_hauler_tackled
                        && *active_hauler_escape_at_sec > 0
                        && now >= *active_hauler_escape_at_sec
                    {
                        *haulers_escaped = haulers_escaped.saturating_add(1);
                        *haulers_spawned = haulers_spawned.saturating_add(1);
                        let new_hid = 1_897_800_000
                            + inst_id * 100
                            + 30
                            + i64::from(*haulers_spawned);
                        *active_hauler_ball_id = Some(new_hid);
                        *active_hauler_escape_at_sec = now.saturating_add(90);
                        rec.spawned_ball_ids.push(new_hid);
                        fx.respawn_raid_hauler = Some((
                            sys_id,
                            old_hid,
                            hauler_tid,
                            pocket_pos + DVec3::new(4_000.0, 0.0, 2_000.0),
                            new_hid,
                        ));
                    }
                }
            }
            HomefrontProgressRecord::EmergencyAid {
                protectee_hp,
                survival_elapsed_secs,
                required_survival_secs,
                waves_repelled,
                ..
            } => {
                if let Some(lhp) = live_protectee_hp {
                    *protectee_hp = lhp;
                }
                if *protectee_hp <= 0.0 {
                    rec.failed = true;
                    rec.failure_reason =
                        Some("Emergency Aid allied vessel was destroyed".into());
                    fx.despawn_fail = Some((sys_id, rec.spawned_ball_ids.clone()));
                    return Ok(fx);
                }
                *survival_elapsed_secs =
                    survival_elapsed_secs.saturating_add(dt).min(*required_survival_secs);
                *waves_repelled = (*waves_repelled).max((*survival_elapsed_secs / 200).min(3) as u8);
                if *survival_elapsed_secs >= *required_survival_secs {
                    fx.settle_complete = true;
                }
            }
            HomefrontProgressRecord::DreadAssault {
                base_ball_id,
                capacitor_gj,
                capacitor_target_gj,
                siege_cycles_completed,
                target_siege_cycles,
                in_siege_mode,
                siege_remaining_secs,
                enemy_base_hp,
                enemy_base_max_hp,
                ..
            } => {
                if !*in_siege_mode {
                    if let Some(dcap) = live_dread_cap {
                        if dcap > *capacitor_gj {
                            *capacitor_gj = dcap.clamp(0.0, *capacitor_target_gj);
                        }
                    }
                    if *capacitor_gj >= *capacitor_target_gj {
                        *in_siege_mode = true;
                        *siege_remaining_secs = 90;
                    }
                }
                if *in_siege_mode {
                    if dt >= *siege_remaining_secs {
                        *siege_remaining_secs = 0;
                        *in_siege_mode = false;
                        *capacitor_gj = 0.0;
                        *siege_cycles_completed = siege_cycles_completed.saturating_add(1);
                        let chunk =
                            *enemy_base_max_hp / f64::from((*target_siege_cycles).max(1));
                        *enemy_base_hp = (*enemy_base_hp - chunk).max(0.0);
                        fx.dread_siege_dmg = Some((sys_id, *base_ball_id, chunk));
                        if *siege_cycles_completed >= *target_siege_cycles && *enemy_base_hp <= 0.0
                        {
                            fx.settle_complete = true;
                        }
                    } else {
                        *siege_remaining_secs -= dt;
                    }
                }
            }
            HomefrontProgressRecord::AbyssalArtifactRecovery {
                current_wave,
                total_waves,
                setup_remaining_secs,
                wave_time_remaining_secs,
                evacuation_active,
                basileus_spawned,
                extraction_window_secs,
                mutanite_units_remaining,
                ..
            } => {
                if *setup_remaining_secs > 0 {
                    *setup_remaining_secs = setup_remaining_secs.saturating_sub(dt);
                } else if *evacuation_active {
                    if dt >= *extraction_window_secs {
                        *extraction_window_secs = 0;
                        if !*basileus_spawned {
                            *basileus_spawned = true;
                            fx.spawn_basileus = Some((sys_id, pocket_pos));
                        }
                    } else {
                        *extraction_window_secs -= dt;
                    }
                } else if *mutanite_units_remaining > 0 {
                    if dt >= *wave_time_remaining_secs {
                        if *current_wave < *total_waves {
                            fx.spawn_aar_wave = Some(*current_wave + 1);
                        }
                    } else {
                        *wave_time_remaining_secs -= dt;
                    }
                } else if *current_wave < *total_waves {
                    fx.spawn_aar_wave = Some(*current_wave + 1);
                }
            }
            HomefrontProgressRecord::SuspiciousSignal {
                relay_ball_id,
                arrays_hacked,
                vulnerability_window_secs,
                relay_hp,
                ..
            } => {
                if *vulnerability_window_secs > 0 && *relay_hp > 0.0 {
                    if dt >= *vulnerability_window_secs {
                        *vulnerability_window_secs = 0;
                        *arrays_hacked = [false; 3];
                        fx.retether_relay = Some((sys_id, *relay_ball_id));
                    } else {
                        *vulnerability_window_secs -= dt;
                    }
                }
            }
            HomefrontProgressRecord::SalvageResearch {
                werpost_activated: true,
                ..
            } => {
                if let Some(target_id) = live_hostile_target {
                    fx.werpost_attack = Some((sys_id, target_id, (dt as f64) * 350.0));
                }
            }
            HomefrontProgressRecord::StabilizeRift {
                arrays_cap_pct,
                held_low_secs,
                required_hold_secs,
                ..
            } => {
                if let Some(caps) = live_rift_caps {
                    *arrays_cap_pct = caps;
                }
                if arrays_cap_pct.iter().all(|&p| p < 0.20) {
                    *held_low_secs =
                        held_low_secs.saturating_add(dt).min(*required_hold_secs);
                    if *held_low_secs >= *required_hold_secs {
                        fx.settle_complete = true;
                    }
                } else {
                    *held_low_secs = 0;
                }
            }
            HomefrontProgressRecord::TrafficStop {
                survival_elapsed_secs,
                required_survival_secs,
                smugglers_escaped,
                civilians_harmed,
                ..
            } => {
                *survival_elapsed_secs = survival_elapsed_secs.saturating_add(dt);
                if *smugglers_escaped >= 5 || *civilians_harmed >= 3 {
                    rec.failed = true;
                    rec.failure_reason = Some("Traffic Stop objectives failed".into());
                    fx.despawn_fail = Some((sys_id, rec.spawned_ball_ids.clone()));
                } else if *survival_elapsed_secs >= *required_survival_secs
                    && runtime_record_objective_complete(rec)
                {
                    fx.settle_complete = true;
                }
            }
            _ => {}
        }
        Ok(fx)
    });

    let Ok(fx) = fx_res else {
        return;
    };
    if let Some((sys_id, old_hid, h_tid, pos, new_hid)) = fx.respawn_raid_hauler {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h.try_remove_ball(old_hid);
        spawn_ball_sync_with_claim(
            ctx,
            &sys_h,
            sys_id,
            Ball {
                id: new_hid,
                type_id: h_tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 180.0,
                inertia: 0.8,
                mass: 15_000_000.0,
                radius: 260.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(h_tid),
        );
    }
    if let Some((sys_id, relay_id)) = fx.retether_relay {
        set_homefront_relay_tethered(ctx, sys_id, relay_id, true);
    }
    if let Some((sys_id, base_id, dmg)) = fx.dread_siege_dmg {
        apply_homefront_direct_damage_in_space(ctx, sys_id, base_id, dmg);
        if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
            let _ = sys_h.try_update_combat_state(1_897_800_000 + inst_id * 100 + 13, |d_cs| {
                d_cs.capacitor = 0.0;
            });
        }
    }
    if let Some((sys_id, target_id, dmg)) = fx.werpost_attack {
        apply_homefront_direct_damage_in_space(ctx, sys_id, target_id, dmg);
    }
    if let Some(next_w) = fx.spawn_aar_wave {
        spawn_next_aar_wave_in_space(ctx, inst_id, next_w);
    }
    if let Some((sys_id, pocket_pos)) = fx.spawn_basileus {
        let mut basileus_ids = None;
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) {
                rec.spawned_ball_ids.push(bid);
                rec.spawned_ball_ids.push(cid);
                if let HomefrontProgressRecord::AbyssalArtifactRecovery { cache_ball_id, .. } =
                    &mut rec.progress
                {
                    *cache_ball_id = Some(cid);
                }
                basileus_ids = Some((bid, cid));
            }
            Ok(())
        });
        if let Some((bid, cid)) = basileus_ids {
            let sys_h = ctx.systems.get_or_spawn(sys_id);
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: bid,
                    type_id: HOMEFRONT_AAR_BASILEUS_TYPE_ID,
                    position: pocket_pos + DVec3::new(0.0, 2_000.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 380.0,
                    inertia: 0.4,
                    mass: 20_000_000.0,
                    radius: 260.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(HOMEFRONT_AAR_BASILEUS_TYPE_ID),
            );
            register_homefront_npc_entity(
                ctx,
                sys_id,
                bid,
                HOMEFRONT_AAR_BASILEUS_TYPE_ID,
                500_026,
                "HomefrontHostile",
                pocket_pos,
                [0.25, 0.25, 0.25, 0.25],
                650.0,
                12_000.0,
                40_000.0,
            );
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                Ball {
                    id: cid,
                    type_id: HOMEFRONT_AAR_CACHE_TYPE_ID,
                    position: pocket_pos + DVec3::new(2_500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 500_000.0,
                    radius: 150.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 15_000.0,
                    max_shield_hp: 15_000.0,
                    armor_hp: 15_000.0,
                    max_armor_hp: 15_000.0,
                    structure_hp: 15_000.0,
                    max_structure_hp: 15_000.0,
                    ..CombatState::default()
                },
            );
        }
    }
    if let Some((sys_id, balls)) = fx.despawn_fail {
        despawn_homefront_balls_sync(ctx, sys_id, inst_id, &balls);
    }
    if fx.settle_complete {
        try_settle_completed_homefront_live(ctx, inst_id);
        return;
    }

    // Ensure AAR wave escorts spawned inside DB-only destruction hooks also have ExtendedNpcEntity AI.
    if let HomefrontProgressRecord::AbyssalArtifactRecovery {
        wave_hostile_ball_ids,
        ..
    } = &pre.progress
    {
        if !wave_hostile_ball_ids.is_empty() {
            let missing_ids: Vec<i64> = with_combat_ephemeral(ctx, |rt| {
                wave_hostile_ball_ids
                    .iter()
                    .copied()
                    .filter(|id| !rt.npc_entities.contains_key(id))
                    .collect()
            });
            if !missing_ids.is_empty() {
                let sys_h = ctx.systems.get_or_spawn(pre.solar_system_id);
                let view = sys_h.view();
                for (idx, eid) in missing_ids.into_iter().enumerate() {
                    if let Some(ball) = view.balls.get(&eid) {
                        register_homefront_npc_entity(
                            ctx,
                            pre.solar_system_id,
                            eid,
                            ball.type_id,
                            pre.blueprint.hostile_faction_id,
                            "HomefrontHostile",
                            ball.position,
                            [0.25, 0.25, 0.25, 0.25],
                            100.0 + (idx as f64) * 5.0,
                            3_500.0,
                            20_000.0,
                        );
                    }
                }
            }
        }
    }
}

/// Async entry point for server clock tick (`pve_special_sites` / `combat_npc_ai`).
pub async fn tick_homefront_operations_on_clock(ctx: &ServerContext) {
    tick_homefront_operations_sync(ctx);
}

pub fn handle_get_homefront_site_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;

    let sys_id = if let Some(arg0) = args.first().filter(|a| !matches!(a, PyRep::None)) {
        let raw = arg0.as_i64().ok_or_else(|| BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: "GetHomefrontSiteInstances requires an integer solarSystemID".into(),
            client_error: None,
        })?;
        u32::try_from(raw)
            .ok()
            .filter(|id| ctx.sde.solar_systems.contains_key(id))
            .ok_or_else(|| BridgeRpcError {
                code: "UnknownSolarSystem".into(),
                message: format!("Unknown solar system ID {raw}"),
                client_error: None,
            })?
    } else {
        session
            .solar_system_id
            .or_else(|| {
                session.character_id.and_then(|cid| {
                    ctx.db
                        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                })
            })
            .ok_or_else(|| BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Action requires an active solar system session".into(),
                client_error: None,
            })?
    };

    let reply = ctx.db.with_snapshot(|snap| {
        let mut entries: Vec<(u32, i64)> = Vec::new();
        if let Some(list) = snap.pve_special_sites.homefront_sites.get(&sys_id) {
            for s in list {
                let rt_closed = snap
                    .dungeon_runtime
                    .homefront_runtimes
                    .get(&s.instance_id)
                    .is_some_and(|r| r.completed || r.failed);
                if !s.completed && !rt_closed {
                    entries.push((s.dungeon_id, s.instance_id));
                }
            }
        }
        entries.sort_unstable_by_key(|&(_, instance_id)| instance_id);
        let items: Vec<PyRep> = entries
            .into_iter()
            .map(|(dungeon_id, instance_id)| {
                PyRep::keyval([
                    ("dungeonID", PyRep::Integer(dungeon_id as i64)),
                    ("instanceID", PyRep::Integer(instance_id)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ])
            })
            .collect();
        PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::List(items))])
    });

    Ok(reply)
}

#[cfg(test)]
#[path = "pve_homefront_sites_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pve_homefront_sites_edges_tests.rs"]
mod edges_tests;
